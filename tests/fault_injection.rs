//! The knife itself: `STORYHOOK_FAULT` must kill the process it is armed in.
//!
//! Every case in `tests/crash_matrix.rs` rests on this, and none of them is
//! *about* it. They ask what a corpse left in the database; this asks whether the
//! corpse died the way it was told to, which is the assumption underneath all of
//! them and the one whose failure is silent — a process that dies of the wrong
//! signal reads, to a crash test, as a fault that never fired.
//!
//! It has failed exactly that way once. `process_env_fault` called
//! `kill(getpid(), SIGKILL)` and then `abort()`, on a comment saying the second
//! line was unreachable. That is true of a single-threaded CLI, which is what
//! every armed process was while `--local` existed. It is not true of a daemon:
//! `kill` *posts* a signal rather than stopping the calling thread, and the
//! kernel let that thread reach the `abort` in six of the crash matrix's thirteen
//! cases the first time they were run against one. All six reported `SIGABRT` and
//! blamed a missing `fault-injection` feature.

use std::os::unix::process::ExitStatusExt;

use storyhook::store::FaultPoint;
use storyhook_test_support::{TestEnv, crash_the_daemon};

/// **The regression test for the abort race**, and the only test whose subject is
/// the mechanism rather than the store.
///
/// Three rounds, because what is being pinned is a race and one round of it was
/// close to a coin toss. Every other crash case asserts the same thing as a
/// precondition, so a regression is caught many times over; this is the one that
/// says what it means.
#[test]
fn an_armed_daemon_dies_by_sigkill_rather_than_by_its_own_abort() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("FI").build();

    for round in 1..=3 {
        let crashed = crash_the_daemon(
            &env,
            project.path(),
            FaultPoint::BeforeCommit,
            &["new", &format!("round {round}")],
        );
        assert_eq!(
            crashed.daemon().signal(),
            Some(libc::SIGKILL),
            "round {round}: an armed daemon must die of the signal the fault sends it, and \
             of nothing else. `SIGABRT` here means the fault fired and then lost a race \
             with the line after it, which every crash test reads as a fault that never \
             fired at all."
        );
    }
}

/// A daemon with no fault armed runs to a normal end, so the assertion above is
/// about the arming rather than about daemons dying in general.
///
/// The control, and it is not idle: `STORYHOOK_FAULT` is read from the process
/// environment on every call, so a test harness that leaked it into a sibling
/// would make unarmed processes die too — and every crash case would still pass.
#[test]
fn an_unarmed_daemon_is_not_killed_by_anything() {
    let env = TestEnv::isolated();
    let project = env.project().prefix("FI").build();
    project.new_story("nothing should happen to this");

    assert!(
        env.daemon_is_live(),
        "the fixture's own commands must have started a daemon for this to be about"
    );
    let stopped = env.daemon().expect("a portfile naming the live daemon");
    env.stop_daemon();
    assert!(
        !env.daemon_is_live(),
        "the daemon holding the store (pid {}) must stand down when asked, rather than \
         needing a signal",
        stopped.pid
    );
}

/// The positive control for the capability probe every armed spawn now passes
/// through (SH-528).
///
/// # Why this is worth a test of its own
///
/// The probe is the reason the two cases above can trust that a daemon which
/// did not die is a *finding* rather than a mis-built binary. It is called from
/// one door — `spawn_daemon`, whenever a fault point is named — so it is
/// already exercised by every crash case in the suite; what none of them do is
/// say so, and a guard whose firing nobody can attribute is the SH-306 shape
/// this project has paid for before. This is the assertion that names it.
///
/// # The negative half, and why it lives in a comment rather than in code
///
/// It cannot be written as a test: the probe's `false` branch requires a
/// `story` built **without** the `fault-injection` feature, and every binary
/// `cargo test` can reach has it — that being exactly what the probe measures.
/// So it was measured by hand, by toggle, and the result is recorded here
/// (SH-295: a pin that cannot fail is not a pin, so say which half is pinned):
///
/// * `cargo build --bin story` over a green tree — marker count 0 —
///   `an_armed_daemon_dies_by_sigkill_rather_than_by_its_own_abort` **FAILED
///   in seconds**, naming the missing feature, the binary's path, and the
///   rebuild that fixes it.
/// * Before the probe existed, the same toggle **hung**, and was killed by
///   `timeout 90`. That is the SH-528 incident, reproduced on demand.
/// * `cargo test --no-run` — marker count 1 — green again in 1.88s.
#[test]
fn the_binary_under_test_can_actually_fire_a_fault() {
    storyhook_test_support::assert_the_binary_can_fire_faults();
}
