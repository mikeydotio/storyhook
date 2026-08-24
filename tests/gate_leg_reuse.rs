//! Black-box coverage for reusable gate-leg evidence.
//!
//! The release gate is fail-fast at the Makefile level: if e2e is red, the
//! aggregate `full` receipt is never written. That must not erase successful
//! evidence from earlier, unrelated batteries. These tests provoke the real
//! `scripts/leg.sh --reuse` wrapper in disposable git repositories and count
//! command executions; no receipt is forged and no implementation text is
//! parsed.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

struct Repo {
    root: TempDir,
}

impl Repo {
    fn new() -> Self {
        let root = storyhook_test_support::scratch_dir();
        let repo = Self { root };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.email", "gate-leg@example.test"]);
        repo.git(&["config", "user.name", "Gate Leg Test"]);

        for (path, contents) in [
            ("Cargo.toml", "[package]\nname='fixture'\nversion='0.1.0'\n"),
            ("Makefile", "test:\n\t@true\n"),
            ("src/lib.rs", "pub fn answer() -> u8 { 42 }\n"),
            ("tests/contract.rs", "#[test] fn contract() {}\n"),
            ("e2e/specs/board.spec.ts", "// browser fixture\n"),
            ("scripts/leg.sh", "# fingerprint fixture\n"),
        ] {
            repo.write(path, contents);
        }
        let fingerprint = repo.path().join("scripts/gate-leg-fingerprint.sh");
        fs::copy(
            checkout().join("scripts/gate-leg-fingerprint.sh"),
            &fingerprint,
        )
        .expect("copying the real fingerprint helper into the fixture");
        let mut permissions = fs::metadata(&fingerprint)
            .expect("reading fingerprint helper metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fingerprint, permissions)
            .expect("making fixture fingerprint helper executable");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "fixture"]);
        repo
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.path().join(relative);
        fs::create_dir_all(path.parent().expect("fixture path has parent"))
            .expect("creating fixture parent");
        fs::write(path, contents).expect("writing fixture file");
    }

    fn git(&self, args: &[&str]) -> Output {
        let out = Command::new("git")
            .args(args)
            .current_dir(self.path())
            .output()
            .expect("running git");
        assert!(
            out.status.success(),
            "git {args:?} failed\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn counter(&self, label: &str) -> PathBuf {
        self.path().join(format!("{label}.count"))
    }

    fn run_leg(&self, label: &str, succeeds: bool) -> Output {
        let counter = self.counter(label);
        let script = if succeeds {
            "printf x >> \"$1\""
        } else {
            "printf x >> \"$1\"; exit 23"
        };
        Command::new("bash")
            .arg(checkout().join("scripts/leg.sh"))
            .args(["--reuse", label, "--", "bash", "-c", script, "gate-leg"])
            .arg(counter)
            .current_dir(self.path())
            .output()
            .expect("running reusable leg")
    }

    fn executions(&self, label: &str) -> usize {
        fs::read_to_string(self.counter(label))
            .unwrap_or_default()
            .len()
    }
}

#[test]
fn successful_results_are_reused_until_that_legs_inputs_change() {
    let repo = Repo::new();

    let first = repo.run_leg("fmt", true);
    assert!(first.status.success(), "first run: {first:?}");
    let retry = repo.run_leg("fmt", true);
    assert!(retry.status.success(), "retry: {retry:?}");
    assert_eq!(repo.executions("fmt"), 1, "an unchanged retry reran fmt");
    assert!(
        String::from_utf8_lossy(&retry.stderr).contains("REUSED"),
        "reuse was not reported: {}",
        String::from_utf8_lossy(&retry.stderr)
    );

    repo.write("e2e/specs/board.spec.ts", "// browser-only edit\n");
    let unrelated_edit = repo.run_leg("fmt", true);
    assert!(
        unrelated_edit.status.success(),
        "unrelated edit: {unrelated_edit:?}"
    );
    assert_eq!(
        repo.executions("fmt"),
        1,
        "a browser-only edit invalidated the Rust formatting result"
    );

    repo.write("src/lib.rs", "pub fn answer() -> u8 { 43 }\n");
    let relevant_edit = repo.run_leg("fmt", true);
    assert!(
        relevant_edit.status.success(),
        "relevant edit: {relevant_edit:?}"
    );
    assert_eq!(
        repo.executions("fmt"),
        2,
        "a Rust-source edit did not invalidate the Rust formatting result"
    );
}

#[test]
fn a_failed_leg_never_creates_reusable_evidence() {
    let repo = Repo::new();

    let first = repo.run_leg("e2e", false);
    assert_eq!(first.status.code(), Some(23), "first failure: {first:?}");
    let retry = repo.run_leg("e2e", false);
    assert_eq!(retry.status.code(), Some(23), "second failure: {retry:?}");
    assert_eq!(
        repo.executions("e2e"),
        2,
        "a failed browser result was reused instead of rerun"
    );
}

#[test]
fn a_browser_failure_does_not_invalidate_prior_green_batteries() {
    let repo = Repo::new();

    for label in ["fmt", "clippy", "rust-suite", "build", "plugin"] {
        let out = repo.run_leg(label, true);
        assert!(out.status.success(), "seeding {label}: {out:?}");
    }
    let browser = repo.run_leg("e2e", false);
    assert_eq!(
        browser.status.code(),
        Some(23),
        "browser failure: {browser:?}"
    );

    for label in ["fmt", "clippy", "rust-suite", "build", "plugin"] {
        let out = repo.run_leg(label, true);
        assert!(out.status.success(), "retrying {label}: {out:?}");
        assert_eq!(
            repo.executions(label),
            1,
            "browser failure invalidated unrelated successful battery {label}"
        );
    }
    assert_eq!(repo.executions("e2e"), 1);
}
