//! Scoped authority observation cancels owned work before releasing its resources.

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
                    && c.human_only_revision == candidate.human_only_revision
                    && c.blocking_revision == candidate.blocking_revision
                    && c.blocked_by.is_empty()
                    && !c.landing_pending
                    && c.pull_request == candidate.pull_request
                    && c.checkout == candidate.checkout
                    && c.project_slug == candidate.project_slug
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
        if let VerificationOutcome::CleanupFailed { cleanup, .. } = &outcome
            && !human_permits(store, candidate)?
        {
            let ctx = Ctx::new(
                store,
                candidate.project,
                candidate.checkout.clone(),
                Environment::at(&candidate.checkout),
            )
            .no_hooks(true);
            VerificationQueue::new(store).record_generation_withdrawn(&ctx, candidate, &format!(
                "{VERIFICATION_WITHDRAWN_PREFIX} human-only revoked the attempt; cleanup still requires attention. No verdict or automatic completion is authorized.\n\n{}",
                crate::text_lint::quote_evidence(&format!("{cleanup:?}")),
            ))?;
        }
        return Ok(None);
    }
    Ok(Some(outcome))
}

/// Observes the human reservation across an entire owned lifecycle, including
/// valid time outside verifying during repair and post-merge resource cleanup.
pub(super) fn human_owned(
    store: &impl Store,
    env: &Environment,
    bus: &ChangeBus,
    candidate: &VerificationCandidate,
    cancellation: &Cancellation,
    run: impl FnOnce() -> Result<TickResult, AppError>,
) -> Result<TickResult, AppError> {
    let subscription = bus.subscribe();
    let permitted = || human_permits(store, candidate);
    if !permitted()? {
        return human_withdrawn(store, env, candidate, None);
    }
    let done = AtomicBool::new(false);
    let (outcome, owned) = std::thread::scope(|scope| {
        let observer = scope.spawn(|| {
            observe(
                &subscription,
                &done,
                &Cancellation::default(),
                cancellation,
                &candidate.project_slug,
                RECOVERY_WAKE,
                permitted,
            )
        });
        let finish = Finish(&done);
        let outcome = run();
        drop(finish);
        (
            outcome,
            observer
                .join()
                .expect("human reservation observer panicked"),
        )
    });
    if !owned? || !permitted()? {
        return human_withdrawn(store, env, candidate, outcome.as_ref().err());
    }
    outcome
}

pub(super) fn human_permits(
    store: &impl Store,
    candidate: &VerificationCandidate,
) -> Result<bool, AppError> {
    store
        .read(|tx| crate::service::verification::human::permits(tx, candidate))
        .map_err(|error| {
            AppError::from(error).with_context(&format!(
                "checking human reservation for project={} story={}",
                candidate.project_slug, candidate.story_id,
            ))
        })
}

fn human_withdrawn(
    store: &impl Store,
    env: &Environment,
    candidate: &VerificationCandidate,
    error: Option<&AppError>,
) -> Result<TickResult, AppError> {
    let ctx = Ctx::new(
        store,
        candidate.project,
        candidate.checkout.clone(),
        env.clone(),
    )
    .no_hooks(true);
    let detail = error
        .map(|error| {
            format!(
                "\n\nCleanup or interrupted operation diagnostic:\n{}",
                crate::text_lint::quote_evidence(&error.to_string())
            )
        })
        .unwrap_or_default();
    let body = format!(
        "{VERIFICATION_WITHDRAWN_PREFIX} human-only revoked verifier ownership for {} (generation {:?}, reservation {:?}). The verifier cancelled its owned work and released this attempt. Story state and manual admission permission were preserved. Any unresolved landing intent remains available for later reconciliation.{detail}",
        candidate.story_id, candidate.verifying_generation, candidate.human_only_revision,
    );
    VerificationQueue::new(store).record_generation_withdrawn(&ctx, candidate, &body)?;
    Ok(TickResult::Returned)
}
