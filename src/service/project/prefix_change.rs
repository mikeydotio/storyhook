//! A prefix is display identity until an operation retains it as authority.
//!
//! Keep those operations under the prefix they accepted. Both preview and the
//! final rename call this inside their existing transaction; a preview is not
//! permission to rename after another writer has admitted an owner.
use crate::store::{EngineRunState, ProjectRecord, ReadOps, StoreError, StoryQuery};

pub(super) fn require_quiescent(
    tx: &impl ReadOps,
    project: &ProjectRecord,
) -> Result<(), StoreError> {
    let refuse = |owner: String| {
        StoreError::Validation(format!(
            "cannot change prefix for `{}` while {owner}; finish or resolve that operation, then retry. The prefix and its owned resources were not changed",
            project.slug
        ))
    };
    // Paused and halted runs remain resumable, including idle epic runs whose
    // scope_story_id still names the old prefix. Finished runs are history.
    for run in tx.engine_runs(&project.slug)? {
        if run.state != EngineRunState::Finished {
            return Err(refuse(format!(
                "Full Auto run `{}` is {}",
                run.id,
                run.state.as_str()
            )));
        }
    }
    for intent in tx.landing_intents()? {
        if intent.project == project.id {
            return Err(refuse(format!(
                "landing intent `{}` owns {}",
                intent.id, intent.story_id
            )));
        }
    }
    // Older reservation rows still retain canonical IDs; native receipts are
    // checked separately below so completed native receipts remain history.
    if let Some((story, reservation)) = tx.story_resets(project.id)?.first_key_value() {
        let record: serde_json::Value = serde_json::from_str(reservation)?;
        let token = record
            .get("operation")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| StoreError::Corrupt("legacy reset has no operation identity".into()))?;
        return Err(refuse(format!(
            "story reset `{token}` owns {}",
            story.to_id(&project.prefix)
        )));
    }
    for cleanup in tx.closure_cleanups(project.id)? {
        if !cleanup.completed {
            return Err(refuse(format!(
                "closure cleanup `{}` owns {}",
                cleanup.token,
                cleanup.story.to_id(&project.prefix)
            )));
        }
    }
    for row in tx.stories(project.id, &StoryQuery::all())? {
        if let Some(reset) = tx.story_reset(project.id, row.story_no)?
            && !reset.completed
        {
            return Err(refuse(format!(
                "story reset `{}` owns {}",
                reset.token, reset.story_id
            )));
        }
        if let Some(reset) = tx.engine_reset(project.id, row.story_no)? {
            return Err(refuse(format!(
                "engine reset `{}` owns {}",
                reset.token, reset.lease.story_id
            )));
        }
        if let Some(cleanup) = tx.dropped_cleanup(project.id, row.story_no)?
            && !cleanup.released
        {
            return Err(refuse(format!(
                "dropped cleanup `{}` owns {}",
                cleanup.token, cleanup.lease.story_id
            )));
        }
    }
    for batch in tx.verification_batches(project.id)? {
        if batch.phase.is_live()
            || batch
                .bisection
                .as_ref()
                .is_some_and(|search| search.is_unfinished())
        {
            return Err(refuse(format!(
                "verification batch `{}` has unfinished work for {}",
                batch.id, batch.head
            )));
        }
    }
    for continuation in tx.continuations(project.id)? {
        if continuation.status.outstanding() {
            return Err(refuse(format!(
                "continuation `{}` owns {}",
                continuation.id, continuation.story_id
            )));
        }
    }
    // Pending deliveries derive a fresh display ID from their numeric key.
    // Once attempting, their existing commit fence pins project identity.
    for delivery in tx.block_deliveries(project.id)? {
        if delivery.status == crate::store::DeliveryStatus::Attempting {
            return Err(refuse(format!(
                "block delivery `{}` is attempting {}",
                delivery.id,
                delivery.story.to_id(&project.prefix)
            )));
        }
    }
    Ok(())
}
