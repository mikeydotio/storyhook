//! Effective transitions are derived once per complete service transaction.
use super::Ctx;
use super::continuation::SubmissionEvidence;
use crate::domain::{SuperState, is_blocked};
use crate::error::AppError;
use crate::store::{
    BlockAction, DeliveryStatus, ProjectId, ReadOps, Store, StoreError, StoryNo, StoryQuery,
    WriteOps,
};
use std::collections::BTreeMap;

/// The exact operator-supplied prompt, never synthesized or paraphrased.
pub const UNBLOCK_PROMPT: &str = "Your story experienced a temporary block, which has been lifted. The environment and dev branch may have changed. Please reread your story, its comments, and its relationships to understand the changes, and adjust your work accordingly. If the change is significant, resetting & rebasing the worktree and restarting the story may be appropriate.";

/// Whether a derivation checks stories that newly enter `verifying` as submissions.
///
/// A choice every caller of [`derive_block_edges`] states, rather than an
/// `Option` it could leave empty by accident: the Git evidence a submission
/// check needs has to be read before the write transaction opens, and reading
/// it costs four bounded subprocesses (`continuation::current_submission`).
pub(crate) enum SubmissionGate<'e> {
    /// An ordinary service mutation. A story that newly enters `verifying` is
    /// checked against this evidence; `None` means the project had no
    /// continuation records when the evidence would have been read.
    Check(Option<&'e Result<SubmissionEvidence, AppError>>),
    /// The transaction completes, relabels or repairs existing history — a
    /// verifier landing moves a story out of `verifying`, never into it — and
    /// so submits nothing. The submission check does not apply.
    NotASubmission,
}

struct State {
    blocked: bool,
    active: bool,
    interruptible: bool,
    verifying: bool,
    sequence: i64,
}

/// Every story's delivery-relevant state, keyed by the row's own number.
///
/// Keyed by [`crate::store::StoryRow::story_no`] rather than by re-parsing the
/// snapshot's id, so the key is the row's identity and not a field of the
/// folded value it carries.
fn snapshot(tx: &impl ReadOps, project: ProjectId) -> Result<BTreeMap<StoryNo, State>, StoreError> {
    let stories = super::query::story_map(tx, project)?;
    tx.stories(project, &StoryQuery::all())?
        .into_iter()
        .map(|row| {
            let story = stories.get(&row.snapshot.id).ok_or_else(|| {
                StoreError::Corrupt("story disappeared during mutation snapshot".into())
            })?;
            Ok((
                row.story_no,
                State {
                    blocked: is_blocked(story, &stories),
                    verifying: story.state == crate::domain::VERIFYING_STATE_SLUG,
                    sequence: row.head_global_seq.get(),
                    active: story.superstate == SuperState::Open && story.state == "in-progress",
                    interruptible: story.superstate == SuperState::Open
                        && matches!(
                            story.state.as_str(),
                            "in-progress" | "blocked" | "verifying"
                        ),
                },
            ))
        })
        .collect()
}

/// Runs `f` inside `tx` and records the block-delivery edges its complete
/// effect implies: an Interrupt for every active story it newly blocks, a
/// Resume for every active story it newly unblocks, and supersession of the
/// pending effects of every block episode it ends.
///
/// The one place those edges are derived. [`Ctx::write_stories`] is the
/// ordinary door; a transaction that is not opened through a [`Ctx`] — a
/// verifier landing, a prefix rename, a read-model repair — calls this inside
/// its own write instead. Intermediate states inside `f` are never delivered:
/// only the project as it stood before `f` and as `f` leaves it are compared.
/// No external operation may run here, because SQLite owns the write lock.
pub(crate) fn derive_block_edges<W: WriteOps, T>(
    tx: &mut W,
    project: ProjectId,
    gate: SubmissionGate<'_>,
    f: impl FnOnce(&mut W) -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    let before = snapshot(tx, project)?;
    let result = f(tx)?;
    let after = snapshot(tx, project)?;
    for (story, next) in after {
        // Newly imported/created rows have no dispatched turn to interrupt.
        let Some(previous) = before.get(&story) else {
            continue;
        };
        if !previous.verifying
            && next.verifying
            && let SubmissionGate::Check(evidence) = &gate
        {
            super::continuation::check_submission(
                tx,
                project,
                story,
                previous.sequence,
                *evidence,
            )?;
        }
        // Pending authority belongs to one effective block episode. A
        // replacement or retired execution cannot inherit an old prompt.
        if previous.blocked != next.blocked || !next.interruptible {
            supersede_pending(tx, project, story, "effective block episode ended")?;
        }
        let action = if !previous.blocked && next.blocked {
            Some(BlockAction::Interrupt)
        } else if previous.blocked
            && !next.blocked
            && next.active
            && !super::project_recovery::owns_resume(tx, project, story)?
        {
            Some(BlockAction::Resume)
        } else {
            None
        };
        if let Some(action) = action {
            tx.enqueue_block_delivery(project, story, action)?;
            if !next.interruptible {
                supersede_pending(
                    tx,
                    project,
                    story,
                    "story has no active execution to interrupt",
                )?;
            }
        }
    }
    Ok(result)
}

impl<S: Store> Ctx<'_, S> {
    /// Commit a story-affecting mutation and its final effective delivery edges.
    /// No external operation occurs while SQLite owns the write transaction.
    pub(crate) fn write_stories<T>(
        &self,
        f: impl FnOnce(&mut S::WriteTx<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let has_continuations = self
            .store()
            .read(|tx| Ok(!tx.continuations(self.project())?.is_empty()))?;
        // Read external Git evidence before acquiring the write transaction. A
        // failure matters only if this mutation actually submits managed work.
        let submission_head =
            has_continuations.then(|| super::continuation::current_submission(self.cwd()));
        self.store().write(|tx| {
            derive_block_edges(
                tx,
                self.project(),
                SubmissionGate::Check(submission_head.as_ref()),
                f,
            )
        })
    }
}

/// Revokes effects that have not started before a block episode or session ends.
/// Replacement callers must hold WorkspaceLock through this transaction and handoff.
/// An Attempting effect retains its ordered acknowledgement and cannot be replayed.
pub(crate) fn supersede_pending(
    tx: &mut impl WriteOps,
    project: ProjectId,
    story: StoryNo,
    reason: &str,
) -> Result<usize, StoreError> {
    let mut superseded = 0;
    for mut delivery in tx.block_deliveries(project)? {
        if delivery.story == story && delivery.status == DeliveryStatus::Pending {
            delivery.status = DeliveryStatus::Superseded;
            delivery.detail = reason.into();
            superseded +=
                usize::from(tx.update_block_delivery(&delivery, DeliveryStatus::Pending)?);
        }
    }
    Ok(superseded)
}

#[cfg(test)]
mod tests;
