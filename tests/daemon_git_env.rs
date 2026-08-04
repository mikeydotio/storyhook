//! What a daemon is allowed to believe about git (SH-160).
//!
//! # The fact this file is about
//!
//! Since SH-114 the daemon is the process that runs every git command
//! storyhook runs, against a working directory carried in the request envelope.
//! It also **outlives** the client that started it, and inherits that client's
//! environment for its whole life. So a single `GIT_DIR` exported in a single
//! shell — once, by whoever happened to run the first `story` command after a
//! restart — decides what every later command on the machine sees, in every
//! repository, until somebody restarts the daemon.
//!
//! `git` obeys `$GIT_DIR` over the working directory, and not uniformly:
//! `git log` and `git config --get remote.origin.url` answer for the repository
//! the variable names while `git rev-parse --show-toplevel` answers for the
//! working directory. The two disagreeing is what makes this silent — every
//! layer is internally consistent and reports success.
//!
//! # What was measured, before any of this was written
//!
//! Both halves of `commit-sync` fail, and they fail in the same direction —
//! the guard stops guarding and the payload reads the wrong repository:
//!
//! | run from | clean daemon | daemon started once with `GIT_DIR` |
//! |---|---|---|
//! | a repository | links its own `HEAD` | links a sha that does not exist in it, and moves the story |
//! | a directory that is not a repository | `error: not a git repository`, exit 2 | syncs the foreign repository's commits |
//!
//! That second row is the one worth keeping in mind: a project in a directory
//! with no `.git` anywhere above it acquired a commit link and a state
//! transition from a repository it has no relationship with.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use storyhook_test_support::{ChildGuard, TestEnv, git, reserve_port, scratch_dir_named};

/// A repository at its own top level, with `origin` set and one empty commit.
///
/// Built through `storyhook_test_support::git` rather than a private helper, so
/// the fixture inherits `TestEnv::apply` — a private `HOME`, and
/// `GIT_TERMINAL_PROMPT=0`. The first draft of this file declared its own copy
/// and therefore built these repositories under the developer's real
/// `~/.gitconfig`, which is the fourth time that copy has been re-created here.
fn repo(env: &TestEnv, label: &str, url: &str) -> tempfile::TempDir {
    let dir = scratch_dir_named(label);
    git(env, dir.path(), &["init", "-q", "-b", "main"]);
    git(env, dir.path(), &["config", "user.email", "t@t"]);
    git(env, dir.path(), &["config", "user.name", "t"]);
    git(env, dir.path(), &["remote", "add", "origin", url]);
    git(env, dir.path(), &["commit", "-qm", "init", "--allow-empty"]);
    dir
}

/// An empty commit whose **body** claims `story` — the shape SH-124 defined as
/// a claim rather than a mention, so a failure here is a failure of the
/// repository the daemon read, never of the grammar.
///
/// `subject` exists because two empty commits with the same message, author and
/// second produce the **same sha**: the first draft of this file built both
/// repositories identically and got one hash for both, which would have made
/// the assertion below unable to tell them apart.
fn claiming_commit(env: &TestEnv, cwd: &Path, subject: &str, story: &str) -> String {
    git(
        env,
        cwd,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            subject,
            "-m",
            &format!("Closes {story}"),
        ],
    );
    let out = Command::new("git")
        .current_dir(cwd)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .expect("running git rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The path a poisoned client would export.
fn git_dir_of(dir: &Path) -> std::path::PathBuf {
    dir.join(".git")
}

/// **The defect.** A daemon started by a client with `GIT_DIR` exported must not
/// spend the rest of its life reading that repository for everybody else.
///
/// The poisoning happens on the *first* command, because that is the one that
/// spawns the daemon — every later command in this test runs with a clean
/// environment and still reaches the same long-lived process.
#[test]
fn a_daemon_started_with_git_dir_set_does_not_read_that_repository() {
    let env = TestEnv::isolated();
    // Nothing may be holding this store: the daemon under test has to be the
    // one this test starts, and it is only poisoned at spawn.
    env.stop_daemon();

    let here = repo(&env, "sh160-here-", "git@github.com:acme/here.git");
    let elsewhere = repo(
        &env,
        "sh160-elsewhere-",
        "git@github.com:acme/elsewhere.git",
    );

    // The spawning command, and the only one carrying the variable.
    env.story(here.path())
        .args(["project", "new", "--prefix", "SH", "--no-agents-md"])
        .env("GIT_DIR", git_dir_of(elsewhere.path()))
        .assert()
        .success();
    env.story(here.path())
        .args(["new", "a story about this repository"])
        .assert()
        .success();

    // Both repositories claim the same story, so the only thing distinguishing
    // a correct sync from a poisoned one is which sha arrives.
    let mine = claiming_commit(&env, here.path(), "feat: work done here", "SH-1");
    let theirs = claiming_commit(&env, elsewhere.path(), "feat: work done elsewhere", "SH-1");
    assert_ne!(
        mine, theirs,
        "the fixture needs two distinguishable commits"
    );

    env.story(here.path())
        .args(["commit-sync"])
        .assert()
        .success();

    let shown = env
        .story(here.path())
        .args(["show", "SH-1"])
        .output()
        .expect("running `story show`");
    let shown = String::from_utf8_lossy(&shown.stdout).to_string();

    assert!(
        shown.contains(&mine),
        "commit-sync must link this repository's own commit {mine}, but `story show` said:\n{shown}"
    );
    assert!(
        !shown.contains(&theirs),
        "commit-sync linked {theirs}, which exists only in the repository $GIT_DIR named when the \
         daemon was started:\n{shown}"
    );
}

/// **The guard fails the same way.** `commit-sync` refuses outside a repository,
/// and an inherited `GIT_DIR` makes that refusal answer about somewhere else —
/// so a project in a plain directory silently acquires a foreign repository's
/// history.
#[test]
fn commit_sync_outside_a_repository_is_still_refused_by_a_poisoned_daemon() {
    let env = TestEnv::isolated();
    env.stop_daemon();

    let elsewhere = repo(
        &env,
        "sh160-elsewhere-",
        "git@github.com:acme/elsewhere.git",
    );
    let plain = scratch_dir_named("sh160-plain-");

    env.story(plain.path())
        .args(["project", "new", "--prefix", "SH", "--no-agents-md"])
        .env("GIT_DIR", git_dir_of(elsewhere.path()))
        .assert()
        .success();
    env.story(plain.path())
        .args(["new", "a story in a directory with no git"])
        .assert()
        .success();
    claiming_commit(&env, elsewhere.path(), "feat: work done elsewhere", "SH-1");

    let out = env
        .story(plain.path())
        .args(["commit-sync"])
        .output()
        .expect("running `story commit-sync`");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "`commit-sync` in a directory that is not a repository must refuse, whatever the daemon \
         inherited, but it said:\n{combined}"
    );
    assert!(
        combined.contains("not a git repository"),
        "and it must refuse for the true reason:\n{combined}"
    );
}

/// How long a directly-spawned daemon gets to publish its portfile.
///
/// Not a performance budget. It distinguishes "started" from "never starts", so
/// that a client below cannot race ahead and spawn a *clean* daemon of its own —
/// which would turn this test permanently green while proving nothing.
const STARTUP: Duration = Duration::from_secs(10);

/// Waits until `env`'s daemon has **published its portfile**, or fails saying it
/// never did.
///
/// The portfile and not `daemon_is_live()`, which was the first thing tried and
/// is wrong here by a margin wide enough to fail the test: liveness is the
/// *pidfile lock*, which a starting daemon takes before it binds and publishes.
/// A client arriving in that window finds no portfile, decides to spawn one of
/// its own, and is refused by the lock the first daemon is already holding —
/// `a storyhook daemon is already running` — so the test failed on the fixture
/// rather than on the property.
fn await_daemon(env: &TestEnv) {
    let portfile = env.environment().daemon_file();
    let deadline = Instant::now() + STARTUP;
    while Instant::now() < deadline {
        if storyhook::daemon::lifecycle::read_info_at(&portfile).is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "the directly-spawned daemon never published {} within {STARTUP:?}; without it the \
         client below would spawn a clean daemon of its own and this test would pass for the \
         wrong reason",
        portfile.display()
    );
}

/// **Routes (b) and (c).** A daemon that was *not* started by a client — the
/// launchd agent, or a service manager, or a human running `story daemon
/// --serve` — inherits its environment from whatever started it, and must scrub
/// it just the same.
///
/// This is the row that decides where the fix may live. A fix at `spawn_child`
/// alone, or one gated to the `--serve` branch after argument parsing, leaves
/// this open while the two rows above stay green — so without it, "routes (b)
/// and (c) are in scope" would be a paper ruling with nothing behind it.
///
/// `story daemon --serve` is spawned directly, the way
/// `storyhook_test_support::crash::spawn_daemon` does, because that is exactly
/// what launchd execs; `spawn_child` is never involved.
#[test]
fn a_daemon_spawned_directly_with_git_dir_set_does_not_read_that_repository() {
    let env = TestEnv::isolated();
    env.stop_daemon();

    let here = repo(&env, "sh160-serve-here-", "git@github.com:acme/here.git");
    let elsewhere = repo(
        &env,
        "sh160-serve-elsewhere-",
        "git@github.com:acme/elsewhere.git",
    );

    // The poison rides on the daemon's *own* command, not on a client's.
    let mut serve = env.raw_story(here.path());
    serve
        .env("GIT_DIR", git_dir_of(elsewhere.path()))
        .args(["daemon", "--serve", "--port", &reserve_port().to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _daemon = ChildGuard::new(serve.spawn().expect("spawning a daemon directly"));
    await_daemon(&env);

    env.story(here.path())
        .args(["project", "new", "--prefix", "SH", "--no-agents-md"])
        .assert()
        .success();
    env.story(here.path())
        .args(["new", "a story about this repository"])
        .assert()
        .success();

    let mine = claiming_commit(&env, here.path(), "feat: work done here", "SH-1");
    let theirs = claiming_commit(&env, elsewhere.path(), "feat: work done elsewhere", "SH-1");
    assert_ne!(
        mine, theirs,
        "the fixture needs two distinguishable commits"
    );

    env.story(here.path())
        .args(["commit-sync"])
        .assert()
        .success();

    let shown = env
        .story(here.path())
        .args(["show", "SH-1"])
        .output()
        .expect("running `story show`");
    let shown = String::from_utf8_lossy(&shown.stdout).to_string();
    assert!(
        shown.contains(&mine),
        "a directly-spawned daemon must read this repository, not the one its own environment \
         named; `story show` said:\n{shown}"
    );
    assert!(
        !shown.contains(&theirs),
        "the daemon read the repository $GIT_DIR named when launchd (or a human) started it:\n\
         {shown}"
    );
}

/// **What the daemon hands on.** A poisoned daemon passes its environment to
/// every user event hook it fires, and hooks run git.
///
/// This is the only assertion in the file that a call-site funnel cannot
/// satisfy. `event_hooks::fire_hook` spawns a user's shell command, which is
/// outside any `git` constructor storyhook owns — so a fix that scrubbed only at
/// the point storyhook builds a `git` would leave every hook poisoned while
/// every other row here went green. It is therefore what pins the decision to
/// scrub the *process* rather than the command.
#[test]
fn an_event_hook_does_not_inherit_the_daemons_git_environment() {
    let env = TestEnv::isolated();
    env.stop_daemon();

    let here = repo(&env, "sh160-hook-here-", "git@github.com:acme/here.git");
    let elsewhere = repo(
        &env,
        "sh160-hook-elsewhere-",
        "git@github.com:acme/elsewhere.git",
    );

    // The spawning command carries the poison, exactly as the first test does.
    env.story(here.path())
        .args(["project", "new", "--prefix", "SH", "--no-agents-md"])
        .env("GIT_DIR", git_dir_of(elsewhere.path()))
        .assert()
        .success();

    // The hook reports the variable rather than a consequence of it: what is
    // under test is the environment handed to somebody else's script, and a
    // hook that ran git would be asserting storyhook's funnel a second time.
    let seen = here.path().join("what-the-hook-saw");
    let pointer = here.path().join(".storyhook.toml");
    let identity = std::fs::read_to_string(&pointer)
        .expect("`story project new` must have written the pointer file");
    std::fs::write(
        &pointer,
        format!(
            "{identity}\n[hooks.on_create]\ncommand = \"printf '%s' \\\"${{GIT_DIR-unset}}\\\" > {}\"\n",
            seen.display()
        ),
    )
    .expect("appending the hook to the pointer file");

    env.story(here.path())
        .args(["new", "a story whose creation fires the hook"])
        .assert()
        .success();

    let reported = std::fs::read_to_string(&seen)
        .expect("the on_create hook must have run and written its file");
    assert_eq!(
        reported.trim(),
        "unset",
        "the daemon handed `GIT_DIR` to a user's event hook; it saw `{}`",
        reported.trim()
    );
}
