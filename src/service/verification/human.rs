//! Human ownership revokes automation without changing readiness or story state.

use super::*;

/// Last durable reservation for a person, even when it was subsequently removed.
pub(crate) fn revision(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
) -> Result<Option<GlobalSeq>, StoreError> {
    Ok(tx
        .events_for(project, story)?
        .iter()
        .rev()
        .find_map(|event| {
            matches!(event.known(), Some(StoryEvent::StoryLabelsSet { labels, .. })
            if labels.iter().any(|label| label == crate::domain::LABEL_HUMAN_ONLY))
            .then_some(event.global_seq)
        }))
}

/// Checks label authority independently of verification, repair, or completion state.
pub(crate) fn permits(
    tx: &impl ReadOps,
    candidate: &VerificationCandidate,
) -> Result<bool, StoreError> {
    let Some(project) = tx.project(candidate.project)? else {
        return Ok(false);
    };
    let number = StoryNo::parse_id(&project.prefix, &candidate.story_id)?;
    let Some(row) = tx.story(candidate.project, number)? else {
        return Ok(false);
    };
    permits_row(tx, &row, candidate)
}

/// Checks an already-read row inside the caller's write transaction.
pub(super) fn permits_row(
    tx: &impl ReadOps,
    row: &StoryRow,
    candidate: &VerificationCandidate,
) -> Result<bool, StoreError> {
    Ok(!crate::domain::is_human_only(&row.snapshot)
        && revision(tx, candidate.project, row.story_no)? == candidate.human_only_revision)
}

impl<S: Store> VerificationQueue<'_, S> {
    /// Adds a lifecycle comment under label authority; cleanup diagnostics are
    /// observations and remain recordable after that authority is revoked.
    pub(crate) fn comment_if_human_permitted(
        &self,
        ctx: &Ctx<'_, S>,
        candidate: &VerificationCandidate,
        text: &str,
    ) -> Result<(), AppError> {
        ctx.write_stories(|tx| {
            let prefix = project_prefix(tx, candidate.project)?;
            let (number, row) = resolve_story(tx, candidate.project, &prefix, &candidate.story_id)?;
            if (!text.starts_with(VERIFICATION_CLEANUP_REQUIRED_PREFIX)
                && !permits_row(tx, &row, candidate)?)
                || row.snapshot.comments.iter().any(|c| c.text == text)
            {
                return Ok(());
            }
            let states = tx.state_map(candidate.project)?;
            append_and_fold(
                tx,
                candidate.project,
                number,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &[StoryEvent::StoryCommentAdded {
                    at: ctx.now(),
                    text: text.into(),
                }],
                ctx.provenance(),
            )?;
            Ok(())
        })?;
        Ok(())
    }
}
