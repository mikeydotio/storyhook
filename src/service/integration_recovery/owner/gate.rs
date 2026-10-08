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
    /// Absolute original operation lifetime and native private resource custody.
    pub fn validate_custody(&self) -> Result<(), AppError> {
        check_live(self.deadline, &self.cancellation)?;
        self.native.validate_custody()
    }
}

impl<'a, S: Store> IntegrationOwnerService<'a, S> {
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
            .read(|tx| {
                claim.validate_custody().map_err(StoreError::from)?;
                if claim.record.project != self.ctx.project() {
                    return Ok(false);
                }
                let (record, state) = find(tx, self.ctx.project(), claim.id())?;
                if record != claim.record
                    || state != claim.owner
                    || state.phase != IntegrationPhase::Gating
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
            })
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
        || attempt.verdict.is_some()
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
