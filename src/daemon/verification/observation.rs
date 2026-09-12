//! Generation authority is observed only while a blocking verification runs.

use super::*;
use crate::daemon::bus::Subscription;

#[cfg(test)]
mod tests;

// Matches subprocess cancellation responsiveness without rereading the store
// on every wake. Only relevant events and the fixed recovery deadline read it.
const STOP_CHECK: Duration = Duration::from_millis(100);

struct Finish<'a>(&'a AtomicBool);

impl Drop for Finish<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn current(store: &impl Store, candidate: &VerificationCandidate) -> Result<bool, AppError> {
    VerificationQueue::new(store)
        .current_for(candidate)
        .map(|current| {
            current.is_some_and(|c| {
                c.verifying_generation == candidate.verifying_generation
                    && c.blocking_revision == candidate.blocking_revision
            })
        })
        .map_err(|error| {
            error.with_context(&format!(
                "observing verification authority for project={} story={} generation={:?}",
                candidate.project_slug, candidate.story_id, candidate.verifying_generation,
            ))
        })
}

fn observe(
    subscription: &Subscription,
    done: &AtomicBool,
    manual: &Cancellation,
    attempt: &Cancellation,
    slug: &str,
    recovery: Duration,
    mut current: impl FnMut() -> Result<bool, AppError>,
) -> Result<bool, AppError> {
    let mut deadline = Instant::now() + recovery;
    loop {
        if done.load(Ordering::Acquire) {
            return Ok(true);
        }
        if manual.is_cancelled() {
            attempt.cancel();
            return Ok(true);
        }
        let event =
            subscription.recv(STOP_CHECK.min(deadline.saturating_duration_since(Instant::now())));
        let relevant = matches!(event, Some(Change::Catalog | Change::Resync))
            || matches!(event, Some(Change::Project(ref project)) if project == slug);
        if relevant || Instant::now() >= deadline {
            match current() {
                Ok(true) => deadline = Instant::now() + recovery,
                result => {
                    attempt.cancel();
                    return result;
                }
            }
        }
    }
}

pub(super) fn verify(
    store: &impl Store,
    bus: &ChangeBus,
    candidate: &VerificationCandidate,
    manual: &Cancellation,
    run: impl FnOnce(&Cancellation) -> VerificationOutcome,
) -> Result<Option<VerificationOutcome>, AppError> {
    let subscription = bus.subscribe();
    if !current(store, candidate)? {
        return Ok(None);
    }
    let attempt = Cancellation::default();
    if manual.is_cancelled() {
        attempt.cancel();
    }
    let done = AtomicBool::new(false);
    let (outcome, owned) = std::thread::scope(|scope| {
        let observer = scope.spawn(|| {
            observe(
                &subscription,
                &done,
                manual,
                &attempt,
                &candidate.project_slug,
                RECOVERY_WAKE,
                || current(store, candidate),
            )
        });
        // This guard also runs during unwinding before scope joins the monitor.
        let finish = Finish(&done);
        let outcome = run(&attempt);
        drop(finish);
        (
            outcome,
            observer
                .join()
                .expect("verification authority observer panicked"),
        )
    });
    if !owned? || !current(store, candidate)? {
        return Ok(None);
    }
    Ok(Some(outcome))
}
