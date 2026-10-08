//! Separate managed gate ownership. The serialized admission is observation;
//! only the live claim can cross the native gate boundary. The daemon must keep
//! its original project VerificationGuard through process settlement.
use super::*;

/// A single central gate operation retaining original native assembly custody.
/// It cannot be rebuilt from a persisted admission or public certification JSON.
///
/// ```compile_fail
/// use storyhook::service::integration_recovery::IntegrationGateClaim;
/// let _: IntegrationGateClaim = serde_json::from_str("{}").unwrap();
/// ```
pub struct IntegrationGateClaim {
    pub(super) record: IntegrationRecovery,
    pub(super) owner: IntegrationOwner,
    pub(super) native: NativeAssembly,
    deadline: Instant,
    cancellation: Cancellation,
}
impl IntegrationGateClaim {
    /// Durable recovery owner, independent of a batch or author PR.
    pub fn id(&self) -> &str {
        &self.record.id
    }
    /// Fresh central admission under the original immutable submission.
    pub fn attempt(&self) -> &str {
        self.owner
            .gate_attempt
            .as_deref()
            .expect("gate constructor")
    }
    /// Original candidate is never replaced with a synthetic managed candidate.
    pub fn candidate(&self) -> &VerificationCandidate {
        &self.owner.candidate
    }
    /// Managed PR/head/tree; observation alone cannot certify them.
    pub fn publication(&self) -> &PublicationEvidence {
        self.owner.publication.as_ref().expect("gate constructor")
    }
    /// The central native runner must retain this exact cancellation owner.
    pub(crate) fn owns_cancellation(&self, cancellation: &Cancellation) -> bool {
        self.cancellation.same_owner(cancellation)
    }
    /// Fresh native inputs retained when the progress-supervised gate began.
    pub(crate) fn inputs(&self) -> Option<&IntegrationGateInputsEvidence> {
        self.owner.gate_inputs.as_ref()
    }
    /// Exact original private objects retained through the central gate.
    pub fn assembly(&self) -> &AssemblyEvidence {
        self.native.evidence()
    }
    /// Preflight has one absolute deadline. Once fresh native inputs start the
    /// central gate, its existing progress/idle supervisor owns timing; the
    /// original cancellation and private resource custody remain immutable.
    pub fn validate_custody(&self) -> Result<(), AppError> {
        if self.owner.phase == IntegrationPhase::Gating {
            check_live(self.deadline, &self.cancellation)?;
        } else if self.cancellation.is_cancelled() {
            return Err(invalid("running integration gate owner cancelled").into());
        }
        self.native.validate_custody()
    }
}

impl<'a, S: Store> IntegrationOwnerService<'a, S> {
    /// Start only from a fresh opaque native input observation, consumed while
    /// the original preflight operation remains live even after store admission.
    /// This is the explicit handoff to existing central progress supervision,
    /// not a renewed deadline or a certification claim.
    pub(crate) fn start_gate(
        &self,
        mut claim: IntegrationGateClaim,
        native: NativeIntegrationGateInputs,
    ) -> Result<Option<IntegrationGateClaim>, AppError> {
        native.validate_for(&claim)?;
        let now = self.ctx.now();
        let started = self.ctx.store().write(|tx| {
            native.validate_for(&claim).map_err(StoreError::from)?;
            if !permitted(tx, self.ctx.project(), &claim)?
                || claim.owner.phase != IntegrationPhase::Gating
            {
                return Ok(None);
            }
            let mut record = claim.record.clone();
            let mut state = claim.owner.clone();
            state.gate_inputs = Some(native.evidence().clone());
            state.phase = IntegrationPhase::Running;
            state.updated_at = now.clone();
            save(tx, &mut record, &state)?;
            native.validate_for(&claim).map_err(StoreError::from)?;
            Ok(Some((record, state)))
        })?;
        Ok(started.map(|(record, owner)| {
            claim.record = record;
            claim.owner = owner;
            claim
        }))
    }

    /// Claim after acquiring the existing central project slot and its fresh
    /// gated admission. Accounting is an extra fence, never a substitute for
    /// that live guard. Before executing, the native gate adapter must refresh
    /// the exact managed PR/head/current base and original immutable inputs.
    pub(crate) fn claim_gate(
        &self,
        ready: PublishedIntegration,
        proof: &BoundIntegrationProposal,
        attempt: &str,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<Option<IntegrationGateClaim>, AppError> {
        proof.check_live()?;
        check_live(deadline, cancellation)?;
        ready.validate_custody()?;
        let now = self.ctx.now();
        let claimed = self.ctx.store().write(|tx| {
            proof.check_live().map_err(StoreError::from)?;
            check_live(deadline, cancellation).map_err(StoreError::from)?;
            ready.validate_custody().map_err(StoreError::from)?;
            let (mut record, mut state) = find(tx, self.ctx.project(), ready.id())?;
            if record != ready.record
                || state != ready.owner
                || state.phase != IntegrationPhase::Published
            {
                return Ok(None);
            }
            check_authority(tx, &record, &state, proof)?;
            settled_attempt(tx, &state)?;
            live_admission(tx, &state, attempt)?;
            state.phase = IntegrationPhase::Gating;
            state.effect_epoch = state
                .effect_epoch
                .checked_add(1)
                .ok_or_else(|| invalid("gate epoch overflow"))?;
            state.effect_started_at = Some(now.clone());
            state.updated_at = now.clone();
            state.gate_attempt = Some(attempt.into());
            save(tx, &mut record, &state)?;
            ready.validate_custody().map_err(StoreError::from)?;
            proof.check_live().map_err(StoreError::from)?;
            check_live(deadline, cancellation).map_err(StoreError::from)?;
            Ok(Some((record, state)))
        })?;
        Ok(claimed.map(|(record, owner)| IntegrationGateClaim {
            record,
            owner,
            native: ready.native,
            deadline,
            cancellation: cancellation.clone(),
        }))
    }

    /// Recheck before and during the owned process. A fresh observation cannot
    /// replace the claim's original cancellation token or extend its deadline.
    pub(crate) fn gate_permitted(&self, claim: &IntegrationGateClaim) -> Result<bool, AppError> {
        claim.validate_custody()?;
        self.ctx
            .store()
            .read(|tx| permitted(tx, self.ctx.project(), claim))
            .map_err(Into::into)
    }
}

fn check_live(deadline: Instant, cancellation: &Cancellation) -> Result<(), AppError> {
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        Err(invalid("original integration gate expired or was cancelled").into())
    } else {
        Ok(())
    }
}

fn live_admission(tx: &impl ReadOps, state: &IntegrationOwner, id: &str) -> Result<(), StoreError> {
    let attempts = tx.gate_attempts(state.candidate.project)?;
    let attempt = attempts
        .iter()
        .find(|attempt| attempt.id == id)
        .ok_or_else(|| invalid("central integration admission is missing"))?;
    if id == state.attribution.attempt
        || attempt.submission != state.attribution.submission
        || attempt.mode != crate::domain::landing::VerificationMode::Gated
        || attempt.control_revision != Some(state.control_revision)
        || attempt.finished_at.is_some()
        || (state.phase != IntegrationPhase::Running && attempt.verdict.is_some())
        || attempt.elapsed.estimated
        || attempts.iter().any(|other| {
            other.id != id && other.submission == attempt.submission && other.finished_at.is_none()
        })
    {
        return Err(invalid(
            "integration gate requires one live gated admission of the original submission and control epoch",
        ));
    }
    Ok(())
}

fn permitted(
    tx: &impl ReadOps,
    project: crate::store::ProjectId,
    claim: &IntegrationGateClaim,
) -> Result<bool, StoreError> {
    claim.validate_custody().map_err(StoreError::from)?;
    if claim.record.project != project {
        return Ok(false);
    }
    let (record, state) = find(tx, project, claim.id())?;
    if record != claim.record
        || state != claim.owner
        || !matches!(
            state.phase,
            IntegrationPhase::Gating | IntegrationPhase::Running
        )
    {
        return Ok(false);
    }
    check_retained_authority(tx, &record, &state)?;
    live_admission(tx, &state, claim.attempt())?;
    if state.assembly.as_ref() != Some(claim.native.evidence()) {
        return Err(invalid("gate lost native assembly"));
    }
    claim.validate_custody().map_err(StoreError::from)?;
    Ok(true)
}

pub(super) fn validate_inputs(
    state: &IntegrationOwner,
    record: &IntegrationRecovery,
) -> Result<(), StoreError> {
    if matches!(
        state.phase,
        IntegrationPhase::Running | IntegrationPhase::Certified | IntegrationPhase::Landing
    ) != state.gate_inputs.is_some()
    {
        return Err(invalid("integration gate phase lacks exact native inputs"));
    }
    if matches!(
        state.phase,
        IntegrationPhase::Certified | IntegrationPhase::Landing
    ) != state.gate.is_some()
    {
        return Err(invalid(
            "integration certification phase lacks exact native gate result",
        ));
    }
    if let Some(gate) = &state.gate {
        gate.certification.validate().map_err(StoreError::from)?;
        if gate.version != 1
            || gate.execution.is_empty()
            || Some(gate.attempt.as_str()) != state.gate_attempt.as_deref()
            || gate.owner != record.id
            || Some(&gate.inputs) != state.gate_inputs.as_ref()
            || gate.certification.head != gate.inputs.publication.commit
            || gate.certification.tree != gate.inputs.tree
        {
            return Err(invalid(
                "integration certificate differs from original owner or native inputs",
            ));
        }
    }
    if let Some(inputs) = &state.gate_inputs {
        let assembly = state
            .assembly
            .as_ref()
            .ok_or_else(|| invalid("native gate inputs lack assembly"))?;
        if inputs.version != 1
            || inputs.owner != record.id
            || Some(inputs.attempt.as_str()) != state.gate_attempt.as_deref()
            || Some(&inputs.publication) != state.publication.as_ref()
            || inputs.current_base != state.plan.base
            || inputs.base_branch != state.submission.base_branch
            || inputs.tree != assembly.tree
            || inputs.policy != state.plan.policy
            || inputs.parents != [state.plan.base.clone(), state.plan.head.clone()]
        {
            return Err(invalid(
                "native gate inputs differ from original owner, policy or exact resolution tree",
            ));
        }
    }
    Ok(())
}

/// Retained certificate metadata, never authority reconstructed from JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationCertificationEvidence {
    /// Certificate envelope version.
    pub version: u8,
    /// Original integration owner.
    pub owner: String,
    /// Original central gate admission.
    pub attempt: String,
    /// Actual physical execution, not another attempt's passing tree.
    pub execution: String,
    /// Fresh native input binding, including original ancestry and policy.
    pub inputs: IntegrationGateInputsEvidence,
    /// Exact result returned by the owned central runner.
    pub certification: crate::domain::landing::VerifiedSubmission,
}

/// Successful managed gate with native custody; no merge has been requested.
///
/// ```compile_fail
/// use storyhook::service::integration_recovery::CertifiedIntegration;
/// let _: CertifiedIntegration = serde_json::from_str("{}").unwrap();
/// ```
pub struct CertifiedIntegration {
    pub(super) record: IntegrationRecovery,
    pub(super) owner: IntegrationOwner,
    pub(super) native: NativeAssembly,
}
impl CertifiedIntegration {
    /// Exact durable owner identity.
    pub fn id(&self) -> &str {
        &self.record.id
    }
    /// Observational result, not a landed receipt.
    pub fn evidence(&self) -> &IntegrationCertificationEvidence {
        self.owner.gate.as_ref().expect("certified constructor")
    }
    /// Still-owned native private resources.
    pub fn validate_custody(&self) -> Result<(), AppError> {
        self.native.validate_custody()
    }
}

impl<'a, S: Store> IntegrationOwnerService<'a, S> {
    /// Only the central native result wrapper constructs this capability. Store
    /// accounting is corroboration and cannot substitute for the opaque result.
    pub(crate) fn accept_gate(
        &self,
        claim: IntegrationGateClaim,
        native: crate::daemon::verification::integration_gate::NativeIntegrationCertification,
    ) -> Result<Option<CertifiedIntegration>, AppError> {
        native.validate_for(&claim)?;
        let now = self.ctx.now();
        let accepted = self.ctx.store().write(|tx| {
            native.validate_for(&claim).map_err(StoreError::from)?;
            if !permitted(tx, self.ctx.project(), &claim)? || claim.owner.phase != IntegrationPhase::Running { return Ok(None); }
            let attempt = tx.gate_attempts(self.ctx.project())?.into_iter().find(|a| a.id == claim.attempt()).ok_or_else(|| invalid("certified admission missing"))?;
            let execution = attempt.executions.iter().find(|e| e.id == native.execution).ok_or_else(|| invalid("certified physical execution missing"))?;
            if execution.purpose != crate::store::GateExecutionPurpose::Gate
                || execution.finished_at.is_none() || execution.estimated || !execution.journal_bound
                || execution.verdict.as_deref() != Some("certified")
                || execution.submissions != [claim.owner.attribution.submission.clone()]
                || execution.inputs.head.as_deref() != Some(native.certification.head.as_str())
                || execution.inputs.tree.as_deref() != Some(native.certification.tree.as_str())
                || execution.inputs.base.as_deref() != Some(native.inputs().current_base.as_str())
            { return Err(invalid("physical managed gate was not exactly certified and settled under the original admission")); }
            let mut record = claim.record.clone();
            let mut state = claim.owner.clone();
            state.gate = Some(IntegrationCertificationEvidence { version: 1, owner: record.id.clone(), attempt: claim.attempt().into(), execution: native.execution.clone(), inputs: native.inputs().clone(), certification: native.certification.clone() });
            state.phase = IntegrationPhase::Certified;
            state.updated_at = now.clone();
            save(tx, &mut record, &state)?;
            native.validate_for(&claim).map_err(StoreError::from)?;
            Ok(Some((record, state)))
        })?;
        Ok(accepted.map(|(record, owner)| CertifiedIntegration {
            record,
            owner,
            native: claim.native,
        }))
    }
}
