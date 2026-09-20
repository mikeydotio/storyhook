//! Settled failed tests re-enter the same durable repair lineage.

use super::{
    AffectedSubmission, ProjectRecoveryService, RepairJudgment, attempts, authority, persistence,
    repair_return,
};
use crate::{
    domain::StoryEvent,
    error::AppError,
    service::VerificationCandidate,
    store::{ReadOps, Store, StoreError, StoryNo},
};

impl<S: Store> ProjectRecoveryService<'_, S> {
    /// Return an admitted failed repair after verifier cleanup; false means ordinary work.
    /// A superseded repair is handled without changing its newer authority.
    pub fn return_failed_repair(
        &self,
        candidate: &VerificationCandidate,
        attempt: &str,
        tree: &str,
        detail: &str,
    ) -> Result<bool, AppError> {
        if candidate.project != self.ctx.project() || detail.trim().is_empty() {
            return Err(AppError::Validation(
                "failed repair needs this project and diagnostics".into(),
            ));
        }
        let now = self.ctx.now();
        self.ctx.write_stories(|tx| {
            let project = candidate.project;
            let prefix = crate::service::project_prefix(tx, project)?;
            let story = StoryNo::parse_id(&prefix, &candidate.story_id)?;
            let Some(mut view) = attempts::owner(tx, project, story)? else { return Ok(false); };
            let admitted = view.state.attempts.iter().find(|a| a.id == attempt)
                .ok_or_else(|| StoreError::Validation("failed repair has no admitted attempt".into()))?;
            if !attempts::authority_matches(candidate, &admitted.candidate)
                || admitted.judgment.as_ref() != Some(&RepairJudgment::TestsFailed { tree: tree.into() }) {
                return Err(StoreError::Validation("failed repair differs from its completed admission".into()));
            }
            if view.state.work.iter().any(|w| w.source_attempt.as_deref() == Some(attempt)) {
                return Ok(true);
            }
            let Some(row) = tx.story(project, story)? else { return Ok(true); };
            if row.awaiting.is_some() || candidate.landing_pending
                || super::resume::resource_hold(tx, project, story)?
                || authority::policy_hold(tx, project, &row.snapshot)?.is_some()
                || authority::label_revision(tx, project, story)? != admitted.label_revision
                || !crate::service::verification::candidate_is_current(tx, &row, candidate)? {
                return Ok(true);
            }
            let states = tx.state_map(project)?;
            let target = states.get(crate::service::verification::RETURNED_STATE)
                .ok_or_else(|| StoreError::Validation("failed repair has no in-progress state".into()))?;
            crate::service::story::append_state_transition(tx, project, story, &row, &prefix,
                &states, target, &now, vec![StoryEvent::StoryCommentAdded {
                    at: now.clone(), text: format!("PROJECT REPAIR TESTS FAILED — attempt {attempt}, tree {tree}. Continue the accepted scope in `story verifier repair show {} --json`. Preserve this worktree, run new and impacted tests, commit, then move {} to verifying. The central verifier owns submission and the full suite.\n\n{}", view.record.id, candidate.story_id, crate::text_lint::quote_evidence(detail)),
                }], self.ctx.provenance())?;
            view.state.subjects.push(AffectedSubmission {
                candidate: candidate.clone(), story, returned: true,
                state_revision: authority::state_revision(tx, project, story)?,
                label_revision: authority::label_revision(tx, project, story)?,
            });
            repair_return::enqueue(tx, self.ctx, &mut view, attempt, &now)?;
            persistence::save(tx, &mut view, &now)?;
            Ok(true)
        }).map_err(Into::into)
    }
}
