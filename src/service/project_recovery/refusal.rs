//! Refused gate input is disposed only after the verifier reports settled cleanup.

use super::{
    ProjectRecoveryService, RecoveryView, RepairRefusal, attempts, authority, persistence,
};
use crate::{
    domain::StoryEvent,
    error::AppError,
    service::VerificationCandidate,
    store::{ExpectedSeq, GlobalSeq, ProjectId, ReadOps, Store, StoreError},
};
use serde::{Deserialize, Serialize};

/// Exact awaiting write made for a refused repair attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairRefusalDisposition {
    /// Awaiting reason owned by this refusal.
    pub awaiting: String,
    /// Sequence of the owned awaiting event.
    pub event: GlobalSeq,
    /// RFC3339 disposition time.
    pub at: String,
}

impl<S: Store> ProjectRecoveryService<'_, S> {
    /// Apply one exact admission refusal after owned children and cleanup settle.
    pub fn apply_refusal(
        &self,
        candidate: &VerificationCandidate,
        attempt: &str,
        recovery: &str,
        reason: RepairRefusal,
    ) -> Result<Option<RecoveryView>, AppError> {
        if candidate.project != self.ctx.project() {
            return Err(AppError::Validation(
                "repair refusal belongs to another project".into(),
            ));
        }
        let now = self.ctx.now();
        self.ctx.write_stories(|tx| {
            let mut view = persistence::find(tx, candidate.project, recovery)?;
            let index = view.state.refusals.iter().position(|r| r.id == attempt).ok_or_else(|| StoreError::Validation("recovery has no matching refused attempt".into()))?;
            let refusal = &view.state.refusals[index];
            if refusal.reason != reason || !attempts::authority_matches(candidate, &refusal.candidate) {
                return Err(StoreError::Validation("refusal disposition differs from its retained reason or candidate authority".into()));
            }
            if refusal.disposition.is_some() { return Ok(Some(view)); }
            let story = refusal.story;
            let project = candidate.project;
            let Some(row) = tx.story(project, story)? else { return Ok(None); };
            if row.awaiting.is_some() || authority::policy_hold(tx, project, &row.snapshot)?.is_some()
                || authority::label_revision(tx, project, story)? != refusal.label_revision
                || candidate.landing_pending
                || tx.landing_intents()?.iter().any(|intent| intent.project == project && intent.story == story)
                || !crate::service::verification::candidate_is_current(tx, &row, candidate)? { return Ok(None); }
            let prefix = crate::service::project_prefix(tx, project)?;
            let awaiting = format!("Project recovery {recovery}: {}", reason.detail());
            let states = tx.state_map(project)?;
            let target = states.get(crate::service::verification::RETURNED_STATE).ok_or_else(|| StoreError::Validation("repair refusal has no in-progress state".into()))?;
            crate::service::story::append_state_transition(tx, project, story, &row, &prefix, &states, target, &now, vec![
                StoryEvent::StoryCommentAdded { at: now.clone(), text: format!("PROJECT REPAIR ADMISSION HELD — {}. No gate ran for attempt {attempt}. Read `story verifier repair show {recovery} --json` for pinned input and the completed repair budget. Preserve all test and certification requirements.", reason.detail()) },
            ], self.ctx.provenance())?;
            let returned = tx.story(project, story)?.ok_or_else(|| StoreError::Corrupt("refused repair disappeared".into()))?;
            crate::service::append_and_fold(tx, project, story, &prefix, &states, ExpectedSeq::Exact(returned.head_seq), &[StoryEvent::StoryAwaitingSet { at: now.clone(), awaiting: awaiting.clone() }], self.ctx.provenance())?;
            let event = tx.events_for(project, story)?.into_iter().rev().find(|e| matches!(e.known(), Some(StoryEvent::StoryAwaitingSet { at, awaiting: text }) if at == &now && text == &awaiting)).ok_or_else(|| StoreError::Corrupt("repair refusal hold event was not retained".into()))?.global_seq;
            view.state.refusals[index].disposition = Some(RepairRefusalDisposition { awaiting, event, at: now.clone() });
            persistence::save(tx, &mut view, &now)?;
            Ok(Some(view))
        }).map_err(Into::into)
    }
}

impl RepairRefusal {
    /// Stable human diagnosis for a typed admission refusal.
    pub fn detail(self) -> &'static str {
        match self {
            Self::UnchangedInput => {
                "repair input repeats committed content already judged in this recovery"
            }
            Self::BudgetExhausted => {
                "three changed repair submissions completed without resolving this recovery"
            }
            Self::PolicyHold => {
                "operator stop or a reserved label prevents automatic repair admission"
            }
        }
    }
}

pub(super) fn validate(
    tx: &impl ReadOps,
    state: &super::RecoveryState,
    project: ProjectId,
) -> Result<(), StoreError> {
    for refusal in &state.refusals {
        if let Some(disposition) = &refusal.disposition {
            persistence::timestamp(&disposition.at)?;
            if disposition.event <= refusal.generation || !tx.events_for(project, refusal.story)?.iter().any(|event| event.global_seq == disposition.event
                && matches!(event.known(), Some(StoryEvent::StoryAwaitingSet { at, awaiting }) if at == &disposition.at && awaiting == &disposition.awaiting)) {
                return Err(StoreError::Corrupt("repair refusal has inconsistent disposition event evidence".into()));
            }
        }
    }
    Ok(())
}
