//! A closure's durable cleanup intent, distinct from destructive ownership.
use super::{GlobalSeq, ProjectId, StoryNo};
use crate::domain::StoryCleanupLease;
use serde::{Deserialize, Serialize};

/// One committed closed lifecycle, retained as an idempotent cleanup receipt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosureCleanup {
    /// Owning project.
    pub project: ProjectId,
    /// Story number within the project.
    pub story: StoryNo,
    /// Unique closure incarnation; reopening invalidates it.
    pub token: String,
    /// Event watermark when the closure was committed.
    pub generation: GlobalSeq,
    /// Agreed resource authority, pinned before destructive admission.
    pub lease: Option<StoryCleanupLease>,
    /// Every resource permitted by the cleanup policy has been reclaimed.
    pub completed: bool,
    /// Earliest automatic retry in UTC; manual cleanup may retry sooner.
    pub retry_at: Option<String>,
    /// Latest refusal or failure, for diagnosis and duplicate suppression.
    pub detail: Option<String>,
}

/// The same effective states used by query views, without rewriting event projections.
pub(crate) fn effective_states(
    rows: &[super::StoryRow],
    states: &[crate::domain::StateDef],
) -> std::collections::BTreeMap<StoryNo, (String, crate::domain::SuperState)> {
    let mut snapshots = rows
        .iter()
        .map(|row| (row.snapshot.id.clone(), row.snapshot.clone()))
        .collect();
    crate::domain::apply_computed_epic_states(&mut snapshots, states);
    rows.iter()
        .map(|row| {
            let snapshot = &snapshots[&row.snapshot.id];
            (
                row.story_no,
                (snapshot.state.clone(), snapshot.superstate.clone()),
            )
        })
        .collect()
}
