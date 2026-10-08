//! Native custody crosses phases only through opaque values. Durable remote
//! intents precede effects and never imply success or authorize restart replay.
use super::*;

/// A recorded native assembly whose original open filesystem custody survives.
/// Serialized AssemblyEvidence cannot reconstruct this value after a crash.
///
/// ```compile_fail
/// use storyhook::service::integration_recovery::AssembledIntegration;
/// let _: AssembledIntegration = serde_json::from_str("{}").unwrap();
/// ```
pub struct AssembledIntegration {
    record: IntegrationRecovery,
    owner: IntegrationOwner,
    native: NativeAssembly,
}

/// A distinct owner for publication, never a batch or author-branch claim.
/// Keep this value and the central worker slot alive through all child drain.
///
/// ```compile_fail
/// use storyhook::service::integration_recovery::PublicationClaim;
/// let _: PublicationClaim = serde_json::from_str("{}").unwrap();
/// ```
pub struct PublicationClaim {
    record: IntegrationRecovery,
    owner: IntegrationOwner,
    native: NativeAssembly,
    deadline: Instant,
    cancellation: Cancellation,
}

/// Native published custody without gate or landing authority. Persisted
/// receipts require separate native reconciliation after restart.
///
/// ```compile_fail
/// use storyhook::service::integration_recovery::PublishedIntegration;
/// let _: PublishedIntegration = serde_json::from_str("{}").unwrap();
/// ```
pub struct PublishedIntegration {
    record: IntegrationRecovery,
    owner: IntegrationOwner,
    native: NativeAssembly,
}
impl PublishedIntegration {
    /// Durable original owner identity.
    pub fn id(&self) -> &str {
        &self.record.id
    }
    /// Exact managed PR. Its existence proves no test result or merge.
    pub fn evidence(&self) -> &PublicationEvidence {
        self.owner
            .publication
            .as_ref()
            .expect("native published constructor")
    }
    /// Original native path/stamp custody, not remote freshness.
    pub fn validate_custody(&self) -> Result<(), AppError> {
        self.native.validate_custody()
    }
}

/// An intent records a potentially begun effect. It is not a remote receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PublicationEffect {
    /// Exactly one ordinary push of the managed branch, never force-push.
    PushBranch,
    /// Exactly one PR creation after native exact-branch observation.
    CreatePullRequest,
}

impl PublicationClaim {
    /// Durable integration owner identity.
    pub fn id(&self) -> &str {
        &self.record.id
    }
    /// Distinct publication operation ordinal.
    pub fn epoch(&self) -> u32 {
        self.owner.effect_epoch
    }
    /// Original submitted identity, never replaced by a synthetic candidate.
    pub fn candidate(&self) -> &VerificationCandidate {
        &self.owner.candidate
    }
    /// Exact original PR/repository/base/head used by current native inspection.
    pub fn submission(&self) -> &SubmissionObservation {
        &self.owner.submission
    }
    /// Native assembled commit/tree/parents and private resource evidence.
    pub fn assembly(&self) -> &AssemblyEvidence {
        self.native.evidence()
    }
    /// Already recorded potentially begun effects; never replay these.
    pub fn effects(&self) -> &[PublicationEffect] {
        &self.owner.publication_effects
    }
    /// Live path/stamp custody only. The adapter must additionally inspect exact
    /// Git objects and native remote metadata immediately before publication.
    pub fn validate_custody(&self) -> Result<(), AppError> {
        if self.cancellation.is_cancelled() || Instant::now() >= self.deadline {
            return Err(invalid("original publication operation expired or was cancelled").into());
        }
        self.native.validate_custody()
    }
}

impl<'a, S: Store> IntegrationOwnerService<'a, S> {
    /// Consume successful native publication while its exact operation is live.
    /// This changes no original PR, story, attribution, generation or head.
    pub fn accept_publication(
        &self,
        claim: PublicationClaim,
        native: NativePublication,
        proof: &BoundIntegrationProposal,
    ) -> Result<Option<PublishedIntegration>, AppError> {
        proof.check_live()?;
        claim.validate_custody()?;
        let now = self.ctx.now();
        let accepted = self.ctx.store().write(|tx| {
            if !check_claim(tx, self.ctx.project(), &claim, proof)? {
                return Ok(None);
            }
            if claim.owner.publication_effects
                != [
                    PublicationEffect::PushBranch,
                    PublicationEffect::CreatePullRequest,
                ]
            {
                return Err(invalid(
                    "native publication lacks both owned effect intents",
                ));
            }
            validate_publication(&claim.record, &claim.owner, native.evidence())?;
            let mut record = claim.record.clone();
            let mut state = claim.owner.clone();
            state.publication = Some(native.evidence().clone());
            state.phase = IntegrationPhase::Published;
            state.updated_at = now.clone();
            save(tx, &mut record, &state)?;
            claim.validate_custody().map_err(StoreError::from)?;
            proof.check_live().map_err(StoreError::from)?;
            Ok(Some((record, state)))
        })?;
        Ok(accepted.map(|(record, owner)| PublishedIntegration {
            record,
            owner,
            native: claim.native,
        }))
    }
    /// Record successful native assembly without releasing the original owner.
    /// A failed CAS loses no durable residue and cannot reopen assembly.
    pub fn accept_assembly(
        &self,
        claim: AssemblyClaim,
        native: NativeAssembly,
        proof: &BoundIntegrationProposal,
    ) -> Result<Option<AssembledIntegration>, AppError> {
        proof.check_live()?;
        native.validate_custody()?;
        let now = self.ctx.now();
        let accepted = self.ctx.store().write(|tx| {
            proof.check_live().map_err(StoreError::from)?;
            native.validate_custody().map_err(StoreError::from)?;
            let (mut record, mut state) = find(tx, self.ctx.project(), claim.id())?;
            if record != claim.record
                || state != claim.owner
                || state.phase != IntegrationPhase::Assembling
            {
                return Ok(None);
            }
            check_authority(tx, &record, &state, proof)?;
            settled_attempt(tx, &state)?;
            validate_assembly(&record, &state, native.evidence())?;
            if native.evidence().epoch != state.effect_epoch {
                return Err(invalid("native assembly belongs to another effect epoch"));
            }
            state.assembly = Some(native.evidence().clone());
            state.phase = IntegrationPhase::Assembled;
            state.updated_at = now.clone();
            save(tx, &mut record, &state)?;
            native.validate_custody().map_err(StoreError::from)?;
            proof.check_live().map_err(StoreError::from)?;
            Ok(Some((record, state)))
        })?;
        Ok(accepted.map(|(record, owner)| AssembledIntegration {
            record,
            owner,
            native,
        }))
    }

    /// Claim one distinct remote publication operation using still-live native
    /// custody and a current source inspection. Stored JSON grants no replay.
    pub fn claim_publication(
        &self,
        ready: AssembledIntegration,
        proof: &BoundIntegrationProposal,
    ) -> Result<Option<PublicationClaim>, AppError> {
        proof.check_live()?;
        ready.native.validate_custody()?;
        let now = self.ctx.now();
        let claimed = self.ctx.store().write(|tx| {
            proof.check_live().map_err(StoreError::from)?;
            ready.native.validate_custody().map_err(StoreError::from)?;
            let (mut record, mut state) = find(tx, self.ctx.project(), &ready.record.id)?;
            if record != ready.record
                || state != ready.owner
                || state.phase != IntegrationPhase::Assembled
            {
                return Ok(None);
            }
            check_authority(tx, &record, &state, proof)?;
            settled_attempt(tx, &state)?;
            state.phase = IntegrationPhase::Publishing;
            state.effect_epoch = state
                .effect_epoch
                .checked_add(1)
                .ok_or_else(|| invalid("publication epoch overflow"))?;
            state.effect_started_at = Some(now.clone());
            state.updated_at = now.clone();
            save(tx, &mut record, &state)?;
            ready.native.validate_custody().map_err(StoreError::from)?;
            proof.check_live().map_err(StoreError::from)?;
            Ok(Some((record, state)))
        })?;
        Ok(claimed.map(|(record, owner)| PublicationClaim {
            record,
            owner,
            native: ready.native,
            deadline: proof.deadline,
            cancellation: proof.cancellation.clone(),
        }))
    }

    /// Recheck exact durable/native/operator/source authority before each read
    /// or remote effect and during capture cancellation polling.
    pub fn publication_permitted(
        &self,
        claim: &PublicationClaim,
        proof: &BoundIntegrationProposal,
    ) -> Result<bool, AppError> {
        proof.check_live()?;
        claim.validate_custody()?;
        self.ctx
            .store()
            .read(|tx| check_claim(tx, self.ctx.project(), claim, proof))
            .map_err(Into::into)
    }

    /// Record a one-shot potentially begun remote effect before performing it.
    /// A transport error leaves this intent intact. Calling again returns false;
    /// it must not be interpreted as permission to retry. Before PR creation,
    /// the native adapter must prove the exact managed branch/commit, not infer
    /// it from the preceding push intent. Neither intent is a success receipt.
    pub fn claim_publication_effect(
        &self,
        claim: &mut PublicationClaim,
        proof: &BoundIntegrationProposal,
        effect: PublicationEffect,
    ) -> Result<bool, AppError> {
        proof.check_live()?;
        claim.validate_custody()?;
        let now = self.ctx.now();
        let changed = self.ctx.store().write(|tx| {
            if !check_claim(tx, self.ctx.project(), claim, proof)? {
                return Ok(None);
            }
            let allowed = match effect {
                PublicationEffect::PushBranch => claim.owner.publication_effects.is_empty(),
                PublicationEffect::CreatePullRequest => {
                    claim.owner.publication_effects == [PublicationEffect::PushBranch]
                }
            };
            if !allowed {
                return Ok(None);
            }
            let mut record = claim.record.clone();
            let mut state = claim.owner.clone();
            state.publication_effects.push(effect);
            state.updated_at = now.clone();
            save(tx, &mut record, &state)?;
            claim.validate_custody().map_err(StoreError::from)?;
            proof.check_live().map_err(StoreError::from)?;
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
}

fn check_claim(
    tx: &impl ReadOps,
    project: crate::store::ProjectId,
    claim: &PublicationClaim,
    proof: &BoundIntegrationProposal,
) -> Result<bool, StoreError> {
    proof.check_live().map_err(StoreError::from)?;
    claim.validate_custody().map_err(StoreError::from)?;
    if claim.record.project != project {
        return Ok(false);
    }
    let (record, state) = find(tx, project, claim.id())?;
    if record != claim.record || state != claim.owner || state.phase != IntegrationPhase::Publishing
    {
        return Ok(false);
    }
    check_authority(tx, &record, &state, proof)?;
    settled_attempt(tx, &state)?;
    if state.assembly.as_ref() != Some(claim.native.evidence()) {
        return Err(invalid("publication lost exact native assembly"));
    }
    claim.validate_custody().map_err(StoreError::from)?;
    proof.check_live().map_err(StoreError::from)?;
    Ok(true)
}

pub(super) fn validate_state(
    record: &IntegrationRecovery,
    state: &IntegrationOwner,
) -> Result<(), StoreError> {
    let assembled = matches!(
        state.phase,
        IntegrationPhase::Assembled | IntegrationPhase::Publishing | IntegrationPhase::Published
    );
    if assembled && state.assembly.is_none()
        || matches!(
            state.phase,
            IntegrationPhase::Reserved | IntegrationPhase::Assembling
        ) && state.assembly.is_some()
        || !matches!(
            state.phase,
            IntegrationPhase::Publishing | IntegrationPhase::Published
        ) && !state.publication_effects.is_empty()
        || (state.phase == IntegrationPhase::Published) != state.publication.is_some()
        || !matches!(
            state.publication_effects.as_slice(),
            [] | [PublicationEffect::PushBranch]
                | [
                    PublicationEffect::PushBranch,
                    PublicationEffect::CreatePullRequest
                ]
        )
    {
        return Err(StoreError::Corrupt(
            "integration publication phase/effect custody is inconsistent".into(),
        ));
    }
    if let Some(evidence) = &state.assembly {
        validate_assembly(record, state, evidence)?;
        let epoch = if matches!(
            state.phase,
            IntegrationPhase::Publishing | IntegrationPhase::Published
        ) {
            evidence.epoch.checked_add(1)
        } else {
            Some(evidence.epoch)
        };
        if assembled && epoch != Some(state.effect_epoch) {
            return Err(StoreError::Corrupt(
                "integration publication epoch differs from assembly".into(),
            ));
        }
    }
    if let Some(publication) = &state.publication {
        if state.publication_effects
            != [
                PublicationEffect::PushBranch,
                PublicationEffect::CreatePullRequest,
            ]
        {
            return Err(invalid("published state lacks complete intent history"));
        }
        validate_publication(record, state, publication)?;
    }
    Ok(())
}

fn validate_publication(
    record: &IntegrationRecovery,
    state: &IntegrationOwner,
    evidence: &PublicationEvidence,
) -> Result<(), StoreError> {
    let assembly = state
        .assembly
        .as_ref()
        .ok_or_else(|| invalid("published state lacks native assembly"))?;
    let original = crate::domain::pr_url::parse_pr_url(&state.submission.pull_request)
        .map_err(|e| invalid(&e.to_string()))?;
    let managed = crate::domain::pr_url::parse_pr_url(&evidence.pull_request)
        .map_err(|e| invalid(&e.to_string()))?;
    if evidence.version != 1
        || evidence.owner != record.id
        || evidence.epoch != state.effect_epoch
        || evidence.original != state.submission
        || evidence.branch != state.branch
        || evidence.commit != assembly.commit
        || evidence.tree != assembly.tree
        || evidence.parents != [state.plan.base.clone(), state.plan.head.clone()]
        || evidence.marker
            != format!(
                "<!-- storyhook-integration-owner:{}:{}:{} -->",
                record.id, evidence.epoch, assembly.stamp_sha256
            )
        || managed.number != evidence.number
        || managed.number == original.number
        || !managed.host.eq_ignore_ascii_case(&original.host)
        || !managed.owner.eq_ignore_ascii_case(&original.owner)
        || !managed.repo.eq_ignore_ascii_case(&original.repo)
    {
        return Err(invalid(
            "native publication differs from original owner, tree, parents or repository",
        ));
    }
    Ok(())
}

fn validate_assembly(
    record: &IntegrationRecovery,
    state: &IntegrationOwner,
    evidence: &AssemblyEvidence,
) -> Result<(), StoreError> {
    if evidence.version != 1
        || evidence.owner != record.id
        || evidence.epoch == 0
        || evidence.branch != state.branch
        || evidence.workspace.path != state.workspace
        || evidence.submission != state.submission
        || evidence.plan != state.plan
        || evidence.workspace.inode == 0
        || evidence.author.trim().is_empty()
        || evidence.committer.trim().is_empty()
        || evidence.stamp_sha256.len() != 64
        || !evidence.stamp_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || [&evidence.commit, &evidence.tree]
            .iter()
            .any(|oid| !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(invalid(
            "assembly evidence differs from its durable original owner",
        ));
    }
    Ok(())
}
