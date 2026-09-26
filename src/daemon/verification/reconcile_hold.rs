//! A conflict-reconcile hold: the project's verifier waits for the story it
//! returned on a merge conflict to resubmit (D-E, SH-650), so `main` cannot
//! move under the reconcile.
//!
//! The hold lasts only while the reconcile can still end in a resubmission
//! (SH-770, council decision D1 on the story). A false release costs one
//! story its reservation: it rejoins the queue in priority order when it
//! resubmits. A false hold blocks the project's whole queue until a person
//! stops the verifier. So every rule here errs toward release.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::error::AppError;
use crate::process::Cancellation;
use crate::service::verification::{HeldStory, RETURNED_STATE, VERIFYING_STATE};
use crate::service::{VerificationCandidate, VerificationQueue};
use crate::store::{GlobalSeq, Store};

/// How a conflict-reconcile wait ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReconcileWait {
    /// The reserved story resubmitted: a newer generation, validated against
    /// the checkout origin. The reservation transfers to it.
    Resubmitted(Box<VerificationCandidate>),
    /// A daemon stop, `story verifier stop`, or `human-only` ended the wait.
    /// The verifier writes nothing: whoever ended it owns the story.
    Ended,
    /// The reconcile stopped, so the verifier released the queue (SH-770).
    Released(HoldRelease),
}

/// Why a conflict-reconcile hold released the queue before a resubmission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HoldRelease {
    /// The story says it cannot proceed: it carries `awaiting` (the agent's
    /// `story block`, or the Full Auto watchdog's quarantine), an open
    /// `blocked-by`, or the `blocked` state. `reason` is in its own words.
    StoryBlocked { reason: String },
    /// The story left `in-progress` for a state that is not `verifying`, so
    /// nobody is reconciling it.
    StoryLeft { state: String },
}

impl HoldRelease {
    /// What stopped the reconcile, as a sentence fragment for the story's
    /// comment and the activity journal.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::StoryBlocked { reason } => format!("the story is blocked: {reason}"),
            Self::StoryLeft { state } => {
                format!("the story moved to `{state}`, so nobody is reconciling it")
            }
        }
    }

    /// What a person or agent does next, as one sentence for the comment.
    #[must_use]
    pub fn next_step(&self) -> &'static str {
        match self {
            Self::StoryBlocked { .. } => "Clear the block before you resubmit.",
            Self::StoryLeft { .. } => "No action is necessary for the queue.",
        }
    }
}

/// Decides from the reserved story's own facts whether its reconcile has
/// stopped. Pure, so every rule is table-testable.
///
/// A blocked story is tested first: a story can be blocked in any state, and
/// its block is the more specific reason.
pub(crate) fn story_release(story: &HeldStory) -> Option<HoldRelease> {
    if let Some(reason) = &story.blocked {
        return Some(HoldRelease::StoryBlocked {
            reason: reason.clone(),
        });
    }
    // `verifying` without a newer generation is the instant between a
    // resubmission's write and its queue membership; the next pass sees it.
    (story.state != RETURNED_STATE && story.state != VERIFYING_STATE).then(|| {
        HoldRelease::StoryLeft {
            state: story.state.clone(),
        }
    })
}

/// Waits until the reserved story creates a newer verification generation,
/// or until its reconcile stops.
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
) -> Result<ReconcileWait, AppError> {
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
) -> Result<ReconcileWait, AppError> {
    let queue = VerificationQueue::new(store);
    let newer = |generation: Option<GlobalSeq>| {
        generation.is_some() && generation != reserved.verifying_generation
    };
    loop {
        if stop.load(Ordering::Relaxed) || cancellation.is_cancelled() {
            return Ok(ReconcileWait::Ended);
        }
        // A pass runs every 100 ms and on every bus wake, so it reads the store
        // alone: validating origins starts `git` (SH-769). Only a resubmission
        // is validated, and the second read may find it gone again.
        let view = queue.hold_view(reserved).map_err(|error| {
            error.with_context(&format!(
                "reading the reconcile hold for project={} story={}",
                reserved.project_slug, reserved.story_id,
            ))
        })?;
        let Some(story) = view.story.filter(|story| story.permitted) else {
            return Ok(ReconcileWait::Ended);
        };
        if newer(view.generation)
            && let Some(candidate) = queue
                .current_for(reserved)?
                .filter(|candidate| newer(candidate.verifying_generation))
        {
            return Ok(ReconcileWait::Resubmitted(Box::new(candidate)));
        }
        if let Some(release) = story_release(&story) {
            return Ok(ReconcileWait::Released(release));
        }
        let _ = subscription.recv(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn story(state: &str, blocked: Option<&str>) -> HeldStory {
        HeldStory {
            permitted: true,
            blocked: blocked.map(str::to_string),
            state: state.into(),
        }
    }

    #[test]
    fn a_story_still_reconciling_or_resubmitting_holds() {
        assert_eq!(story_release(&story(RETURNED_STATE, None)), None);
        assert_eq!(story_release(&story(VERIFYING_STATE, None)), None);
    }

    #[test]
    fn a_blocked_story_releases_in_any_state_with_its_own_reason() {
        for state in [RETURNED_STATE, VERIFYING_STATE, "blocked", "todo"] {
            assert_eq!(
                story_release(&story(state, Some("blocked by SH-2"))),
                Some(HoldRelease::StoryBlocked {
                    reason: "blocked by SH-2".into()
                }),
                "{state}"
            );
        }
    }

    #[test]
    fn a_story_that_left_the_reconcile_releases() {
        for state in ["todo", "done", "dropped", "backlog"] {
            assert_eq!(
                story_release(&story(state, None)),
                Some(HoldRelease::StoryLeft {
                    state: state.into()
                }),
                "{state}"
            );
        }
    }
}
