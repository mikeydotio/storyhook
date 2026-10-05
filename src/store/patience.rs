//! Waiting out cross-process write contention for work that must finish.
//!
//! `StoreError::Busy` means another connection held SQLite's write lock past
//! `busy_timeout`. It is raised by `BEGIN IMMEDIATE`, before a write closure
//! runs, so repeating the whole write is safe whenever its closure keeps its
//! effects inside the transaction. A reset is such work: once it has removed
//! resources, losing its final write to contention leaves the story reserved
//! and more wedged than before (SH-886), so it waits instead of failing.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::StoreError;

/// The first pause after contention; later pauses double up to [`MAX_PAUSE`].
const FIRST_PAUSE: Duration = Duration::from_millis(50);

/// The longest pause between attempts, so a released lock is noticed quickly.
const MAX_PAUSE: Duration = Duration::from_secs(2);

/// A request, shared across threads, that patient waits stop and report.
///
/// The daemon sets it when it stands down; the interrupted work resumes from
/// its durable record when the next daemon starts.
#[derive(Clone, Debug, Default)]
pub struct Shutdown(Arc<AtomicBool>);

impl Shutdown {
    /// A signal nothing has requested yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks every wait that shares this signal to stop at its next pause.
    pub fn request(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether a stop was requested.
    #[must_use]
    pub fn requested(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Repeats `attempt` while it reports [`StoreError::Busy`].
///
/// Returns the first other outcome. Contention is returned only when
/// `shutdown` is requested or `deadline` passes; every other error is returned
/// at once, because only contention is known to leave nothing behind.
pub fn patiently<T>(
    shutdown: &Shutdown,
    deadline: Option<Instant>,
    mut attempt: impl FnMut() -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    let mut pause = FIRST_PAUSE;
    loop {
        match attempt() {
            Err(StoreError::Busy(detail)) => {
                if shutdown.requested() || deadline.is_some_and(|end| Instant::now() >= end) {
                    return Err(StoreError::Busy(detail));
                }
                let pause_now = deadline.map_or(pause, |end| {
                    pause.min(end.saturating_duration_since(Instant::now()))
                });
                std::thread::sleep(pause_now);
                pause = (pause * 2).min(MAX_PAUSE);
            }
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn busy() -> StoreError {
        StoreError::Busy("timed out waiting for the project write lock".into())
    }

    #[test]
    fn contention_is_repeated_until_the_write_commits() {
        let mut calls = 0;
        let result = patiently(&Shutdown::new(), None, || {
            calls += 1;
            if calls < 4 { Err(busy()) } else { Ok(calls) }
        });
        assert_eq!(result.unwrap(), 4);
    }

    #[test]
    fn other_errors_are_returned_without_a_repeat() {
        let mut calls = 0;
        let result: Result<(), _> = patiently(&Shutdown::new(), None, || {
            calls += 1;
            Err(StoreError::Invariant("refused".into()))
        });
        assert!(matches!(result, Err(StoreError::Invariant(_))));
        assert_eq!(calls, 1);
    }

    #[test]
    fn a_requested_shutdown_returns_the_contention() {
        let shutdown = Shutdown::new();
        shutdown.request();
        let mut calls = 0;
        let result: Result<(), _> = patiently(&shutdown, None, || {
            calls += 1;
            Err(busy())
        });
        assert!(matches!(result, Err(StoreError::Busy(_))));
        assert_eq!(calls, 1);
    }

    #[test]
    fn a_passed_deadline_returns_the_contention() {
        let mut calls = 0;
        let result: Result<(), _> = patiently(&Shutdown::new(), Some(Instant::now()), || {
            calls += 1;
            Err(busy())
        });
        assert!(matches!(result, Err(StoreError::Busy(_))));
        assert_eq!(calls, 1);
    }
}
