//! `--deadline` — a caller-declared bound on how long a command waits for the
//! daemon, in place of `SPAWN_LOCK_DEADLINE` (30s) plus `SERVED_DEADLINE`
//! (120s).
//!
//! # Why this exists
//!
//! SH-182: a Claude Code session hook is killed by its own harness at a
//! timeout far shorter than storyhook's own client-side bounds, and used to
//! have no way to ask storyhook to give up first and say so. `--deadline` is
//! that ask. These tests drive it directly against a held daemon spawn lock —
//! the same fixture `tests/daemon_timeouts.rs::ensure_gives_up_on_a_spawn_lock_somebody_else_holds`
//! uses — because it is the one contention storyhook cannot resolve any
//! faster than a person would tolerate, which is exactly the case `--deadline`
//! exists to bound.

use std::time::{Duration, Instant};

use fs4::FileExt;
use storyhook_test_support::{TestEnv, scratch_dir};

/// Named rather than inline (SH-394's `tests/timing_assertions.rs` fence):
/// well below the 30s spawn-lock bound a working `--deadline 1` must never
/// reach, generous for a whole CLI process spawn plus the ~1s deadline
/// itself.
const DEADLINE_GIVE_UP_CEILING: Duration = Duration::from_secs(10);

/// Named for the same reason: an ordinary command with nothing held must
/// start and finish well inside this, whatever the machine's ambient load.
const ORDINARY_COMMAND_CEILING: Duration = Duration::from_secs(5);

/// Opens (creating if needed) and exclusively locks `env`'s daemon spawn
/// lock, simulating another client mid-spawn. Unlocking is the caller's job;
/// the file is closed (and any lock released) when the returned handle drops
/// regardless, same as the fixture in `tests/daemon_timeouts.rs`.
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

/// A `--deadline` shorter than the contention it is raced against gives up
/// and says so, rather than waiting out `SPAWN_LOCK_DEADLINE` (30s).
#[test]
fn a_deadline_expires_behind_a_held_spawn_lock_and_says_so() {
    let env = TestEnv::isolated();
    let held = hold_spawn_lock(&env);
    let dir = scratch_dir();

    let started = Instant::now();
    let output = env
        .story(dir.path())
        .args(["--deadline", "1", "list"])
        .output()
        .expect("running story --deadline 1 list");
    let elapsed = started.elapsed();

    let _ = FileExt::unlock(&held);
    drop(held);

    assert!(
        !output.status.success(),
        "a command abandoned mid-wait must not report success"
    );
    assert_eq!(
        output.status.code(),
        Some(12),
        "deadline abandonment needs its own exit code so callers do not confuse it with \
         storage or integrity failures"
    );
    assert!(
        elapsed < DEADLINE_GIVE_UP_CEILING,
        "--deadline 1 should give up in about a second, took {elapsed:?} against a \
         30s spawn-lock bound it must never reach"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("1s"),
        "the failure should name the deadline that fired: {stderr}"
    );
    assert!(
        stderr.contains("not cancelled") && stderr.contains("will not repeat"),
        "the failure must say the command may still be running and will not be \
         retried, not merely that it failed: {stderr}"
    );
}

/// Abandoning one client mid-wait leaves the spawn lock usable: a plain
/// command run immediately afterward starts a daemon and succeeds normally.
///
/// This is the guard against a deadline that "gives up" by leaking a stuck
/// lock file, a zombie polling thread, or a daemon spawn left half-finished —
/// `run_with_deadline` does not cancel the abandoned attempt, so this is the
/// only place that would show one of those.
#[test]
fn abandoning_a_deadline_does_not_wedge_the_next_command() {
    let env = TestEnv::isolated();
    let held = hold_spawn_lock(&env);
    let dir = scratch_dir();

    env.story(dir.path())
        .args(["--deadline", "1", "list"])
        .output()
        .expect("running story --deadline 1 list");

    let _ = FileExt::unlock(&held);
    drop(held);

    env.story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    let started = Instant::now();
    env.story(dir.path()).arg("list").assert().success();
    assert!(
        started.elapsed() < ORDINARY_COMMAND_CEILING,
        "an ordinary command after the abandoned one took {:?}; the earlier \
         giving-up should have left nothing behind to wait on",
        started.elapsed()
    );

    env.stop_daemon();
}

/// `--deadline` rejects a value that is not a whole, non-negative number of
/// seconds, naming the flag rather than failing on an unrelated parse error
/// downstream.
#[test]
fn a_non_numeric_deadline_is_refused_by_name() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();

    env.story(dir.path())
        .args(["--deadline", "soon", "list"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicates::str::contains("--deadline"));
}

/// `--deadline 0` is accepted (an extreme but legitimate request to abandon
/// the very first wait) rather than refused as a bad value.
#[test]
fn a_zero_deadline_is_a_legitimate_value_not_a_parse_error() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();

    let output = env
        .story(dir.path())
        .args(["--deadline", "0", "list"])
        .output()
        .expect("running story --deadline 0 list");

    // It may or may not win the race against a fresh daemon spawn — that is
    // timing, not this test's subject — but it must never be *rejected* as an
    // invalid flag value (exit code 2).
    assert_ne!(
        output.status.code(),
        Some(2),
        "--deadline 0 must parse as a valid (if extreme) deadline: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    env.stop_daemon();
}
