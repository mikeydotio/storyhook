//! SH-406: two builds of `story` must never report the same version string.
//!
//! `build.rs` stamps every build with the git tree object id of its tracked
//! content, via `scripts/tracked-tree.sh`. This suite proves both artifacts
//! **for real** — a throwaway git repository, the actual tracked scripts
//! (symlinked in, never copied — the `tests/push_gate.rs` convention), and
//! `build.rs` itself compiled standalone with `rustc` and executed. The
//! standalone-compile approach exists because `cargo test` never runs a
//! crate's own build script as a test target, and a full `cargo build` in an
//! isolated target directory to exercise it end to end would cost as long as
//! a clean build (tens of seconds) per test — `build.rs` has zero external
//! dependencies (only `std`), so `rustc --edition 2024 build.rs -o <bin>`
//! compiles the literal production file in under a second and runs it with
//! `CARGO_MANIFEST_DIR` set by hand, the one environment variable cargo would
//! otherwise supply. This tests the actual file that ships, never a copy
//! pasted into this suite (the SH-136 doctrine).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

use storyhook_test_support::scratch_dir;
use tempfile::TempDir;

/// The checkout under test — the tracked scripts and `build.rs` live here.
fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn run(cwd: &Path, program: &str, args: &[&str]) -> Output {
    Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|e| panic!("running {program}: {e}"))
}

fn assert_ok(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} should have succeeded\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// ---------------------------------------------------------------------------
// scripts/tracked-tree.sh — provoked directly, real git, real repo
// ---------------------------------------------------------------------------

/// A minimal repo for exercising `scripts/tracked-tree.sh` in isolation.
struct TreeRepo {
    dir: TempDir,
}

impl TreeRepo {
    fn new() -> Self {
        let repo = Self { dir: scratch_dir() };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.email", "t@t"]);
        repo.git(&["config", "user.name", "t"]);
        repo.write("f", "a\n");
        repo.git(&["add", "f"]);
        repo.git(&["commit", "-qm", "init"]);
        repo
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.path().join(name), body).expect("fixture: writing a tracked file");
    }

    fn git(&self, args: &[&str]) -> Output {
        run(self.path(), "git", args)
    }

    /// Runs the real, tracked `scripts/tracked-tree.sh` against this repo.
    fn tree(&self) -> Output {
        run(
            self.path(),
            "bash",
            &[&checkout()
                .join("scripts")
                .join("tracked-tree.sh")
                .display()
                .to_string()],
        )
    }
}

#[test]
fn a_clean_repo_prints_a_tree_oid() {
    let repo = TreeRepo::new();
    let out = repo.tree();
    assert_ok(&out, "tracked-tree.sh on a clean repo");
    let oid = stdout(&out);
    assert_eq!(
        oid.len(),
        40,
        "a git tree oid is 40 hex characters: {oid:?}"
    );
    assert!(
        oid.bytes().all(|b| b.is_ascii_hexdigit()),
        "not all hex: {oid:?}"
    );
}

#[test]
fn editing_a_tracked_file_changes_the_oid_and_reverting_restores_it() {
    let repo = TreeRepo::new();
    let before = stdout(&repo.tree());

    repo.write("f", "b\n");
    let edited = stdout(&repo.tree());
    assert_ne!(before, edited, "editing a tracked file must change the oid");

    repo.write("f", "a\n");
    let reverted = stdout(&repo.tree());
    assert_eq!(
        before, reverted,
        "reverting the edit must reproduce the exact original oid — the oid is \
         content, not a counter"
    );
}

#[test]
fn an_untracked_file_does_not_change_the_oid() {
    let repo = TreeRepo::new();
    let before = stdout(&repo.tree());

    repo.write("scratch.txt", "not part of the identity\n");
    let after = stdout(&repo.tree());

    assert_eq!(
        before, after,
        "an untracked file must not affect the tracked-content identity"
    );
}

#[test]
fn a_commit_that_changes_no_content_leaves_the_oid_unchanged() {
    let repo = TreeRepo::new();
    let before = stdout(&repo.tree());

    assert_ok(
        &repo.git(&["commit", "--allow-empty", "-qm", "empty"]),
        "an empty commit",
    );
    let after = stdout(&repo.tree());

    assert_eq!(
        before, after,
        "a commit with no content change must not change the oid — this is what \
         makes cargo's default 'rerun on any file change' policy a safe superset \
         trigger rather than a source of spurious stamp churn"
    );
}

/// Positive control (this project's convention, e.g. `tests/store_isolation.rs`):
/// proves the script can fail, so a script that silently always "succeeds"
/// with garbage is distinguishable from a correct one.
#[test]
fn fails_outside_a_git_repository() {
    let dir = scratch_dir();
    let out = run(
        dir.path(),
        "bash",
        &[&checkout()
            .join("scripts")
            .join("tracked-tree.sh")
            .display()
            .to_string()],
    );
    assert!(
        !out.status.success(),
        "must refuse outside a git repository, not print garbage"
    );
    assert!(
        stdout(&out).is_empty(),
        "must print nothing on failure, not a partial answer: {:?}",
        stdout(&out)
    );
}

// ---------------------------------------------------------------------------
// build.rs — compiled standalone with rustc, run for real
// ---------------------------------------------------------------------------

/// Compiles the literal, tracked `build.rs` once per test process with
/// `rustc` (it has zero external dependencies) and returns the path to the
/// resulting binary. `build.rs` never fails a build, so any panic here is a
/// fixture-compile problem, never a "build.rs rejected its own compilation"
/// case.
fn compiled_build_rs() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let out_dir = scratch_dir();
        // Leaked deliberately: the TempDir must outlive every test using this
        // binary, and this helper is called from many `#[test]` functions
        // across threads. It is scratch build output, not project state.
        let out_path = out_dir.path().join("build_script_under_test");
        std::mem::forget(out_dir);

        let compile = Command::new("rustc")
            .args(["--edition", "2024"])
            .arg(checkout().join("build.rs"))
            .arg("-o")
            .arg(&out_path)
            .output()
            .expect("running rustc");
        assert!(
            compile.status.success(),
            "compiling build.rs failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&compile.stdout),
            String::from_utf8_lossy(&compile.stderr)
        );
        out_path
    })
}

/// A repo shaped like a real checkout for `build.rs`'s purposes: a `.git`
/// directory and the real `scripts/tracked-tree.sh`, symlinked in rather than
/// copied (the `tests/push_gate.rs` convention) so this exercises the exact
/// file that ships.
struct ManifestFixture {
    dir: TempDir,
}

impl ManifestFixture {
    /// `.git` present, `scripts/tracked-tree.sh` present and working — the
    /// production shape.
    fn with_git_and_script() -> Self {
        let fixture = Self::bare();
        fixture.git(&["init", "-q", "-b", "main"]);
        fixture.git(&["config", "user.email", "t@t"]);
        fixture.git(&["config", "user.name", "t"]);
        std::fs::write(fixture.path().join("f"), "a\n").expect("fixture: writing a tracked file");
        fixture.git(&["add", "f"]);
        fixture.git(&["commit", "-qm", "init"]);

        std::fs::create_dir(fixture.path().join("scripts")).expect("fixture: scripts dir");
        std::os::unix::fs::symlink(
            checkout().join("scripts").join("tracked-tree.sh"),
            fixture.path().join("scripts").join("tracked-tree.sh"),
        )
        .expect("fixture: linking the tracked script");

        fixture
    }

    /// `.git` present, but no `scripts/tracked-tree.sh` — the "checkout is
    /// missing the script" case, distinct from "no `.git` at all."
    fn with_git_no_script() -> Self {
        let fixture = Self::bare();
        fixture.git(&["init", "-q", "-b", "main"]);
        fixture
    }

    /// No `.git` at all — a release tarball or a packaged `cargo install`
    /// source. The expected, silent case.
    fn bare() -> Self {
        Self { dir: scratch_dir() }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn git(&self, args: &[&str]) -> Output {
        run(self.path(), "git", args)
    }

    /// Runs the compiled `build.rs` with `CARGO_MANIFEST_DIR` pointed at this
    /// fixture — the one variable cargo would otherwise supply.
    fn run_build_script(&self, extra_env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(compiled_build_rs());
        cmd.env("CARGO_MANIFEST_DIR", self.path())
            .env_remove("STORYHOOK_BUILD_ID");
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        cmd.output().expect("running the compiled build.rs")
    }
}

fn emitted_build_id(out: &Output) -> Option<String> {
    stdout(out)
        .lines()
        .find_map(|line| line.strip_prefix("cargo::rustc-env=STORYHOOK_BUILD_ID="))
        .map(str::to_string)
}

fn emitted_warning(out: &Output) -> bool {
    stdout(out)
        .lines()
        .any(|line| line.starts_with("cargo::warning="))
}

#[test]
fn with_git_and_the_script_it_stamps_the_tracked_tree_id() {
    let fixture = ManifestFixture::with_git_and_script();
    let out = fixture.run_build_script(&[]);
    assert_ok(&out, "build.rs against a real fixture");

    let expected_full = stdout(&run(
        fixture.path(),
        "bash",
        &[&checkout()
            .join("scripts")
            .join("tracked-tree.sh")
            .display()
            .to_string()],
    ));
    let expected = &expected_full[..12];

    assert_eq!(
        emitted_build_id(&out).as_deref(),
        Some(expected),
        "build.rs must emit the first 12 hex chars of the real tracked-tree.sh \
         output\nfull stdout: {}",
        stdout(&out)
    );
    assert!(!emitted_warning(&out), "the expected case must be silent");
}

#[test]
fn the_env_override_wins_and_is_trimmed_to_its_first_line() {
    let fixture = ManifestFixture::with_git_and_script();
    let out = fixture.run_build_script(&[("STORYHOOK_BUILD_ID", "  deadBEEF1234  \nignored\n")]);
    assert_ok(&out, "build.rs with an override set");

    assert_eq!(
        emitted_build_id(&out).as_deref(),
        Some("deadBEEF1234"),
        "the override must win verbatim (only trimmed and first-line-only), \
         even when the real tree is available"
    );
    assert!(
        !emitted_warning(&out),
        "an explicit override is never an error case"
    );
}

#[test]
fn an_empty_override_falls_through_to_the_tracked_tree() {
    let fixture = ManifestFixture::with_git_and_script();
    let with_empty = fixture.run_build_script(&[("STORYHOOK_BUILD_ID", "")]);
    let with_none = fixture.run_build_script(&[]);

    assert_eq!(
        emitted_build_id(&with_empty),
        emitted_build_id(&with_none),
        "an empty override must be treated as absent, not as a literal empty stamp"
    );
}

#[test]
fn no_git_directory_is_silent() {
    let fixture = ManifestFixture::bare();
    let out = fixture.run_build_script(&[]);
    assert_ok(&out, "build.rs with no .git at all");

    assert_eq!(
        emitted_build_id(&out),
        None,
        "a release tarball / packaged source has no .git — this must be silent"
    );
    assert!(
        !emitted_warning(&out),
        "the expected 'nothing to stamp with' case must not warn"
    );
}

#[test]
fn a_missing_script_with_git_present_warns_but_still_succeeds() {
    let fixture = ManifestFixture::with_git_no_script();
    let out = fixture.run_build_script(&[]);
    assert_ok(
        &out,
        "build.rs must never fail a build even when its own script is missing",
    );

    assert_eq!(
        emitted_build_id(&out),
        None,
        "no script means no stamp — but the build itself must still succeed"
    );
    assert!(
        emitted_warning(&out),
        "a .git directory IS present, so a missing script is unexpected and \
         must not be silent (the SH-306 doctrine: a gate's silence must not be \
         mistaken for 'nothing needed reporting')"
    );
}

// ---------------------------------------------------------------------------
// build.rs's own contract: no rerun-if-* directive
// ---------------------------------------------------------------------------

#[test]
fn build_rs_emits_no_rerun_if_directive() {
    let source = std::fs::read_to_string(checkout().join("build.rs")).expect("reading build.rs");

    // Only code lines count -- the module doc legitimately discusses
    // `cargo::rerun-if-*` in prose (backtick-quoted) to explain why none is
    // emitted, and that discussion must not trip this fence. An actual
    // emission is a string literal fed to `println!`, which appears as a
    // double-quote immediately followed by the directive name; prose never
    // has the quote directly abutting it.
    let code_lines: Vec<&str> = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect();
    let has_emission = code_lines.iter().any(|line| {
        line.contains("\"cargo::rerun-if-changed") || line.contains("\"cargo::rerun-if-env-changed")
    });

    assert!(
        !has_emission,
        "build.rs must not emit any cargo::rerun-if-* directive: doing so \
         REPLACES cargo's default 'rerun on any tracked-adjacent file change' \
         policy with only the directives listed, which would stop the stamp \
         from ever being recomputed on ordinary source edits unless every \
         relevant file were hand-enumerated here"
    );

    // Positive control: prove the scan can actually detect the shape it is
    // guarding against, on non-comment code, so a scan that stopped working
    // reads as a failure rather than a silently vacuous pass.
    let provoked = "    println!(\"cargo::rerun-if-changed=build.rs\");";
    assert!(
        provoked.contains("\"cargo::rerun-if-changed"),
        "the detector pattern itself must match an actual emission line"
    );
}

// ---------------------------------------------------------------------------
// story --version's actual contract
// ---------------------------------------------------------------------------

fn version_output() -> String {
    let out = assert_cmd::Command::cargo_bin("story")
        .unwrap()
        .arg("--version")
        .output()
        .expect("running story --version");
    assert_ok(&out, "story --version");
    stdout(&out)
}

#[test]
fn version_output_matches_the_documented_shape() {
    let line = version_output();
    let bare = format!("story {}", env!("CARGO_PKG_VERSION"));
    assert!(
        line == bare || line.starts_with(&format!("{bare} (build ")),
        "expected `{bare}` or `{bare} (build <12 hex chars>)`, got {line:?}"
    );
    if let Some(rest) = line.strip_prefix(&format!("{bare} (build ")) {
        let id = rest
            .strip_suffix(')')
            .unwrap_or_else(|| panic!("build id suffix must end in a closing paren: {line:?}"));
        assert_eq!(id.len(), 12, "build id must be 12 hex chars: {line:?}");
        assert!(
            id.bytes().all(|b| b.is_ascii_hexdigit()),
            "build id must be hex: {line:?}"
        );
    }
}

#[test]
fn the_second_whitespace_field_is_the_bare_semver() {
    let line = version_output();
    let field = line
        .split_whitespace()
        .nth(1)
        .expect("a second whitespace-separated field");
    assert_eq!(
        field,
        env!("CARGO_PKG_VERSION"),
        "scripts/release.sh and Makefile's `awk '{{print $2}}'` both depend on \
         field 2 being the bare semver, with or without a build id present"
    );
}

/// This checkout is itself a git repo (worktree or not), so the real binary
/// under test must actually carry a build id — proving the stamp reaches
/// production, not just the isolated fixtures above.
#[test]
fn this_checkouts_own_binary_carries_a_build_id() {
    let line = version_output();
    assert!(
        line.contains(" (build "),
        "this checkout has a .git — story --version must carry a build id: {line:?}"
    );
}
