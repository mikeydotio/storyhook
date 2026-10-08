//! Durable records that forbid permanently deleting a story.
//!
//! A permanent delete purges the story's row and every event. A record that
//! still depends on the story would then name something that no longer exists,
//! so the delete stops here first — and so does its preview, which must never
//! promise a deletion the delete itself refuses.
//!
//! The store refuses to purge a story that one of its own owners holds (by
//! foreign key or the ownership fence), but that refusal reads as store damage.
//! Each owner is named here instead, as an ordinary validation error.

use crate::error::AppError;
use crate::store::{DeliveryStatus, ProjectId, ReadOps, StoreError, StoryNo};

/// Refuses deleting `story`, whose canonical id is `id`, while a durable
/// record depends on it.
pub(crate) fn refuse_deletion(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
    id: &str,
) -> Result<(), StoreError> {
    // First, so a reset keeps the refusal text its callers already match.
    super::engine::reset::refuse_reserved(tx, project, story)?;
    if tx.story_resets(project)?.contains_key(&story) {
        return Err(refusal(format!(
            "story `{id}` reset in progress (story reset); run `story reset {id}` to finish \
             cleanup before deleting it"
        )));
    }
    if let Some(intent) = tx
        .landing_intents()?
        .into_iter()
        .find(|intent| intent.project == project && intent.story == story)
    {
        return Err(refusal(format!(
            "story `{id}` has a landing in progress (landing {}, {}); the verifier must \
             finish or recover it before the story can be deleted",
            intent.id, intent.pull_request
        )));
    }
    if let Some(delivery) = tx
        .block_deliveries(project)?
        .into_iter()
        .find(|delivery| delivery.story == story && delivery.status == DeliveryStatus::Attempting)
    {
        return Err(refusal(format!(
            "story `{id}` has block delivery {} in flight to its session; retry the delete \
             after it settles",
            delivery.id
        )));
    }
    refuse_recovery_evidence(tx, project, story, id)
}

/// A project recovery keeps exact event references into every story it names.
fn refuse_recovery_evidence(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
    id: &str,
) -> Result<(), StoreError> {
    if let Some(owner) = tx
        .integration_recoveries(project)?
        .iter()
        .find(|owner| owner.story == story)
    {
        return Err(refusal(format!(
            "story `{id}` is retained evidence in integration recovery {}; reconcile its resources and retain its history before any deletion",
            owner.id
        )));
    }
    let recoveries = super::project_recovery::naming(tx, project, story)?;
    let Some(first) = recoveries.first() else {
        return Ok(());
    };
    let named = recoveries
        .iter()
        .map(|recovery| format!("{} ({})", recovery.id, recovery.code))
        .collect::<Vec<_>>()
        .join(", ");
    let noun = if recoveries.len() == 1 {
        "project recovery"
    } else {
        "project recoveries"
    };
    Err(refusal(format!(
        "story `{id}` is evidence in {noun} {named} and cannot be deleted; a recovery keeps \
         every story it names for as long as its record lives. Read `story verifier repair \
         show {}`. To retire the story, close it: `story close {id} \"<reason>\"`. Closing \
         keeps its history; it does not release the recovery.",
        first.id
    )))
}

fn refusal(message: String) -> StoreError {
    AppError::Validation(message).into()
}
