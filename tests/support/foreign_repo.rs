//! A real foreign project for the portable gate writers (SH-665, SH-777).
//!
//! Real Git, the materialized production bundle, and a detached merge
//! checkout, with no storyhook file in the project: the shape every project
//! gate other than storyhook's own runs in. `tests/portable_receipt.rs` and
//! `tests/portable_progress.rs` both include this module by `#[path]`; it
//! holds only what both of them call, because each includes it into its own
//! crate and an item one of them leaves unused fails clippy's `-D warnings`.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use storyhook::daemon::verifier_bundle;
use storyhook::env::Environment;
use storyhook_test_support::scratch_dir;

/// One foreign repository with a feature branch, its uncertified merge tree,
/// a detached poller worktree at the base, and this build's verifier bundle.
pub(crate) struct ForeignRepo {
    pub(crate) root: tempfile::TempDir,
    pub(crate) repo: PathBuf,
    pub(crate) bundle: PathBuf,
    pub(crate) poller: PathBuf,
    pub(crate) base: String,
    pub(crate) head: String,
    pub(crate) tree: String,
}

/// Runs `command` to completion; the production scripts bound themselves.
pub(crate) fn output(command: &mut Command) -> Output {
    command.output().expect("running production command")
}

/// Asserts success and returns trimmed stdout.
pub(crate) fn success(result: Output) -> String {
    assert!(result.status.success(), "{result:?}");
    String::from_utf8(result.stdout).unwrap().trim().to_owned()
}

/// Runs git in `root` with no inherited repository targeting.
pub(crate) fn git(root: &Path, args: &[&str]) -> String {
    success(output(storyhook::env::git_env::command(root).args(args)))
}

impl ForeignRepo {
    pub(crate) fn new() -> Self {
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
            root,
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

    /// `program` in `cwd` with nothing inherited that would aim it at another
    /// repository, journal, lock or attempt. A run of storyhook's own gate
    /// hands its test children STORYHOOK_GATE_PROGRESS_WRITER and
    /// STORYHOOK_VERIFICATION_ATTEMPT; a fixture must set what it needs.
    pub(crate) fn scrubbed(&self, program: impl AsRef<OsStr>, cwd: &Path) -> Command {
        let mut command = Command::new(program);
        for name in storyhook::env::git_env::scrubbed() {
            command.env_remove(name);
        }
        command
            .current_dir(cwd)
            .env_remove("STORYHOOK_GATE_PROGRESS")
            .env_remove("STORYHOOK_GATE_PROGRESS_WRITER")
            .env_remove("STORYHOOK_GATE_RESULT_FILE")
            .env_remove("STORYHOOK_MACHINE_LOCKS")
            .env_remove("STORYHOOK_VERIFICATION_ATTEMPT")
            .env(
                "STORYHOOK_ACTIVITY_LOG_DIR",
                self.root.path().join("activity"),
            )
            .env("STORYHOOK_LOCK_DIR", self.root.path().join("locks"))
            .env("STORYHOOK_VERIFIER_MIRROR", "0");
        command
    }

    /// The bundled `script` under bash, [scrubbed](Self::scrubbed).
    pub(crate) fn command(&self, script: &str, cwd: &Path) -> Command {
        let mut command = self.scrubbed("bash", cwd);
        command.arg(self.bundle.join(script));
        command
    }

    pub(crate) fn preflight(&self) -> Output {
        output(
            self.command("merge-preflight.sh", &self.repo)
                .args([&self.base, &self.head]),
        )
    }

    /// `merge-watch.sh --speculative-run` of this merge with the gate
    /// `bash -eu -c <body> foreign-gate <args…>`, ready for the caller's
    /// environment.
    pub(crate) fn speculative_run(&self, body: &str, args: &[&str]) -> Command {
        let mut command = self.command("merge-watch.sh", &self.repo);
        command
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
            .args(args);
        command
    }

    /// The poller is back at the base, clean, and owns no merge objects.
    pub(crate) fn assert_restored(&self) {
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
