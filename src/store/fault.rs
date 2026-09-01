//! Compile-time-gated fault injection points.
//!
//! The store's crash-consistency claims — "a `kill -9` mid-write leaves the
//! database consistent", "an acknowledged write is durable" — are only worth
//! what the tests that provoke them are worth, and you cannot provoke a crash
//! between `COMMIT` and the acknowledgement from outside the process. These
//! named points let a test stop the world exactly there.
//!
//! **The `fault-injection` feature is never on by default.** With it off,
//! [`fire`] is an inlined `Ok(())` and there is no arming machinery to call:
//! the points cost nothing and cannot be reached in a release build. The
//! feature is enabled for `cargo test` only, by way of the test-support crate's
//! dependency on `storyhook` — see that crate's manifest.
//!
//! Arming is *thread-local*, because integration tests in one binary are
//! parallel threads sharing a process and a process-global switch would make
//! one test's fault another test's failure. For the out-of-process cases (W8's
//! `kill -9` matrix) the `STORYHOOK_FAULT` environment variable arms a child at
//! start-up instead.

use std::time::Duration;

use crate::store::error::StoreError;

/// How long an armed process waits to die after posting its own `SIGKILL`.
///
/// **Not a guess at how long signal delivery takes** — it is several orders of
/// magnitude longer than one, and exists only so that a platform which somehow
/// ignored `SIGKILL` fails loudly (by the `abort()` after it) instead of
/// hanging for ever. See [`process_env_fault`], where it is spent.
///
/// # Why this is `pub`, and what a harness may derive from it
///
/// It is the **self-bound that makes an armed process's death certain**: once
/// the fault fires, that process is gone within this, by `SIGKILL` immediately
/// or by `abort()` at the far end. So a harness still waiting on an armed
/// corpse after this has not observed a slow death — it has *disproved the
/// fault having fired at all*, which is a different failure with a different
/// cause, and one worth naming rather than waiting out
/// (`storyhook_test_support::crash`, SH-528).
///
/// [`process_env_fault`]: self
pub const DELIVERY_BACKSTOP: Duration = Duration::from_secs(10);

/// A place in the store where a test can inject a failure.
///
/// The variants are the moments where a crash has a *different* consequence
/// than a crash a microsecond earlier or later.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FaultPoint {
    /// Inside a write transaction, immediately before `COMMIT`.
    ///
    /// Everything the closure did must vanish.
    BeforeCommit,
    /// After `COMMIT` returned, before the caller is told it succeeded.
    ///
    /// The write is durable but unacknowledged — the case that must never
    /// produce a *false failure* the caller then retries into a duplicate.
    AfterCommitBeforeAck,
    /// Inside `put_story`, between the story row and its labels and relations.
    ///
    /// A partially-written read model must never be observable.
    MidReadModelUpdate,
    /// Between two migrations.
    MidMigration,
    /// While verifying a pre-migration backup.
    ///
    /// A fifth point beyond the four the design named, and the only way to
    /// test "refuse to migrate when the backup cannot be verified" without
    /// hand-corrupting a SQLite file — which would test the corruption, not the
    /// refusal.
    BackupVerify,
}

impl FaultPoint {
    /// The name this point is armed by in `STORYHOOK_FAULT`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BeforeCommit => "before_commit",
            Self::AfterCommitBeforeAck => "after_commit_before_ack",
            Self::MidReadModelUpdate => "mid_read_model_update",
            Self::MidMigration => "mid_migration",
            Self::BackupVerify => "backup_verify",
        }
    }

    /// Every point, for tests that sweep the whole set.
    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::BeforeCommit,
            Self::AfterCommitBeforeAck,
            Self::MidReadModelUpdate,
            Self::MidMigration,
            Self::BackupVerify,
        ]
    }
}

/// Fires the named point if this thread has armed it.
///
/// Compiles to `Ok(())` without the `fault-injection` feature.
#[inline]
pub(crate) fn fire(point: FaultPoint) -> Result<(), StoreError> {
    #[cfg(feature = "fault-injection")]
    {
        armed::fire(point)
    }
    #[cfg(not(feature = "fault-injection"))]
    {
        let _ = point;
        Ok(())
    }
}

#[cfg(feature = "fault-injection")]
pub use armed::{FaultAction, FaultGuard, arm, armed_points, disarm_all};

#[cfg(feature = "fault-injection")]
mod armed {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use super::FaultPoint;
    use crate::store::error::StoreError;

    /// What an armed point does when reached.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum FaultAction {
        /// Return a [`StoreError::Storage`]-shaped failure with this detail.
        Fail(String),
        /// Panic, so the enclosing transaction unwinds and must roll back.
        Panic(String),
        /// Abort the process immediately, without unwinding or flushing —
        /// the in-process stand-in for `kill -9`.
        Abort,
    }

    thread_local! {
        static ARMED: RefCell<BTreeMap<FaultPoint, FaultAction>> =
            const { RefCell::new(BTreeMap::new()) };
    }

    /// Disarms a point when dropped, so a failing assertion cannot leave a
    /// fault armed for whatever runs next on this thread.
    #[must_use = "the fault is disarmed as soon as this guard is dropped"]
    pub struct FaultGuard(FaultPoint);

    impl Drop for FaultGuard {
        fn drop(&mut self) {
            ARMED.with_borrow_mut(|armed| armed.remove(&self.0));
        }
    }

    /// Arms `point` on the current thread until the returned guard is dropped.
    pub fn arm(point: FaultPoint, action: FaultAction) -> FaultGuard {
        ARMED.with_borrow_mut(|armed| armed.insert(point, action));
        FaultGuard(point)
    }

    /// Disarms every point on the current thread.
    pub fn disarm_all() {
        ARMED.with_borrow_mut(BTreeMap::clear);
    }

    /// The points currently armed on this thread.
    #[must_use]
    pub fn armed_points() -> Vec<FaultPoint> {
        ARMED.with_borrow(|armed| armed.keys().copied().collect())
    }

    pub(super) fn fire(point: FaultPoint) -> Result<(), StoreError> {
        let action = ARMED.with_borrow(|armed| armed.get(&point).cloned());
        match action {
            None => process_env_fault(point),
            Some(FaultAction::Fail(detail)) => Err(StoreError::Corrupt(format!(
                "injected fault at {}: {detail}",
                point.as_str()
            ))),
            Some(FaultAction::Panic(detail)) => {
                panic!("injected fault at {}: {detail}", point.as_str())
            }
            Some(FaultAction::Abort) => std::process::abort(),
        }
    }

    /// `STORYHOOK_FAULT=<point>` **`kill -9`s** the process at `<point>`.
    ///
    /// The out-of-process arming path: a test spawns a child with this set and
    /// then inspects the database the dead child left behind. Read fresh each
    /// time rather than cached, because a test may set it for one child and not
    /// the next.
    ///
    /// `SIGKILL` rather than [`std::process::abort`], which is what the
    /// in-process [`FaultAction::Abort`] uses. For SQLite's durability the two
    /// are equivalent — neither unwinds, neither flushes, and the database's
    /// state lives in the write-ahead log and the page cache rather than in this
    /// process's memory — but only one of them is *unarguably* equivalent.
    /// `abort` raises a catchable signal and runs whatever handler a linked
    /// library installed; `SIGKILL` cannot be caught, blocked, or handled by
    /// anyone, so a crash test that uses it is testing the store's crash
    /// consistency rather than the runtime's shutdown path. It is also literally
    /// what the design asked for: `kill -9` at each named point.
    fn process_env_fault(point: FaultPoint) -> Result<(), StoreError> {
        match std::env::var("STORYHOOK_FAULT") {
            Ok(name) if name == point.as_str() => {
                // SAFETY: `kill` with this process's own pid and a valid signal
                // number.
                unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
                // **`kill` posts a signal; it does not stop this thread
                // mid-instruction.** So this is a wait to die, not a fallback.
                //
                // The line here used to be a bare `abort()`, on a comment
                // calling itself unreachable. That was true while every armed
                // process was a single-threaded CLI, and false the moment the
                // armed process became the daemon: the kernel lets the calling
                // thread run on often enough that six of `tests/crash_matrix.rs`'s
                // thirteen cases reported `SIGABRT` the first time they were run
                // against one. A crash test that reads `SIGABRT` concludes the
                // fault did not fire — the exact opposite of what happened, and
                // a diagnosis that sends the reader looking for a missing
                // `fault-injection` feature.
                //
                // The sleep is not a guess at how long delivery takes; it is
                // several orders of magnitude longer, and exists only so that a
                // platform which somehow ignored `SIGKILL` fails loudly instead
                // of hanging for ever. Named rather than written inline,
                // because a harness waiting on an armed corpse derives its own
                // bound from it — see [`super::DELIVERY_BACKSTOP`].
                std::thread::sleep(super::DELIVERY_BACKSTOP);
                std::process::abort()
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_point_has_a_distinct_name() {
        let mut names: Vec<&str> = FaultPoint::all().iter().map(|p| p.as_str()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "fault point names must be distinct");
    }

    #[test]
    fn nothing_fires_when_nothing_is_armed() {
        for point in FaultPoint::all() {
            assert!(fire(point).is_ok(), "{point:?} fired unarmed");
        }
    }
}
