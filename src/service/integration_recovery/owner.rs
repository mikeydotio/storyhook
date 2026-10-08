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

pub(super) mod gate;
pub(super) mod landing;
pub(super) mod publication;

/// An integration phase never borrows a batch's identity or authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntegrationPhase {
    /// Native proposal retained; no external operation has begun.
    Reserved,
    /// Assembly is durably owned; restart must reconcile it before replay.
    Assembling,
    /// Native assembly is recorded; its live custody is still required.
    Assembled,
    /// Remote effects are owned separately and may be uncertain after restart.
    Publishing,
    /// Native publication is observed; the exact tree still needs a fresh gate.
    Published,
    /// The original central slot owns a distinct physical integration gate.
    Gating,
    /// A fresh native input proof started the centrally supervised gate.
    Running,
    /// Actual central result and fresh native inputs certified the exact tree.
    Certified,
    /// One durable managed merge intent; uncertainty never permits replay.
    Landing,
    /// Actual native managed landing completed the original story truthfully.
    Landed,
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
    /// Point-in-time remote resource diagnostic; never merge or cleanup authority.
    #[serde(default)]
    pub retained_branch: Option<RetainedBranchObservation>,
    /// Native assembly receipt; deserialization does not restore custody.
    #[serde(default)]
    pub assembly: Option<AssemblyEvidence>,
    /// Ordered one-shot remote effect intents, not success receipts.
    #[serde(default)]
    pub publication_effects: Vec<publication::PublicationEffect>,
    /// Exact managed PR observation, never a gate or original PR merge receipt.
    #[serde(default)]
    pub publication: Option<PublicationEvidence>,
    /// Physical admission identity, never a certificate or replay permission.
    #[serde(default)]
    pub gate_attempt: Option<String>,
    /// Native admission inputs; metadata alone grants no gate result.
    #[serde(default)]
    pub gate_inputs: Option<IntegrationGateInputsEvidence>,
    /// Certified physical execution; JSON retains evidence but cannot land.
    #[serde(default)]
    pub gate: Option<gate::IntegrationCertificationEvidence>,
    /// Exact distinct managed landing authority retained before a request.
    #[serde(default)]
    pub landing: Option<crate::store::LandingIntent>,
    /// A potentially sent merge request; no error may clear it for replay.
    #[serde(default)]
    pub landing_started: bool,
    /// True only after the exact native merge operation proved local effect drain.
    #[serde(default)]
    pub landing_settled: bool,
    /// Retained actual remote landing evidence; JSON grants no completion capability.
    #[serde(default)]
    pub landed: Option<IntegrationLandedEvidence>,
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
    pub(super) ctx: &'a Ctx<'a, S>,
}

impl<'a, S: Store> IntegrationOwnerService<'a, S> {
    /// Bind the service without causing native effects.
    pub fn new(ctx: &'a Ctx<'a, S>) -> Self {
        Self { ctx }
    }

    /// Owning environment carries declared subprocess policy into native
    /// adapters; it grants no additional repository or effect authority.
    pub fn environment(&self) -> &crate::env::Environment {
        self.ctx.env()
    }

    /// Preserve a concrete failure without releasing or rewinding its phase.
    /// Landed owners retain cleanup residue diagnostics without becoming active.
    /// This is a diagnostic, never a semantic decision or retry authorization.
    pub(crate) fn note_hold(&self, id: &str, detail: &str) -> Result<(), AppError> {
        let now = self.ctx.now();
        self.ctx
            .store()
            .write(|tx| {
                let (mut record, mut state) = find(tx, self.ctx.project(), id)?;
                if detail.trim().is_empty() {
                    return Err(invalid("managed hold diagnostic is empty"));
                }
                state.hold = Some(detail.chars().take(4096).collect());
                // The terminal completion instant anchors cumulative elapsed
                // time. Later cleanup diagnostics must not extend it.
                if state.phase != IntegrationPhase::Landed {
                    state.updated_at = now;
                }
                save(tx, &mut record, &state)
            })
            .map_err(Into::into)
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
        proof.check_live()?;
        if candidate.project != self.ctx.project() {
            return Err(AppError::Validation(
                "integration proposal belongs to another project".into(),
            ));
        }
        let now = self.ctx.now();
        self.ctx.write_stories(|tx| {
            proof.check_live().map_err(StoreError::from)?;
            let prefix = crate::service::project_prefix(tx, candidate.project)?;
            let story = StoryNo::parse_id(&prefix, &candidate.story_id)?;
            for record in tx.integration_recoveries(candidate.project)?.into_iter().filter(|record| record.active && record.story == story) {
                let state = decode(&record)?;
                if state.candidate == *candidate && state.attribution.id == attribution && state.component == component && state.plan == *proof.plan() && state.submission == *proof.submission() {
                    proof.check_live().map_err(StoreError::from)?;
                    return Ok(record);
                }
                return Err(invalid("the original story already has an integration owner; reconcile it before replacing source"));
            }
            let retained = tx.attributions(candidate.project)?.into_iter().find(|record| record.id == attribution)
                .ok_or_else(|| invalid("original attribution is missing"))?;
            retained.validate()?;
            super::pending::validate_retained(tx, candidate, &retained)?;
            if !retained.held || retained.retired.is_some() || retained.has_unsettled_diagnosis()
                || retained.submission.project != candidate.project
                || retained.submission.story_number() != Some(story)
                || retained.submission.generation != candidate.verifying_generation
                || retained.inputs.head.as_deref() != Some(proof.plan().head.as_str())
                || !retained.components.iter().any(|entry| entry.id == component && entry.observed_cause == FailureCause::Integration)
                || proof.submission().head != proof.plan().head || proof.submission().base != proof.plan().base
                || proof.submission().checkout != candidate.checkout
                || !same_original_pr(candidate, proof.submission())
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
                control_revision, phase: IntegrationPhase::Reserved, effect_epoch: 0, effect_started_at: None, hold: None, retained_branch: None,
                assembly: None, publication_effects: Vec::new(), publication: None, gate_attempt: None, gate_inputs: None, gate: None, landing: None, landing_started: false, landing_settled: false, landed: None,
            };
            let record = IntegrationRecovery { id, project: candidate.project, story, generation: candidate.verifying_generation.ok_or_else(|| invalid("submission has no generation"))?, revision: 0, active: true, state: encode(&state)? };
            decode(&record)?;
            if !tx.insert_integration_recovery(&record)? { return Err(invalid("integration ownership changed during reservation")); }
            let row = tx.story(candidate.project, story)?.ok_or_else(|| invalid("submitted story disappeared"))?;
            let states = tx.state_map(candidate.project)?;
            append_and_fold(tx, candidate.project, story, &prefix, &states, ExpectedSeq::Exact(row.head_seq),
                &[StoryEvent::StoryCommentAdded { at: now.clone(), text: format!("INTEGRATION RECOVERY {}: retain this Verifying submission and original head {}. A separately managed integration branch/PR owns the deterministic non-code proposal; publication and exact-tree certification are still required. No author resubmission or branch rewrite is authorized.", record.id, state.plan.head) }], self.ctx.provenance())?;
            proof.check_live().map_err(StoreError::from)?;
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
        proof.check_live()?;
        let now = self.ctx.now();
        self.ctx
            .store()
            .write(|tx| {
                proof.check_live().map_err(StoreError::from)?;
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
                proof.check_live().map_err(StoreError::from)?;
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
        proof.check_live()?;
        if claim.record.project != self.ctx.project() {
            return Ok(false);
        }
        self.ctx
            .store()
            .read(|tx| {
                proof.check_live().map_err(StoreError::from)?;
                let (record, state) = find(tx, self.ctx.project(), claim.id())?;
                if record != claim.record
                    || state != claim.owner
                    || state.phase != IntegrationPhase::Assembling
                {
                    return Ok(false);
                }
                check_authority(tx, &record, &state, proof)?;
                settled_attempt(tx, &state)?;
                proof.check_live().map_err(StoreError::from)?;
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
        || record.active == (state.phase == IntegrationPhase::Landed)
        || (state.phase == IntegrationPhase::Landed) != state.landed.is_some()
        || state.candidate.project != record.project
        || state.candidate.verifying_generation != Some(record.generation)
        || state.attribution.submission.story_number() != Some(record.story)
        || state.attribution.submission.project != record.project
        || state.attribution.submission.generation != Some(record.generation)
        || state.attribution.inputs.head.as_deref() != Some(state.plan.head.as_str())
        || state.submission.head != state.plan.head
        || state.submission.base != state.plan.base
        || state.submission.checkout != state.candidate.checkout
        || !same_original_pr(&state.candidate, &state.submission)
        || !state.workspace.is_absolute()
        || state.workspace.file_name().and_then(|name| name.to_str()) != Some(record.id.as_str())
        || state.branch != format!("storyhook/integration/{}", record.id)
        || uuid::Uuid::parse_str(&record.id).is_err()
        || (state.phase == IntegrationPhase::Held && state.hold.is_none())
        || state
            .hold
            .as_ref()
            .is_some_and(|reason| reason.trim().is_empty())
        || (state.effect_epoch > 0) != state.effect_started_at.is_some()
        || (state.phase == IntegrationPhase::Reserved && state.effect_epoch != 0)
        || (state.phase == IntegrationPhase::Assembling && state.effect_epoch == 0)
    {
        return Err(corrupt());
    }
    state.attribution.validate()?;
    publication::validate_state(record, &state)?;
    if let Some(observation) = &state.retained_branch {
        validate_branch_observation(&state, observation)?;
    }
    Ok(state)
}

pub(super) fn same_original_pr(
    candidate: &VerificationCandidate,
    submission: &SubmissionObservation,
) -> bool {
    let Ok(link) = &candidate.pull_request else {
        return false;
    };
    let Ok(expected) = crate::domain::pr_url::parse_pr_url(&link.url) else {
        return false;
    };
    let Ok(observed) = crate::domain::pr_url::parse_pr_url(&submission.pull_request) else {
        return false;
    };
    expected == observed
        && submission.repository.eq_ignore_ascii_case(&format!(
            "{}/{}/{}",
            expected.host, expected.owner, expected.repo
        ))
}

pub(super) fn check_candidate(
    tx: &impl ReadOps,
    candidate: &VerificationCandidate,
) -> Result<(), StoreError> {
    check_candidate_with_landing(tx, candidate, None)
}

fn check_candidate_with_landing(
    tx: &impl ReadOps,
    candidate: &VerificationCandidate,
    owned_landing: Option<&crate::store::LandingIntent>,
) -> Result<(), StoreError> {
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
    let resource_hold = match owned_landing {
        Some(intent) => {
            crate::service::project_recovery::recovery_resource_hold_except_managed_landing(
                tx,
                candidate.project,
                story,
                intent,
            )?
        }
        None => {
            crate::service::project_recovery::recovery_resource_hold(tx, candidate.project, story)?
        }
    };
    if crate::service::host_recovery::blocks_admission(tx)?
        || crate::service::project_recovery::shared_blocks_admission(
            tx,
            candidate.project,
            Some(story),
        )?
        || !tx.verification_enabled(candidate.project)?
        || crate::domain::is_reserved(&row.snapshot)
        || !crate::service::automations::permits_generation(
            tx,
            candidate.project,
            candidate.verifying_generation,
        )?
        || row.awaiting.is_some()
        || !crate::service::verification::candidate_is_current(tx, &row, candidate)?
        || !crate::service::verification::submission_is_current(tx, &row, candidate)?
        || !crate::service::verification::recovery_cleanup_history_is_current(tx, candidate)?
        || resource_hold
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
    check_retained_authority(tx, record, state)?;
    if proof.plan() != &state.plan || proof.submission() != &state.submission {
        return Err(invalid(
            "native source, policy, original head, diagnostic evidence or operator epoch changed",
        ));
    }
    Ok(())
}

// Current policy/lineage checks shared by distinct operations. Native input
// inspection is required separately before each new operation is admitted.
fn check_retained_authority(
    tx: &impl ReadOps,
    record: &IntegrationRecovery,
    state: &IntegrationOwner,
) -> Result<(), StoreError> {
    check_candidate_with_landing(tx, &state.candidate, state.landing.as_ref())?;
    if tx.verification_control_revision(record.project)? != state.control_revision
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
            "original diagnostic, labels or operator epoch changed",
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

/// Read-only original-submission ownership; no serialized phase grants effects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationRecoveryStatus {
    /// Exact original-generation admissions/costs, never the effect ordinal.
    #[serde(default)]
    pub retained_submission: Option<crate::service::gate_cost::view::RetainedSubmissionStatus>,
    /// Durable single-submission coordinator identity.
    pub id: String,
    /// Original submitted story.
    pub story: String,
    /// Current phase, or invalid when retained evidence fails validation.
    pub phase: String,
    /// Original gate admission, not the most recent retry or effect.
    pub started_at: Option<String>,
    /// Wall time including all held and stopped intervals.
    pub elapsed_milliseconds: Option<u64>,
    /// Original immutable submitted commit.
    pub original_head: Option<String>,
    /// Exact proposed tree, absent before native assembly.
    pub assembled_tree: Option<String>,
    /// Claimed operation ordinal; never a certification or success count.
    pub effect_epoch: Option<u32>,
    /// Observed retained/absent/moved/unknown managed branch, independently of landing.
    pub retained_branch: Option<RetainedBranchObservation>,
    /// Concrete unresolved requirement.
    pub next_action: String,
}

pub(crate) fn status_snapshot(
    tx: &impl ReadOps,
    project: crate::store::ProjectId,
) -> Result<Vec<IntegrationRecoveryStatus>, StoreError> {
    let metadata = tx
        .project(project)?
        .ok_or_else(|| invalid("status project missing"))?;
    let prefix = &metadata.prefix;
    let attempts = tx.gate_attempts(project);
    let mut result = Vec::new();
    for record in tx.integration_recoveries(project)? {
        let state = match decode(&record) {
            Ok(state) => state,
            Err(error) => {
                result.push(IntegrationRecoveryStatus { retained_submission: None, id: record.id, story: record.story.to_id(&prefix), phase: "invalid".into(), started_at: None, elapsed_milliseconds: None, original_head: None, assembled_tree: None, effect_epoch: None, retained_branch: None, next_action: format!("Retained integration ownership is invalid; reconcile it without replaying effects: {error}") });
                continue;
            }
        };
        let (phase, next) = match state.phase {
            IntegrationPhase::Reserved => (
                "reserved",
                "Fresh native source and current operator authority are required before claiming assembly.",
            ),
            IntegrationPhase::Assembling => (
                "assembling",
                "The original assembly operation owns its private resources; after interruption reconcile native custody before any further effect.",
            ),
            IntegrationPhase::Assembled => (
                "assembled",
                "A separately claimed native publication must prove original inputs and exact assembled objects; no gate has certified this tree.",
            ),
            IntegrationPhase::Publishing => (
                "publishing",
                "Reconcile each possibly begun push/PR effect from native remote identity; intents are not success receipts and must not be replayed.",
            ),
            IntegrationPhase::Published => (
                "published",
                "The separate managed PR needs a fresh central gate on the exact assembled tree and proven native landing; the original PR remains unchanged.",
            ),
            IntegrationPhase::Gating => (
                "gating",
                "The central slot must finish and settle its exact managed gate; stored admission metadata grants no certificate or restart replay.",
            ),
            IntegrationPhase::Running => (
                "running",
                "The original central slot supervises progress and owns child settlement; native input metadata is not a passed gate or merge receipt.",
            ),
            IntegrationPhase::Certified => (
                "certified",
                "The exact managed tree has a native central certificate; a separately owned protected landing must still prove merge and original-head ancestry.",
            ),
            IntegrationPhase::Landing => (
                "landing",
                if state.landing_started && !state.landing_settled {
                    "Local effect settlement is unproved; retain registry, central admission hold and private assembly. Remote merge evidence cannot release this custody."
                } else {
                    "Observe the exact managed merge intent after any uncertainty; never repeat a request or mark the original PR merged from ancestry alone."
                },
            ),
            IntegrationPhase::Landed => (
                "landed",
                "Native managed landing completed the retained submission; original PR status is unchanged. Reconcile only exact owned residual resources.",
            ),
            IntegrationPhase::Held => (
                "held",
                "Resolve the recorded authority or semantic hold without replacing the original submission.",
            ),
        };
        let started = chrono::DateTime::parse_from_rfc3339(&state.started_at)
            .map_err(|e| StoreError::Corrupt(e.to_string()))?;
        let observed_at = if state.phase == IntegrationPhase::Landed {
            chrono::DateTime::parse_from_rfc3339(&state.updated_at)
                .map_err(|e| StoreError::Corrupt(e.to_string()))?
                .with_timezone(&chrono::Utc)
        } else {
            chrono::Utc::now()
        };
        result.push(IntegrationRecoveryStatus {
            retained_submission: Some(
                crate::service::gate_cost::view::RetainedSubmissionStatus::from_history(
                    &state.attribution.submission,
                    &metadata.slug,
                    prefix,
                    &attempts,
                ),
            ),
            id: record.id,
            story: state.candidate.story_id,
            phase: phase.into(),
            elapsed_milliseconds: Some(
                (observed_at - started.with_timezone(&chrono::Utc))
                    .num_milliseconds()
                    .max(0) as u64,
            ),
            started_at: Some(state.started_at),
            original_head: Some(state.plan.head),
            assembled_tree: state.assembly.map(|a| a.tree),
            effect_epoch: Some(state.effect_epoch),
            retained_branch: state.retained_branch,
            next_action: state.hold.unwrap_or_else(|| next.into()),
        });
    }
    Ok(result)
}

fn validate_branch_observation(
    state: &IntegrationOwner,
    observation: &RetainedBranchObservation,
) -> Result<(), StoreError> {
    let assembly = state
        .assembly
        .as_ref()
        .ok_or_else(|| invalid("branch diagnostic has no retained assembly identity"))?;
    if observation.version != 1
        || observation.owner != assembly.owner
        || observation.assembly_epoch != assembly.epoch
        || observation.repository != assembly.submission.repository
        || observation.reference != format!("refs/heads/{}", assembly.branch)
        || observation.expected_head != assembly.commit
    {
        return Err(invalid(
            "branch observation belongs to another owner, epoch, origin, reference or tip",
        ));
    }
    chrono::DateTime::parse_from_rfc3339(&observation.observed_at)
        .map_err(|e| invalid(&e.to_string()))?;
    match &observation.outcome {
        RetainedBranchOutcome::MovedPreserved { observed_head }
            if !crate::service::project_fault::is_pinned_oid(observed_head)
                || observed_head == &observation.expected_head =>
        {
            return Err(invalid(
                "moved branch diagnostic lacks a distinct pinned tip",
            ));
        }
        RetainedBranchOutcome::Unknown { detail }
            if detail.trim().is_empty() || detail.len() > 8192 =>
        {
            return Err(invalid("unknown branch diagnostic lacks a bounded reason"));
        }
        _ => {}
    }
    Ok(())
}
impl<S: Store> IntegrationOwnerService<'_, S> {
    /// Bind a read-only observation to the exact owner revision observed before
    /// native capture. It cannot complete/release an owner or alter any phase.
    pub(crate) fn record_branch_observation(
        &self,
        id: &str,
        expected: i64,
        observation: &RetainedBranchObservation,
    ) -> Result<bool, AppError> {
        self.ctx
            .store()
            .write(|tx| {
                let (mut record, mut state) = find(tx, self.ctx.project(), id)?;
                if record.revision != expected {
                    return Ok(false);
                }
                validate_branch_observation(&state, observation)?;
                state.retained_branch = Some(observation.clone());
                save(tx, &mut record, &state)?;
                Ok(true)
            })
            .map_err(Into::into)
    }
}
