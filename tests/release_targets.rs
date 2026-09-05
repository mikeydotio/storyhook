//! The supported release targets and the local assembly path (SH-540).
//!
//! `make test` is a macOS build, so it cannot compile Linux-only code. The
//! release command closes that gap locally with `cross`, packages all four
//! supported targets, verifies them, and uploads one draft. These tests pin
//! the repository-level contracts around that expensive path without running
//! a real release or reaching GitHub.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use storyhook_test_support::scratch_dir_named;

/// The repository root, which is this package's manifest directory: the root
/// package and the workspace root are the same crate here (see `Cargo.toml`'s
/// opening comment).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Reads a repository file, failing with the path rather than with `None`.
fn read(relative: &str) -> String {
    let path: PathBuf = repo_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

// ---------------------------------------------------------------------------
// The manifest: what the Linux target resolves to
// ---------------------------------------------------------------------------

/// Every dependency declared under a `cfg(...)` that names Linux and whose
/// crate name mentions `secret-service`, paired with the feature list it
/// selects.
///
/// Found by scanning rather than by naming one key, so that swapping the
/// backing crate — or reformatting the `cfg` expression — keeps the invariant
/// under test instead of quietly retiring it.
fn linux_secret_service_dependencies() -> Vec<(String, Vec<String>)> {
    let manifest: toml::Value = read("Cargo.toml")
        .parse()
        .expect("Cargo.toml must be valid TOML");

    let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) else {
        panic!("Cargo.toml must declare per-target dependency tables");
    };

    let mut found = Vec::new();
    for (cfg, table) in targets {
        if !cfg.contains("linux") {
            continue;
        }
        let Some(dependencies) = table.get("dependencies").and_then(toml::Value::as_table) else {
            continue;
        };
        for (name, spec) in dependencies {
            if !name.contains("secret-service") {
                continue;
            }
            let features = spec
                .get("features")
                .and_then(toml::Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            found.push((name.clone(), features));
        }
    }
    found
}

/// `secret-service` 5.x compiles only if its consumer picks a runtime: with no
/// `rt-*` feature selected it is a bare `compile_error!` in `session.rs`, which
/// is what took every Linux release build down. The feature has to be named
/// here because nothing downstream of us will pick one by default.
#[test]
fn the_linux_keyring_dependency_picks_a_secret_service_runtime() {
    let dependencies = linux_secret_service_dependencies();
    assert!(
        !dependencies.is_empty(),
        "no Linux secret-service dependency found in Cargo.toml — if the \
         backing crate was replaced, this test needs to learn the new name; \
         if it was removed, so should this test be"
    );

    for (name, features) in dependencies {
        assert!(
            features.iter().any(|feature| feature.starts_with("rt-")),
            "`{name}` selects no `rt-*` feature, so `secret-service` picks no \
             runtime and refuses to compile: every Linux release artifact \
             fails to build. Enable one of its forwarding features (see \
             SH-259)."
        );
    }
}

/// The runtime has to be a pure-Rust one. `crypto-openssl` would put
/// `openssl-sys` on the Linux path, and `aarch64-unknown-linux-gnu` is built
/// under `cross` — cross-compiling a system OpenSSL is a second, worse version
/// of the problem this test exists to prevent. The same reasoning already
/// chose `zbus` over the `libdbus`-backed alternative.
#[test]
fn the_linux_secret_service_runtime_needs_no_system_library() {
    for (name, features) in linux_secret_service_dependencies() {
        for feature in features.iter().filter(|f| f.starts_with("rt-")) {
            assert!(
                feature.ends_with("-crypto-rust"),
                "`{name}` selects `{feature}`, which links a system crypto \
                 library; the aarch64 Linux artifact is cross-compiled and \
                 cannot count on one. Pick the `-crypto-rust` variant."
            );
        }
    }
}

// ---------------------------------------------------------------------------
// SH-540: releases are assembled locally, never by a tag-triggered Action
// ---------------------------------------------------------------------------

#[test]
fn no_github_workflow_runs_for_version_tags() {
    for path in tracked_files(&repo_root(), ".github/workflows/*") {
        if !path.is_file() {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("workflow is readable");
        let workflow: serde_yml::Value =
            serde_yml::from_str(&source).expect("tracked workflow must be valid YAML");
        let triggers = workflow
            .get("on")
            .or_else(|| workflow.get(serde_yml::Value::Bool(true)));
        let tag_push = triggers
            .and_then(|on| on.get("push"))
            .and_then(|push| push.get("tags"));
        assert!(
            tag_push.is_none(),
            "{} runs on tag pushes; releases must be assembled locally (SH-540)",
            path.display()
        );
    }
}

#[test]
fn release_sh_assembles_locally_and_never_delegates_to_actions() {
    let source = read("scripts/release.sh");
    assert!(source.contains("scripts/build-release-assets.sh --check"));
    assert!(source.contains("scripts/build-release-assets.sh"));
    assert!(source.contains("gh release create"));
    for required in [
        "--verify-tag",
        "--draft",
        "--generate-notes",
        "--notes-file",
    ] {
        assert!(
            source.contains(required),
            "release creation is missing {required}"
        );
    }
    for forbidden in ["gh run", "gh workflow", "release.yml"] {
        assert!(
            !source.contains(forbidden),
            "release.sh still delegates through GitHub Actions with `{forbidden}`"
        );
    }

    let check = source
        .find("scripts/build-release-assets.sh --check")
        .unwrap();
    let branch = source.find("git switch -c").unwrap();
    assert!(
        check < branch,
        "toolchain preflight must precede version mutation"
    );
    let build = source.rfind("scripts/build-release-assets.sh").unwrap();
    let tag = source.find("git tag -a").unwrap();
    assert!(
        build < tag,
        "all artifacts must be verified before the tag is minted"
    );
}

fn write_executable(path: &Path, body: &str) {
    std::fs::write(path, body).expect("writing command shim");
    let mut permissions = path.metadata().unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

fn release_tool_shims() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let fixture = scratch_dir_named("local-release");
    let bin = fixture.path().join("bin");
    let target = fixture.path().join("target");
    std::fs::create_dir_all(&bin).unwrap();

    write_executable(&bin.join("uname"), "#!/bin/bash\necho Darwin\n");
    write_executable(
        &bin.join("docker"),
        "#!/bin/bash\n[ \"$1\" = info ] || exit 2\nexit 0\n",
    );
    let builder = r#"#!/bin/bash
set -eu
echo "$(basename "$0") $*" >> "$RELEASE_TEST_LOG"
target=""
previous=""
for argument in "$@"; do
  if [ "$previous" = "--target" ]; then target="$argument"; fi
  previous="$argument"
done
if [ -n "${RELEASE_FAIL_TARGET:-}" ] && [ "$target" = "$RELEASE_FAIL_TARGET" ]; then
  exit 42
fi
if [ -n "$target" ]; then
  mkdir -p "$CARGO_TARGET_DIR/$target/release"
  printf '#!/bin/sh\nexit 0\n' > "$CARGO_TARGET_DIR/$target/release/story"
  chmod +x "$CARGO_TARGET_DIR/$target/release/story"
fi
if [ "$(basename "$0")" = rustup ] && [ "${1:-}" = target ]; then
  printf 'x86_64-apple-darwin\naarch64-apple-darwin\n'
fi
if [ "$(basename "$0")" = rustup ] && [ "${3:-}" = rustc ]; then
  echo 'rustc 1.test'
fi
"#;
    write_executable(&bin.join("rustup"), builder);
    write_executable(&bin.join("cross"), builder);
    write_executable(
        &bin.join("file"),
        r#"#!/bin/bash
path="${@: -1}"
case "$path" in
  *x86_64-unknown-linux-gnu*) echo 'ELF 64-bit LSB executable, x86-64' ;;
  *aarch64-unknown-linux-gnu*) echo 'ELF 64-bit LSB executable, ARM aarch64' ;;
  *x86_64-apple-darwin*) echo 'Mach-O 64-bit executable x86_64' ;;
  *aarch64-apple-darwin*) echo 'Mach-O 64-bit executable arm64' ;;
  *) echo 'ASCII text' ;;
esac
"#,
    );
    write_executable(
        &bin.join("tar"),
        r#"#!/bin/bash
if [ "${RELEASE_CORRUPT_ARCHIVE:-0}" = 1 ] && [ "${1:-}" = -tzf ]; then
  printf 'story\nextra\n'
  exit 0
fi
exec /usr/bin/tar "$@"
"#,
    );

    (fixture, bin, target)
}

fn run_asset_builder(extra_env: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf, Output) {
    let (fixture, bin, target) = release_tool_shims();
    let output_dir = fixture.path().join("assets");
    let log = fixture.path().join("commands.log");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut command = Command::new("/bin/bash");
    command
        .current_dir(repo_root())
        .arg(repo_root().join("scripts/build-release-assets.sh"))
        .args(["--version", "v9.9.9", "--output-dir"])
        .arg(&output_dir)
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", target)
        .env("RELEASE_TEST_LOG", log);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let result = command.output().expect("running local release builder");
    (fixture, output_dir, result)
}

#[test]
fn local_builder_creates_and_checksums_all_four_archives() {
    let (fixture, output_dir, result) = run_asset_builder(&[]);
    assert!(
        result.status.success(),
        "builder failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let log = std::fs::read_to_string(fixture.path().join("commands.log")).unwrap();
    for target in release_targets() {
        assert!(log.contains(&format!("--locked --release --target {target}")));
        if target.contains("linux") {
            assert!(log.contains(&format!(
                "cross +stable build --locked --release --target {target}"
            )));
        } else {
            assert!(log.contains(&format!(
                "rustup run stable cargo build --locked --release --target {target}"
            )));
        }
        let archive = output_dir.join(format!("story-{target}.tar.gz"));
        assert!(archive.is_file(), "missing {}", archive.display());
        let listing = Command::new("tar")
            .args(["-tzf"])
            .arg(&archive)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&listing.stdout).trim(), "story");
    }

    let checksums = std::fs::read_to_string(output_dir.join("SHA256SUMS")).unwrap();
    assert_eq!(checksums.lines().count(), 4);
    let verified = Command::new("shasum")
        .args(["-a", "256", "-c", "SHA256SUMS"])
        .current_dir(&output_dir)
        .output()
        .unwrap();
    assert!(
        verified.status.success(),
        "checksum verification failed: {}",
        String::from_utf8_lossy(&verified.stderr)
    );
}

#[test]
fn failed_target_removes_stale_assets_and_writes_no_complete_manifest() {
    let (fixture, bin, target) = release_tool_shims();
    let output_dir = fixture.path().join("assets");
    std::fs::create_dir_all(&output_dir).unwrap();
    for target in release_targets() {
        std::fs::write(output_dir.join(format!("story-{target}.tar.gz")), "stale").unwrap();
    }
    std::fs::write(output_dir.join("SHA256SUMS"), "stale").unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let result = Command::new("/bin/bash")
        .current_dir(repo_root())
        .arg(repo_root().join("scripts/build-release-assets.sh"))
        .args(["--version", "v9.9.9", "--output-dir"])
        .arg(&output_dir)
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", target)
        .env("RELEASE_TEST_LOG", fixture.path().join("commands.log"))
        .env("RELEASE_FAIL_TARGET", "x86_64-unknown-linux-gnu")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(42));
    assert!(!output_dir.join("SHA256SUMS").exists());
    for target in release_targets() {
        assert!(!output_dir.join(format!("story-{target}.tar.gz")).exists());
    }
}

#[test]
fn malformed_archive_is_refused_before_its_digest_is_accepted() {
    let (_fixture, output_dir, result) = run_asset_builder(&[("RELEASE_CORRUPT_ARCHIVE", "1")]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("exactly one top-level story"));
    assert!(!output_dir.join("SHA256SUMS").exists());
}

#[test]
fn builder_refuses_before_work_when_release_tools_are_missing() {
    let result = Command::new("/bin/bash")
        .current_dir(repo_root())
        .arg(repo_root().join("scripts/build-release-assets.sh"))
        .arg("--check")
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("rustup is required"));
}

fn publish_fixture() -> (tempfile::TempDir, PathBuf) {
    let fixture = scratch_dir_named("release-publish");
    let root = fixture.path();
    std::fs::create_dir_all(root.join("scripts")).unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::copy(
        repo_root().join("scripts/release.sh"),
        root.join("scripts/release.sh"),
    )
    .unwrap();
    std::fs::copy(
        repo_root().join("scripts/release-targets.sh"),
        root.join("scripts/release-targets.sh"),
    )
    .unwrap();
    std::fs::write(root.join("VERSION"), "v9.9.9\n").unwrap();
    write_executable(&root.join("bin/claude"), "#!/bin/bash\nexit 0\n");
    write_executable(
        &root.join("bin/gh"),
        r#"#!/bin/bash
set -eu
if [ "$1 $2" = "auth status" ]; then exit 0; fi
if [ "$1" = api ]; then
  endpoint=""
  for argument in "$@"; do
    case "$argument" in repos/*) endpoint="$argument" ;; esac
  done
  case "$endpoint" in
    *'/assets?'*)
      for target in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu x86_64-apple-darwin aarch64-apple-darwin; do
        if [ "${RELEASE_SCENARIO:-ok}" = missing ] && [ "$target" = aarch64-apple-darwin ]; then continue; fi
        digest="sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        if [ "${RELEASE_SCENARIO:-ok}" = bad-digest ] && [ "$target" = x86_64-unknown-linux-gnu ]; then digest="missing"; fi
        printf 'story-%s.tar.gz\t%s\n' "$target" "$digest"
      done
      ;;
    *)
      printf '101\ttrue\n'
      if [ "${RELEASE_SCENARIO:-ok}" = duplicate ]; then printf '102\ttrue\n'; fi
      ;;
  esac
  exit 0
fi
if [ "$1 $2" = "release edit" ]; then echo edit >> "$RELEASE_TEST_LOG"; exit 0; fi
if [ "$1 $2" = "release view" ]; then echo v9.9.9; exit 0; fi
exit 2
"#,
    );

    for args in [
        ["init", "-q", "-b", "main"].as_slice(),
        ["config", "user.email", "release@test"].as_slice(),
        ["config", "user.name", "release-test"].as_slice(),
        ["add", "-A"].as_slice(),
        ["commit", "-qm", "fixture"].as_slice(),
    ] {
        let status = Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }
    let log = root.join("publish.log");
    (fixture, log)
}

fn publish_with_scenario(scenario: &str) -> (Output, String) {
    let (fixture, log) = publish_fixture();
    let path = format!(
        "{}:{}",
        fixture.path().join("bin").display(),
        std::env::var("PATH").unwrap()
    );
    let result = Command::new("/bin/bash")
        .current_dir(fixture.path())
        .arg(fixture.path().join("scripts/release.sh"))
        .args(["--publish", "v9.9.9", "--yes"])
        .env("PATH", path)
        .env("RELEASE_SCENARIO", scenario)
        .env("RELEASE_TEST_LOG", &log)
        .output()
        .unwrap();
    let calls = std::fs::read_to_string(log).unwrap_or_default();
    (result, calls)
}

#[test]
fn publish_refuses_ambiguous_same_tag_releases() {
    let (result, calls) = publish_with_scenario("duplicate");
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("ambiguous same-tag"));
    assert!(calls.is_empty(), "an ambiguous release was published");
}

#[test]
fn publish_refuses_a_non_version_before_calling_github() {
    let (fixture, log) = publish_fixture();
    let path = format!(
        "{}:{}",
        fixture.path().join("bin").display(),
        std::env::var("PATH").unwrap()
    );
    let result = Command::new("/bin/bash")
        .current_dir(fixture.path())
        .arg(fixture.path().join("scripts/release.sh"))
        .args(["--publish", "v9.9.9\") | .[]", "--yes"])
        .env("PATH", path)
        .env("RELEASE_TEST_LOG", &log)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("vMAJOR.MINOR.PATCH"));
    assert!(!log.exists(), "invalid input reached GitHub");
}

#[test]
fn publish_refuses_missing_assets_and_bad_digests() {
    for (scenario, expected) in [("missing", "matching assets"), ("bad-digest", "SHA-256")] {
        let (result, calls) = publish_with_scenario(scenario);
        assert!(
            !result.status.success(),
            "{scenario} unexpectedly published"
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains(expected));
        assert!(calls.is_empty(), "{scenario} was published");
    }
}

#[test]
fn publish_edits_the_only_complete_digest_verified_draft() {
    let (result, calls) = publish_with_scenario("ok");
    assert!(
        result.status.success(),
        "valid draft refused: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(calls, "edit\n");
}

// ---------------------------------------------------------------------------
// The target definition is the source of truth
// ---------------------------------------------------------------------------

/// Target triples exported by the shared shell definition.
fn release_targets() -> Vec<String> {
    let output = std::process::Command::new("bash")
        .current_dir(repo_root())
        .args([
            "-c",
            "source scripts/release-targets.sh; printf '%s\\n' \"${RELEASE_TARGETS[@]}\"",
        ])
        .output()
        .expect("reading the release target definition");
    assert!(
        output.status.success(),
        "release-targets.sh must be valid Bash: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("release targets are UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn release_targets_are_exactly_the_four_installer_platforms() {
    assert_eq!(
        release_targets(),
        [
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "x86_64-apple-darwin",
            "aarch64-apple-darwin",
        ]
    );
}

/// Every tracked file matching `pathspec`, found by `git ls-files` rather
/// than a hand-maintained list — the same idiom `tests/store_isolation.rs`'s
/// `data_dir_harnesses` and `tests/harness_path_entries.rs` use for shell
/// scripts.
fn tracked_files(root: &std::path::Path, pathspec: &str) -> Vec<std::path::PathBuf> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--", pathspec])
        .output()
        .expect("listing this repository's tracked files");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );

    listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|path| root.join(std::str::from_utf8(path).expect("a UTF-8 path")))
        .collect()
}

/// Every tracked `Cargo.toml` — [`tracked_files`] narrowed to the manifests
/// [`target_os_dependency_tables`] reads.
fn tracked_manifests(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    tracked_files(root, "*Cargo.toml")
}

/// Every `target_os` a manifest's `[target.'cfg(...)'.dependencies]` tables
/// name, paired with the manifest path and the cfg key itself (for a
/// legible failure message).
///
/// Scans by shape — any `[target.*]` key containing `target_os = "…"`, with
/// a non-empty `dependencies` sub-table — rather than by one literal key, so
/// a new platform-gated dependency is caught the moment it is declared
/// rather than only once someone remembers to extend this test.
///
/// Deliberately narrower than "every `cfg(...)`": a table gated on
/// `cfg(unix)` or a bare target triple names no single OS, so it says
/// nothing this function can check against the release set's per-OS targets. That
/// gap is real and is not what this test closes — it closes the gap SH-260
/// found, which is a `target_os` literal naming a platform the release does
/// not build.
fn target_os_dependency_tables(manifest_path: &std::path::Path) -> Vec<(String, String)> {
    let manifest: toml::Value = std::fs::read_to_string(manifest_path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", manifest_path.display()))
        .parse()
        .unwrap_or_else(|e| panic!("{} must be valid TOML: {e}", manifest_path.display()));

    let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) else {
        return Vec::new();
    };

    let mut found = Vec::new();
    for (cfg, table) in targets {
        let has_dependencies = table
            .get("dependencies")
            .and_then(toml::Value::as_table)
            .is_some_and(|deps| !deps.is_empty());
        if !has_dependencies {
            continue;
        }
        // `cfg` is a key like `cfg(target_os = "windows")`. Extract every
        // quoted value following a `target_os` mention, rather than
        // requiring the key be exactly this shape, so `cfg(all(target_os =
        // "windows", target_arch = "x86_64"))` is still caught.
        let mut rest = cfg.as_str();
        while let Some(at) = rest.find("target_os") {
            rest = &rest[at + "target_os".len()..];
            let Some(open) = rest.find('"') else { break };
            let Some(close) = rest[open + 1..].find('"') else {
                break;
            };
            let os = rest[open + 1..open + 1 + close].to_string();
            found.push((cfg.clone(), os));
            rest = &rest[open + 1 + close + 1..];
        }
    }
    found
}

/// Maps a `target_os` literal to the substring a release target would
/// spell it with, e.g. `"linux"` -> the targets ending
/// `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`.
///
/// Explicit and closed on purpose: an OS this function has not been taught
/// panics rather than silently passing, because "no dependency table names
/// an OS the release doesn't build" is a claim this function can only make
/// about OSes it knows how to look for.
fn matrix_substring_for(target_os: &str) -> &'static str {
    match target_os {
        "macos" => "-apple-darwin",
        "linux" => "-unknown-linux-",
        "windows" => "-pc-windows-",
        other => panic!(
            "target_os \"{other}\" has no known release-target triple shape — \
             teach `matrix_substring_for` what a target for it looks like \
             before this test can vouch for it"
        ),
    }
}

/// The general invariant SH-259 deliberately left unshipped, because it
/// failed at the time (SH-259's own Linux fix landed before this test could
/// be written against a clean tree): every `cfg(target_os = ...)`
/// dependency table, in every tracked manifest, corresponds to a target the
/// local release actually builds. SH-260 found the instance this was
/// generalized from — `windows-native-keyring-store`, gated on Windows and
/// activated by the default `github-sync` feature, for a platform nothing
/// here builds, tests, or ships.
///
/// If a platform's dependency table is later added deliberately (Windows
/// joining the release set, say), this test starts passing for it without
/// modification — the shared target definition changes to add support.
/// If a table is removed instead, as SH-260 did, there is nothing left for
/// this test to check for that OS and it is silent about it, which is
/// correct: an absent claim needs no verification.
#[test]
fn every_platform_gated_dependency_table_targets_a_built_platform() {
    let root = repo_root();
    let manifests = tracked_manifests(&root);
    assert!(
        manifests
            .iter()
            .any(|path| path == &root.join("Cargo.toml")),
        "the manifest scan did not find the repository's own Cargo.toml — \
         `git ls-files -- '*Cargo.toml'` proved nothing"
    );

    let matrix_targets = release_targets();

    for manifest_path in &manifests {
        for (cfg, target_os) in target_os_dependency_tables(manifest_path) {
            let substring = matrix_substring_for(&target_os);
            let relative = manifest_path.strip_prefix(&root).unwrap_or(manifest_path);
            assert!(
                matrix_targets
                    .iter()
                    .any(|target| target.contains(substring)),
                "{}'s `[target.'{cfg}'.dependencies]` names target_os \
                 \"{target_os}\", but no local release target matches \
                 (release targets: {matrix_targets:?}). Either add {target_os} \
                 to the target definition and prove the dependency by \
                 building it, or drop the dependency for a platform this \
                 project does not ship (SH-260).",
                relative.display()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// SH-276: the manifest scan's sibling, for `cfg` arms with no manifest entry
// ---------------------------------------------------------------------------

/// The one file allowed to contain a `target_os = "…"` string that is not a
/// real `cfg` predicate: this file's own control test below, which feeds
/// [`cfg_target_oses`] example attribute text as string literals to prove it
/// matches what it claims to. Byte for byte, an escaped fixture like
/// `"#[cfg(target_os = \"windows\")]"` is indistinguishable from a genuine
/// attribute once quote *characters* are all this scan looks for — scanning
/// this file for its own examples would flag the tests that prove the
/// scanner works, not a real defect. Excluded the same way
/// `tests/store_isolation.rs` excludes `REAL_STORE_OWNER`, with the same
/// staleness check on the exclusion.
const SOURCE_SCAN_OWNER: &str = "tests/release_targets.rs";

/// Every `target_os` literal inside a `cfg`-shaped line of `text`, paired
/// with its 1-indexed line number (for a legible failure message).
///
/// Line-based and deliberately crude, in the spirit of
/// `tests/invoker_seam.rs::the_legacy_write_path_is_gone`: a line is a
/// candidate only if its trimmed form does not open a comment (`//` or `*`
/// — doc comments describe a platform without claiming to support it) and
/// the raw line contains `cfg`, so `#[cfg(target_os = "windows")]` and
/// `cfg!(target_os = "macos")` both qualify while an unrelated mention of
/// the bare identifier `target_os` (a parameter name, a doc reference) does
/// not. Quoted values are extracted with the same loop
/// `target_os_dependency_tables` above uses on a TOML key, so
/// `not(any(target_os = "macos", target_os = "windows"))` yields both.
///
/// Single-line only — every `cfg(target_os = ...)` this tree writes today is
/// one line, the same scope `target_os_dependency_tables` has for a TOML
/// key. A multi-line attribute would need a different scanner; nothing here
/// writes one.
fn cfg_target_oses(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('*') || !line.contains("cfg") {
            continue;
        }
        let mut rest = line;
        while let Some(at) = rest.find("target_os") {
            rest = &rest[at + "target_os".len()..];
            let Some(open) = rest.find('"') else { break };
            let Some(close) = rest[open + 1..].find('"') else {
                break;
            };
            let os = rest[open + 1..open + 1 + close].to_string();
            found.push((index + 1, os));
            rest = &rest[open + 1 + close + 1..];
        }
    }
    found
}

/// The pattern above matches the shapes this tree actually writes, and
/// stays quiet on shapes that only mention the same identifier — the same
/// self-check
/// `store_isolation.rs::the_real_store_regression_pattern_matches_what_it_claims_to`
/// makes for its own predicate. A scan that matched everything, or nothing,
/// would pass every assertion built on it for the wrong reason.
#[test]
fn the_source_arm_pattern_matches_what_it_claims_to() {
    assert_eq!(
        cfg_target_oses("#[cfg(target_os = \"windows\")]"),
        vec![(1, "windows".to_string())]
    );
    assert_eq!(
        cfg_target_oses("    if cfg!(target_os = \"macos\") && unindexed.is_dir() {"),
        vec![(1, "macos".to_string())]
    );
    assert_eq!(
        cfg_target_oses(
            "#[cfg(not(any(target_os = \"macos\", target_os = \"linux\", target_os = \"windows\")))]"
        ),
        vec![
            (1, "macos".to_string()),
            (1, "linux".to_string()),
            (1, "windows".to_string()),
        ]
    );
    // A doc comment that quotes the pattern is not the pattern.
    assert!(cfg_target_oses("/// See #[cfg(target_os = \"windows\")] above.").is_empty());
    assert!(cfg_target_oses("* #[cfg(target_os = \"windows\")] in a block comment").is_empty());
    // No `cfg` on the line at all.
    assert!(cfg_target_oses("let target_os = \"windows\";").is_empty());
    // `cfg`, and the identifier, but no quoted value to find.
    assert!(
        cfg_target_oses("fn matrix_substring_for(target_os: &str) -> &'static str {").is_empty()
    );
    // `#[cfg(unix)]` names no target_os at all.
    assert!(cfg_target_oses("#[cfg(unix)]").is_empty());
}

/// The general invariant SH-276 built from the one SH-260 shipped for
/// dependency tables: every `target_os` a tracked source file's `cfg` arms
/// name corresponds to a target the local release actually builds.
/// SH-260's manifest scan could not see `src/clipboard.rs`'s and
/// `src/web.rs`'s Windows arms — hardcoded argv, no Cargo dependency, so
/// nothing about them appeared in a manifest. This is the same rule,
/// applied to the artifact type that hid them.
///
/// As with the manifest scan, this is target-derived rather than a
/// Windows-specific block: it goes green for a new platform the day that
/// platform's target joins `release-targets.sh`, with no edit here. No
/// allowlist — a real hit gets fixed or the target set grows to justify it, the
/// same policy `dead_public_surface.rs` states outright ("extend the scan
/// rather than special-casing the name").
#[test]
fn every_platform_gated_source_arm_targets_a_built_platform() {
    let root = repo_root();
    assert!(
        root.join(SOURCE_SCAN_OWNER).is_file(),
        "SOURCE_SCAN_OWNER is stale — {SOURCE_SCAN_OWNER} no longer exists, so \
         this scan's one exclusion excludes nothing"
    );

    let matrix_targets = release_targets();

    let mut hits = 0;
    for source_path in tracked_files(&root, "*.rs") {
        let relative = source_path
            .strip_prefix(&root)
            .unwrap_or(&source_path)
            .to_string_lossy()
            .replace('\\', "/");
        if relative == SOURCE_SCAN_OWNER {
            continue;
        }
        let text = std::fs::read_to_string(&source_path)
            .unwrap_or_else(|e| panic!("reading {relative}: {e}"));
        for (line, target_os) in cfg_target_oses(&text) {
            hits += 1;
            let substring = matrix_substring_for(&target_os);
            assert!(
                matrix_targets
                    .iter()
                    .any(|target| target.contains(substring)),
                "{relative}:{line} names target_os \"{target_os}\" in a `cfg` \
                 arm, but no local release target matches (release targets: \
                 {matrix_targets:?}). Either add {target_os} to the target \
                 definition and prove the arm by building it, or delete \
                 the arm for a platform this project does not ship (SH-276)."
            );
        }
    }

    // A scan that matched nothing would pass the loop above by vacuum.
    // src/clipboard.rs and src/web.rs still carry real macOS and Linux arms
    // after SH-276's fix, so this floor stays meaningful rather than
    // needing a fixture of its own.
    assert!(
        hits >= 10,
        "this scan is supposed to find every `target_os` in a tracked source \
         file's `cfg` arms, and it found {hits}. The pattern is broken, not \
         the tree."
    );
}

/// A guard on the guard: `read` resolves against the repository, and a test
/// that silently read nothing would pass every assertion above by vacuum.
#[test]
fn the_repository_files_under_test_are_the_real_ones() {
    assert!(
        repo_root().join("Cargo.toml").is_file(),
        "repo_root() must resolve to a checkout, not a build directory"
    );
    assert!(
        read("Cargo.toml").contains("name = \"storyhook\""),
        "the manifest read here must be storyhook's own"
    );
}
