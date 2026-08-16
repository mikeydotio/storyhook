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

/// The hooks `story hooks install` manages, named here so a fourth cannot be
/// added without this file noticing (SH-121).
///
/// It said `["post-commit", "prepare-commit-msg"]` until then, and the omission
/// was not visible: `post-merge` is the one hook that fires on something other
/// than `git commit`, so no test in this file could reach it and nothing said
/// so. The obligation is about *hooks*, not about committing.
const MANAGED_HOOKS: [&str; 3] = ["post-commit", "post-merge", "prepare-commit-msg"];

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
        for hook in MANAGED_HOOKS {
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

    /// Creates a story and returns the id storyhook minted for it.
    fn new_story(&self, title: &str) -> String {
        let out = self
            .env
            .story(self.path())
            .args(["new", title, "--json"])
            .output()
            .expect("running story");
        serde_json::from_slice::<serde_json::Value>(&out.stdout)
            .expect("`story new --json` prints JSON")["story"]["story"]["id"]
            .as_str()
            .expect("a minted id")
            .to_string()
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

/// **The third hook**, and the only one that fires on something other than a
/// commit — so nothing above could reach it.
///
/// `post-merge` runs `story move <id> done` for every story a merged commit
/// claims, on `main` or `master` only. A merge is where the noise would be
/// worst: it is often the last step of landing a branch, and a wall of
/// store-corruption diagnosis after `git merge` reads as a merge that failed.
///
/// Written against the same broken store rather than a second mechanism,
/// because it is the same obligation: what changes is which hook has to honour
/// it.
#[test]
fn a_merge_says_nothing_when_the_daemon_cannot_start() {
    let repo = HookRepo::new();

    // A branch with a commit that *claims* a story, so the hook has real work
    // to decline — SH-124's grammar means a merge of a commit naming nothing
    // would exercise the early exit rather than the failure path.
    let id = repo.new_story("something a merge would close");
    repo.git(&["checkout", "-q", "-b", "topic"]);
    std::fs::write(repo.path().join("f"), "b\n").expect("changing the tracked file");
    repo.git(&["commit", "-qam", &format!("work\n\nCloses {id}")]);
    repo.git(&["checkout", "-q", "main"]);

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

    // `--no-ff` so this is a real merge with an ORIG_HEAD the hook can read,
    // rather than a fast-forward that would exercise nothing.
    let out = repo.git(&["merge", "-q", "--no-ff", "-m", "merge topic", "topic"]);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a merge must not fail because storyhook cannot answer.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(
        stderr.trim(),
        "",
        "the post-merge hook must say nothing at all — after `git merge`, a wall of \
         diagnosis reads as a merge that did not land"
    );
    assert_eq!(stdout.trim(), "", "and nothing on stdout either: {stdout}");
}

/// **SH-341's arrival guard**, held to the same obligation: a merge concluded
/// by `git commit` — the shape a conflict forces — runs `post-commit`'s own
/// copy of the merge-arrival shell, and that copy must stay exactly as silent
/// as `post-merge`'s.
///
/// Conflicted rather than clean, so this exercises `post-commit`'s
/// `HEAD^2`-guarded branch specifically and not the flat `--since 1h` path
/// every other test in this file already covers.
#[test]
fn a_conflicted_merge_says_nothing_when_the_daemon_cannot_start() {
    let repo = HookRepo::new();

    let id = repo.new_story("something a conflicted merge would close");
    repo.git(&["checkout", "-q", "-b", "feature"]);
    std::fs::write(repo.path().join("f"), "theirs\n").expect("branch edit");
    repo.git(&["commit", "-qam", &format!("work\n\nCloses {id}")]);
    repo.git(&["checkout", "-q", "main"]);
    std::fs::write(repo.path().join("f"), "ours\n").expect("main edit");
    repo.git(&["commit", "-qam", "main side"]);

    let conflicted = repo.git(&["merge", "--no-ff", "feature"]);
    assert!(
        !conflicted.status.success(),
        "fixture: the merge must actually conflict"
    );

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

    std::fs::write(repo.path().join("f"), "resolved\n").expect("resolving");
    repo.git(&["add", "f"]);
    let out = repo.git(&["commit", "-qm", "conclude the conflicted merge"]);

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "concluding a conflicted merge must not fail because storyhook cannot \
         answer.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(
        stderr.trim(),
        "",
        "post-commit's arrival guard must say nothing at all — the same \
         obligation post-merge already has"
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
