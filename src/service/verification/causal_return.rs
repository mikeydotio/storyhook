//! Native causal authority and its state effect commit together.
use super::*;
use crate::service::attribution::CausalReturnEvidence;

impl<S: Store> VerificationQueue<'_, S> {
    /// Return only the exact generation and component proved by native execution.
    pub fn record_causal_return(
        &self,
        ctx: &Ctx<'_, S>,
        candidate: &VerificationCandidate,
        proof: &CausalReturnEvidence,
    ) -> Result<bool, AppError> {
        if ctx.project() != candidate.project {
            return Err(AppError::Validation(
                "causal return belongs to another project".into(),
            ));
        }
        Ok(ctx.write_stories(|tx| apply(tx, ctx, candidate, proof))?)
    }
}

/// Apply only native authority, inside the caller's complete state-effect transaction.
pub(in crate::service) fn apply<S: Store>(
    tx: &mut impl WriteOps,
    ctx: &Ctx<'_, S>,
    candidate: &VerificationCandidate,
    proof: &CausalReturnEvidence,
) -> Result<bool, StoreError> {
    let now = ctx.now();
    if !proof.validate(tx, candidate)? {
        return Ok(false);
    }
    let project = candidate.project;
    let prefix = project_prefix(tx, project)?;
    let (story, row) = resolve_story(tx, project, &prefix, &candidate.story_id)?;
    let states = tx.state_map(project)?;
    let target = states.get(RETURNED_STATE).ok_or_else(|| {
        AppError::Validation("causal return requires the in-progress state".into())
    })?;
    clear_candidate_incident(tx, candidate)?;
    append_state_transition(
        tx,
        project,
        story,
        &row,
        &prefix,
        &states,
        target,
        &now,
        vec![StoryEvent::StoryCommentAdded {
            at: now.clone(),
            text: proof.diagnosis(),
        }],
        ctx.provenance(),
    )?;
    proof.retire(tx)?;
    Ok(true)
}
