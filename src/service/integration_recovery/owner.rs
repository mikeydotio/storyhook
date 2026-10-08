//! Durable original-submission ownership before any managed integration effect.
use super::*;
use crate::{
    domain::StoryEvent,
    service::{
        Ctx, VerificationCandidate, append_and_fold,
        attribution::{AttributionRecord, FailureCause},
    },
    store::{
        ExpectedSeq, GlobalSeq, IntegrationRecovery, ReadOps, Store, StoreError, StoryNo, WriteOps,
    },
};

/// An integration phase never borrows a batch's identity or authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntegrationPhase {
    /// Native proposal retained; no external operation has begun.
    Reserved,
    /// Assembly is durably owned; restart must reconcile it before replay.
    Assembling,
    /// Authority, semantic ambiguity, or uncertain effects require reconciliation.
    Held,
}

/// Durable state of an original submission's distinct integration owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationOwner {
    /// Persistence envelope version.
    pub version: u8,
    /// Original submission authority, never a synthetic managed PR candidate.
    pub candidate: VerificationCandidate,
    /// Exact original diagnostic snapshot; changed evidence withholds effects.
    pub attribution: AttributionRecord,
    /// Independently retained integration component.
    pub component: String,
    /// Actual origin-bound original PR metadata used for native inspection.
    pub submission: SubmissionObservation,
    /// Pinned native proposal; no source or parent can silently change on restart.
    pub plan: IntegrationPlan,
    /// Private managed branch, separate from every author and batch branch.
    pub branch: String,
    /// Exact private resource location retained before any filesystem operation.
    pub workspace: std::path::PathBuf,
    /// Original gate admission wall time, retained across every recovery boundary.
    pub started_at: String,
    /// First integration reservation time.
    pub reserved_at: String,
    /// Most recent state change.
    pub updated_at: String,
    /// Exact reserved-label episode at enrollment.
    pub label_revision: Option<GlobalSeq>,
    /// Operator control epoch before the proposal was accepted.
    pub control_revision: i64,
    /// Durable lifecycle phase.
    pub phase: IntegrationPhase,
    /// Monotonic external-operation claim ordinal.
    pub effect_epoch: u32,
    /// Original claim time for the outstanding operation, never renewed by replay.
    pub effect_started_at: Option<String>,
    /// A concrete held reason; no hold itself releases native resource ownership.
    pub hold: Option<String>,
}

/// One native assembly claim, minted only by the durable compare-and-swap.
/// Public persisted state cannot reconstruct this capability after a crash.
/// The adapter must check `assembly_permitted` immediately before each effect;
/// this claim does not replace the central worker's process/resource ownership.
///
/// ```compile_fail
/// use storyhook::service::integration_recovery::AssemblyClaim;
/// let _: AssemblyClaim = serde_json::from_str("{}").unwrap();
/// ```
pub struct AssemblyClaim {
    record: IntegrationRecovery,
    owner: IntegrationOwner,
}

impl AssemblyClaim {
    /// Durable owner identity, distinct from the original PR and batch identities.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.record.id
    }
    /// Exact claimed operation ordinal, never renewed on restart.
    #[must_use]
    pub fn epoch(&self) -> u32 {
        self.owner.effect_epoch
    }
    /// Single managed branch allowed for this claim.
    #[must_use]
    pub fn branch(&self) -> &str {
        &self.owner.branch
    }
    /// Exact private assembly location; an adapter must never adopt a replacement.
    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.owner.workspace
    }
    /// Immutable native resolution and original parent identities.
    #[must_use]
    pub fn plan(&self) -> &IntegrationPlan {
        &self.owner.plan
    }
    /// Origin-bound original PR observation.
    #[must_use]
    pub fn submission(&self) -> &SubmissionObservation {
        &self.owner.submission
    }
    /// Original story admission identity.
    #[must_use]
    pub fn candidate(&self) -> &VerificationCandidate {
        &self.owner.candidate
    }
}

/// Uses the selected project's normal transactional story/event boundary.
pub struct IntegrationOwnerService<'a, S: Store> {
    ctx: &'a Ctx<'a, S>,
}

impl<'a, S: Store> IntegrationOwnerService<'a, S> {
    /// Bind the service without causing native effects.
    pub fn new(ctx: &'a Ctx<'a, S>) -> Self {
        Self { ctx }
    }

    /// Reserve only a live native proposal matching immutable diagnostic head
    /// evidence. Serialized plan/advice cannot call this door with a capability.
    pub fn reserve(
        &self,
        candidate: &VerificationCandidate,
        attribution: &str,
        component: &str,
        proof: &BoundIntegrationProposal,
    ) -> Result<IntegrationRecovery, AppError> {
        if candidate.project != self.ctx.project() {
            return Err(AppError::Validation(
                "integration proposal belongs to another project".into(),
            ));
        }
        let now = self.ctx.now();
        self.ctx.write_stories(|tx| {
            let prefix = crate::service::project_prefix(tx, candidate.project)?;
            let story = StoryNo::parse_id(&prefix, &candidate.story_id)?;
            for record in tx.integration_recoveries(candidate.project)?.into_iter().filter(|record| record.active && record.story == story) {
                let state = decode(&record)?;
                if state.candidate == *candidate && state.attribution.id == attribution && state.component == component && state.plan == *proof.plan() && state.submission == *proof.submission() {
                    return Ok(record);
                }
                return Err(invalid("the original story already has an integration owner; reconcile it before replacing source"));
            }
            let retained = tx.attributions(candidate.project)?.into_iter().find(|record| record.id == attribution)
                .ok_or_else(|| invalid("original attribution is missing"))?;
            retained.validate()?;
            if !retained.held || retained.retired.is_some() || retained.has_unsettled_diagnosis()
                || retained.submission.project != candidate.project
                || retained.submission.story_number() != Some(story)
                || retained.submission.generation != candidate.verifying_generation
                || retained.inputs.head.as_deref() != Some(proof.plan().head.as_str())
                || !retained.components.iter().any(|entry| entry.id == component && entry.observed_cause == FailureCause::Integration)
                || proof.submission().head != proof.plan().head || proof.submission().base != proof.plan().base
                || proof.submission().checkout != candidate.checkout
            { return Err(invalid("native proposal differs from retained submission, integration component, or settled diagnostic custody")); }
            let attempt = tx.gate_attempts(candidate.project)?.into_iter().find(|attempt| attempt.id == retained.attempt && attempt.submission == retained.submission)
                .ok_or_else(|| invalid("original gate admission is unavailable"))?;
            let control_revision = tx.verification_control_revision(candidate.project)?;
            if attempt.control_revision != Some(control_revision) { return Err(invalid("original gate control epoch changed")); }
            check_candidate(tx, candidate)?;
            let id = uuid::Uuid::new_v4().simple().to_string();
            let state = IntegrationOwner {
                version: 1, candidate: candidate.clone(), attribution: retained, component: component.into(),
                submission: proof.submission().clone(), plan: proof.plan().clone(), branch: format!("storyhook/integration/{id}"),
                workspace: self.ctx.env().daemon_state_dir().join("integrations").join(&id),
                started_at: attempt.admitted_at, reserved_at: now.clone(), updated_at: now.clone(),
                label_revision: crate::service::project_recovery::recovery_label_revision(tx, candidate.project, story)?,
                control_revision, phase: IntegrationPhase::Reserved, effect_epoch: 0, effect_started_at: None, hold: None,
            };
            let record = IntegrationRecovery { id, project: candidate.project, story, generation: candidate.verifying_generation.ok_or_else(|| invalid("submission has no generation"))?, revision: 0, active: true, state: encode(&state)? };
            decode(&record)?;
            if !tx.insert_integration_recovery(&record)? { return Err(invalid("integration ownership changed during reservation")); }
            let row = tx.story(candidate.project, story)?.ok_or_else(|| invalid("submitted story disappeared"))?;
            let states = tx.state_map(candidate.project)?;
            append_and_fold(tx, candidate.project, story, &prefix, &states, ExpectedSeq::Exact(row.head_seq),
                &[StoryEvent::StoryCommentAdded { at: now.clone(), text: format!("INTEGRATION RECOVERY {}: retain this Verifying submission and original head {}. A separately managed integration branch/PR owns the deterministic non-code proposal; publication and exact-tree certification are still required. No author resubmission or branch rewrite is authorized.", record.id, state.plan.head) }], self.ctx.provenance())?;
            Ok(record)
        }).map_err(Into::into)
    }

    /// Read a strict retained owner without recreating native authority.
    pub fn show(&self, id: &str) -> Result<(IntegrationRecovery, IntegrationOwner), AppError> {
        self.ctx
            .store()
            .read(|tx| find(tx, self.ctx.project(), id))
            .map_err(Into::into)
    }

    /// Claim assembly once, after a fresh native PR/policy/source inspection.
    /// An outstanding claim survives restart; calling this again cannot replay it.
    pub fn claim_assembly(
        &self,
        id: &str,
        expected: i64,
        proof: &BoundIntegrationProposal,
    ) -> Result<Option<AssemblyClaim>, AppError> {
        let now = self.ctx.now();
        self.ctx
            .store()
            .write(|tx| {
                let (mut record, mut state) = find(tx, self.ctx.project(), id)?;
                if !record.active
                    || record.revision != expected
                    || state.phase != IntegrationPhase::Reserved
                {
                    return Ok(None);
                }
                check_authority(tx, &record, &state, proof)?;
                settled_attempt(tx, &state)?;
                state.phase = IntegrationPhase::Assembling;
                state.effect_epoch = state
                    .effect_epoch
                    .checked_add(1)
                    .ok_or_else(|| invalid("integration effect epoch overflow"))?;
                state.effect_started_at = Some(now.clone());
                state.updated_at = now.clone();
                save(tx, &mut record, &state)?;
                Ok(Some(AssemblyClaim {
                    record,
                    owner: state,
                }))
            })
            .map_err(Into::into)
    }

    /// Recheck the exact in-process claim and current authority before an adapter
    /// performs an external operation. A persisted Assembling row grants no replay.
    pub fn assembly_permitted(
        &self,
        claim: &AssemblyClaim,
        proof: &BoundIntegrationProposal,
    ) -> Result<bool, AppError> {
        if claim.record.project != self.ctx.project() {
            return Ok(false);
        }
        self.ctx
            .store()
            .read(|tx| {
                let (record, state) = find(tx, self.ctx.project(), claim.id())?;
                if record != claim.record
                    || state != claim.owner
                    || state.phase != IntegrationPhase::Assembling
                {
                    return Ok(false);
                }
                check_authority(tx, &record, &state, proof)?;
                settled_attempt(tx, &state)?;
                Ok(true)
            })
            .map_err(Into::into)
    }
}

/// Accounting alone does not confer process ownership; this is an additional
/// fail-closed fence. The daemon adapter must also own its normal verifier slot.
fn settled_attempt(tx: &impl ReadOps, state: &IntegrationOwner) -> Result<(), StoreError> {
    let attempt = tx
        .gate_attempts(state.candidate.project)?
        .into_iter()
        .find(|attempt| {
            attempt.id == state.attribution.attempt
                && attempt.submission == state.attribution.submission
        })
        .ok_or_else(|| invalid("original gate admission disappeared"))?;
    if attempt.finished_at.is_none()
        || attempt.elapsed.estimated
        || attempt.verdict.as_deref() != Some("conflict")
        || attempt.executions.iter().any(|execution| {
            execution.finished_at.is_none()
                || execution.estimated
                || execution
                    .verdict
                    .as_deref()
                    .is_none_or(|v| matches!(v, "interrupted" | "cleanup-failed"))
        })
    {
        return Err(invalid(
            "original integration gate or cleanup has not conclusively settled",
        ));
    }
    Ok(())
}

pub(super) fn decode(record: &IntegrationRecovery) -> Result<IntegrationOwner, StoreError> {
    let state: IntegrationOwner = serde_json::from_value(record.state.clone())
        .map_err(|e| StoreError::Corrupt(format!("integration {} state: {e}", record.id)))?;
    let corrupt = || {
        StoreError::Corrupt(format!(
            "integration {} has inconsistent immutable ownership or phase",
            record.id
        ))
    };
    for at in [&state.started_at, &state.reserved_at, &state.updated_at]
        .into_iter()
        .chain(state.effect_started_at.iter())
    {
        chrono::DateTime::parse_from_rfc3339(at).map_err(|_| corrupt())?;
    }
    if state.version != 1
        || state.plan.version != 1
        || !record.active
        || state.candidate.project != record.project
        || state.candidate.verifying_generation != Some(record.generation)
        || state.attribution.submission.story_number() != Some(record.story)
        || state.attribution.submission.project != record.project
        || state.attribution.submission.generation != Some(record.generation)
        || state.attribution.inputs.head.as_deref() != Some(state.plan.head.as_str())
        || state.submission.head != state.plan.head
        || state.submission.base != state.plan.base
        || state.submission.checkout != state.candidate.checkout
        || !state.workspace.is_absolute()
        || state.workspace.file_name().and_then(|name| name.to_str()) != Some(record.id.as_str())
        || state.branch != format!("storyhook/integration/{}", record.id)
        || uuid::Uuid::parse_str(&record.id).is_err()
        || (state.phase == IntegrationPhase::Held) != state.hold.is_some()
        || (state.effect_epoch > 0) != state.effect_started_at.is_some()
        || (state.phase == IntegrationPhase::Reserved && state.effect_epoch != 0)
        || (state.phase == IntegrationPhase::Assembling && state.effect_epoch == 0)
    {
        return Err(corrupt());
    }
    state.attribution.validate()?;
    Ok(state)
}

fn check_candidate(tx: &impl ReadOps, candidate: &VerificationCandidate) -> Result<(), StoreError> {
    let prefix = crate::service::project_prefix(tx, candidate.project)?;
    let story = StoryNo::parse_id(&prefix, &candidate.story_id)?;
    let row = tx
        .story(candidate.project, story)?
        .ok_or_else(|| invalid("submitted story disappeared"))?;
    let expected_pr = candidate
        .pull_request
        .as_ref()
        .map_err(|_| invalid("submission PR missing"))?;
    let expected = crate::domain::pr_url::parse_pr_url(&expected_pr.url)
        .map_err(|e| invalid(&e.to_string()))?;
    let links = tx.open_pr_links_for_story(candidate.project, story)?;
    if !tx.verification_enabled(candidate.project)?
        || crate::domain::is_reserved(&row.snapshot)
        || !crate::service::automations::permits_generation(
            tx,
            candidate.project,
            candidate.verifying_generation,
        )?
        || row.awaiting.is_some()
        || !crate::service::verification::candidate_is_current(tx, &row, candidate)?
        || crate::service::project_recovery::recovery_resource_hold(tx, candidate.project, story)?
        || tx.checkout_path(candidate.project)?.as_ref() != Some(&candidate.checkout)
        || links.iter().filter(|link| link.close_on_merge).count() != 1
        || !links.iter().any(|link| {
            link.close_on_merge
                && crate::domain::pr_url::parse_pr_url(&link.url)
                    .is_ok_and(|reference| reference == expected)
        })
    {
        return Err(invalid(
            "operator, labels, dependencies, cleanup, checkout, PR or generation authority changed",
        ));
    }
    Ok(())
}

fn check_authority(
    tx: &impl ReadOps,
    record: &IntegrationRecovery,
    state: &IntegrationOwner,
    proof: &BoundIntegrationProposal,
) -> Result<(), StoreError> {
    check_candidate(tx, &state.candidate)?;
    if proof.plan() != &state.plan
        || proof.submission() != &state.submission
        || tx.verification_control_revision(record.project)? != state.control_revision
        || crate::service::project_recovery::recovery_label_revision(
            tx,
            record.project,
            record.story,
        )? != state.label_revision
        || !tx
            .attributions(record.project)?
            .contains(&state.attribution)
    {
        return Err(invalid(
            "native source, policy, original head, diagnostic evidence or operator epoch changed",
        ));
    }
    Ok(())
}

fn find(
    tx: &impl ReadOps,
    project: crate::store::ProjectId,
    id: &str,
) -> Result<(IntegrationRecovery, IntegrationOwner), StoreError> {
    let record = tx
        .integration_recoveries(project)?
        .into_iter()
        .find(|record| record.id == id)
        .ok_or_else(|| invalid("integration owner missing in selected project"))?;
    let state = decode(&record)?;
    Ok((record, state))
}

fn encode(state: &IntegrationOwner) -> Result<serde_json::Value, StoreError> {
    serde_json::to_value(state)
        .map_err(|e| invalid(&format!("cannot encode integration state: {e}")))
}

fn save(
    tx: &mut impl WriteOps,
    record: &mut IntegrationRecovery,
    state: &IntegrationOwner,
) -> Result<(), StoreError> {
    let expected = record.revision;
    record.revision = record
        .revision
        .checked_add(1)
        .ok_or_else(|| invalid("integration revision overflow"))?;
    record.state = encode(state)?;
    decode(record)?;
    if !tx.update_integration_recovery(record, expected)? {
        return Err(invalid("integration owner revision changed"));
    }
    Ok(())
}

fn invalid(detail: &str) -> StoreError {
    StoreError::Validation(format!("integration recovery: {detail}"))
}
