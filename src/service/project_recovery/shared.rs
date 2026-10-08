//! Native shared-fault ownership. Stored descriptions never mint this capability.
pub(super) mod join;
pub(super) mod readmit;
use super::{
    AffectedSubmission, Assessment, AssessmentStatus, ProjectRecoveryService, RecoveryState,
    RecoveryView, authority, persistence,
};
use crate::{
    error::AppError,
    service::{
        VerificationCandidate,
        attribution::{AttributionRecord, SharedRecoveryEvidence},
    },
    store::{
        GlobalSeq, ProjectId, ProjectRecovery, ProjectRecoveryObservation, ReadOps, Store,
        StoreError, StoryNo, WriteOps,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const CODE: &str = "native-shared-project";

/// A project-scoped native finding. Candidate heads remain per-submission facts.
/// A native Rust comparison cannot establish machine-wide fault authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedFaultIdentity {
    /// Explicit project scope; host-wide repair needs its own supported proof adapter.
    pub project: ProjectId,
    /// Exact detector case and stable diagnostic identity.
    pub check: String,
    /// Diagnostic signature, independent of output paths and timestamps.
    pub signature: String,
    /// Pinned base commit on which the same failure reproduces.
    pub base: String,
    /// Exact prepared control tree.
    pub control_tree: String,
    /// Content identity of the detector dependency closure.
    pub detector: String,
    /// Validated exact reproduction command.
    pub argv: Vec<String>,
    /// Toolchain observed on all four native executions.
    pub toolchain: String,
    /// Fixture and external input identity.
    pub fixtures: String,
    /// Supported resource policy identity.
    pub resource_policy: String,
}

/// Durable extension for retained submissions; legacy records have no such authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedRecovery {
    /// Extension version.
    pub version: u8,
    /// Exact native evidence identity owned by this coordinator.
    pub fault: SharedFaultIdentity,
    /// Atomic readmission receipts; they do not claim a new gate or certification.
    pub readmissions: Vec<SharedReadmission>,
}

/// An exact release of one recovery-owned component, never a blanket unblock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedReadmission {
    /// Retained subject, also present in the recovery's subjects.
    pub story: StoryNo,
    /// Original submitted generation; readmission does not ask the author to resubmit.
    pub generation: GlobalSeq,
    /// Immutable diagnostic record whose component was released.
    pub attribution: String,
    /// Exact component covered by this recovery.
    pub component: String,
    /// Durable release event, after the repair/prerequisite release anchor.
    pub event: GlobalSeq,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Observation {
    pub version: u8,
    pub candidate: VerificationCandidate,
    pub attribution: AttributionRecord,
    pub component: String,
    pub native: crate::service::attribution::RetainedNativeEvidence,
}

impl SharedFaultIdentity {
    fn from_observation(observation: &Observation) -> Result<Self, StoreError> {
        use crate::service::attribution::{FailureCause, classify};
        let record = &observation.attribution;
        record.validate()?;
        let component = record
            .components
            .iter()
            .find(|c| c.id == observation.component)
            .ok_or_else(|| invalid("shared component is absent"))?;
        let (index, plan) = record
            .plans
            .iter()
            .enumerate()
            .find(|(_, p)| p.component == component.id)
            .ok_or_else(|| invalid("shared contrast is absent"))?;
        let environment = record
            .probes
            .iter()
            .find(|p| p.plan == index)
            .and_then(|p| p.completed.as_ref())
            .and_then(|p| p.environment.as_ref())
            .ok_or_else(|| invalid("shared native environment is absent"))?;
        if observation.version != 2
            || record
                .inputs
                .head
                .as_deref()
                .is_none_or(|head| !crate::service::project_fault::is_pinned_oid(head))
            || record.inputs.base.as_deref() != Some(plan.base.as_str())
            || record.inputs.tree.as_deref() != Some(plan.candidate_tree.as_str())
            || classify(record, component) != FailureCause::SharedProject
            || record.has_unsettled_diagnosis()
            || !record.held
            || record.retired.is_some()
            || !record.submission.matches_story(
                observation.candidate.project,
                &observation.candidate.story_id,
            )
            || record.submission.generation != observation.candidate.verifying_generation
        {
            return Err(invalid(
                "shared observation has no complete matching native finding",
            ));
        }
        Ok(Self {
            project: observation.candidate.project,
            check: component.check.clone(),
            signature: component.signature.clone(),
            base: plan.base.clone(),
            control_tree: plan.control_tree.clone(),
            detector: plan.detector.clone(),
            argv: plan.argv.clone(),
            toolchain: environment.toolchain.clone(),
            fixtures: environment.fixtures.clone(),
            resource_policy: environment.resource_policy.clone(),
        })
    }

    fn locus(&self) -> Result<String, StoreError> {
        let bytes = serde_json::to_vec(self)
            .map_err(|e| invalid(&format!("encode shared identity: {e}")))?;
        Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
    }
}

impl<S: Store> ProjectRecoveryService<'_, S> {
    /// Atomically enroll native-proved shared failure without returning its submission.
    /// The opaque proof is revalidated inside the same transaction as ownership acquisition.
    pub fn observe_shared(
        &self,
        candidate: &VerificationCandidate,
        proof: &SharedRecoveryEvidence,
    ) -> Result<Option<RecoveryView>, AppError> {
        if candidate.project != self.ctx.project() {
            return Err(AppError::Validation(
                "shared proof belongs to another project".into(),
            ));
        }
        let observation = Observation {
            version: 2,
            candidate: candidate.clone(),
            attribution: proof.record().clone(),
            component: proof.plan().component.clone(),
            native: proof.retained(),
        };
        let fault = SharedFaultIdentity::from_observation(&observation)?;
        let locus = fault.locus()?;
        let evidence = persistence::serialize(&observation)?;
        let now = self.ctx.now();
        self.ctx.write_stories(|tx| {
            let records = tx.project_recoveries(candidate.project)?;
            for record in &records {
                if let Some(previous) = tx.project_recovery_observations(candidate.project, &record.id)?.iter().find(|o| o.attempt_id == observation.attribution.attempt) {
                    if previous.evidence != evidence { return Err(invalid("shared attempt replay has different evidence")); }
                    return persistence::read_view(tx, record.clone()).map(Some);
                }
            }
            if !proof.validate(tx, candidate)? { return Ok(None); }
            let generation = candidate.verifying_generation.ok_or_else(|| invalid("shared submission has no generation"))?;
            let prefix = crate::service::project_prefix(tx, candidate.project)?;
            let (story, row) = crate::service::resolve_story(tx, candidate.project, &prefix, &candidate.story_id)?;
            if row.awaiting.is_some() || authority::policy_hold(tx, candidate.project, &row.snapshot)?.is_some()
                || super::resume::resource_hold(tx, candidate.project, story)? {
                return Ok(None);
            }
            // An admitted repair cannot create another lineage while its parent remains active.
            if super::attempts::owner(tx, candidate.project, story)?.is_some() {
                return Err(invalid("shared fault in an existing repair remains held in its current lineage"));
            }
            let existing = records.iter().find(|r| r.active && r.code == CODE && r.locus == locus);
            let mut view = if let Some(record) = existing {
                persistence::read_view(tx, record.clone())?
            } else {
                let state = RecoveryState {
                    version: 1, created_at: now.clone(), updated_at: now.clone(), subjects: Vec::new(),
                    decision: None, holds: Vec::new(), work: Vec::new(), attempts: Vec::new(), refusals: Vec::new(), landing: None,
                    legacy_incidents: Vec::new(), prerequisite: None,
                    supersedes: records.iter().rev().find(|r| !r.active && r.code == CODE && r.locus == locus).map(|r| r.id.clone()),
                    shared: Some(SharedRecovery { version: 1, fault: fault.clone(), readmissions: Vec::new() }),
                    assessment: Assessment { dispatch_identity: uuid::Uuid::new_v4().to_string(), story, generation,
                        status: AssessmentStatus::Pending, hold: None, epoch: 0, failures: 0, started_at: None, delivered_at: None,
                        detail: "Native shared failure proved; retained submission awaits managed scope assessment".into(), last_result: None },
                };
                let record = ProjectRecovery { id: uuid::Uuid::new_v4().to_string(), project: candidate.project, code: CODE.into(), locus: locus.clone(), revision: 0, active: true, state: persistence::serialize(&state)? };
                if !tx.insert_project_recovery(&record)? { return Err(invalid("shared fault acquired another coordinator")); }
                RecoveryView { record, state, observations: Vec::new() }
            };
            let retained = ProjectRecoveryObservation { recovery_id: view.record.id.clone(), project: candidate.project, story, generation,
                attempt_id: observation.attribution.attempt.clone(), observed_at: now.clone(), evidence: evidence.clone() };
            tx.insert_project_recovery_observation(&retained)?;
            view.observations.push(retained);
            view.state.subjects.push(AffectedSubmission { candidate: candidate.clone(), story, returned: false,
                state_revision: authority::state_revision(tx, candidate.project, story)?, label_revision: authority::label_revision(tx, candidate.project, story)? });
            crate::service::append_and_fold(tx, candidate.project, story, &prefix, &tx.state_map(candidate.project)?, crate::store::ExpectedSeq::Exact(row.head_seq),
                &[crate::domain::StoryEvent::StoryCommentAdded { at: now.clone(), text: format!("SHARED VERIFICATION FAULT — recovery {} owns {} on pinned base {}. This submission remains verifying and unjudged. A certified repair or explicitly satisfied prerequisite will readmit it for a fresh merge and gate; no author resubmission is required.", view.record.id, fault.check, fault.base) }], self.ctx.provenance())?;
            if let Some(mut decision) = view.state.decision.take() {
                let mut latest = view.clone();
                latest.state.subjects = view.state.subjects.last().cloned().into_iter().collect();
                super::decision_effects::apply(tx, self.ctx, &latest, &mut decision, &now)?;
                view.state.decision = Some(decision);
            }
            persistence::save(tx, &mut view, &now)?;
            Ok(Some(view))
        }).map_err(Into::into)
    }
}

pub(super) fn validate_observation(
    state: &RecoveryState,
    record: &ProjectRecovery,
    observation: &ProjectRecoveryObservation,
) -> Result<Observation, StoreError> {
    let shared = state
        .shared
        .as_ref()
        .ok_or_else(|| invalid("shared extension is absent"))?;
    let evidence: Observation = serde_json::from_value(observation.evidence.clone())
        .map_err(|e| invalid(&format!("shared evidence: {e}")))?;
    if shared.version != 1
        || record.code != CODE
        || record.project != shared.fault.project
        || record.locus != shared.fault.locus()?
        || shared.fault != SharedFaultIdentity::from_observation(&evidence)?
        || observation.project != record.project
        || observation.recovery_id != record.id
        || observation.attempt_id != evidence.attribution.attempt
        || evidence.candidate.verifying_generation != Some(observation.generation)
        || !state.subjects.iter().any(|s| {
            !s.returned && s.story == observation.story && s.candidate == evidence.candidate
        })
    {
        return Err(invalid(
            "shared observation disagrees with its owner or retained submission",
        ));
    }
    Ok(evidence)
}

pub(super) fn retained_current(
    tx: &impl ReadOps,
    view: &RecoveryView,
    subject: &AffectedSubmission,
) -> Result<bool, StoreError> {
    let project = view.record.project;
    let Some(row) = tx.story(project, subject.story)? else {
        return Ok(false);
    };
    let attributions = tx.attributions(project)?;
    Ok(view.state.shared.is_some()
        && !subject.returned
        && crate::service::automations::permits_generation(
            tx,
            project,
            subject.candidate.verifying_generation,
        )?
        && row.state == crate::service::verification::VERIFYING_STATE
        && row.awaiting.is_none()
        && authority::state_revision(tx, project, subject.story)? == subject.state_revision
        && authority::label_revision(tx, project, subject.story)? == subject.label_revision
        && authority::blocking_revision(tx, project, subject.story)?
            == subject.candidate.blocking_revision
        && authority::policy_hold(tx, project, &row.snapshot)?.is_none()
        && crate::service::verification::candidate_is_current(tx, &row, &subject.candidate)?
        && !super::resume::resource_hold(tx, project, subject.story)?
        && view
            .observations
            .iter()
            .filter(|o| {
                o.story == subject.story
                    && Some(o.generation) == subject.candidate.verifying_generation
            })
            .any(|o| {
                validate_observation(&view.state, &view.record, o).is_ok_and(|e| {
                    attributions.contains(&e.attribution) && e.native.verify().is_ok()
                })
            }))
}

/// A native project fault fences project admission, with a bounded managed
/// repair exception. No project-local observation pauses another project.
pub(crate) fn blocks_admission(
    tx: &impl ReadOps,
    project: ProjectId,
    story: Option<StoryNo>,
) -> Result<bool, StoreError> {
    let records: Vec<_> = tx
        .project_recoveries(project)?
        .into_iter()
        .filter(|r| r.active && r.code == CODE)
        .collect();
    if records.is_empty() {
        return Ok(false);
    }
    for record in &records {
        let view = persistence::read_view(tx, record.clone())?;
        if story.is_none() || view.state.decision.as_ref().and_then(|d| d.repair_story) != story {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Recheck enrolled custody at a managed effect boundary, including when the
/// originating submission has this recovery's own awaiting/dependency hold.
pub(super) fn evidence_current(tx: &impl ReadOps, view: &RecoveryView) -> Result<bool, StoreError> {
    if view.state.shared.is_none() {
        return Ok(true);
    }
    let records = tx.attributions(view.record.project)?;
    let followers = if join::leader(&view.state).is_none() {
        join::followers(tx, view)?
    } else {
        Vec::new()
    };
    for member in std::iter::once(view).chain(followers.iter()) {
        for observation in &member.observations {
            let evidence = validate_observation(&member.state, &member.record, observation)?;
            if !records.contains(&evidence.attribution) || evidence.native.verify().is_err() {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn invalid(detail: &str) -> StoreError {
    StoreError::Validation(format!("shared recovery: {detail}"))
}
