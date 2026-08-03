//! A git hook says nothing when storyhook cannot answer.
//!
//! This obligation predates the transport collapse and was never pinned. It got
//! sharper when the collapse landed: a hook used to have somewhere else to go,
//! because `--local` ran the command in the hook's own process and needed no
//! daemon. It has nowhere else to go now, so "the daemon will not start" became
//! a failure mode that every `git commit` on the machine can meet.
//!
//! What must not happen then is noise. A person committing code did not ask
//! storyhook anything; the hook is storyhook asking *them* for an opportunity,
//! and an error printed into the middle of `git commit` is a tool that has
//! forgotten whose turn it is. Worse, it reads as a failed commit — the commit
//! succeeds, and the last thing on the screen is an error.
//!
//! The mechanism is in `src/hooks.rs`, where each managed hook redirects
//! storyhook's stderr to `/dev/null` and swallows its exit code (`2>/dev/null`
//! plus `|| true`, or an early `exit 0`). That is three shell fragments in a
//! string literal, which is the shape SH-56 was filed about: hook logic shipped
//! untested. This file runs them.

use std::path::Path;
use std::process::Command;

use storyhook_test_support::{TestEnv, scratch_dir};
use tempfile::TempDir;

/// A git repository with a storyhook project and the managed hooks installed,
/// in an environment of its own.
///
/// Isolated rather than shared, because the test below **breaks the store** —
/// which is a fact about a file every other test in a shared environment would
/// also see.
struct HookRepo {
    env: TestEnv,
    dir: TempDir,
}

impl HookRepo {
    fn new() -> Self {
        let repo = HookRepo {
            env: TestEnv::isolated(),
            dir: scratch_dir(),
        };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.email", "t@t"]);
        repo.git(&["config", "user.name", "t"]);
        std::fs::write(repo.path().join("f"), "a\n").expect("seeding a tracked file");
        repo.git(&["add", "f"]);
        repo.git(&["commit", "-qm", "init"]);

        repo.story(&["project", "new", "--prefix", "SH"]);
        repo.story(&["hooks", "install"]);
        for hook in ["post-commit", "prepare-commit-msg"] {
            assert!(
                repo.path().join(".git/hooks").join(hook).is_file(),
                "fixture: the managed {hook} hook must be installed"
            );
        }
        repo
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Runs `git <args>`, with the fixture's environment and a `PATH` that finds
    /// the binary under test.
    ///
    /// The `PATH` is the load-bearing part: a hook runs `story` by name, and
    /// with the ambient one that is whatever the developer has installed.
    fn git(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new("git");
        cmd.current_dir(self.path());
        self.env.apply(&mut cmd);
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        cmd.args(args);
        cmd.output().expect("running git")
    }

    fn story(&self, args: &[&str]) {
        let out = self
            .env
            .story(self.path())
            .args(args)
            .output()
            .expect("running story");
        assert!(
            out.status.success(),
            "fixture: `story {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Replaces the store with bytes no SQLite can open, and makes sure nothing
    /// is holding the old one.
    ///
    /// The `-wal` and `-shm` go too, and that is not tidiness: SQLite rebuilds
    /// the database from its log, so a `store.db` full of garbage beside a
    /// healthy log is a store that opens perfectly and proves nothing.
    fn break_the_store(&self) {
        self.env.stop_daemon();
        for suffix in ["", "-wal", "-shm"] {
            let path = self
                .env
                .store_path()
                .with_file_name(format!("store.db{suffix}"));
            let _ = std::fs::remove_file(path);
        }
        std::fs::write(self.env.store_path(), b"not a database\n").expect("breaking the store");
    }
}

/// **The obligation.** A commit made while storyhook cannot run prints nothing
/// of storyhook's, and succeeds.
///
/// Two assertions rather than one, because they fail differently and only one of
/// them is about silence. A hook that crashed the commit would be caught by the
/// exit status; a hook that let the commit through while printing a wall of
/// diagnosis would not.
#[test]
fn a_commit_says_nothing_when_the_daemon_cannot_start() {
    let repo = HookRepo::new();

    // The precondition, asserted rather than assumed: a plain `story` command
    // has to be genuinely unable to answer, or this test passes for free.
    repo.break_the_store();
    let probe = repo
        .env
        .story(repo.path())
        .args(["list"])
        .output()
        .expect("running story");
    assert!(
        !probe.status.success(),
        "the fixture needs a storyhook that cannot answer; `story list` succeeded"
    );

    std::fs::write(repo.path().join("f"), "b\n").expect("changing the tracked file");
    let out = repo.git(&["commit", "-qam", "a commit made on a broken machine"]);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a commit must not fail because storyhook cannot answer.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(
        stderr.trim(),
        "",
        "the hooks must say nothing at all — a person committing code did not ask \
         storyhook a question, and an error here reads as a failed commit"
    );
    assert_eq!(stdout.trim(), "", "and nothing on stdout either: {stdout}");
}

/// The control, and it is not idle.
///
/// Without it the test above passes just as well on a hook that was never
/// installed, on a `story` that is not on `PATH`, and on a managed hook someone
/// replaced with `exit 0`. What it pins is that the same commit, on a machine
/// where storyhook *can* answer, still says nothing — so silence is the
/// contract rather than a symptom of the breakage.
#[test]
fn a_commit_says_nothing_when_the_daemon_is_perfectly_healthy_either() {
    let repo = HookRepo::new();
    let id = repo
        .env
        .story(repo.path())
        .args(["new", "something to claim", "--json"])
        .output()
        .expect("running story");
    let id = serde_json::from_slice::<serde_json::Value>(&id.stdout)
        .expect("`story new --json` prints JSON")["story"]["story"]["id"]
        .as_str()
        .expect("a minted id")
        .to_string();

    std::fs::write(repo.path().join("f"), "b\n").expect("changing the tracked file");
    let out = repo.git(&["commit", "-qam", &format!("start {id}\n\nclaims the story")]);

    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim(),
        "",
        "a working storyhook is quiet too"
    );

    // And it did something, which is what makes the silence worth having rather
    // than a hook that is switched off.
    let shown = repo
        .env
        .story(repo.path())
        .args(["show", &id, "--json"])
        .output()
        .expect("running story");
    let state = serde_json::from_slice::<serde_json::Value>(&shown.stdout).expect("JSON")["story"]
        ["story"]["state"]
        .as_str()
        .expect("a state")
        .to_string();
    assert_eq!(
        state, "in-progress",
        "the post-commit hook must have claimed the story it was told about — a \
         silent hook that does nothing would pass the test above too"
    );
}
