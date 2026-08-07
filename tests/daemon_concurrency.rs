//! SH-173 — the daemon dispatches serially, so one slow command blocks every
//! client on the machine.
//!
//! Two shapes, each proving half of the fix:
//!
//! - a command sitting inside a slow event hook must not block an unrelated
//!   client's `story list`, the story's own measured defect (a `sleep 20`
//!   hook made `story list` return together with it, at 16.95s);
//! - a hook that calls `story` back into this daemon must never queue behind
//!   the very dispatcher pool its own parent occupies, or enough concurrent
//!   hook-firing commands would deadlock the daemon on itself.
//!
//! Both fixtures reuse the shape `tests/daemon_lifecycle.rs`'s
//! `a_running_command_is_published_and_retracted` established: an event hook
//! is the only way to hold a real request open long enough to look at it.

use std::io::Write;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use storyhook::daemon::lifecycle;
use storyhook_test_support::TestEnv;

/// Stops whatever daemon `env` is running, even if the test panics first.
struct DaemonGuard<'a>(&'a TestEnv);

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        self.0.stop_daemon();
    }
}

/// A command with a deadline, failing loudly rather than hanging `make test`.
///
/// The shape `tests/concurrency_soak.rs::run_bounded` established: the
/// deadline covers spawning, waiting *and* collecting output as one bound,
/// because a pipe outlives the process that was handed it. The worker thread
/// is not joined on timeout — it may be blocked in a syscall nothing here can
/// interrupt, and leaving it is the price of reporting the failure at all.
fn run_bounded(
    mut cmd: std::process::Command,
    what: &str,
    deadline: Duration,
) -> std::process::Output {
    let (tx, rx) = mpsc::channel();
    let label = what.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).output());
    });
    match rx.recv_timeout(deadline) {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => panic!("spawning `{label}`: {e}"),
        Err(_) => panic!(
            "`{label}` did not finish within {deadline:?} — a deadlock rather than \
             slowness, since every wait inside a `story` command is bounded."
        ),
    }
}

/// Blocks until `ready`, or fails the test.
fn wait_for(what: &str, deadline: Duration, ready: impl Fn() -> bool) {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out after {deadline:?} waiting for {what}");
}

/// A slow command sitting inside its hook does not block an unrelated
/// client's `story list` — the story's own measured defect, self-calibrated
/// per `tests/daemon_lifecycle.rs::four_clients_behind_a_wedged_daemon_share_one_attempt`:
/// a hard-coded second count is a claim about the machine running this test;
/// a multiple of a measurement taken moments earlier on the same machine is a
/// claim about the shape.
#[test]
fn a_slow_command_does_not_block_another_client() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let environment = env.environment();

    let project = env.project().prefix("PB").build();
    std::fs::create_dir_all(project.path().join(".storyhook")).expect("the hooks directory");
    let mut hooks = std::fs::File::create(project.path().join(".storyhook/hooks.toml"))
        .expect("writing hooks.toml");
    hooks
        .write_all(
            b"[settings]\ntimeout_seconds = 60\n\n\
              [on_comment]\ncommand = \"sleep 3\"\ntimeout_seconds = 60\n",
        )
        .expect("writing the hook");
    drop(hooks);
    env.story(project.path())
        .args(["new", "a story"])
        .assert()
        .success();

    // Warm the binary first: macOS's first-exec Mach-O validation can cost
    // tens of seconds on the very first invocation, which would otherwise be
    // charged to the baseline measurement below.
    env.raw_story(project.path())
        .arg("--version")
        .output()
        .expect("warming the binary");

    let mut baseline_cmd = env.raw_story(project.path());
    baseline_cmd.args(["list", "--json"]);
    let alone = Instant::now();
    run_bounded(
        baseline_cmd,
        "baseline `story list`",
        Duration::from_secs(15),
    );
    let baseline = alone.elapsed();

    let mut slow = env.raw_story(project.path());
    let mut child = slow
        .args(["comment", "PB-1", "trip the hook"])
        .spawn()
        .expect("spawning the slow command");

    wait_for(
        "the daemon to publish the slow comment as in flight",
        Duration::from_secs(5),
        || {
            lifecycle::read_inflight(&environment)
                .iter()
                .any(|r| r.command == "comment")
        },
    );

    let mut concurrent_cmd = env.raw_story(project.path());
    concurrent_cmd.args(["list", "--json"]);
    let started = Instant::now();
    let concurrent_output = run_bounded(
        concurrent_cmd,
        "concurrent `story list`",
        Duration::from_secs(15),
    );
    let concurrent = started.elapsed();

    assert!(
        concurrent_output.status.success(),
        "the concurrent `story list` must succeed: {concurrent_output:?}"
    );
    // A claim about the shape (queued vs. concurrent), not about this
    // machine's absolute speed.
    assert!(
        concurrent < baseline * 4 + Duration::from_millis(500),
        "a concurrent `story list` (took {concurrent:?}) must not be inflated by an \
         unrelated slow command; baseline alone was {baseline:?}"
    );
    // And a claim about the hook itself: the hook sleeps 3s, so a `story
    // list` that queued behind it would take at least that long.
    assert!(
        concurrent < Duration::from_secs(2),
        "`story list` took {concurrent:?} — it queued behind the 3s hook instead of \
         running concurrently with it"
    );

    child.wait().expect("the slow command finishes");
}

/// A hook that calls `story` never queues behind its own parent.
///
/// `STORYHOOK_DISPATCHERS=2` shrinks the pool so the deadlock this test
/// guards against is reachable with three ordinary commands rather than
/// nine: with no hook-depth lane, three concurrent `story new` each occupy a
/// dispatcher waiting on their own nested `story new` call, and with only
/// two dispatchers to share, at least one nested call could never get one.
#[test]
fn a_hook_that_calls_story_never_queues_behind_its_own_parent() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);

    let project = env.project().prefix("PB").build();
    let pointer = project.path().join(".storyhook.toml");
    let existing = std::fs::read_to_string(&pointer).expect("the project has a pointer file");
    std::fs::write(
        &pointer,
        format!(
            "{existing}\n[hooks.on_create]\ncommand = \"{} new 'spawned by the hook'\"\n",
            storyhook_test_support::story_binary().display()
        ),
    )
    .expect("writing hooks");

    // The first command started the daemon before the hook was written, so
    // stop it and let the next command start a fresh one that reads the
    // hook configuration above.
    env.stop_daemon();

    let deadline = Duration::from_secs(20);
    let outer: Vec<_> = (0..3)
        .map(|n| {
            let mut cmd = env.raw_story(project.path());
            cmd.args(["new", "outer"]).env("STORYHOOK_DISPATCHERS", "2");
            (n, cmd)
        })
        .collect();

    let (tx, rx) = mpsc::channel();
    for (n, mut cmd) in outer {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let output = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).output();
            let _ = tx.send((n, output));
        });
    }
    drop(tx);

    let mut seen = 0;
    let started = Instant::now();
    while seen < 3 {
        let remaining = deadline
            .checked_sub(started.elapsed())
            .unwrap_or(Duration::ZERO);
        match rx.recv_timeout(remaining) {
            Ok((n, Ok(output))) => {
                assert!(
                    output.status.success(),
                    "outer command {n} failed: {output:?}"
                );
                seen += 1;
            }
            Ok((n, Err(e))) => panic!("spawning outer command {n}: {e}"),
            Err(_) => panic!(
                "only {seen} of 3 concurrent `story new` commands finished within \
                 {deadline:?} — a hook-nested call queued behind its own parent's \
                 dispatcher slot instead of taking the unbounded lane."
            ),
        }
    }

    // Three asked for, three from hooks — a third would mean the hook's own
    // `story new` fired the hook again (the shape
    // `tests/daemon_invoke.rs::a_hook_that_runs_story_terminates` pins for
    // depth alone; this pins that depth-lane scheduling never loses one).
    let listed = env
        .story(project.path())
        .args(["list", "--json"])
        .output()
        .expect("listing stories");
    let json: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("a JSON listing");
    let stories = json["stories"].as_array().expect("a stories array");
    assert_eq!(
        stories.len(),
        6,
        "expected 3 outer stories and 3 hook-created ones, got: {json}"
    );
}
