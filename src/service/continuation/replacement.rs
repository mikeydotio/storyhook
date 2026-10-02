//! A manual resume replaces the continuation chain of a story (SH-850).
//!
//! A context handoff binds its delivery, and later the story's submission, to
//! one receiving session. After a reboot that session is gone for good: its
//! request sits `needs-attention` (still outstanding, so the next handoff is
//! refused), and an acknowledged request keeps fencing submission on a review
//! that only the lost session could refresh. A resume launches a fresh session
//! with the full charter, which rereads the story, where every handoff already
//! left its `CONTEXT HANDOFF` comment. So the resume supersedes the chain, in
//! one transaction, under the workspace lock its dispatch already holds.
use super::save;
use crate::store::{ContinuationStatus, ProjectId, StoreError, StoryNo, WriteOps};
use serde::Serialize;

/// What one replacement did to the story's context handoffs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ReplacementOutcome {
    /// Requests this replacement superseded, oldest first.
    pub superseded: Vec<String>,
    /// Requests whose delivery may be in flight. When any exists, nothing
    /// was superseded: the continuation monitor owns that gap, and a second
    /// launcher must not race it.
    pub attempting: Vec<String>,
}

/// Supersedes every context handoff of `story` that a fresh session replaces:
/// pending, awaiting acknowledgement, needing attention, or acknowledged (an
/// acknowledged review fences submission on a session that no longer runs).
///
/// Refuses by returning the `attempting` ids, changing nothing, while any of
/// the story's context handoffs is attempting delivery. Obviation-review
/// handoffs are administrative holds for a person and are never touched; the
/// resume's eligibility check already refuses the story they hold.
///
/// Callers must hold the story's workspace lock through the launch this
/// replacement precedes, the rule `block_delivery::supersede_pending` states
/// for block deliveries.
pub fn supersede_for_replacement(
    tx: &mut impl WriteOps,
    project: ProjectId,
    story: StoryNo,
    now: &str,
) -> Result<ReplacementOutcome, StoreError> {
    let mut chain: Vec<_> = tx
        .continuations(project)?
        .into_iter()
        .filter(|record| record.story_no == story && record.handoff["kind"] == "context")
        .collect();
    chain.sort_by(|a, b| (&a.created_at, &a.id).cmp(&(&b.created_at, &b.id)));
    let attempting: Vec<String> = chain
        .iter()
        .filter(|record| record.status == ContinuationStatus::Attempting)
        .map(|record| record.id.clone())
        .collect();
    if !attempting.is_empty() {
        return Ok(ReplacementOutcome {
            superseded: Vec::new(),
            attempting,
        });
    }
    let mut superseded = Vec::new();
    for mut record in chain {
        if record.status == ContinuationStatus::Superseded {
            continue;
        }
        record.detail = format!(
            "superseded by a manual resume ({:?} before): a fresh session with the full charter \
             replaces the lost receiving session; the handoff evidence stays in the story comments",
            record.status
        );
        record.status = ContinuationStatus::Superseded;
        save(tx, &mut record, now)?;
        superseded.push(record.id);
    }
    Ok(ReplacementOutcome {
        superseded,
        attempting: Vec::new(),
    })
}
