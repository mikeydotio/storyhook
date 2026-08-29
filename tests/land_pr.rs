//! `scripts/land-pr.sh`'s lock-and-certify core, provoked with real git.
//!
//! SH-458 replaces the autonomous charter's bare `gh pr merge` with a tool
//! that takes the machine-wide `merge` lock itself and refuses to invoke its
//! merge command until `merge-preflight.sh` recognizes the exact predicted
//! tree. GitHub orchestration deliberately stays outside this Rust suite:
//! imitating `gh` would validate the imitation, not GitHub. The script's
//! private `--certified-run` seam instead accepts an arbitrary command after
//! the production preflight. These tests use a filesystem witness for that
//! command, which proves whether it ran without asserting fake API behaviour.
//!
//! The fixture is a disposable real repository. The tracked production
//! scripts are reached through symlinks, receipts are written only through
//! `gate-receipt.sh`, and every lock lives under a per-test override.
//!
//! # Mutation checks
//!
//! Measured by hand before merge, snapshotting the script before each change:
//!
//! - Move the merge command before `merge-preflight.sh` -> the uncertified and
//!   conflict cases go red because their witness commands run.
//! - Delete `require_merge_lock` from `--certified-run` ->
//!   `the_certification_core_refuses_to_run_outside_the_merge_lock` goes red.
//! - Point preflight at a nonexistent receipt store ->
//!   `a_production_written_receipt_allows_the_merge_command` goes red.
//!
//! With the shipped script all tests in this file pass.

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use storyhook_test_support::scratch_dir;
use tempfile::TempDir;

fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

struct LandRepo {
    dir: TempDir,
}

impl LandRepo {
    fn new() -> Self {
        let repo = Self { dir: scratch_dir() };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.email", "t@t"]);
        repo.git(&["config", "user.name", "t"]);

        std::fs::create_dir_all(repo.path().join("scripts"))
            .expect("fixture: creating scripts directory");
        for name in [
            "land-pr.sh",
            "machine-lock.sh",
            "merge-preflight.sh",
            "gate-receipt.sh",
            "tracked-tree.sh",
        ] {
            std::os::unix::fs::symlink(
                checkout().join("scripts").join(name),
                repo.path().join("scripts").join(name),
            )
            .unwrap_or_else(|e| panic!("fixture: linking {name}: {e}"));
        }
        std::os::unix::fs::symlink(checkout().join(".githooks"), repo.path().join(".githooks"))
            .expect("fixture: linking production hooks");
        std::fs::create_dir_all(repo.lock_root()).expect("fixture: creating lock root");

        repo.write("f", "base\n");
        repo.git(&["add", "f"]);
        repo.git(&["commit", "-qm", "base"]);
        repo
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn lock_root(&self) -> PathBuf {
        self.path().join("locks")
    }

    fn script(&self, name: &str) -> String {
        self.path().join("scripts").join(name).display().to_string()
    }

    fn write(&self, name: &str, body: &str) {
        std::fs::write(self.path().join(name), body).expect("fixture: writing file");
    }

    fn helper(&self, name: &str, body: &str) -> PathBuf {
        let path = self.path().join(name);
        let mut file = std::fs::File::create(&path)
            .unwrap_or_else(|e| panic!("fixture: creating {}: {e}", path.display()));
        file.write_all(body.as_bytes())
            .expect("fixture: writing helper");
        drop(file);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("fixture: making helper executable");
        path
    }

    fn git(&self, args: &[&str]) -> Output {
        let out = command(self.path(), "git", args)
            .output()
            .expect("running git");
        assert_ok(&out, &format!("git {}", args.join(" ")));
        out
    }

    fn rev_parse(&self, rev: &str) -> String {
        stdout(&self.git(&["rev-parse", rev]))
    }

    fn branch(&self, name: &str, from: &str, file: &str, body: &str) -> String {
        self.git(&["checkout", "-q", "-b", name, from]);
        self.write(file, body);
        self.git(&["add", file]);
        self.git(&["commit", "-qm", &format!("{name}: {file}")]);
        self.rev_parse("HEAD")
    }

    fn gate(&self, args: &[&str]) -> Output {
        let mut all = vec![self.script("gate-receipt.sh")];
        all.extend(args.iter().map(|s| (*s).to_string()));
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        command(self.path(), "bash", &refs)
            .output()
            .expect("running receipt writer")
    }

    /// Materialize the predicted merge in this same repository and certify
    /// its tree through the production writer.
    fn certify_merge(&self, base: &str, head: &str) -> String {
        self.git(&["checkout", "-q", "-B", "certified", base]);
        self.git(&["merge", "-q", "--no-edit", head]);
        let tree = self.rev_parse("HEAD^{tree}");
        assert_ok(&self.gate(&["preflight"]), "enrolling the merge tree");
        assert_ok(&self.gate(&["postlude"]), "certifying the merge tree");
        tree
    }

    fn core_command(&self, base: &str, head: &str, after: &[&str]) -> Command {
        let mut cmd = command(
            self.path(),
            "bash",
            &[
                &self.script("machine-lock.sh"),
                "merge",
                "--",
                "bash",
                &self.script("land-pr.sh"),
                "--certified-run",
                base,
                head,
                "--",
            ],
        );
        cmd.args(after)
            .env("STORYHOOK_LOCK_DIR", self.lock_root())
            .env_remove("STORYHOOK_MACHINE_LOCKS");
        cmd
    }

    fn run_core(&self, base: &str, head: &str, after: &[&str]) -> Output {
        self.core_command(base, head, after)
            .output()
            .expect("running locked certification core")
    }

    fn run_core_unlocked(&self, base: &str, head: &str, after: &[&str]) -> Output {
        let mut args = vec![
            self.script("land-pr.sh"),
            "--certified-run".to_string(),
            base.to_string(),
            head.to_string(),
            "--".to_string(),
        ];
        args.extend(after.iter().map(|s| (*s).to_string()));
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        command(self.path(), "bash", &refs)
            .env("STORYHOOK_LOCK_DIR", self.lock_root())
            .env_remove("STORYHOOK_MACHINE_LOCKS")
            .output()
            .expect("running unlocked certification core")
    }

    fn hold_merge_lock(&self, seconds: &str) -> Child {
        command(
            self.path(),
            "bash",
            &[
                &self.script("machine-lock.sh"),
                "merge",
                "--",
                "sleep",
                seconds,
            ],
        )
        .env("STORYHOOK_LOCK_DIR", self.lock_root())
        .env_remove("STORYHOOK_MACHINE_LOCKS")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawning merge-lock holder")
    }
}

fn command(cwd: &Path, program: &str, args: &[&str]) -> Command {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    cmd
}

fn assert_ok(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn wait_for(path: &Path) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if path.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("{} did not appear", path.display());
}

#[test]
fn the_public_command_refuses_a_missing_pr() {
    let repo = LandRepo::new();
    let out = command(repo.path(), "bash", &[&repo.script("land-pr.sh")])
        .output()
        .expect("running land-pr without a PR");

    assert!(!out.status.success());
    assert!(stderr(&out).contains("usage: land-pr.sh <pr>"));
}

#[test]
fn an_uncertified_tree_never_reaches_the_merge_command() {
    let repo = LandRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("feature", "main", "g", "feature\n");
    let witness = repo.path().join("merged");
    let helper = repo.helper("witness.sh", "#!/bin/sh\nprintf ran > \"$1\"\n");

    let out = repo.run_core(
        &base,
        &head,
        &[
            &helper.display().to_string(),
            &witness.display().to_string(),
        ],
    );

    assert_eq!(out.status.code(), Some(1));
    assert!(!witness.exists(), "the merge witness must not run");
    assert!(stderr(&out).contains("not certified"));
}

#[test]
fn a_production_written_receipt_allows_the_merge_command() {
    let repo = LandRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("feature", "main", "g", "feature\n");
    let expected = repo.certify_merge(&base, &head);
    let witness = repo.path().join("merged");
    let helper = repo.helper(
        "witness.sh",
        "#!/bin/sh\nprintf '%s\n' \"$STORYHOOK_CERTIFIED_MERGE_TREE\" > \"$1\"\n",
    );

    let out = repo.run_core(
        &base,
        &head,
        &[
            &helper.display().to_string(),
            &witness.display().to_string(),
        ],
    );

    assert_ok(&out, "a certified merge command");
    assert_eq!(
        std::fs::read_to_string(&witness).expect("reading merge witness"),
        format!("{expected}\n"),
        "the live phase must receive the exact tree preflight certified"
    );
}

#[test]
fn a_conflict_never_reaches_the_merge_command() {
    let repo = LandRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("feature", "main", "f", "feature\n");
    repo.git(&["checkout", "-q", "main"]);
    repo.write("f", "main\n");
    repo.git(&["add", "f"]);
    repo.git(&["commit", "-qm", "main: conflict"]);
    let moved_base = repo.rev_parse("main");
    let witness = repo.path().join("merged");
    let helper = repo.helper("witness.sh", "#!/bin/sh\nprintf ran > \"$1\"\n");

    let out = repo.run_core(
        &moved_base,
        &head,
        &[
            &helper.display().to_string(),
            &witness.display().to_string(),
        ],
    );

    assert_eq!(out.status.code(), Some(2));
    assert!(!witness.exists(), "a conflicted merge command must not run");
    assert!(stderr(&out).contains("CONFLICT"));
    assert_ne!(base, moved_base, "fixture: main must have advanced");
}

#[test]
fn the_certification_core_refuses_to_run_outside_the_merge_lock() {
    let repo = LandRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("feature", "main", "g", "feature\n");
    repo.certify_merge(&base, &head);
    let witness = repo.path().join("merged");
    let helper = repo.helper("witness.sh", "#!/bin/sh\nprintf ran > \"$1\"\n");

    let out = repo.run_core_unlocked(
        &base,
        &head,
        &[
            &helper.display().to_string(),
            &witness.display().to_string(),
        ],
    );

    assert!(!out.status.success());
    assert!(!witness.exists());
    assert!(stderr(&out).contains("must run under machine-lock.sh merge"));
}

#[test]
fn the_merge_commands_failure_is_preserved() {
    let repo = LandRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("feature", "main", "g", "feature\n");
    repo.certify_merge(&base, &head);

    let out = repo.run_core(&base, &head, &["sh", "-c", "exit 7"]);

    assert_eq!(out.status.code(), Some(7));
}

#[test]
fn certification_and_the_merge_command_wait_behind_the_merge_lock() {
    let repo = LandRepo::new();
    let base = repo.rev_parse("main");
    let head = repo.branch("feature", "main", "g", "feature\n");
    repo.certify_merge(&base, &head);
    let witness = repo.path().join("merged");
    let helper = repo.helper("witness.sh", "#!/bin/sh\nprintf ran > \"$1\"\n");

    let mut holder = repo.hold_merge_lock("2");
    wait_for(&repo.lock_root().join("merge.lock").join("pid"));
    let out = repo.run_core(
        &base,
        &head,
        &[
            &helper.display().to_string(),
            &witness.display().to_string(),
        ],
    );
    holder.wait().expect("reaping merge-lock holder");

    assert_ok(&out, "the queued landing command");
    assert!(witness.exists());
    assert!(
        stderr(&out).contains("waiting for the 'merge' lock"),
        "waiting must be reported: {}",
        stderr(&out)
    );
}
