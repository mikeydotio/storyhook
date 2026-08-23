//! Every managed git hook's own `story` calls carry `--deadline
//! GIT_HOOK_DEADLINE` (SH-343), so a cold or unreachable daemon cannot hold
//! `git commit`/`merge`/`pull` open for the full `SPAWN_LOCK_DEADLINE` +
//! `SERVED_DEADLINE` a bare `story` invocation would otherwise be allowed.
//!
//! # Why this is not `tests/hook_budgets.rs`
//!
//! That file anchors a Claude Code *session* hook script's own `--deadline`
//! against the `hooks.json` manifest `timeout` that kills the whole script —
//! an external budget git has no counterpart for. Nothing bounds how long git
//! itself will let `post-commit`/`post-merge`/`prepare-commit-msg` run; the
//! anchor here is [`storyhook::daemon::lifecycle::GIT_HOOK_DEADLINE`] itself,
//! the daemon's own deadline family.
//!
//! # Why the static scan reads back what install actually wrote
//!
//! `src/hooks.rs`'s `HOOKS` table is private, so this cannot import the hook
//! bodies directly — and should not want to: reading back the files
//! `story hooks install` actually produced, the same way a user's checkout
//! would receive them, is what makes a fourth managed hook (should one ever
//! be added) covered on arrival rather than needing this file edited too.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use fs4::FileExt;
use storyhook::daemon::lifecycle::GIT_HOOK_DEADLINE;
use storyhook_test_support::{TestEnv, scratch_dir};
use tempfile::TempDir;

const MANAGED_HOOKS: [&str; 3] = ["post-commit", "post-merge", "prepare-commit-msg"];

/// A git repository with the managed hooks installed, isolated so this file's
/// spawn-lock manipulation cannot park any other test in the binary.
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

        let out = repo
            .env
            .story(repo.path())
            .args(["project", "new", "--prefix", "SH"])
            .output()
            .expect("running story project new");
        assert!(out.status.success(), "fixture: story project new failed");

        let out = repo
            .env
            .story(repo.path())
            .args(["hooks", "install"])
            .output()
            .expect("running story hooks install");
        assert!(out.status.success(), "fixture: story hooks install failed");

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

    fn git(&self, args: &[&str]) -> std::process::Output {
        let mut cmd = Command::new("git");
        cmd.current_dir(self.path());
        self.env.apply(&mut cmd);
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        cmd.args(args);
        cmd.output().expect("running git")
    }
}

/// Opens (creating if needed) and exclusively locks `env`'s daemon spawn
/// lock — the same fixture `tests/cli_deadline.rs::hold_spawn_lock` uses,
/// duplicated rather than shared because neither file exposes the other's
/// internals and the fixture is four lines. Unlocking is the caller's job.
fn hold_spawn_lock(env: &TestEnv) -> std::fs::File {
    let environment = env.environment();
    std::fs::create_dir_all(environment.daemon_state_dir()).expect("the daemon directory");
    let held = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(environment.daemon_spawn_lock())
        .expect("opening the spawn lock");
    held.try_lock_exclusive()
        .expect("this test must be the one holding it");
    held
}

/// With the daemon spawn lock held by another "client" and no daemon of its
/// own running, a hooked `git commit` gives up on storyhook well inside
/// `SPAWN_LOCK_DEADLINE` (30s) rather than waiting it out.
///
/// **The precondition that makes this a real test, not a vacuous one:**
/// `story hooks install` above starts a daemon. If one were still running when
/// the lock is taken, `lifecycle::ensure`'s `usable` check would return `Some`
/// before `spawn_locked` is ever reached, and this test would pass against
/// unfixed code. `stop_daemon()` removes that daemon first.
///
/// `git commit -m` gives `prepare-commit-msg` a `$2` of `"message"`, which its
/// own `case` statement exits on before touching `story` — so only
/// `post-commit` pays here. Pre-fix this was ~30s (`SPAWN_LOCK_DEADLINE`);
/// post-fix it is `GIT_HOOK_DEADLINE` (~10s).
#[test]
fn a_hooked_commit_gives_up_on_a_held_spawn_lock_well_inside_spawn_lock_deadline() {
    let repo = HookRepo::new();
    repo.env.stop_daemon();
    let held = hold_spawn_lock(&repo.env);

    std::fs::write(repo.path().join("f"), "b\n").expect("changing the tracked file");
    let started = Instant::now();
    let out = repo.git(&["commit", "-qam", "a commit against a held spawn lock"]);
    let elapsed = started.elapsed();

    let _ = FileExt::unlock(&held);
    drop(held);

    // The overhead margin covers git's own startup, this process's two nested
    // `story` invocations, and CI slop — not GIT_HOOK_DEADLINE's own budget.
    const OVERHEAD_MARGIN: Duration = Duration::from_secs(10);
    let ceiling = GIT_HOOK_DEADLINE + OVERHEAD_MARGIN;
    assert!(
        ceiling < storyhook::daemon::lifecycle::SPAWN_LOCK_DEADLINE,
        "this test's ceiling ({ceiling:?}) must stay below SPAWN_LOCK_DEADLINE (30s) or it \
         would pass just as well against the unfixed ~30s wait this test exists to catch"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a commit must not fail because storyhook gave up.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_eq!(
        stdout.trim(),
        "",
        "the hook must stay silent on stdout: {stdout}"
    );
    assert_eq!(
        stderr.trim(),
        "",
        "the hook must stay silent on stderr: {stderr}"
    );
    assert!(
        elapsed < ceiling,
        "commit took {elapsed:?} against a held spawn lock; expected well under {ceiling:?} \
         (GIT_HOOK_DEADLINE {GIT_HOOK_DEADLINE:?} + {OVERHEAD_MARGIN:?} margin) now that \
         post-commit's `story` call declares --deadline"
    );
}

/// Every `story` invocation inside every installed managed hook declares
/// `--deadline <GIT_HOOK_DEADLINE.as_secs()>` — read back from the files
/// `story hooks install` actually wrote, not from a copy of the hook bodies
/// kept in this file.
#[test]
fn every_managed_hooks_story_calls_declare_git_hook_deadline() {
    let repo = HookRepo::new();

    let mut checked = 0usize;
    for hook in MANAGED_HOOKS {
        let path = repo.path().join(".git/hooks").join(hook);
        let script = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()));

        for line in story_invocation_lines(&script) {
            checked += 1;
            let deadline = deadline_arg(line).unwrap_or_else(|| {
                panic!(
                    "{hook}'s installed script invokes `story` with no --deadline on this \
                     line, which reopens SH-343 for whichever call this is:\n{line}"
                )
            });
            assert_eq!(
                deadline,
                GIT_HOOK_DEADLINE.as_secs(),
                "{hook} declares --deadline {deadline}s, which has drifted from \
                 GIT_HOOK_DEADLINE ({}s) on this line:\n{line}",
                GIT_HOOK_DEADLINE.as_secs(),
            );
        }
    }

    // A guard against a matcher that silently stopped matching anything: four
    // is the current known count (post-commit's sync, post-merge's sync and
    // its story-move, prepare-commit-msg's next) — this must never fall below
    // it, though a future hook may raise it.
    assert!(
        checked >= 4,
        "expected at least 4 `story` invocations across the managed hooks, found {checked} — \
         the line matcher in this test may have stopped matching"
    );
}

/// The lines in `script` that invoke `story` as a command, excluding comments
/// and the `command -v story` PATH probe (which names `story` but never runs
/// it, and carries no `--deadline` to check).
fn story_invocation_lines(script: &str) -> Vec<&str> {
    script
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter(|line| line.contains("story "))
        .filter(|line| !line.contains("command -v story"))
        .collect()
}

/// The value after a `--deadline` in `line`, if any.
fn deadline_arg(line: &str) -> Option<u64> {
    let after = line.split("--deadline").nth(1)?;
    after.split_whitespace().next()?.parse::<u64>().ok()
}

/// `post-git.sh`, the Claude Code plugin's stand-in, skips its own sync when a
/// managed `post-commit` exists — crediting that hook for work it may not
/// finish. That credit is only sound if the managed hook is at least as
/// patient as the stand-in it displaces: if `post-git.sh` would have given up
/// sooner than `post-commit` now does, nothing is lost by the skip. The
/// reverse — a managed hook that gives up before `post-git.sh` would have —
/// silently loses sync coverage a plugin user currently gets.
#[test]
fn post_git_shs_own_deadline_stays_at_or_under_git_hook_deadline() {
    let path = storyhook_test_support::hook_script("post-git.sh");
    let script = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()));
    let functional: String = script
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let deadline = deadline_arg(&functional).unwrap_or_else(|| {
        panic!("plugins/story/hooks/post-git.sh must declare its own --deadline")
    });
    assert!(
        deadline <= GIT_HOOK_DEADLINE.as_secs(),
        "post-git.sh declares --deadline {deadline}s, longer than GIT_HOOK_DEADLINE \
         ({}s) — the managed post-commit hook it defers to would now give up sooner \
         than post-git.sh's own stand-in sync would have, silently losing coverage a \
         plugin user currently gets from the skip in post-git.sh's own guard",
        GIT_HOOK_DEADLINE.as_secs(),
    );
}
