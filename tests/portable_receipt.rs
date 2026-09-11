//! SH-665: foreign gates certify through the daemon's portable receipt writer.
//!
//! Real Git, the materialized production bundle, and the production merge
//! reader exercise the contract without copying StoryHook hooks into a project.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use storyhook::daemon::verifier_bundle;
use storyhook::env::Environment;
use storyhook_test_support::scratch_dir;

struct ForeignRepo {
    _root: tempfile::TempDir,
    repo: PathBuf,
    bundle: PathBuf,
    poller: PathBuf,
    base: String,
    head: String,
    tree: String,
}

fn output(command: &mut Command) -> Output {
    command.output().expect("running production command")
}

fn success(result: Output) -> String {
    assert!(result.status.success(), "{result:?}");
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}

fn git(root: &Path, args: &[&str]) -> String {
    success(output(storyhook::env::git_env::command(root).args(args)))
}

impl ForeignRepo {
    fn new() -> Self {
        let root = scratch_dir();
        let repo = root.path().join("foreign project");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "user.email", "t@t"]);
        storyhook_test_support::approve_fixture_identity(&repo, "t", "t@t");
        fs::write(repo.join("base"), "base\n").unwrap();
        git(&repo, &["add", "base"]);
        git(&repo, &["commit", "-qm", "base"]);
        git(&repo, &["checkout", "-qb", "feature"]);
        fs::write(repo.join("feature"), "feature\n").unwrap();
        git(&repo, &["add", "feature"]);
        git(&repo, &["commit", "-qm", "feature"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["checkout", "-q", "main"]);
        fs::write(repo.join("main"), "main\n").unwrap();
        git(&repo, &["add", "main"]);
        git(&repo, &["commit", "-qm", "main"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        let poller = root.path().join("merge checkout");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                poller.to_str().unwrap(),
                &base,
            ],
        );
        let bundle =
            verifier_bundle::materialize(&Environment::at(root.path().join("daemon state")))
                .unwrap();
        let mut fixture = Self {
            _root: root,
            repo,
            bundle,
            poller,
            base,
            head,
            tree: String::new(),
        };
        let result = fixture.preflight();
        assert_eq!(
            result.status.code(),
            Some(1),
            "initial tree is uncertified: {result:?}"
        );
        fixture.tree = String::from_utf8(result.stdout).unwrap().trim().to_owned();
        assert!(!fixture.tree.is_empty());
        fixture
    }

    fn command(&self, script: &str, cwd: &Path) -> Command {
        let mut command = Command::new("bash");
        for name in storyhook::env::git_env::scrubbed() {
            command.env_remove(name);
        }
        command
            .arg(self.bundle.join(script))
            .current_dir(cwd)
            .env_remove("STORYHOOK_GATE_PROGRESS")
            .env_remove("STORYHOOK_GATE_RESULT_FILE")
            .env_remove("STORYHOOK_MACHINE_LOCKS")
            .env(
                "STORYHOOK_ACTIVITY_LOG_DIR",
                self._root.path().join("activity"),
            )
            .env("STORYHOOK_LOCK_DIR", self._root.path().join("locks"))
            .env("STORYHOOK_VERIFIER_MIRROR", "0");
        command
    }

    fn preflight(&self) -> Output {
        output(
            self.command("merge-preflight.sh", &self.repo)
                .args([&self.base, &self.head]),
        )
    }

    fn gate(&self, body: &str, args: &[&str]) -> Output {
        output(
            self.command("merge-watch.sh", &self.repo)
                .env("STORYHOOK_GATE_RECEIPT", "/inherited/invalid/receipt")
                .args([
                    "--speculative-run",
                    &self.tree,
                    &self.base,
                    &self.head,
                    self.poller.to_str().unwrap(),
                    "--",
                    "bash",
                    "-eu",
                    "-c",
                    body,
                    "foreign-gate",
                ])
                .args(args),
        )
    }

    fn writer(&self, args: &[&str]) -> Output {
        output(self.command("tree-receipt.sh", &self.repo).args(args))
    }

    fn receipt(&self) -> PathBuf {
        self.repo
            .join(".git/storyhook/gate-receipts")
            .join(&self.tree)
    }

    fn assert_restored(&self) {
        assert_eq!(git(&self.poller, &["rev-parse", "HEAD"]), self.base);
        assert_eq!(git(&self.poller, &["status", "--porcelain"]), "");
        let state = self.repo.join(".git/storyhook");
        if state.exists() {
            assert!(fs::read_dir(state).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("merge-watch-objects.")
            }));
        }
    }
}

#[test]
fn bundled_writer_certifies_foreign_merge_without_enrolling_hooks() {
    for hooks in [None, Some("custom hooks")] {
        for tier in ["gate", "full"] {
            let fixture = ForeignRepo::new();
            if let Some(hooks) = hooks {
                git(&fixture.repo, &["config", "core.hooksPath", hooks]);
            }
            let config_before = fs::read(fixture.repo.join(".git/config")).unwrap();
            success(fixture.gate(
                r#"
test "$STORYHOOK_GATE_RECEIPT" = "$1"
test ! -e scripts
test ! -e .githooks
"$STORYHOOK_GATE_RECEIPT" preflight
test "$(cat base)" = base
test "$(cat main)" = main
test "$(cat feature)" = feature
"$STORYHOOK_GATE_RECEIPT" postlude "$2"
"#,
                &[
                    fixture.bundle.join("tree-receipt.sh").to_str().unwrap(),
                    tier,
                ],
            ));
            assert_eq!(success(fixture.preflight()), fixture.tree);
            assert!(
                fs::read_to_string(fixture.receipt())
                    .unwrap()
                    .contains(&format!("tier {tier}\n"))
            );
            assert_eq!(
                fs::read(fixture.repo.join(".git/config")).unwrap(),
                config_before
            );
            fixture.assert_restored();
        }
    }
}

#[test]
fn a_failed_or_unbracketed_foreign_gate_never_certifies() {
    for (body, status) in [
        ("true", 0),
        ("\"$STORYHOOK_GATE_RECEIPT\" preflight", 0),
        ("\"$STORYHOOK_GATE_RECEIPT\" postlude gate", 1),
        (
            "\"$STORYHOOK_GATE_RECEIPT\" preflight\nfalse\n\"$STORYHOOK_GATE_RECEIPT\" postlude gate",
            1,
        ),
    ] {
        let fixture = ForeignRepo::new();
        let result = fixture.gate(body, &[]);
        assert_eq!(result.status.code(), Some(status), "{body}: {result:?}");
        assert!(!fixture.receipt().exists());
        assert_eq!(fixture.preflight().status.code(), Some(1));
        fixture.assert_restored();
    }
}

#[test]
fn portable_changed_receipt_cannot_certify_a_merge() {
    let fixture = ForeignRepo::new();
    success(fixture.writer(&["preflight"]));
    success(fixture.writer(&["postlude", "gate"]));
    let base_tree = git(&fixture.repo, &["rev-parse", "HEAD^{tree}"]);
    success(fixture.gate("\"$STORYHOOK_GATE_RECEIPT\" preflight\n\"$STORYHOOK_GATE_RECEIPT\" postlude changed \"$1\"", &[&base_tree]));
    assert!(
        fs::read_to_string(fixture.receipt())
            .unwrap()
            .contains("tier changed\n")
    );
    assert_eq!(fixture.preflight().status.code(), Some(1));
    fixture.assert_restored();
}

#[test]
fn portable_writer_rejects_drift_and_keeps_receipts_project_local() {
    let fixture = ForeignRepo::new();
    success(fixture.writer(&["preflight"]));
    fs::write(fixture.repo.join("base"), "changed during gate\n").unwrap();
    let result = fixture.writer(&["postlude", "gate"]);
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("tracked content changed"),
        "{result:?}"
    );
    assert!(!fixture.repo.join(".git/storyhook/gate-receipts").exists());
    assert!(!fixture.repo.join(".git/storyhook-gate-preflight").exists());
    fs::write(fixture.repo.join("base"), "base\n").unwrap();
    success(fixture.gate(
        "\"$STORYHOOK_GATE_RECEIPT\" preflight\n\"$STORYHOOK_GATE_RECEIPT\" postlude gate",
        &[],
    ));
    assert_eq!(success(fixture.preflight()), fixture.tree);
    let other = ForeignRepo::new();
    assert_eq!(
        other.tree, fixture.tree,
        "identical content, separate projects"
    );
    assert_eq!(other.preflight().status.code(), Some(1));
    assert!(!other.receipt().exists());
}
