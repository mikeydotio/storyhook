//! One exact managed landing owner. Ordinary story/batch controllers continue
//! to refuse this authority; a dedicated native adapter must retain the claim.
use super::*;
use crate::domain::landing::{IntegrationAuthority, IntegrationLanding, LandingAuthority};
use crate::store::LandingIntent;

/// Native managed merge custody, never reconstructed from a saved intent.
///
/// ```compile_fail
/// use storyhook::service::integration_recovery::IntegrationLandingClaim;
/// let _: IntegrationLandingClaim = serde_json::from_str("{}").unwrap();
/// ```
pub struct IntegrationLandingClaim {
    record: IntegrationRecovery,
    owner: IntegrationOwner,
    native: NativeAssembly,
    deadline: Instant,
    cancellation: Cancellation,
    request_taken: std::sync::atomic::AtomicBool,
}
impl IntegrationLandingClaim {
    /// Original integration coordinator.
    pub fn id(&self) -> &str {
        &self.record.id
    }
    /// Distinct merge operation ordinal.
    pub fn epoch(&self) -> u32 {
        self.owner.effect_epoch
    }
    /// Original author submission remains the story's identity.
    pub fn candidate(&self) -> &VerificationCandidate {
        &self.owner.candidate
    }
    /// Exact durable managed target and unique request marker identity.
    pub fn intent(&self) -> &LandingIntent {
        self.owner.landing.as_ref().expect("landing constructor")
    }
    /// Retained native private objects.
    pub fn assembly(&self) -> &AssemblyEvidence {
        self.native.evidence()
    }
    /// Observed separate PR, not a merged claim.
    pub fn publication(&self) -> &PublicationEvidence {
        self.owner
            .publication
            .as_ref()
            .expect("landing constructor")
    }
    /// Exact central certificate and original ancestry inputs.
    pub fn certification(&self) -> &gate::IntegrationCertificationEvidence {
        self.owner.gate.as_ref().expect("landing constructor")
    }
    /// The native adapter consumes this latch once after the durable intent.
    /// An error never renews it, even when no child was demonstrably launched.
    pub(crate) fn take_request(&self) -> Result<bool, AppError> {
        self.validate_custody()?;
        if !self.owner.landing_started {
            return Err(invalid("managed merge request lacks its durable one-shot intent").into());
        }
        Ok(self
            .request_taken
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_ok())
    }
    /// Original bounded merge operation and retained private custody.
    pub fn validate_custody(&self) -> Result<(), AppError> {
        if self.cancellation.is_cancelled() || Instant::now() >= self.deadline {
            return Err(invalid("original managed landing operation expired or cancelled").into());
        }
        self.native.validate_custody()?;
        if self.cancellation.is_cancelled() || Instant::now() >= self.deadline {
            return Err(invalid(
                "original managed landing operation expired during custody validation",
            )
            .into());
        }
        Ok(())
    }
    /// Release only original still-open private assembly custody after the
    /// central worker has settled every effect. Historical receipts cannot call it.
    pub(crate) fn settle_assembly(self) -> Result<(), AppError> {
        self.native.settle()
    }
    pub(crate) fn operation_lifetime(&self) -> (Instant, &Cancellation) {
        (self.deadline, &self.cancellation)
    }
}

impl<'a, S: Store> IntegrationOwnerService<'a, S> {
    /// Claim a distinct protected merge only after a new native original input
    /// inspection. The adapter must additionally refresh the managed target and
    /// certificate at the actual merge boundary; this intent is not a request.
    pub(crate) fn claim_landing(
        &self,
        ready: CertifiedIntegration,
        proof: &BoundIntegrationProposal,
    ) -> Result<Option<IntegrationLandingClaim>, AppError> {
        proof.check_live()?;
        ready.validate_custody()?;
        let now = self.ctx.now();
        let claimed = self.ctx.store().write(|tx| {
            proof.check_live().map_err(StoreError::from)?;
            ready.validate_custody().map_err(StoreError::from)?;
            let (mut record, mut state) = find(tx, self.ctx.project(), ready.id())?;
            if record != ready.record
                || state != ready.owner
                || state.phase != IntegrationPhase::Certified
            {
                return Ok(None);
            }
            check_authority(tx, &record, &state, proof)?;
            if tx
                .landing_intents()?
                .iter()
                .any(|intent| intent.project == record.project && intent.story == record.story)
            {
                return Err(invalid("another landing already owns original story"));
            }
            state.effect_epoch = state
                .effect_epoch
                .checked_add(1)
                .ok_or_else(|| invalid("landing epoch overflow"))?;
            let certificate = state
                .gate
                .as_ref()
                .ok_or_else(|| invalid("native certificate missing"))?;
            let publication = state
                .publication
                .as_ref()
                .ok_or_else(|| invalid("native publication missing"))?;
            let attempt = uuid::Uuid::new_v4().to_string();
            let intent = LandingIntent {
                id: attempt.clone(),
                project: record.project,
                story: record.story,
                story_id: state.candidate.story_id.clone(),
                project_slug: state.candidate.project_slug.clone(),
                generation: record.generation,
                pull_request: state.submission.pull_request.clone(),
                checkout: state.candidate.checkout.clone(),
                created_at: now.clone(),
                batch: None,
                certification: LandingAuthority::Integration(IntegrationAuthority {
                    integration: IntegrationLanding {
                        version: 1,
                        owner: record.id.clone(),
                        epoch: state.effect_epoch,
                        attempt,
                        pull_request: publication.pull_request.clone(),
                        original_head: state.plan.head.clone(),
                        pinned_base: state.plan.base.clone(),
                        base: certificate.inputs.current_base.clone(),
                        certification: certificate.certification.clone(),
                    },
                }),
            };
            state.phase = IntegrationPhase::Landing;
            state.effect_started_at = Some(now.clone());
            state.updated_at = now.clone();
            state.landing = Some(intent.clone());
            save(tx, &mut record, &state)?;
            crate::service::landing::admit_intent(tx, &intent)?;
            proof.check_live().map_err(StoreError::from)?;
            ready.validate_custody().map_err(StoreError::from)?;
            Ok(Some((record, state)))
        })?;
        Ok(claimed.map(|(record, owner)| IntegrationLandingClaim {
            record,
            owner,
            native: ready.native,
            deadline: proof.deadline,
            cancellation: proof.cancellation.clone(),
            request_taken: std::sync::atomic::AtomicBool::new(false),
        }))
    }
    /// Durable one-shot intent immediately before the protected merge adapter.
    /// A timeout, spawn failure or lost reply remains potentially begun until a
    /// dedicated native reconciliation proves its disposition; never retry it.
    pub(crate) fn claim_landing_effect(
        &self,
        claim: &mut IntegrationLandingClaim,
    ) -> Result<bool, AppError> {
        claim.validate_custody()?;
        let now = self.ctx.now();
        let changed = self.ctx.store().write(|tx| {
            if !permitted(tx, self.ctx.project(), claim)? || claim.owner.landing_started {
                return Ok(None);
            }
            let mut record = claim.record.clone();
            let mut state = claim.owner.clone();
            state.landing_started = true;
            state.updated_at = now.clone();
            save(tx, &mut record, &state)?;
            claim.validate_custody().map_err(StoreError::from)?;
            Ok(Some((record, state)))
        })?;
        if let Some((record, owner)) = changed {
            claim.record = record;
            claim.owner = owner;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Record native local settlement independently of remote landing. This
    /// cannot renew a request and remains recordable after cancellation.
    pub(crate) fn record_landing_settlement(
        &self,
        claim: &mut IntegrationLandingClaim,
        outcome: &crate::daemon::verification::ManagedLandingOutcome,
    ) -> Result<bool, AppError> {
        if !outcome.proves_settlement(claim) {
            return Ok(false);
        }
        let changed = self.ctx.store().write(|tx| {
            let (mut record, mut state) = find(tx, self.ctx.project(), claim.id())?;
            if record != claim.record
                || state != claim.owner
                || state.phase != IntegrationPhase::Landing
                || !state.landing_started
                || !tx.landing_intents()?.contains(claim.intent())
            {
                return Ok(None);
            }
            state.landing_settled = true;
            save(tx, &mut record, &state)?;
            Ok(Some((record, state)))
        })?;
        if let Some((record, owner)) = changed {
            claim.record = record;
            claim.owner = owner;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Before any new native merge effect, recheck the exact opaque/durable
    /// owner and original revocation fences. No JSON intent grants a retry.
    pub(crate) fn landing_permitted(
        &self,
        claim: &IntegrationLandingClaim,
    ) -> Result<bool, AppError> {
        claim.validate_custody()?;
        self.ctx
            .store()
            .read(|tx| permitted(tx, self.ctx.project(), claim))
            .map_err(Into::into)
    }
}

fn permitted(
    tx: &impl ReadOps,
    project: crate::store::ProjectId,
    claim: &IntegrationLandingClaim,
) -> Result<bool, StoreError> {
    claim.validate_custody().map_err(StoreError::from)?;
    if claim.record.project != project {
        return Ok(false);
    }
    let (record, state) = find(tx, project, claim.id())?;
    if record != claim.record || state != claim.owner || state.phase != IntegrationPhase::Landing {
        return Ok(false);
    }
    check_retained_authority(tx, &record, &state)?;
    crate::store::landing::validate_intent(tx, claim.intent())?;
    if !tx.landing_intents()?.contains(claim.intent()) {
        return Ok(false);
    }
    claim.validate_custody().map_err(StoreError::from)?;
    Ok(true)
}

pub(super) fn validate_state(
    state: &IntegrationOwner,
    record: &IntegrationRecovery,
) -> Result<(), StoreError> {
    if matches!(
        state.phase,
        IntegrationPhase::Landing | IntegrationPhase::Landed
    ) != state.landing.is_some()
        || (!matches!(
            state.phase,
            IntegrationPhase::Landing | IntegrationPhase::Landed
        ) && state.landing_started)
        || (state.landing_settled && !state.landing_started)
    {
        return Err(invalid(
            "integration landing phase has inconsistent intent custody",
        ));
    }
    if let Some(intent) = &state.landing {
        let binding = intent
            .certification
            .integration()
            .ok_or_else(|| invalid("managed landing lost strict integration envelope"))?;
        let certificate = state
            .gate
            .as_ref()
            .ok_or_else(|| invalid("managed landing lacks central certificate"))?;
        let publication = state
            .publication
            .as_ref()
            .ok_or_else(|| invalid("managed landing lacks exact native publication"))?;
        intent.certification.validate().map_err(StoreError::from)?;
        if intent.batch.is_some()
            || intent.id != binding.attempt
            || intent.project != record.project
            || intent.story != record.story
            || intent.generation != record.generation
            || intent.story_id != state.candidate.story_id
            || intent.project_slug != state.candidate.project_slug
            || intent.checkout != state.candidate.checkout
            || intent.pull_request != state.submission.pull_request
            || binding.owner != record.id
            || binding.epoch != state.effect_epoch
            || binding.pull_request != publication.pull_request
            || binding.original_head != state.plan.head
            || binding.pinned_base != state.plan.base
            || binding.base != certificate.inputs.current_base
            || binding.certification != certificate.certification
        {
            return Err(invalid(
                "managed landing differs from immutable original owner, native PR or certified tree",
            ));
        }
    }
    if let Some(evidence) = &state.landed {
        validate_landed(state, record, evidence)?;
        if !state.landing_started || !state.landing_settled {
            return Err(invalid("landed owner lacks proven local effect settlement"));
        }
    }
    Ok(())
}

// Store final validation protects the retained intent, including raw imports.
// Do not apply admission-only operator/host pauses here: stopping must remain
// possible while an already begun remote merge is uncertain and fenced.
pub(crate) fn validate_intent(tx: &impl ReadOps, intent: &LandingIntent) -> Result<(), StoreError> {
    let binding = intent
        .certification
        .integration()
        .ok_or_else(|| invalid("managed landing envelope missing"))?;
    let (record, state) = find(tx, intent.project, &binding.owner)?;
    if !record.active
        || state.phase != IntegrationPhase::Landing
        || state.landing.as_ref() != Some(intent)
    {
        return Err(invalid("managed landing has no exact active durable owner"));
    }
    validate_state(&state, &record)
}

/// A fresh read-only query of an exact retained merge. It deliberately owns no
/// old assembly file descriptors and cannot authorize mutation, retry or cleanup.
/// Native observation must fetch and prove actual remote objects in a new private
/// namespace; historical assembly JSON is only the expected immutable identity.
///
/// ```compile_fail
/// use storyhook::service::integration_recovery::IntegrationLandingObservation;
/// let _: IntegrationLandingObservation = serde_json::from_str("{}").unwrap();
/// ```
pub struct IntegrationLandingObservation {
    record: IntegrationRecovery,
    owner: IntegrationOwner,
    deadline: Instant,
    cancellation: Cancellation,
}
impl IntegrationLandingObservation {
    /// Original retained coordinator.
    pub fn id(&self) -> &str {
        &self.record.id
    }
    /// Original author submission, never rewritten to the managed PR.
    pub fn candidate(&self) -> &VerificationCandidate {
        &self.owner.candidate
    }
    /// Exact managed merge intent and request marker.
    pub fn intent(&self) -> &LandingIntent {
        self.owner
            .landing
            .as_ref()
            .expect("observation constructor")
    }
    /// Historical expected Git identities only, not permission to use this path.
    pub fn assembly(&self) -> &AssemblyEvidence {
        self.owner
            .assembly
            .as_ref()
            .expect("observation constructor")
    }
    /// Managed and original PR identities to prove from the actual origin.
    pub fn publication(&self) -> &PublicationEvidence {
        self.owner
            .publication
            .as_ref()
            .expect("observation constructor")
    }
    /// The retained exact central certificate to compare with actual landed Git.
    pub fn certification(&self) -> &gate::IntegrationCertificationEvidence {
        self.owner.gate.as_ref().expect("observation constructor")
    }
    /// A new bounded read-only observation, never renewal of the old merge call.
    pub fn validate_lifetime(&self) -> Result<(), AppError> {
        observation_live(self.deadline, &self.cancellation)
    }
}
impl<'a, S: Store> IntegrationOwnerService<'a, S> {
    /// Reopen only an observation boundary after crash/expiry. Persisted native
    /// receipts never recreate the original mutation or filesystem capability.
    pub(crate) fn observe_landing(
        &self,
        id: &str,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<IntegrationLandingObservation, AppError> {
        observation_live(deadline, cancellation)?;
        let (record, owner) = self.ctx.store().read(|tx| {
            observation_live(deadline, cancellation).map_err(StoreError::from)?;
            let (record, owner) = find(tx, self.ctx.project(), id)?;
            if !owner.landing_settled { return Err(invalid("managed landing local effect settlement is unproved; retained owner blocks restart observation and completion")); }
            let intent = owner
                .landing
                .as_ref()
                .ok_or_else(|| invalid("no managed landing to observe"))?;
            crate::store::landing::validate_intent(tx, intent)?;
            if !tx.landing_intents()?.contains(intent) {
                return Err(invalid(
                    "managed landing observation lost exact pending intent",
                ));
            }
            observation_live(deadline, cancellation).map_err(StoreError::from)?;
            Ok((record, owner))
        })?;
        Ok(IntegrationLandingObservation {
            record,
            owner,
            deadline,
            cancellation: cancellation.clone(),
        })
    }
    /// Observations remain available after an operator stop. This confers no
    /// new effect or completion: the latter must recheck its own human/resource
    /// fences against native proven landing in the final transaction.
    pub(crate) fn landing_observation_permitted(
        &self,
        query: &IntegrationLandingObservation,
    ) -> Result<bool, AppError> {
        query.validate_lifetime()?;
        self.ctx
            .store()
            .read(|tx| {
                query.validate_lifetime().map_err(StoreError::from)?;
                if query.record.project != self.ctx.project() {
                    return Ok(false);
                }
                let (record, state) = find(tx, self.ctx.project(), query.id())?;
                if record != query.record || state != query.owner || !state.landing_settled {
                    return Ok(false);
                }
                crate::store::landing::validate_intent(tx, query.intent())?;
                if !tx.landing_intents()?.contains(query.intent()) {
                    return Ok(false);
                }
                query.validate_lifetime().map_err(StoreError::from)?;
                Ok(true)
            })
            .map_err(Into::into)
    }
}
fn observation_live(deadline: Instant, cancellation: &Cancellation) -> Result<(), AppError> {
    if cancellation.is_cancelled() || Instant::now() >= deadline {
        Err(invalid("managed landing observation expired or cancelled").into())
    } else {
        Ok(())
    }
}

// Observation-only validation for retained receipts. Only the opaque native
// factory can supply a live proof to the completion door below.
fn validate_landed(
    state: &IntegrationOwner,
    record: &IntegrationRecovery,
    evidence: &IntegrationLandedEvidence,
) -> Result<(), StoreError> {
    let intent = state
        .landing
        .as_ref()
        .ok_or_else(|| invalid("landed receipt lacks original intent"))?;
    let publication = state
        .publication
        .as_ref()
        .ok_or_else(|| invalid("landed receipt lacks publication"))?;
    let certificate = state
        .gate
        .as_ref()
        .ok_or_else(|| invalid("landed receipt lacks actual certificate"))?;
    let oid = |s: &str| matches!(s.len(), 40 | 64) && s.bytes().all(|c| c.is_ascii_hexdigit());
    if evidence.version != 1
        || evidence.owner != record.id
        || evidence.intent_id != intent.id
        || evidence.repository != state.submission.repository
        || evidence.original_pr != state.submission.pull_request
        || evidence.original_head != state.plan.head
        || evidence.managed_pr != publication.pull_request
        || evidence.managed_head != publication.commit
        || evidence.merge_tree != certificate.certification.tree
        || evidence.base_branch != state.submission.base_branch
        || ![
            &evidence.merge_commit,
            &evidence.merge_tree,
            &evidence.observed_base,
            &evidence.observed_base_tree,
        ]
        .iter()
        .all(|v| oid(v))
    {
        return Err(invalid(
            "native landing differs from exact owner, original ancestry inputs, managed PR or certified tree",
        ));
    }
    Ok(())
}

impl<'a, S: Store> IntegrationOwnerService<'a, S> {
    /// Accept only a fresh, opaque native Git/remote proof. The caller retains
    /// the proof and explicitly settles its newly owned private repository after
    /// this transaction, including on refusal. Stopping new work does not make
    /// an already landed fact unrecordable; human and resource replacement do.
    pub(crate) fn complete_landing(
        &self,
        native: &NativeIntegrationLanded,
    ) -> Result<bool, AppError> {
        native.validate_lifetime()?;
        let query = native.query();
        if query.record.project != self.ctx.project() {
            return Err(invalid("landing proof belongs to another project").into());
        }
        let now = self.ctx.now();
        self.ctx.write_stories(|tx|{
            native.validate_lifetime().map_err(StoreError::from)?;
            let (mut record,mut state)=find(tx,self.ctx.project(),query.id())?;
            if record!=query.record || state!=query.owner || !record.active
                || state.phase!=IntegrationPhase::Landing || !state.landing_started || !state.landing_settled {
                return Ok(false);
            }
            crate::store::landing::validate_intent(tx,query.intent())?;
            if !tx.landing_intents()?.contains(query.intent()){return Ok(false);}
            let row=tx.story(record.project,record.story)?.ok_or_else(||invalid("landing story disappeared"))?;
            if crate::domain::is_human_only(&row.snapshot)
                || !crate::service::verification::human::permits(tx,&state.candidate)?
                || !crate::service::verification::submission_is_current(tx,&row,&state.candidate)?
                || !crate::service::verification::recovery_cleanup_history_is_current(tx,&state.candidate)?
                || crate::service::project_recovery::recovery_resource_hold(tx,record.project,record.story)? {
                return Ok(false);
            }
            validate_landed(&state,&record,native.evidence())?;
            let attributions=tx.attributions(record.project)?;
            if !attributions.contains(&state.attribution) || state.attribution.has_unsettled_diagnosis()
                || state.attribution.components.len()!=1 || attributions.iter().any(|a| a.id!=state.attribution.id && a.submission==state.attribution.submission && a.held && a.retired.is_none()) {
                return Err(invalid("unrelated or changed attribution remains held; native integration landing cannot erase it"));
            }
            let mut retired=state.attribution.clone();
            retired.revision+=1;
            retired.held=false;
            retired.retired=Some(format!("integration {} landed exact certified tree {} through managed PR {}",record.id,native.evidence().merge_tree,native.evidence().managed_pr));
            if !tx.update_attribution(&retired,state.attribution.revision)?{return Err(invalid("attribution changed before native landing completion"));}
            crate::service::landing::complete_integration_story(tx,self.ctx,native)?;
            state.phase=IntegrationPhase::Landed;
            state.updated_at=now.clone();
            state.landed=Some(native.evidence().clone());
            record.active=false;
            save(tx,&mut record,&state)?;
            native.validate_lifetime().map_err(StoreError::from)?;
            Ok(true)
        }).map_err(Into::into)
    }
}

/// Persisted local uncertainty fences every new central admission after restart.
/// A remote merged fact alone can never clear this custody hold.
pub(crate) fn local_effect_unsettled(
    tx: &impl ReadOps,
    project: crate::store::ProjectId,
) -> Result<bool, StoreError> {
    for record in tx.integration_recoveries(project)? {
        if !record.active {
            continue;
        }
        let state = decode(&record)?;
        if state.phase == IntegrationPhase::Landing
            && state.landing_started
            && !state.landing_settled
        {
            return Ok(true);
        }
    }
    Ok(false)
}
