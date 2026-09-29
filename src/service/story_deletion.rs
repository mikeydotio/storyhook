//! Durable records that forbid permanently deleting a story.
//!
//! A permanent delete purges the story's row and every event. A record that
//! still depends on the story would then name something that no longer exists,
//! so the delete stops here first — and so does its preview, which must never
//! promise a deletion the delete itself refuses.

use crate::error::AppError;
use crate::store::{ProjectId, ReadOps, StoreError, StoryNo};

/// Refuses deleting `story`, whose canonical id is `id`, while a durable
/// record depends on it.
pub(crate) fn refuse_deletion(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
    id: &str,
) -> Result<(), StoreError> {
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
    Err(AppError::Validation(format!(
        "story `{id}` is evidence in {noun} {named} and cannot be deleted; a recovery keeps \
         every story it names for as long as its record lives. Read `story verifier repair \
         show {}`. To retire the story, close it: `story close {id} \"<reason>\"`. Closing \
         keeps its history; it does not release the recovery.",
        first.id
    ))
    .into())
}
