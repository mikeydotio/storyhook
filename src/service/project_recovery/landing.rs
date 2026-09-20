//! Confirmed landing receipts originate only in the landing authority transaction.

use super::{RecoveryState, RepairAttempt, RepairCompletion, attempts, persistence};
use crate::domain::StoryEvent;
use crate::store::{GlobalSeq, LandingIntent, ProjectId, ReadOps, StoreError, WriteOps};
use serde::{Deserialize, Serialize};

/// Exact certified repair landing, distinct from a manual story closure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairLanding {
    /// Validated external merge authorization retained after its pending row is removed.
    pub intent: LandingIntent,
    /// Exact completed repair attempt whose head and tree were certified.
    pub attempt: String,
    /// Event that recorded the confirmed pull request merge.
    pub event: GlobalSeq,
    /// RFC3339 time of the landing transaction.
    pub at: String,
}

pub(crate) fn record_landing(
    tx: &mut impl WriteOps,
    intent: &LandingIntent,
    now: &str,
) -> Result<(), StoreError> {
    let Some(mut view) = attempts::owner(tx, intent.project, intent.story)? else {
        return Ok(());
    };
    let attempt = view.state.attempts.iter().rev().find(|attempt| matches_intent(attempt, intent))
        .ok_or_else(|| StoreError::Validation("confirmed repair landing has no matching certified recovery attempt; retain pending landing authority".into()))?;
    let event = tx.events_for(intent.project, intent.story)?.into_iter().rev().find(|event| {
        matches!(event.known(), Some(StoryEvent::StoryPrMerged { at, url }) if at == now && url == &intent.pull_request)
    }).ok_or_else(|| StoreError::Corrupt("repair landing completion event was not retained".into()))?;
    let receipt = RepairLanding {
        intent: intent.clone(),
        attempt: attempt.id.clone(),
        event: event.global_seq,
        at: now.into(),
    };
    if let Some(previous) = &view.state.landing {
        return if previous == &receipt {
            Ok(())
        } else {
            Err(StoreError::Validation(
                "recovery already has a different confirmed repair landing".into(),
            ))
        };
    }
    view.state.landing = Some(receipt);
    view.record.active = false;
    persistence::save(tx, &mut view, now)
}

fn matches_intent(attempt: &RepairAttempt, intent: &LandingIntent) -> bool {
    attempt.story == intent.story
        && attempt.generation == intent.generation
        && attempt.completion == Some(RepairCompletion::Certified)
        && attempt.input.head == intent.certification.head
        && attempt.input.tree == intent.certification.tree
        && attempt.candidate.project == intent.project
        && attempt.candidate.project_slug == intent.project_slug
        && attempt.candidate.story_id == intent.story_id
        && attempt.candidate.checkout == intent.checkout
        && attempt
            .candidate
            .pull_request
            .as_ref()
            .is_ok_and(|pr| pr.url == intent.pull_request)
}

pub(super) fn validate(
    tx: &impl ReadOps,
    state: &RecoveryState,
    project: ProjectId,
) -> Result<(), StoreError> {
    let Some(receipt) = &state.landing else {
        return Ok(());
    };
    receipt
        .intent
        .certification
        .validate()
        .map_err(|error| StoreError::Corrupt(format!("repair landing certification: {error}")))?;
    persistence::timestamp(&receipt.at)?;
    if receipt.intent.project != project
        || state.decision.as_ref().and_then(|d| d.repair_story) != Some(receipt.intent.story)
        || !state.attempts.iter().any(|attempt| {
            attempt.id == receipt.attempt && matches_intent(attempt, &receipt.intent)
        })
        || receipt.event <= receipt.intent.generation
        || !tx.events_for(project, receipt.intent.story)?.iter().any(|event| event.global_seq == receipt.event
            && matches!(event.known(), Some(StoryEvent::StoryPrMerged { at, url }) if at == &receipt.at && url == &receipt.intent.pull_request))
    {
        return Err(StoreError::Corrupt(
            "repair landing has inconsistent certified attempt or completion authority".into(),
        ));
    }
    Ok(())
}
