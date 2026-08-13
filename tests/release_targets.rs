//! The release build's invariants, as far as a native gate can hold them.
//!
//! `make test` is a macOS build. It never evaluates a
//! `cfg(target_os = "linux")` dependency and never runs the release workflow,
//! so the Linux target went two releases without compiling and nothing said
//! so until `v2.1.0`'s tag was already pushed and unmovable (SH-259). The
//! compile itself still belongs to CI — there is no Linux toolchain here to
//! borrow. What belongs *here* is the pair of facts that made that failure
//! possible and survivable-in-silence, because both are plain files this gate
//! can read on any platform:
//!
//! 1. the Linux keyring dependency selected no `secret-service` runtime, so
//!    the crate it pulls refused to compile at all; and
//! 2. the build matrix let one broken target cancel the three that worked, so
//!    the release produced nothing rather than three quarters of something.
//!
//! Neither of these is the real cross-target build, and neither pretends to
//! be — `.github/workflows/release.yml` carries a `workflow_dispatch` trigger
//! now so that build can be run on demand without minting a tag. These tests
//! are the part of it that costs nothing and runs every time.

use std::path::PathBuf;

/// The repository root, which is this package's manifest directory: the root
/// package and the workspace root are the same crate here (see `Cargo.toml`'s
/// opening comment).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Reads a repository file, failing with the path rather than with `None` —
/// a missing manifest or workflow is a finding, not a reason to skip.
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
// The workflow: what one broken target costs
// ---------------------------------------------------------------------------

/// `.github/workflows/release.yml`, parsed.
fn release_workflow() -> serde_yml::Value {
    serde_yml::from_str(&read(".github/workflows/release.yml"))
        .expect("release.yml must be valid YAML")
}

/// The workflow's trigger mapping. YAML 1.1 read a bare `on` as the boolean
/// `true` and some parsers still do, so both spellings are accepted — the
/// point of the lookup is the triggers, not the quoting.
fn triggers(workflow: &serde_yml::Value) -> &serde_yml::Value {
    workflow
        .get("on")
        .or_else(|| workflow.get(serde_yml::Value::Bool(true)))
        .expect("release.yml must declare its triggers")
}

/// GitHub Actions defaults a matrix to `fail-fast: true`, which cancels every
/// sibling job the moment one fails. That is a reasonable default for a test
/// matrix and the wrong one for a release matrix: when the Linux build broke,
/// it took both macOS artifacts and the aarch64 Linux one with it, and the
/// `release` job then skipped for want of anything to upload. A broken
/// platform should cost that platform.
#[test]
fn one_broken_target_does_not_cancel_the_others() {
    let workflow = release_workflow();
    let fail_fast = workflow
        .get("jobs")
        .and_then(|jobs| jobs.get("build"))
        .and_then(|build| build.get("strategy"))
        .and_then(|strategy| strategy.get("fail-fast"));

    assert_eq!(
        fail_fast.and_then(serde_yml::Value::as_bool),
        Some(false),
        "release.yml's build matrix must set `fail-fast: false`, or one \
         target's failure cancels the artifacts of every target that works \
         (SH-259)"
    );
}

/// The feedback loop the tag-only trigger left open: these targets were built
/// exactly once per release, after the tag was pushed and could no longer be
/// moved. A `workflow_dispatch` trigger makes the same cross-target build
/// runnable on demand. This is a build of the deploy artifact, not a test
/// suite, so it stays on the right side of the project rule that Actions are
/// for deploys.
#[test]
fn the_cross_target_build_can_run_without_minting_a_tag() {
    let workflow = release_workflow();
    let triggers = triggers(&workflow);
    assert!(
        triggers.get("workflow_dispatch").is_some(),
        "release.yml must be dispatchable, or a shipped target can go \
         unbuilt from one release to the next with nothing able to say so \
         (SH-259)"
    );
    assert!(
        triggers.get("push").is_some(),
        "release.yml must still build on a version tag"
    );
}

/// A dispatched run has no tag to release, so publishing has to be conditional
/// on one — otherwise adding the trigger above turns every manual build into a
/// failed or, worse, a spurious release.
#[test]
fn a_dispatched_run_builds_but_does_not_publish() {
    let workflow = release_workflow();
    let guard = workflow
        .get("jobs")
        .and_then(|jobs| jobs.get("release"))
        .and_then(|release| release.get("if"))
        .and_then(serde_yml::Value::as_str)
        .unwrap_or_default();

    assert!(
        guard.contains("refs/tags/"),
        "the `release` job must be guarded on a tag ref, or a \
         `workflow_dispatch` build tries to publish a release it has no tag \
         for; found `if: {guard}`"
    );
}

// ---------------------------------------------------------------------------
// The release body: rendered from the changelog, never generated alone
// ---------------------------------------------------------------------------

/// The job that renders the release body, found by the script it runs rather
/// than by name — a renamed job should keep the invariant, not retire it.
///
/// Returns the job's name alongside it, because the publishing job has to
/// depend on it and the dependency is spelled by name.
fn body_rendering_job(workflow: &serde_yml::Value) -> (String, serde_yml::Value) {
    let jobs = workflow
        .get("jobs")
        .and_then(serde_yml::Value::as_mapping)
        .expect("release.yml must declare jobs");

    let mut found: Vec<(String, serde_yml::Value)> = jobs
        .iter()
        .filter(|(_, job)| {
            serde_yml::to_string(job)
                .unwrap_or_default()
                .contains("render-release-body.sh")
        })
        .map(|(name, job)| (name.as_str().unwrap_or_default().to_string(), job.clone()))
        .collect();

    assert_eq!(
        found.len(),
        1,
        "exactly one job in release.yml must render the release body; found \
         {:?}",
        found.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
    found.pop().expect("the single rendering job")
}

/// The `Create release` step, found by the action it uses.
fn create_release_step(workflow: &serde_yml::Value) -> serde_yml::Value {
    workflow
        .get("jobs")
        .and_then(|jobs| jobs.get("release"))
        .and_then(|release| release.get("steps"))
        .and_then(serde_yml::Value::as_sequence)
        .expect("the release job must declare steps")
        .iter()
        .find(|step| {
            step.get("uses")
                .and_then(serde_yml::Value::as_str)
                .is_some_and(|uses| uses.contains("action-gh-release"))
        })
        .expect("the release job must publish with action-gh-release")
        .clone()
}

/// GitHub generates release notes from the newest **tag**, published or not.
/// `v2.1.0` was tagged and never published, so the notes generated for the
/// next release describe `v2.1.0...v2.1.1` — six pull requests — while a user
/// upgrading from the newest *release* receives seven hundred and forty-three
/// commits. Generated notes are an appendix here, never the whole body.
#[test]
fn the_release_body_comes_from_the_repository_not_only_from_generated_notes() {
    let workflow = release_workflow();
    let with = create_release_step(&workflow)
        .get("with")
        .cloned()
        .expect("the publishing step must configure the release");

    let body_path = with
        .get("body_path")
        .and_then(serde_yml::Value::as_str)
        .unwrap_or_default();

    assert!(
        !body_path.trim().is_empty(),
        "release.yml must supply `body_path`, or every release publishes \
         GitHub's generated notes alone — which measure from the newest tag \
         rather than the newest release, and silently hide everything an \
         unpublished tag swallowed (SH-262)"
    );
}

/// Publishing depends on the body existing. Without the dependency the
/// rendering job could fail and the release would publish anyway, with the
/// fallback body this whole mechanism exists to prevent.
#[test]
fn publishing_depends_on_the_body_having_been_rendered() {
    let workflow = release_workflow();
    let (name, _) = body_rendering_job(&workflow);

    let needs = workflow
        .get("jobs")
        .and_then(|jobs| jobs.get("release"))
        .and_then(|release| release.get("needs"))
        .expect("the release job must declare what it needs");

    let needed: Vec<&str> = match needs {
        serde_yml::Value::String(one) => vec![one.as_str()],
        serde_yml::Value::Sequence(many) => {
            many.iter().filter_map(serde_yml::Value::as_str).collect()
        }
        other => panic!("unexpected `needs` shape: {other:?}"),
    };

    assert!(
        needed.contains(&name.as_str()),
        "the release job must need `{name}`, or a failed render publishes \
         generated-only notes instead of failing the release (SH-262); found \
         needs: {needed:?}"
    );
}

/// The rendering runs on a dispatched build too, which is the only place its
/// failure is cheap. `release.yml:  the publishing job` is gated on a tag, and
/// tags are never moved here — so a body that cannot be rendered on a tag push
/// mints a second permanently dangling tag. The dispatch rehearsal that already
/// proves the four targets build now proves the body renders as well.
#[test]
fn the_body_is_rendered_on_a_dispatched_run_too() {
    let workflow = release_workflow();
    let (name, job) = body_rendering_job(&workflow);

    let guard = job
        .get("if")
        .and_then(serde_yml::Value::as_str)
        .unwrap_or_default();

    assert!(
        !guard.contains("refs/tags/"),
        "`{name}` is gated on a tag ref, so the body is first rendered when \
         the tag can no longer be moved; it must run on a dispatched build as \
         well (SH-262). Found `if: {guard}`"
    );
}

/// The rendered body is not a release asset. The publishing step globs a
/// directory of build artifacts, and the body must not be downloaded into it —
/// `install.sh` and `story update` fetch assets by name, and a stray markdown
/// file beside them is at best noise on the release page.
#[test]
fn the_rendered_body_is_not_published_as_an_asset() {
    let workflow = release_workflow();
    let with = create_release_step(&workflow)
        .get("with")
        .cloned()
        .expect("the publishing step must configure the release");

    let files = with
        .get("files")
        .and_then(serde_yml::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let body_path = with
        .get("body_path")
        .and_then(serde_yml::Value::as_str)
        .unwrap_or_default()
        .to_string();

    let asset_directory = files.rsplit_once('/').map(|(head, _)| head).unwrap_or("");
    let body_directory = body_path
        .rsplit_once('/')
        .map(|(head, _)| head)
        .unwrap_or("");

    assert_ne!(
        asset_directory, body_directory,
        "the release body (`{body_path}`) lands in the directory the \
         publishing step uploads as assets (`{files}`), so it would be \
         attached to the release"
    );
}

/// The span is measured from a published *release*, which is what `install.sh`
/// and `story update` resolve — never from a tag. Measuring from a tag is the
/// original defect wearing the new mechanism's clothes: an unpublished tag
/// silently shortens the span to nothing anyone can install.
#[test]
fn the_body_measures_from_a_published_release_rather_than_a_tag() {
    let workflow = release_workflow();
    let (name, job) = body_rendering_job(&workflow);
    let step = serde_yml::to_string(&job).unwrap_or_default();

    assert!(
        step.contains("releases"),
        "`{name}` must ask the releases API which version it is measuring \
         from (SH-262)"
    );
    for tag_derived in ["git describe", "git tag"] {
        assert!(
            !step.contains(tag_derived),
            "`{name}` derives the span with `{tag_derived}`, which counts \
             from the newest tag — published or not — and that is the defect \
             `body_path` exists to fix (SH-262)"
        );
    }
}

/// The renderer itself is tracked and runnable. A `body_path` pointing at a
/// file nothing produces — or at one the runner may not execute — fails the
/// release after the tag is unmovable.
#[test]
fn the_renderer_is_tracked_where_the_workflow_expects_it() {
    let renderer = repo_root().join("scripts/render-release-body.sh");
    assert!(
        renderer.is_file(),
        "release.yml renders the body with scripts/render-release-body.sh, \
         which is not in the tree"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = renderer
            .metadata()
            .expect("the renderer's metadata")
            .permissions()
            .mode();
        assert_ne!(
            mode & 0o111,
            0,
            "the workflow runs scripts/render-release-body.sh directly, so it \
             has to carry the executable bit; found mode {mode:o}"
        );
    }
}

// ---------------------------------------------------------------------------
// The two files agree
// ---------------------------------------------------------------------------

/// The release matrix's target triples, e.g. `x86_64-unknown-linux-gnu`.
///
/// Shared by every test in this section that needs "what does the release
/// actually build" — the manifest invariants below are only worth holding
/// for platforms this list names.
fn release_matrix_targets(workflow: &serde_yml::Value) -> Vec<&str> {
    workflow
        .get("jobs")
        .and_then(|jobs| jobs.get("build"))
        .and_then(|build| build.get("strategy"))
        .and_then(|strategy| strategy.get("matrix"))
        .and_then(|matrix| matrix.get("include"))
        .and_then(serde_yml::Value::as_sequence)
        .expect("release.yml's build matrix must list its targets")
        .iter()
        .filter_map(|entry| entry.get("target").and_then(serde_yml::Value::as_str))
        .collect()
}

/// Every tracked `Cargo.toml`, found by `git ls-files` rather than a
/// hand-maintained list — the same idiom `tests/store_isolation.rs`'s
/// `data_dir_harnesses` and `tests/harness_path_entries.rs` use for shell
/// scripts, applied to manifests instead.
fn tracked_manifests(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--", "*Cargo.toml"])
        .output()
        .expect("listing this repository's tracked manifests");
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
/// nothing this function can check against the matrix's per-OS targets. That
/// gap is real and is not what this test closes — it closes the gap SH-260
/// found, which is a `target_os` literal naming a platform the matrix does
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

/// Maps a `target_os` literal to the substring the release matrix would
/// spell it with, e.g. `"linux"` -> the targets ending
/// `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`.
///
/// Explicit and closed on purpose: an OS this function has not been taught
/// panics rather than silently passing, because "no dependency table names
/// an OS the matrix doesn't build" is a claim this function can only make
/// about OSes it knows how to look for.
fn matrix_substring_for(target_os: &str) -> &'static str {
    match target_os {
        "macos" => "-apple-darwin",
        "linux" => "-unknown-linux-",
        "windows" => "-pc-windows-",
        other => panic!(
            "target_os \"{other}\" has no known release-matrix triple shape — \
             teach `matrix_substring_for` what a target for it looks like \
             before this test can vouch for it"
        ),
    }
}

/// The general invariant SH-259 deliberately left unshipped, because it
/// failed at the time (SH-259's own Linux fix landed before this test could
/// be written against a clean tree): every `cfg(target_os = ...)`
/// dependency table, in every tracked manifest, corresponds to a target the
/// release matrix actually builds. SH-260 found the instance this was
/// generalized from — `windows-native-keyring-store`, gated on Windows and
/// activated by the default `github-sync` feature, for a platform nothing
/// here builds, tests, or ships.
///
/// If a platform's dependency table is later added deliberately (Windows
/// joining the release matrix, say), this test starts passing for it without
/// modification — the matrix, not this file, is what changes to add support.
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

    let workflow = release_workflow();
    let matrix_targets = release_matrix_targets(&workflow);

    for manifest_path in &manifests {
        for (cfg, target_os) in target_os_dependency_tables(manifest_path) {
            let substring = matrix_substring_for(&target_os);
            let relative = manifest_path.strip_prefix(&root).unwrap_or(manifest_path);
            assert!(
                matrix_targets
                    .iter()
                    .any(|target| target.contains(substring)),
                "{}'s `[target.'{cfg}'.dependencies]` names target_os \
                 \"{target_os}\", but no target in release.yml's build \
                 matrix matches (matrix targets: {matrix_targets:?}). Either \
                 add {target_os} to the matrix and prove the dependency by \
                 building it, or drop the dependency for a platform this \
                 project does not ship (SH-260).",
                relative.display()
            );
        }
    }
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
