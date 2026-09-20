//! Durable scope assessment for proven project-owned verifier faults.

mod attempts;
mod attempts_validation;
mod authority;
mod decision;
mod decision_effects;
mod holds;
mod judgment;
mod landing;
mod model;
mod persistence;
mod refusal;
mod repair_return;
mod resume;
mod test_return;
mod work;
mod work_holds;
use super::{
    Ctx, project_fault::ProjectFault, project_prefix, resolve_story,
    verification::VerificationCandidate,
};
use crate::{
    domain::StoryEvent,
    error::AppError,
    store::{ProjectRecovery, ProjectRecoveryObservation, ReadOps, Store, StoreError, WriteOps},
};
pub use attempts::{
    RepairAdmission, RepairAttempt, RepairCompletion, RepairInput, RepairRefusal,
    RepairRefusalRecord,
};
pub use decision::{DecisionInput, DecisionReceipt, RepairScope, RepairSpec};
pub use judgment::RepairJudgment;
pub use landing::RepairLanding;
pub(crate) use landing::record_landing;
pub use model::*;
use persistence::{find, read_view, save, serialize, timestamp};
pub use refusal::RepairRefusalDisposition;
pub(crate) use resume::owns_resume;
pub use work::{WorkDelivery, WorkKind, WorkStatus};
pub use work_holds::WorkHold;

/// Coordinates project recovery using the selected project's ordinary story transactions.
pub struct ProjectRecoveryService<'a, S: Store> {
    ctx: &'a Ctx<'a, S>,
}

impl<'a, S: Store> ProjectRecoveryService<'a, S> {
    /// Bind policy without performing external effects.
    pub fn new(ctx: &'a Ctx<'a, S>) -> Self {
        Self { ctx }
    }
    /// Atomically retain a current fault and enqueue its assessment charter.
    pub fn observe(
        &self,
        candidate: &VerificationCandidate,
        fault: &ProjectFault,
        attempt: &str,
    ) -> Result<Option<RecoveryView>, AppError> {
        fault.validate().map_err(AppError::Validation)?;
        if candidate.project != self.ctx.project()
            || attempt.trim().is_empty()
            || candidate.pull_request.is_err()
        {
            return Err(AppError::Validation(
                "project recovery requires a scoped, submitted verification attempt".into(),
            ));
        }
        let generation = candidate.verifying_generation.ok_or_else(|| {
            AppError::Validation("project fault has no verification generation".into())
        })?;
        let evidence = serialize(&FaultObservation {
            version: 1,
            candidate: candidate.clone(),
            fault: fault.clone(),
        })?;
        let now = self.ctx.now();
        self.ctx.write_stories(|tx| {
            let records = tx.project_recoveries(candidate.project)?;
            // Replay preserves the original receipt, never renewed story authority.
            for record in &records {
                let observations = tx.project_recovery_observations(candidate.project, &record.id)?;
                if let Some(previous) = observations.iter().find(|o| o.attempt_id == attempt) {
                    if previous.evidence != evidence {
                        return Err(StoreError::Validation(format!("project recovery attempt {attempt} has conflicting evidence")));
                    }
                    return read_view(tx, record.clone()).map(Some);
                }
            }
            let prefix = project_prefix(tx, candidate.project)?;
            let (story, row) = resolve_story(tx, candidate.project, &prefix, &candidate.story_id)?;
            if row.awaiting.is_some()
                || candidate.landing_pending
                || tx.landing_intents()?.iter().any(|intent| intent.project == candidate.project && intent.story == story)
                || !super::verification::candidate_is_current(tx, &row, candidate)? {
                return Ok(None);
            }
            let policy_hold = authority::policy_hold(tx, candidate.project, &row.snapshot)?;
            let (code, locus) = fault.identity();
            let repair_owner = attempts::owner(tx, candidate.project, story)?;
            if let Some(owner) = &repair_owner {
                let (head, head_tree) = fault.source();
                let (base, tree) = fault.proposed_merge();
                if !owner.state.attempts.iter().any(|admitted| admitted.id == attempt
                    && admitted.story == story && admitted.generation == generation
                    && attempts::authority_matches(candidate, &admitted.candidate)
                    && admitted.completion == Some(RepairCompletion::ProjectFault)
                    && admitted.judgment.as_ref() == Some(&RepairJudgment::ProjectFault { fault: fault.clone() })
                    && admitted.input.head == head && admitted.input.head_tree == head_tree
                    && admitted.input.base == base && admitted.input.tree == tree) {
                    return Err(StoreError::Validation("repair fault does not match a completed admitted attempt in its lineage".into()));
                }
            }
            let existing = repair_owner.map(|owner| owner.record).or_else(|| records.into_iter().find(|r| r.active && r.code == code && r.locus == locus));
            let mut view = if let Some(record) = existing {
                read_view(tx, record)?
            } else {
                let state = RecoveryState {
                    version: 1, created_at: now.clone(), updated_at: now.clone(), subjects: Vec::new(), decision: None, holds: Vec::new(), work: Vec::new(), attempts: Vec::new(), refusals: Vec::new(), landing: None,
                    assessment: Assessment {
                        dispatch_identity: uuid::Uuid::new_v4().to_string(), story, generation,
                        status: if policy_hold.is_some() { AssessmentStatus::Held } else { AssessmentStatus::Pending },
                        hold: policy_hold,
                        epoch: 0, failures: 0, started_at: None, delivered_at: None,
                        detail: policy_hold.map(|hold| hold.detail().to_string()).unwrap_or_else(|| "scope assessment pending after verifier ownership release".into()),
                        last_result: None,
                    },
                };
                let record = ProjectRecovery { id: uuid::Uuid::new_v4().to_string(), project: candidate.project,
                    code: code.into(), locus: locus.into(), revision: 0, active: true, state: serialize(&state)? };
                if !tx.insert_project_recovery(&record)? {
                    return Err(StoreError::Validation("project fault acquired another coordinator inside the write transaction".into()));
                }
                RecoveryView { record, state, observations: Vec::new() }
            };
            let observation = ProjectRecoveryObservation { recovery_id: view.record.id.clone(), project: candidate.project,
                story, generation, attempt_id: attempt.into(), observed_at: now.clone(), evidence: evidence.clone() };
            tx.insert_project_recovery_observation(&observation)?;
            view.observations.push(observation);
            let returned = policy_hold.is_none();
            if returned {
                let states = tx.state_map(candidate.project)?;
                let target = states.get(super::verification::RETURNED_STATE).ok_or_else(|| StoreError::Validation("project has no in-progress state for fault assessment".into()))?;
                let instructions = if view.state.decision.is_some() {
                    format!("Read `story verifier repair show {} --json` and follow its accepted scope decision; retain the existing repair lineage.", view.record.id)
                } else if story == view.state.assessment.story {
                    assessment_charter(&view)
                } else {
                    format!("{} owns the scope assessment. Read `story verifier repair show {} --json`. Wait for the recorded scope decision; do not start a competing repair.", view.state.assessment.story.to_id(&prefix), view.record.id)
                };
                let diagnosis = format!("PROJECT VERIFICATION FAULT — recovery {} retains the unjudged submission. {}\n\n{}",
                    view.record.id, instructions, crate::text_lint::quote_evidence(fault.detail()));
                super::story::append_state_transition(tx, candidate.project, story, &row, &prefix, &states, target, &now,
                    vec![StoryEvent::StoryCommentAdded { at: now.clone(), text: diagnosis }], self.ctx.provenance())?;
            }
            view.state.subjects.push(AffectedSubmission {
                candidate: candidate.clone(), story, returned,
                state_revision: authority::state_revision(tx, candidate.project, story)?,
                label_revision: authority::label_revision(tx, candidate.project, story)?,
            });
            if let Some(mut receipt) = view.state.decision.take() {
                let mut latest = view.clone();
                latest.state.subjects = view.state.subjects.last().cloned().into_iter().collect();
                decision_effects::apply(tx, self.ctx, &latest, &mut receipt, &now)?;
                view.state.decision = Some(receipt);
            }
            repair_return::enqueue(tx, self.ctx, &mut view, attempt, &now)?;
            holds::record(tx, self.ctx, &mut view, &now)?;
            save(tx, &mut view, &now)?;
            Ok(Some(view))
        }).map_err(Into::into)
    }
    /// Read one recovery in the selected project.
    pub fn show(&self, id: &str) -> Result<RecoveryView, AppError> {
        self.ctx
            .store()
            .read(|tx| find(tx, self.ctx.project(), id))
            .map_err(Into::into)
    }
    /// Claim pending assessment delivery under current story and operator authority.
    pub fn claim_assessment(&self, id: &str) -> Result<Option<RecoveryView>, AppError> {
        let now = self.ctx.now();
        self.ctx
            .write_stories(|tx| {
                let mut view = find(tx, self.ctx.project(), id)?;
                if !view.record.active {
                    return Ok(None);
                }
                if view.state.assessment.status == AssessmentStatus::Delivered {
                    let delivered =
                        view.state
                            .assessment
                            .delivered_at
                            .as_deref()
                            .ok_or_else(|| {
                                StoreError::Corrupt(
                                    "delivered assessment has no delivery time".into(),
                                )
                            })?;
                    if timestamp(&now)? - timestamp(delivered)? >= chrono::Duration::minutes(30) {
                        view.state.assessment.status = AssessmentStatus::Held;
                        view.state.assessment.hold = Some(AssessmentHold::ResponseExpired);
                        view.state.assessment.detail =
                            AssessmentHold::ResponseExpired.detail().into();
                        holds::record(tx, self.ctx, &mut view, &now)?;
                        save(tx, &mut view, &now)?;
                    }
                    return Ok(None);
                }
                if view.state.assessment.status != AssessmentStatus::Pending {
                    return Ok(None);
                }
                if let Some(reason) = authority::assessment_hold(tx, &view)? {
                    view.state.assessment.status = AssessmentStatus::Held;
                    view.state.assessment.hold = Some(reason);
                    view.state.assessment.detail = reason.detail().into();
                    holds::record(tx, self.ctx, &mut view, &now)?;
                    save(tx, &mut view, &now)?;
                    return Ok(None);
                }
                view.state.assessment.status = AssessmentStatus::InFlight;
                view.state.assessment.hold = None;
                view.state.assessment.epoch =
                    view.state.assessment.epoch.checked_add(1).ok_or_else(|| {
                        StoreError::Corrupt("assessment delivery epoch overflow".into())
                    })?;
                view.state.assessment.started_at = Some(now.clone());
                view.state.assessment.last_result = None;
                save(tx, &mut view, &now)?;
                Ok(Some(view))
            })
            .map_err(Into::into)
    }
    /// Retain the result of exactly one claimed external delivery attempt.
    pub fn settle_assessment(
        &self,
        id: &str,
        identity: &str,
        epoch: u32,
        result: AssessmentDelivery,
    ) -> Result<RecoveryView, AppError> {
        if matches!(&result, AssessmentDelivery::ProvenFailure(detail) | AssessmentDelivery::Uncertain(detail) if detail.trim().is_empty())
        {
            return Err(AppError::Validation(
                "assessment transport failure requires diagnostics".into(),
            ));
        }
        let now = self.ctx.now();
        self.ctx
            .write_stories(|tx| {
                let mut view = find(tx, self.ctx.project(), id)?;
                let assessment = &mut view.state.assessment;
                if assessment.dispatch_identity != identity || assessment.epoch != epoch {
                    return Err(StoreError::Validation(
                        "stale or foreign assessment delivery identity".into(),
                    ));
                }
                if assessment.last_result.as_ref() == Some(&result) {
                    return Ok(view);
                }
                if assessment.status != AssessmentStatus::InFlight {
                    return Err(StoreError::Validation(
                        "assessment delivery has already settled".into(),
                    ));
                }
                match &result {
                    AssessmentDelivery::Delivered => {
                        assessment.status = AssessmentStatus::Delivered;
                        assessment.delivered_at = Some(now.clone());
                        assessment.detail =
                            "scope charter delivered; decision required before implementation"
                                .into();
                    }
                    AssessmentDelivery::ProvenFailure(detail) => {
                        assessment.failures = assessment.failures.saturating_add(1);
                        assessment.status = if assessment.failures >= 3 {
                            AssessmentStatus::Held
                        } else {
                            AssessmentStatus::Pending
                        };
                        assessment.hold =
                            (assessment.failures >= 3).then_some(AssessmentHold::DeliveryExhausted);
                        assessment.detail = format!(
                            "proven assessment delivery failure {}/3: {detail}",
                            assessment.failures
                        );
                    }
                    AssessmentDelivery::Uncertain(detail) => {
                        assessment.status = AssessmentStatus::Held;
                        assessment.hold = Some(AssessmentHold::OwnershipUncertain);
                        assessment.detail =
                            format!("assessment ownership remains uncertain: {detail}");
                    }
                }
                assessment.last_result = Some(result);
                if let Some(reason) = authority::assessment_hold(tx, &view)? {
                    view.state.assessment.status = AssessmentStatus::Held;
                    view.state.assessment.hold = Some(reason);
                    view.state
                        .assessment
                        .detail
                        .push_str(&format!("; authority withheld: {}", reason.detail()));
                }
                holds::record(tx, self.ctx, &mut view, &now)?;
                save(tx, &mut view, &now)?;
                Ok(view)
            })
            .map_err(Into::into)
    }
}

/// The durable charter reused by managed notification delivery.
pub fn assessment_charter(view: &RecoveryView) -> String {
    format!(
        "Read `story help scope-rubric` and `story verifier repair show {} --json`. Inspect the project source and retained evidence. Before edits, decide scope: same-story, separate-story, or external. Submit the versioned decision with `story verifier repair decide {} --input <json-file>`, using the current recovery revision, owning project {}, originating generation {}, and dispatch_identity {}. Include evidence references and nonempty Context, Question, Decision, and Rationale. Separate work needs a repair description and acceptance criteria; it becomes one critical repair in the owning project. Do not edit before the decision is accepted. Preserve all required coverage and certification; never manufacture receipts or bypass permissions.",
        view.record.id,
        view.record.id,
        view.record.project.get(),
        view.state.assessment.generation.get(),
        view.state.assessment.dispatch_identity
    )
}
