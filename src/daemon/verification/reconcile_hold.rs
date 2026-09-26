//! A conflict-reconcile hold: the project's verifier waits for the story it
//! returned on a merge conflict to resubmit (D-E, SH-650), so `main` cannot
//! move under the reconcile.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::observation;
use crate::error::AppError;
use crate::process::Cancellation;
use crate::service::{VerificationCandidate, VerificationQueue};
use crate::store::{GlobalSeq, Store};

/// Waits until the reserved story creates a newer verification generation.
///
/// Other queue arrivals and coarse bus wakes only cause a fresh observation;
/// they cannot transfer the reservation. An observation reads the store alone
/// and starts no process; the checkout origin is validated once, for the
/// resubmission this returns (SH-769). A daemon stop ends the wait without
/// manufacturing a candidate. Public for shutdown and event-order integration
/// tests.
pub fn wait_for_reconciled_candidate(
    store: &impl Store,
    subscription: &crate::daemon::bus::Subscription,
    stop: &AtomicBool,
    reserved: &VerificationCandidate,
) -> Result<Option<VerificationCandidate>, AppError> {
    wait_for_reconciled_candidate_cancellable(
        store,
        subscription,
        stop,
        reserved,
        &Cancellation::default(),
    )
}

pub(super) fn wait_for_reconciled_candidate_cancellable(
    store: &impl Store,
    subscription: &crate::daemon::bus::Subscription,
    stop: &AtomicBool,
    reserved: &VerificationCandidate,
    cancellation: &Cancellation,
) -> Result<Option<VerificationCandidate>, AppError> {
    let queue = VerificationQueue::new(store);
    let newer = |generation: Option<GlobalSeq>| {
        generation.is_some() && generation != reserved.verifying_generation
    };
    loop {
        if stop.load(Ordering::Relaxed)
            || cancellation.is_cancelled()
            || !observation::human_permits(store, reserved)?
        {
            return Ok(None);
        }
        // A pass runs every 100 ms and on every bus wake, so it reads the store
        // alone: validating origins starts `git` (SH-769). Only a resubmission
        // is validated, and the second read may find it gone again.
        if newer(queue.current_generation_for(reserved)?)
            && let Some(candidate) = queue
                .current_for(reserved)?
                .filter(|candidate| newer(candidate.verifying_generation))
        {
            return Ok(Some(candidate));
        }
        let _ = subscription.recv(Duration::from_millis(100));
    }
}
