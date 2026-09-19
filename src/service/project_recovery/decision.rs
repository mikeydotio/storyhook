//! An assessor's scope choice, committed with its repair and dependency effects.

use super::{
    AssessmentDelivery, AssessmentStatus, ProjectRecoveryService, RecoveryView, authority,
    persistence,
};
use crate::{
    error::AppError,
    store::{GlobalSeq, ProjectId, ReadOps, Store, StoreError, StoryNo},
};
use serde::{Deserialize, Serialize};

/// Scope selected by the managed assessor, never inferred from error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RepairScope {
    /// Repair within the original submission's story and worktree.
    SameStory,
    /// Create a dedicated repair in the proven owning project.
    SeparateStory,
    /// A named prerequisite requires explicit resolution.
    External,
}

/// Required work description for a dedicated repair.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairSpec {
    /// Standalone repair title.
    pub title: String,
    /// Source diagnosis and bounded work description.
    pub description: String,
    /// Observable completion conditions, including regression coverage.
    pub acceptance: String,
}

/// Strict, versioned request shared by CLI and RPC decision doors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionInput {
    /// Request schema version, currently one.
    pub version: u8,
    /// Exact recovery revision inspected by the assessor.
    pub revision: i64,
    /// Proven owning project; cannot be reassigned by scope advice.
    pub project: ProjectId,
    /// Originating unjudged verification generation.
    pub generation: GlobalSeq,
    /// Stable claimed assessment delivery identity.
    pub dispatch_identity: String,
    /// Chosen repair scope.
    pub scope: RepairScope,
    /// Facts and constraints sufficient to understand the decision.
    pub context: String,
    /// The scope question answered.
    pub question: String,
    /// The selected answer.
    pub decision: String,
    /// Alternatives, trade-offs, and reasons.
    pub rationale: String,
    /// Source references, including at least one retained `attempt:<id>`.
    pub evidence: Vec<String>,
    /// Present exactly for separate-story scope.
    pub repair: Option<RepairSpec>,
    /// Actual unmet prerequisite, present exactly for external scope.
    pub prerequisite: Option<String>,
}

/// Durable result retained for exact replay and subsequent managed delivery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionReceipt {
    /// The accepted request, preserved verbatim as structured input.
    pub input: DecisionInput,
    /// RFC3339 commit time.
    pub accepted_at: String,
    /// Repair owner, absent for external prerequisites.
    pub repair_story: Option<StoryNo>,
    /// Stable identity of the pending repair delivery, absent for external scope.
    pub delivery_identity: Option<String>,
    /// Subjects whose blocked-by edge was created by this recovery.
    pub owned_edges: Vec<StoryNo>,
    /// Joined subjects whose changed authority prevented mutation.
    pub skipped_subjects: Vec<StoryNo>,
}

impl<S: Store> ProjectRecoveryService<'_, S> {
    /// Accept scope exactly once under current assessment authority.
    pub fn decide(&self, id: &str, input: &DecisionInput) -> Result<RecoveryView, AppError> {
        input.validate()?;
        let now = self.ctx.now();
        self.ctx
            .write_stories(|tx| {
                let mut view = persistence::find(tx, self.ctx.project(), id)?;
                if let Some(previous) = &view.state.decision {
                    return if &previous.input == input {
                        Ok(view)
                    } else {
                        Err(StoreError::Validation(
                            "recovery already has a different scope decision".into(),
                        ))
                    };
                }
                validate_authority(tx, &view, input, &now)?;
                let repair_story = match input.scope {
                    RepairScope::SameStory => Some(view.state.assessment.story),
                    RepairScope::SeparateStory => Some(super::decision_effects::create_repair(
                        tx, self.ctx, &view, input, &now,
                    )?),
                    RepairScope::External => None,
                };
                let mut receipt = DecisionReceipt {
                    input: input.clone(),
                    accepted_at: now.clone(),
                    repair_story,
                    delivery_identity: repair_story.map(|_| uuid::Uuid::new_v4().to_string()),
                    owned_edges: Vec::new(),
                    skipped_subjects: Vec::new(),
                };
                super::decision_effects::apply(tx, self.ctx, &view, &mut receipt, &now)?;
                view.state.assessment.status = AssessmentStatus::Decided;
                view.state.assessment.hold = None;
                view.state
                    .assessment
                    .delivered_at
                    .get_or_insert_with(|| now.clone());
                view.state.assessment.last_result = Some(AssessmentDelivery::Delivered);
                view.state.assessment.detail =
                    "scope decision accepted; follow the retained repair disposition".into();
                view.state.decision = Some(receipt);
                persistence::save(tx, &mut view, &now)?;
                Ok(view)
            })
            .map_err(Into::into)
    }
}

impl DecisionInput {
    /// Reject unsupported versions, blank evidence, and inconsistent scope payloads.
    pub fn validate(&self) -> Result<(), AppError> {
        let nonblank = |text: &str| !text.trim().is_empty();
        let scope_valid = match self.scope {
            RepairScope::SameStory => self.repair.is_none() && self.prerequisite.is_none(),
            RepairScope::SeparateStory => {
                self.prerequisite.is_none()
                    && self.repair.as_ref().is_some_and(|r| {
                        nonblank(&r.title) && nonblank(&r.description) && nonblank(&r.acceptance)
                    })
            }
            RepairScope::External => {
                self.repair.is_none() && self.prerequisite.as_deref().is_some_and(nonblank)
            }
        };
        if self.version != 1
            || self.revision < 0
            || !scope_valid
            || [
                &self.dispatch_identity,
                &self.context,
                &self.question,
                &self.decision,
                &self.rationale,
            ]
            .iter()
            .any(|s| !nonblank(s))
            || self.evidence.is_empty()
            || self.evidence.iter().any(|s| !nonblank(s))
        {
            return Err(AppError::Validation("invalid recovery decision: require version 1, exact authority, nonempty Context/Question/Decision/Rationale and evidence, and scope-consistent repair or prerequisite fields".into()));
        }
        Ok(())
    }
    pub(super) fn comment(&self, recovery: &str) -> String {
        format!(
            "PROJECT RECOVERY {recovery}\n\nContext: {}\nQuestion: {}\nDecision: {}\nRationale: {}\nEvidence: {}",
            self.context,
            self.question,
            self.decision,
            self.rationale,
            self.evidence.join("; ")
        )
    }
}

fn validate_authority(
    tx: &impl ReadOps,
    view: &RecoveryView,
    input: &DecisionInput,
    now: &str,
) -> Result<(), StoreError> {
    let assessment = &view.state.assessment;
    if !view.record.active
        || input.project != view.record.project
        || input.revision != view.record.revision
        || input.generation != assessment.generation
        || input.dispatch_identity != assessment.dispatch_identity
        || assessment.epoch == 0
        || !matches!(
            assessment.status,
            AssessmentStatus::InFlight | AssessmentStatus::Delivered
        )
        || !view.observations.iter().any(|o| {
            input
                .evidence
                .contains(&format!("attempt:{}", o.attempt_id))
        })
    {
        return Err(StoreError::Validation(
            "stale, foreign, or unclaimed recovery decision authority or evidence".into(),
        ));
    }
    if let Some(delivered) = &assessment.delivered_at
        && persistence::timestamp(now)? - persistence::timestamp(delivered)?
            >= chrono::Duration::minutes(30)
    {
        return Err(StoreError::Validation(
            "scope assessment response deadline expired".into(),
        ));
    }
    if let Some(hold) = authority::assessment_hold(tx, view)? {
        return Err(StoreError::Validation(format!(
            "scope assessment authority refused: {}",
            hold.detail()
        )));
    }
    Ok(())
}
