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

use std::process::Stdio;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use storyhook_test_support::{ChildGuard, TestEnv};

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
fn wait_for(what: &str, idle: Duration, ready: impl Fn() -> bool) {
    storyhook_test_support::load_grace::wait_for(
        storyhook_test_support::load_grace::Patience::new(idle),
        Duration::from_millis(25),
        || format!("waiting for {what}"),
        || ready().then_some(()),
    );
}

/// An unrelated client completes while a hook remains held until explicit release.
#[test]
fn a_slow_command_does_not_block_another_client() {
    check_concurrent_client(Duration::ZERO);
}

/// Scheduling delay must not turn concurrent dispatch into a false failure.
#[test]
fn a_delayed_observer_still_proves_concurrent_dispatch() {
    check_concurrent_client(Duration::from_secs(1));
}

fn check_concurrent_client(observer_delay: Duration) {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let project = env.project().prefix("PB").build();
    let hooks_dir = project.path().join(".storyhook");
    std::fs::create_dir_all(&hooks_dir).expect("the hooks directory");
    // The hook cannot complete by itself. Its timeout is only a cleanup
    // backstop; the final marker proves it accepted our explicit release.
    std::fs::write(
        hooks_dir.join("hold.sh"),
        "#!/bin/sh\nset -eu\n: > .storyhook/entered\n\
         while [ ! -f .storyhook/release ]; do sleep 0.025; done\n\
         : > .storyhook/released\n",
    )
    .unwrap();
    std::fs::write(
        hooks_dir.join("hooks.toml"),
        format!(
            "[on_comment]\ncommand = \"sh .storyhook/hold.sh\"\ntimeout_seconds = {}\n",
            storyhook::event_hooks::HOOK_TIMEOUT_CEILING_SECS
        ),
    )
    .unwrap();
    env.story(project.path())
        .args(["new", "a story"])
        .assert()
        .success();

    let mut slow = env.raw_story(project.path());
    slow.args(["comment", "PB-1", "trip the hook"]);
    let mut slow = ChildGuard::spawn_with_output(&mut slow).expect("spawning the slow command");
    wait_for("the hook to enter its hold", Duration::from_secs(5), || {
        hooks_dir.join("entered").exists()
    });

    // Model an observer that loses CPU after the hook enters. This delay is
    // fixture stimulus, never a deadline or a claim about production speed.
    std::thread::sleep(observer_delay);
    let mut concurrent_cmd = env.raw_story(project.path());
    concurrent_cmd.args(["list", "--json"]);
    let concurrent_output = run_bounded(
        concurrent_cmd,
        "concurrent `story list` while the hook is held",
        storyhook_test_support::load_grace::graced_now(Duration::from_secs(30)),
    );
    assert!(
        concurrent_output.status.success(),
        "the concurrent `story list` must succeed: {concurrent_output:?}"
    );
    assert!(
        slow.try_wait().is_none(),
        "the hook's command must still be held"
    );
    std::fs::write(hooks_dir.join("release"), "").unwrap();
    let slow_output = slow.wait_with_output_within(
        storyhook_test_support::load_grace::graced_now(Duration::from_secs(15)),
        || "the held hook did not complete after release".into(),
    );
    assert!(slow_output.status.success(), "{slow_output:?}");
    assert!(
        hooks_dir.join("released").exists(),
        "the hook must exit through release, not through its timeout"
    );
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

    let deadline = storyhook_test_support::load_grace::graced_now(Duration::from_secs(20));
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
