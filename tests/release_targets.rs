//! The supported release targets and the local assembly path (SH-540).
//!
//! `make test` is a macOS build, so it cannot compile Linux-only code. The
//! release command closes that gap locally with a pinned private Rust
//! toolchain and a Lima guest, packages all four supported targets, verifies
//! them, and uploads one draft. These tests pin the repository-level contracts
//! around that expensive path without running a real release or reaching
//! GitHub.

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
/// `openssl-sys` on the Linux path, and one Linux artifact is cross-compiled
/// in Lima — cross-compiling a system OpenSSL is a second, worse version of
/// the problem this test exists to prevent. The same reasoning already chose
/// `zbus` over the `libdbus`-backed alternative.
#[test]
fn the_linux_secret_service_runtime_needs_no_system_library() {
    for (name, features) in linux_secret_service_dependencies() {
        for feature in features.iter().filter(|f| f.starts_with("rt-")) {
            assert!(
                feature.ends_with("-crypto-rust"),
                "`{name}` selects `{feature}`, which links a system crypto \
                 library; the cross-compiled Linux artifact cannot count on \
                 one. Pick the `-crypto-rust` variant."
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

#[test]
fn release_builder_depends_on_capabilities_not_rustup_cross_or_docker() {
    let builder = read("scripts/build-release-assets.sh");
    let linux_runner = read("scripts/release-linux.sh");
    let help = read("scripts/release.sh");

    for obsolete in ["rustup", "cross +stable", "docker info"] {
        assert!(
            !builder.contains(obsolete),
            "the release builder still requires the obsolete `{obsolete}` path"
        );
    }
    for required in [
        "scripts/release-toolchain.lock",
        "limactl",
        "CARGO_PROFILE_RELEASE_STRIP=symbols",
        "glibc 2.31",
    ] {
        assert!(
            builder.contains(required) || linux_runner.contains(required),
            "the capability-based builder is missing `{required}`"
        );
    }
    assert!(!help.contains("rustup stable"));
    assert!(!help.contains("running Docker engine"));
}

#[test]
fn release_toolchain_lock_pins_every_required_upstream_component() {
    let lock = read("scripts/release-toolchain.lock");
    let rows: Vec<Vec<&str>> = lock
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split('\t').collect())
        .collect();

    assert_eq!(rows.len(), 12, "the lock must contain 12 component rows");
    for row in &rows {
        assert_eq!(
            row.len(),
            4,
            "lock row must be component, target, URL, hash"
        );
        assert!(matches!(row[0], "cargo" | "rustc" | "rust-std"));
        assert!(
            release_targets().contains(&row[1].to_string()),
            "{} is not a release platform",
            row[1]
        );
        assert!(
            row[2].starts_with("https://static.rust-lang.org/dist/2026-09-03/")
                && row[2].contains("-1.98.1-"),
            "component URL is not pinned to Rust 1.98.1: {}",
            row[2]
        );
        assert_eq!(row[3].len(), 64, "component hash must be SHA-256");
        assert!(row[3].bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    for target in release_targets() {
        assert!(
            rows.iter()
                .any(|row| row[0] == "rust-std" && row[1] == target),
            "missing rust-std for {target}"
        );
        for host_component in ["cargo", "rustc"] {
            assert!(
                rows.iter()
                    .any(|row| row[0] == host_component && row[1] == target),
                "missing {host_component} for possible host {target}"
            );
        }
    }
}

fn write_executable(path: &Path, body: &str) {
    std::fs::write(path, body).expect("writing command shim");
    let mut permissions = path.metadata().unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

fn release_tool_shims() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf, PathBuf) {
    let fixture = scratch_dir_named("local-release");
    let bin = fixture.path().join("bin");
    let target = fixture.path().join("target");
    let toolchain = fixture.path().join("toolchain");
    let linux_runner = fixture.path().join("release-linux");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(toolchain.join("bin")).unwrap();

    write_executable(
        &bin.join("uname"),
        "#!/bin/bash\ncase \"${1:-}\" in -s) echo Darwin ;; -m) echo arm64 ;; *) echo Darwin ;; esac\n",
    );
    write_executable(&bin.join("xcrun"), "#!/bin/bash\necho /usr/bin/clang\n");
    let builder = r#"#!/bin/bash
set -eu
# A real rustc reports the toolchain root it lives in, and
# `ensure_release_toolchain` refuses a toolchain that does not (SH-576).
# Answered before the log line, so the verification probe never appears in the
# command log the packaging assertions below read.
if [ "${1:-}" = --print ] && [ "${2:-}" = sysroot ]; then
  cd "$(dirname "$0")/.." && pwd -P
  exit 0
fi
echo "$(basename "$0") $*" >> "$RELEASE_TEST_LOG"
echo "strip=${CARGO_PROFILE_RELEASE_STRIP:-} x86_linker=${CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER:-} rustc=${RUSTC:-}" >> "$RELEASE_TEST_LOG"
target=""
output=""
previous=""
for argument in "$@"; do
  if [ "$previous" = "--target" ]; then target="$argument"; fi
  if [ "$previous" = "-o" ]; then output="$argument"; fi
  previous="$argument"
done
if [ -n "${RELEASE_FAIL_TARGET:-}" ] && [ "$target" = "$RELEASE_FAIL_TARGET" ]; then
  exit 42
fi
if [ -n "$output" ]; then
  mkdir -p "$(dirname "$output")"
  printf '#!/bin/sh\nexit 0\n' > "$output"
  chmod +x "$output"
elif [ -n "$target" ]; then
  mkdir -p "$CARGO_TARGET_DIR/$target/release"
  printf '#!/bin/sh\nexit 0\n' > "$CARGO_TARGET_DIR/$target/release/story"
  chmod +x "$CARGO_TARGET_DIR/$target/release/story"
fi
"#;
    write_executable(&toolchain.join("bin/rustc"), builder);
    write_executable(&toolchain.join("bin/cargo"), builder);
    write_executable(&bin.join("cc"), "#!/bin/bash\nexit 0\n");
    write_executable(&bin.join("x86_64-linux-gnu-gcc"), "#!/bin/bash\nexit 0\n");
    write_executable(
        &linux_runner,
        r#"#!/bin/bash
set -eu
echo "lima $* CARGO_PROFILE_RELEASE_STRIP=symbols" >> "$RELEASE_TEST_LOG"
[ "${1:-}" = --check ] && exit 0
target=""
output=""
previous=""
for argument in "$@"; do
  if [ "$previous" = "--target" ]; then target="$argument"; fi
  if [ "$previous" = "--output" ]; then output="$argument"; fi
  previous="$argument"
done
if [ -n "${RELEASE_FAIL_TARGET:-}" ] && [ "$target" = "$RELEASE_FAIL_TARGET" ]; then
  exit 42
fi
mkdir -p "$(dirname "$output")"
printf '#!/bin/sh\nexit 0\n' > "$output"
chmod +x "$output"
"#,
    );
    write_executable(
        &bin.join("file"),
        r#"#!/bin/bash
path="${@: -1}"
case "$path" in
  *x86_64-unknown-linux-gnu*)
    if [ "${RELEASE_UNSTRIPPED:-0}" = 1 ]; then
      echo 'ELF 64-bit LSB executable, x86-64, not stripped'
    else
      echo 'ELF 64-bit LSB executable, x86-64, stripped'
    fi
    ;;
  *aarch64-unknown-linux-gnu*)
    if [ "${RELEASE_UNSTRIPPED:-0}" = 1 ]; then
      echo 'ELF 64-bit LSB executable, ARM aarch64, not stripped'
    else
      echo 'ELF 64-bit LSB executable, ARM aarch64, stripped'
    fi
    ;;
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

    (fixture, bin, target, toolchain, linux_runner)
}

fn run_asset_builder(extra_env: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf, Output) {
    let (fixture, bin, target, toolchain, linux_runner) = release_tool_shims();
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
        .env("RELEASE_TEST_LOG", log)
        .env("STORYHOOK_RELEASE_TOOLCHAIN_DIR", &toolchain)
        .env("STORYHOOK_RELEASE_LINUX_RUNNER", linux_runner);
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
        if target.contains("linux") {
            assert!(log.contains(&format!("lima --target {target}")));
            assert!(log.contains("CARGO_PROFILE_RELEASE_STRIP=symbols"));
        } else {
            assert!(log.contains(&format!("cargo build --locked --release --target {target}")));
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
    let (fixture, bin, target, toolchain, linux_runner) = release_tool_shims();
    let output_dir = fixture.path().join("assets");
    std::fs::create_dir_all(&output_dir).unwrap();
    for target in release_targets() {
        std::fs::write(output_dir.join(format!("story-{target}.tar.gz")), "stale").unwrap();
    }
    std::fs::write(output_dir.join("SHA256SUMS"), "stale").unwrap();
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let result = Command::new("/bin/bash")
        .current_dir(repo_root())
        .arg(repo_root().join("scripts/build-release-assets.sh"))
        .args(["--version", "v9.9.9", "--output-dir"])
        .arg(&output_dir)
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", target)
        .env("RELEASE_TEST_LOG", fixture.path().join("commands.log"))
        .env("RELEASE_FAIL_TARGET", "x86_64-unknown-linux-gnu")
        .env("STORYHOOK_RELEASE_TOOLCHAIN_DIR", toolchain)
        .env("STORYHOOK_RELEASE_LINUX_RUNNER", linux_runner)
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
fn check_proves_all_four_target_capabilities_without_creating_archives() {
    let (fixture, bin, target, toolchain, linux_runner) = release_tool_shims();
    let log = fixture.path().join("commands.log");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let result = Command::new("/bin/bash")
        .current_dir(repo_root())
        .arg(repo_root().join("scripts/build-release-assets.sh"))
        .arg("--check")
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", target)
        .env("RELEASE_TEST_LOG", &log)
        .env("STORYHOOK_RELEASE_TOOLCHAIN_DIR", toolchain)
        .env("STORYHOOK_RELEASE_LINUX_RUNNER", linux_runner)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "preflight failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let calls = std::fs::read_to_string(log).unwrap();
    for target in ["aarch64-apple-darwin", "x86_64-apple-darwin"] {
        assert!(calls.contains(&format!("rustc --target {target}")));
    }
    assert!(calls.contains("lima --check"));
    assert!(!fixture.path().join("assets").exists());
}

#[test]
fn linux_guest_probe_uses_native_and_cross_linkers_and_strips_symbols() {
    let (fixture, bin, _target, toolchain, _linux_runner) = release_tool_shims();
    let log = fixture.path().join("commands.log");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let result = Command::new("/bin/bash")
        .current_dir(repo_root())
        .arg(repo_root().join("scripts/release-linux.sh"))
        .args([
            "--guest-check",
            "aarch64-unknown-linux-gnu",
            fixture.path().join("cache").to_str().unwrap(),
        ])
        .env("PATH", path)
        .env("RELEASE_TEST_LOG", &log)
        .env("STORYHOOK_RELEASE_TOOLCHAIN_DIR", &toolchain)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "guest probe failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let calls = std::fs::read_to_string(log).unwrap();
    assert!(
        calls.contains("rustc --target aarch64-unknown-linux-gnu -C linker=cc -C strip=symbols")
    );
    assert!(calls.contains(
        "rustc --target x86_64-unknown-linux-gnu -C linker=x86_64-linux-gnu-gcc -C strip=symbols"
    ));
}

#[test]
fn linux_guest_build_uses_locked_cargo_and_exports_the_binary() {
    let (fixture, bin, _target_dir, toolchain, _linux_runner) = release_tool_shims();
    let source = fixture.path().join("source");
    let archive = fixture.path().join("source.tar.gz");
    let output = fixture.path().join("out/story");
    let log = fixture.path().join("commands.log");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(
        source.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='0.0.0'\n",
    )
    .unwrap();
    let archived = Command::new("tar")
        .args(["-czf"])
        .arg(&archive)
        .arg("-C")
        .arg(&source)
        .arg(".")
        .status()
        .unwrap();
    assert!(archived.success());

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let result = Command::new("/bin/bash")
        .current_dir(repo_root())
        .arg(repo_root().join("scripts/release-linux.sh"))
        .args([
            "--guest-build",
            "aarch64-unknown-linux-gnu",
            fixture.path().join("cache").to_str().unwrap(),
            "x86_64-unknown-linux-gnu",
            archive.to_str().unwrap(),
            output.to_str().unwrap(),
            "0123456789ab",
        ])
        .env("PATH", path)
        .env("RELEASE_TEST_LOG", &log)
        .env("STORYHOOK_RELEASE_TOOLCHAIN_DIR", &toolchain)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "guest build failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(output.is_file(), "guest build did not export its binary");

    let calls = std::fs::read_to_string(log).unwrap();
    assert!(calls.contains("cargo build --locked --release --target x86_64-unknown-linux-gnu"));
    assert!(calls.contains("strip=symbols"));
    assert!(calls.contains("x86_linker=x86_64-linux-gnu-gcc"));
    assert!(calls.contains(&format!("rustc={}", toolchain.join("bin/rustc").display())));
}

#[test]
fn linux_runner_refuses_a_guest_above_the_glibc_compatibility_floor() {
    let (fixture, bin, _target, _toolchain, _linux_runner) = release_tool_shims();
    write_executable(
        &bin.join("limactl"),
        r#"#!/bin/bash
set -eu
case "$1" in
  list) printf '{"status":"Stopped"}\n' ;;
  start) ;;
  shell)
    case "$*" in
      *' uname -m') echo aarch64 ;;
      *' getconf GNU_LIBC_VERSION') echo "${RELEASE_GLIBC:-glibc 2.31}" ;;
    esac
    ;;
  copy) ;;
  *) exit 2 ;;
esac
"#,
    );
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let result = Command::new("/bin/bash")
        .current_dir(repo_root())
        .arg(repo_root().join("scripts/release-linux.sh"))
        .arg("--check")
        .env("PATH", path)
        .env("RELEASE_GLIBC", "glibc 2.35")
        .env("RELEASE_TEST_LOG", fixture.path().join("commands.log"))
        .output()
        .unwrap();
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("must provide glibc 2.31; found glibc 2.35"),
        "wrong glibc diagnostic: {stderr}"
    );
}

#[test]
fn unstripped_linux_binary_is_refused_before_packaging() {
    let (_fixture, output_dir, result) = run_asset_builder(&[("RELEASE_UNSTRIPPED", "1")]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("wrong symbol state"));
    assert!(!output_dir.join("SHA256SUMS").exists());
}

#[test]
fn corrupt_cached_toolchain_component_is_refused_by_its_pinned_hash() {
    let fixture = scratch_dir_named("release-toolchain-hash");
    let lock = fixture.path().join("release-toolchain.lock");
    let cache = fixture.path().join("cache");
    let expected_hash = "0000000000000000000000000000000000000000000000000000000000000000";
    std::fs::create_dir_all(cache.join("downloads")).unwrap();
    std::fs::write(
        &lock,
        format!(
            "rustc\taarch64-apple-darwin\thttps://invalid.example/rustc.tar.gz\t{expected_hash}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        cache
            .join("downloads")
            .join(format!("{expected_hash}.tar.gz")),
        "corrupt",
    )
    .unwrap();

    let script = format!(
        "source '{}'; ensure_release_toolchain aarch64-apple-darwin '{}' '{}' aarch64-apple-darwin",
        repo_root().join("scripts/release-toolchain.sh").display(),
        cache.display(),
        lock.display()
    );
    let result = Command::new("/bin/bash")
        .args(["-c", &script])
        .env_remove("STORYHOOK_RELEASE_TOOLCHAIN_DIR")
        .output()
        .unwrap();
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("cached rustc for aarch64-apple-darwin failed SHA-256"),
        "wrong checksum diagnostic: {stderr}"
    );
}

#[test]
fn pinned_components_assemble_a_private_toolchain_without_global_installation() {
    let fixture = scratch_dir_named("release-toolchain-install");
    let package = fixture.path().join("package/component");
    let archive = fixture.path().join("component.tar.gz");
    let cache = fixture.path().join("cache");
    let lock = fixture.path().join("release-toolchain.lock");
    std::fs::create_dir_all(&package).unwrap();
    write_executable(
        &package.join("install.sh"),
        r#"#!/bin/bash
set -eu
echo "$*" >> "$RELEASE_TEST_INSTALL_LOG"
prefix=""
for argument in "$@"; do
  case "$argument" in --prefix=*) prefix="${argument#--prefix=}" ;; esac
done
mkdir -p "$prefix/bin"
cat > "$prefix/bin/rustc" <<'RUSTC'
#!/bin/bash
# Reports the root it was installed under, as a real rustc does. Resolved at
# call time, so it still answers correctly once the staging directory has been
# moved to its final name (SH-576).
if [ "${1:-}" = --print ] && [ "${2:-}" = sysroot ]; then
  cd "$(dirname "$0")/.." && pwd -P
fi
exit 0
RUSTC
printf '#!/bin/bash\nexit 0\n' > "$prefix/bin/cargo"
chmod +x "$prefix/bin/rustc" "$prefix/bin/cargo"
"#,
    );
    let archived = Command::new("tar")
        .args(["-czf"])
        .arg(&archive)
        .arg("-C")
        .arg(fixture.path().join("package"))
        .arg("component")
        .status()
        .unwrap();
    assert!(archived.success());
    let digest_output = Command::new("shasum")
        .args(["-a", "256"])
        .arg(&archive)
        .output()
        .unwrap();
    assert!(digest_output.status.success());
    let digest = String::from_utf8(digest_output.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    std::fs::create_dir_all(cache.join("downloads")).unwrap();
    std::fs::rename(
        &archive,
        cache.join("downloads").join(format!("{digest}.tar.gz")),
    )
    .unwrap();
    let rows = [
        ("rustc", "aarch64-apple-darwin"),
        ("cargo", "aarch64-apple-darwin"),
        ("rust-std", "aarch64-apple-darwin"),
        ("rust-std", "x86_64-apple-darwin"),
    ]
    .map(|(component, target)| {
        format!("{component}\t{target}\thttps://invalid.example/component.tar.gz\t{digest}\n")
    })
    .concat();
    std::fs::write(&lock, rows).unwrap();

    let script = format!(
        "source '{}'; ensure_release_toolchain aarch64-apple-darwin '{}' '{}' aarch64-apple-darwin x86_64-apple-darwin",
        repo_root().join("scripts/release-toolchain.sh").display(),
        cache.display(),
        lock.display()
    );
    let result = Command::new("/bin/bash")
        .args(["-c", &script])
        .env_remove("STORYHOOK_RELEASE_TOOLCHAIN_DIR")
        .env(
            "RELEASE_TEST_INSTALL_LOG",
            fixture.path().join("install.log"),
        )
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "private install failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let installed = PathBuf::from(String::from_utf8(result.stdout).unwrap().trim());
    assert!(installed.starts_with(&cache));
    assert!(installed.join("bin/rustc").is_file());
    assert!(installed.join("bin/cargo").is_file());
    assert!(installed.join(".storyhook-release-toolchain").is_file());
    let installs = std::fs::read_to_string(fixture.path().join("install.log")).unwrap();
    for invocation in installs.lines() {
        let arguments: Vec<&str> = invocation.split_whitespace().collect();
        let prefix = arguments
            .iter()
            .find_map(|argument| argument.strip_prefix("--prefix="))
            .unwrap();
        assert!(Path::new(prefix).starts_with(&cache));
        for destination in [
            "bindir",
            "libdir",
            "sysconfdir",
            "datadir",
            "docdir",
            "mandir",
        ] {
            let value = arguments
                .iter()
                .find_map(|argument| argument.strip_prefix(&format!("--{destination}=")))
                .unwrap_or_else(|| panic!("missing {destination}: {invocation}"));
            assert!(
                Path::new(value).starts_with(prefix),
                "{destination} escaped the private prefix: {invocation}"
            );
        }
    }
}

#[test]
fn builder_refuses_before_work_when_lima_capability_is_missing() {
    let (fixture, bin, target, toolchain, linux_runner) = release_tool_shims();
    write_executable(
        &linux_runner,
        "#!/bin/bash\nprintf 'error: limactl is required for Linux release assembly\\n' >&2\nexit 1\n",
    );
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let result = Command::new("/bin/bash")
        .current_dir(repo_root())
        .arg(repo_root().join("scripts/build-release-assets.sh"))
        .arg("--check")
        .env("PATH", path)
        .env("CARGO_TARGET_DIR", target)
        .env("RELEASE_TEST_LOG", fixture.path().join("commands.log"))
        .env("STORYHOOK_RELEASE_TOOLCHAIN_DIR", toolchain)
        .env("STORYHOOK_RELEASE_LINUX_RUNNER", linux_runner)
        .output()
        .unwrap();
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("limactl is required"),
        "missing Lima diagnostic: {stderr}"
    );
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

// ---------------------------------------------------------------------------
// The assembled toolchain must be able to find its own sysroot (SH-576)
// ---------------------------------------------------------------------------
//
// rustc does not read its sysroot off argv0; it reads it off the directory
// holding the `librustc_driver` dylib it loaded, and decides how far to walk
// up by the names it finds there. A dylib in `<anything>/<target-triple>/lib/`
// is indistinguishable by name from one in `$sysroot/lib/rustlib/$target/lib/`
// — the layout pre-built target libraries ship in — so rustc strips four
// components instead of one. Assembling the toolchain at
// `.../toolchains/$hash/$host_target` did exactly that, put the sysroot three
// levels above the toolchain, and failed every compile with E0463 "can't find
// crate for `std`" plus advice to run a `rustup target add` that is not part
// of this path at all.
//
// Two mechanisms, because neither covers the other. The layout is pinned
// below from `release_toolchain_root_path`, the one function that spells the
// path. The obligation to *prove* it is structural: `ensure_release_toolchain`
// has a single exit, and the verification sits on it, so no branch can hand
// back a toolchain that was never asked. What these tests deliberately do NOT
// prove is rustc's own walking rule — that needs a real 500 MB toolchain and
// belongs to `build-release-assets.sh --check`, which now runs the same
// verification against the real thing on every release.

/// Every release target triple, read from the one file that defines them
/// rather than repeated here (SH-136).
fn release_target_triples() -> Vec<String> {
    let script = format!(
        "source '{}'; printf '%s\\n' \"${{RELEASE_TARGETS[@]}}\"",
        repo_root().join("scripts/release-targets.sh").display()
    );
    let output = Command::new("/bin/bash")
        .args(["-c", &script])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "could not read the release targets"
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .filter(|triple| !triple.is_empty())
        .collect()
}

/// Asks the production function where a toolchain for `host_target` goes.
fn toolchain_root_path(cache_root: &Path, lock_hash: &str, host_target: &str) -> PathBuf {
    let script = format!(
        "source '{}'; release_toolchain_root_path '{}' '{}' '{}'",
        repo_root().join("scripts/release-toolchain.sh").display(),
        cache_root.display(),
        lock_hash,
        host_target
    );
    let output = Command::new("/bin/bash")
        .args(["-c", &script])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "release_toolchain_root_path failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}

/// A toolchain whose `rustc` answers `--print sysroot` with `answer`, which is
/// evaluated by the shim at call time so a fixture can lie about it.
fn fake_toolchain(root: &Path, answer: &str) {
    std::fs::create_dir_all(root.join("bin")).unwrap();
    write_executable(
        &root.join("bin/rustc"),
        &format!(
            "#!/bin/bash\nif [ \"${{1:-}}\" = --print ] && [ \"${{2:-}}\" = sysroot ]; then\n  {answer}\nfi\nexit 0\n"
        ),
    );
    write_executable(&root.join("bin/cargo"), "#!/bin/bash\nexit 0\n");
}

/// Reports its own root, the way a real toolchain does.
const HONEST_SYSROOT: &str = "cd \"$(dirname \"$0\")/..\" && pwd -P";

/// Prepares a warm cache hit for `host_target` and returns its root.
fn warm_cache(cache_root: &Path, lock: &Path, host_target: &str, answer: &str) -> PathBuf {
    warm_cache_for(cache_root, lock, host_target, &[host_target], |root| {
        fake_toolchain(root, answer)
    })
}

/// Prepares a warm cache hit whose toolchain is written by `populate`, for the
/// exact `required_targets` the caller will ask `ensure_release_toolchain` for
/// — the marker has to match those verbatim or the cache is not a hit.
fn warm_cache_for(
    cache_root: &Path,
    lock: &Path,
    host_target: &str,
    required_targets: &[&str],
    populate: impl FnOnce(&Path),
) -> PathBuf {
    let digest = Command::new("shasum")
        .args(["-a", "256"])
        .arg(lock)
        .output()
        .unwrap();
    assert!(digest.status.success());
    let lock_hash = String::from_utf8(digest.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    let root = toolchain_root_path(cache_root, &lock_hash, host_target);
    populate(&root);
    std::fs::write(
        root.join(".storyhook-release-toolchain"),
        format!("{lock_hash} {host_target} {}\n", required_targets.join(" ")),
    )
    .unwrap();
    root
}

/// Runs `ensure_release_toolchain` against a fixture cache.
fn ensure_toolchain(cache_root: &Path, lock: &Path, host_target: &str) -> Output {
    let script = format!(
        "source '{}'; ensure_release_toolchain '{}' '{}' '{}' '{}'",
        repo_root().join("scripts/release-toolchain.sh").display(),
        host_target,
        cache_root.display(),
        lock.display(),
        host_target
    );
    Command::new("/bin/bash")
        .args(["-c", &script])
        .env_remove("STORYHOOK_RELEASE_TOOLCHAIN_DIR")
        .output()
        .unwrap()
}

#[test]
fn the_assembled_toolchain_root_is_never_named_after_a_release_target() {
    let triples = release_target_triples();
    assert!(
        !triples.is_empty(),
        "the release target list must not be empty"
    );

    for host_target in &triples {
        let root = toolchain_root_path(Path::new("/cache"), "deadbeef", host_target);
        let leaf = root.file_name().unwrap().to_string_lossy().to_string();
        assert!(
            !triples.contains(&leaf),
            "the toolchain for {host_target} is assembled at {}, whose own directory is \
             named after a target triple — rustc reads that as the pre-built-library \
             layout and resolves the sysroot three levels too high (SH-576)",
            root.display()
        );
        // The host still has to key the cache, or two hosts would share one
        // toolchain; the triple moves off the leaf, it does not leave.
        assert!(
            root.components()
                .any(|component| component.as_os_str() == host_target.as_str()),
            "{} no longer keys the cache by host",
            root.display()
        );
    }
}

#[test]
fn a_warm_cached_toolchain_that_misresolves_its_own_sysroot_is_refused_by_name() {
    let fixture = scratch_dir_named("release-toolchain-sysroot");
    let cache = fixture.path().join("cache");
    let lock = fixture.path().join("release-toolchain.lock");
    std::fs::write(&lock, "irrelevant\n").unwrap();
    let root = warm_cache(
        &cache,
        &lock,
        "aarch64-apple-darwin",
        "echo /somewhere/else",
    );

    let result = ensure_toolchain(&cache, &lock, "aarch64-apple-darwin");
    assert!(
        !result.status.success(),
        "a toolchain that cannot find its own sysroot was handed back"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("/somewhere/else") && stderr.contains(&root.display().to_string()),
        "the refusal must name what rustc answered and what was expected: {stderr}"
    );
}

#[test]
fn an_operator_supplied_toolchain_that_misresolves_its_own_sysroot_is_refused_by_name() {
    let fixture = scratch_dir_named("release-toolchain-override");
    let supplied = fixture.path().join("supplied");
    fake_toolchain(&supplied, "echo /somewhere/else");

    let script = format!(
        "source '{}'; ensure_release_toolchain aarch64-apple-darwin '{}' '{}' aarch64-apple-darwin",
        repo_root().join("scripts/release-toolchain.sh").display(),
        fixture.path().join("cache").display(),
        fixture.path().join("missing.lock").display()
    );
    let result = Command::new("/bin/bash")
        .args(["-c", &script])
        .env("STORYHOOK_RELEASE_TOOLCHAIN_DIR", &supplied)
        .output()
        .unwrap();

    assert!(
        !result.status.success(),
        "an override is a claim about a toolchain, not an exemption from proving it"
    );
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("/somewhere/else"),
        "the refusal must name what rustc answered: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn a_toolchain_that_reports_no_sysroot_is_named_rather_than_read_as_the_working_directory() {
    let fixture = scratch_dir_named("release-toolchain-silent");
    let cache = fixture.path().join("cache");
    let lock = fixture.path().join("release-toolchain.lock");
    std::fs::write(&lock, "irrelevant\n").unwrap();
    warm_cache(&cache, &lock, "aarch64-apple-darwin", "true");

    let result = ensure_toolchain(&cache, &lock, "aarch64-apple-darwin");
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("reported no sysroot at all"),
        "an empty answer must be reported as one — `cd \"\"` succeeds and stays put, \
         so resolving it would name the caller's own directory as rustc's answer: {stderr}"
    );
}

#[test]
fn a_warm_cached_toolchain_that_resolves_its_own_sysroot_is_handed_back() {
    let fixture = scratch_dir_named("release-toolchain-honest");
    let cache = fixture.path().join("cache");
    let lock = fixture.path().join("release-toolchain.lock");
    std::fs::write(&lock, "irrelevant\n").unwrap();
    let root = warm_cache(&cache, &lock, "aarch64-apple-darwin", HONEST_SYSROOT);

    let result = ensure_toolchain(&cache, &lock, "aarch64-apple-darwin");
    assert!(
        result.status.success(),
        "an honest toolchain must still be usable: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let handed_back = PathBuf::from(String::from_utf8(result.stdout).unwrap().trim().to_string());
    assert_eq!(
        std::fs::canonicalize(&handed_back).unwrap(),
        std::fs::canonicalize(&root).unwrap()
    );
}

// ---------------------------------------------------------------------------
// The guest's cache root, and what happens when it cannot be written (SH-578)
// ---------------------------------------------------------------------------
//
// `limactl shell` does not start in the guest home. It starts in the host's
// working directory when that path is mounted, and otherwise falls back to the
// host home — which Lima's default template mounts READ-ONLY. This checkout
// lives outside every mount, so `$PWD` in the guest was `/Users/mikey`, and the
// relative cache root resolved against it could not be created at all.
//
// The failure was reported as `could not download rustc ... from
// https://static.rust-lang.org/...`: a network diagnosis for a filesystem
// fault, because the `mkdir` two lines earlier printed its own error and was
// not treated as fatal. `set -e` cannot catch that on its own — every caller
// invokes `ensure_release_toolchain` as `x="$(...)" || ...`, and `set -e` is
// suppressed inside a command substitution whose assignment is part of a `||`
// list — so the check is written out.

#[test]
fn a_relative_guest_cache_root_resolves_against_the_guest_home_not_its_working_directory() {
    let (fixture, bin, _target, toolchain, _runner) = release_tool_shims();
    let home = fixture.path().join("guest-home");
    let read_only_mount = fixture.path().join("read-only-mount");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&read_only_mount).unwrap();

    // The cache the guest is meant to find: under its HOME, never under the
    // directory limactl happened to start in.
    let lock = repo_root().join("scripts/release-toolchain.lock");
    warm_cache_for(
        &home.join(".cache/storyhook-release"),
        &lock,
        "aarch64-unknown-linux-gnu",
        &["aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"],
        |root| {
            std::fs::create_dir_all(root.join("bin")).unwrap();
            for tool in ["rustc", "cargo"] {
                std::fs::copy(
                    toolchain.join("bin").join(tool),
                    root.join("bin").join(tool),
                )
                .unwrap();
            }
        },
    );

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let result = Command::new("/bin/bash")
        // Stand where limactl actually leaves the guest: somewhere that is not
        // the home, and on the real machine is not writable either.
        .current_dir(&read_only_mount)
        .arg(repo_root().join("scripts/release-linux.sh"))
        .args([
            "--guest-check",
            "aarch64-unknown-linux-gnu",
            ".cache/storyhook-release",
        ])
        .env("PATH", path)
        .env("HOME", &home)
        .env("RELEASE_TEST_LOG", fixture.path().join("commands.log"))
        .env_remove("STORYHOOK_RELEASE_TOOLCHAIN_DIR")
        .output()
        .unwrap();

    assert!(
        result.status.success(),
        "the guest probe did not find the toolchain in its own home, so it fell \
         through to assembling one — which on the real guest means writing to \
         the read-only host-home mount (SH-578): {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn both_guest_phases_resolve_their_cache_root_through_the_one_function() {
    let script = read("scripts/release-linux.sh");
    assert_eq!(
        script
            .matches("guest_absolute_cache_root \"$guest_cache_root\"")
            .count(),
        2,
        "--guest-check and --guest-build must both resolve through the shared \
         function; they carried identical copies of the wrong normalization, \
         which is how one fix could have missed the other half (SH-136)"
    );
    assert!(
        !script.contains("guest_cache_root=\"$PWD/$guest_cache_root\""),
        "the working-directory normalization is the defect and must not survive"
    );
}

#[test]
fn a_cache_root_that_cannot_be_created_is_refused_by_name_not_reported_as_a_download() {
    let fixture = scratch_dir_named("release-toolchain-readonly");
    let sealed = fixture.path().join("sealed");
    let lock = fixture.path().join("release-toolchain.lock");
    std::fs::create_dir_all(&sealed).unwrap();
    std::fs::write(
        &lock,
        "rustc\taarch64-apple-darwin\thttps://invalid.example/rustc.tar.gz\t0000000000000000000000000000000000000000000000000000000000000000\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&sealed).unwrap().permissions();
    permissions.set_mode(0o555);
    std::fs::set_permissions(&sealed, permissions).unwrap();

    let result = ensure_toolchain(&sealed.join("cache"), &lock, "aarch64-apple-darwin");

    // Restore write permission before any assertion, so a failure here still
    // leaves the fixture removable.
    let mut restored = std::fs::metadata(&sealed).unwrap().permissions();
    restored.set_mode(0o755);
    std::fs::set_permissions(&sealed, restored).unwrap();

    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("could not create the release toolchain cache"),
        "an unwritable cache root must name itself; it used to carry on and \
         surface as a failed download from static.rust-lang.org: {stderr}"
    );
    assert!(
        !stderr.contains("could not download"),
        "the download is downstream of the real fault and must not be blamed \
         for it: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// A cross linker needs a cross C runtime, or it cannot link (SH-578)
// ---------------------------------------------------------------------------
//
// `release-linux.yaml` installs both cross compilers with
// `--no-install-recommends`, and on Debian/Ubuntu the matching
// `libc6-dev-<arch>-cross` is a *Recommends* of the compiler, not a Depends.
// So the guest had `gcc-x86-64-linux-gnu` and no `Scrt1.o`, and every
// cross-link died at `ld: cannot find Scrt1.o` — a compiler that is present
// and cannot do the one job it was installed for.
//
// Derived from the linkers `release-linux.sh` will actually ask for, so
// teaching that script a new cross target fails here until its runtime is
// installed too, with no list to remember to update (SH-136).

/// Every cross linker `linker_for_target` can name, with the Debian
/// architecture its C runtime package is keyed by.
fn cross_linkers_and_runtime_architectures() -> Vec<(String, String)> {
    let script = read("scripts/release-linux.sh");
    let mut found = Vec::new();
    for line in script.lines() {
        // `      aarch64-unknown-linux-gnu) printf 'aarch64-linux-gnu-gcc\n' ;;`
        let Some((target, rest)) = line.trim().split_once(") printf '") else {
            continue;
        };
        if !target.ends_with("-unknown-linux-gnu") {
            continue;
        }
        let Some(linker) = rest.split('\\').next() else {
            continue;
        };
        if !linker.ends_with("-linux-gnu-gcc") {
            continue;
        }
        let debian_architecture = match target {
            "x86_64-unknown-linux-gnu" => "amd64",
            "aarch64-unknown-linux-gnu" => "arm64",
            other => panic!(
                "{other} is a Linux release target this test has not been taught a Debian \
                 architecture for. Name it here rather than letting the check pass \
                 vacuously — a target whose C runtime is unnamed is one whose cross-link \
                 will fail in the guest, where it costs a whole release to find out."
            ),
        };
        found.push((linker.to_string(), debian_architecture.to_string()));
    }
    found
}

#[test]
fn every_cross_linker_the_guest_may_use_has_its_c_runtime_installed() {
    let provisioning = read("scripts/release-linux.yaml");
    let linkers = cross_linkers_and_runtime_architectures();
    assert_eq!(
        linkers.len(),
        2,
        "expected both Linux cross linkers to be discoverable in release-linux.sh; \
         found {linkers:?}"
    );

    for (linker, debian_architecture) in linkers {
        let compiler = linker.trim_end_matches("-gcc");
        let runtime = format!("libc6-dev-{debian_architecture}-cross");
        assert!(
            provisioning.contains(&format!("gcc-{}", compiler.replace('_', "-"))),
            "the guest must install the {compiler} cross compiler"
        );
        assert!(
            provisioning.contains(&runtime),
            "the guest installs a cross compiler for {compiler} but not {runtime}. \
             `--no-install-recommends` drops it, because the runtime is a Recommends \
             of the compiler rather than a Depends, and the compiler then fails at \
             `ld: cannot find Scrt1.o` (SH-578)"
        );
    }
}
