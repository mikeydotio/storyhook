//! Only a settled native owner can grant a causal return witness.
mod retained;
use super::*;
use crate::service::VerificationCandidate;
use crate::service::verification::{FAILED_GATE_RERUN_SCOPE, IMPLEMENTER_TEST_SCOPE};
use crate::store::{GateExecution, ReadOps, StoreError};
pub(super) use retained::Archive;

/// Non-serializable native evidence after explicit process and workspace cleanup.
pub struct SettledRustComparison {
    milliseconds: u64,
    started: Instant,
    deadline: Instant,
    cancellation: Cancellation,
    plan: ContrastPlan,
    gate: crate::store::GateInputs,
    case: RustCase,
    binding: Option<(String, String, i64)>,
    requests: std::collections::BTreeSet<String>,
    observations: Vec<(ProbeSide, ProbeResult)>,
    archives: Vec<Archive>,
}

/// Opaque repair authority; it grants no gate or landing authority.
///
/// An assessor cannot deserialize a repair capability:
/// ```compile_fail
/// use storyhook::service::attribution::CausalReturnEvidence;
/// let _: CausalReturnEvidence = serde_json::from_str("{}").unwrap();
/// ```
/// A caller cannot construct a capability from record fields:
/// ```compile_fail
/// use storyhook::service::attribution::CausalReturnEvidence;
/// let proof = CausalReturnEvidence {};
/// ```
pub struct CausalReturnEvidence {
    finding: NativeFinding,
}

/// Native proof of a fault also present in the pinned base. This authorizes
/// enrollment in managed shared recovery, never an implementer return or a gate.
///
/// It cannot be reconstructed from assessor-provided JSON:
/// ```compile_fail
/// use storyhook::service::attribution::SharedRecoveryEvidence;
/// let _: SharedRecoveryEvidence = serde_json::from_str("{}").unwrap();
/// ```
/// Nor can a caller fabricate it from public observation fields:
/// ```compile_fail
/// use storyhook::service::attribution::SharedRecoveryEvidence;
/// let proof = SharedRecoveryEvidence {};
/// ```
pub struct SharedRecoveryEvidence {
    finding: NativeFinding,
}

/// Retained digest receipt for an already-enrolled native finding. This is
/// evidence to recheck after restart, not a capability that can enroll work.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::service) struct RetainedNativeEvidence {
    original: Archive,
    probes: Vec<Archive>,
}

impl RetainedNativeEvidence {
    pub(in crate::service) fn verify(&self) -> Result<(), StoreError> {
        if self.probes.is_empty() {
            return Err(refused("retained native evidence has no probe archives"));
        }
        self.original.verify()?;
        for archive in &self.probes {
            archive.verify()?;
        }
        Ok(())
    }
}

/// Both capabilities retain the same native custody, cleanup and revocation
/// proof. The private expected cause keeps their effect authorities distinct.
struct NativeFinding {
    cause: FailureCause,
    candidate: VerificationCandidate,
    record: AttributionRecord,
    history: Vec<AttributionRecord>,
    executions: Vec<GateExecution>,
    control: i64,
    plan: usize,
    original: Archive,
    archives: Vec<Archive>,
    started: Instant,
    deadline: Instant,
    cancellation: Cancellation,
}

impl NativeRustComparison {
    /// Consume the owner and settle all resources before evidence can grant authority.
    pub fn settle(mut self) -> Result<SettledRustComparison, AppError> {
        let inputs = self
            .candidate
            .verify_unchanged()
            .and_then(|()| self.control.verify_unchanged());
        let mut settled = SettledRustComparison {
            milliseconds: 0,
            started: self.started,
            deadline: self.deadline,
            cancellation: self.cancellation.clone(),
            plan: self.plan(""),
            gate: self.gate.clone(),
            case: self.case.clone(),
            binding: self.binding.take(),
            requests: std::mem::take(&mut self.requests),
            observations: std::mem::take(&mut self.observations),
            archives: std::mem::take(&mut self.archives),
        };
        // Cleanup runs even when the final source check fails. Both errors remain visible.
        let cleanup = self.close();
        let errors: Vec<_> = [inputs, cleanup]
            .into_iter()
            .filter_map(Result::err)
            .map(|e| e.to_string())
            .collect();
        if !errors.is_empty() {
            return Err(invalid(&errors.join("; ")));
        }
        settled.milliseconds = elapsed(settled.started);
        Ok(settled)
    }
}

impl SettledRustComparison {
    /// Full active duration through explicit cleanup, for durable budget accounting.
    pub fn milliseconds(&self) -> u64 {
        self.milliseconds
    }

    /// Match private native results with committed original and diagnostic executions.
    /// This does not change story state. The return transaction must call `validate`.
    pub fn prove(
        &self,
        tx: &impl ReadOps,
        candidate: &VerificationCandidate,
        attribution: &str,
        original_execution: &str,
    ) -> Result<CausalReturnEvidence, StoreError> {
        self.prove_finding(
            tx,
            candidate,
            attribution,
            original_execution,
            FailureCause::CandidateCaused,
        )
        .map(|finding| CausalReturnEvidence { finding })
    }

    /// Authorize shared recovery only after the same native custody checks as
    /// a causal return, with matching failures on candidate and pinned control.
    pub fn prove_shared(
        &self,
        tx: &impl ReadOps,
        candidate: &VerificationCandidate,
        attribution: &str,
        original_execution: &str,
    ) -> Result<SharedRecoveryEvidence, StoreError> {
        self.prove_finding(
            tx,
            candidate,
            attribution,
            original_execution,
            FailureCause::SharedProject,
        )
        .map(|finding| SharedRecoveryEvidence { finding })
    }

    fn prove_finding(
        &self,
        tx: &impl ReadOps,
        candidate: &VerificationCandidate,
        attribution: &str,
        original_execution: &str,
        cause: FailureCause,
    ) -> Result<NativeFinding, StoreError> {
        let records = tx.attributions(candidate.project)?;
        let record = records
            .iter()
            .find(|r| r.id == attribution)
            .ok_or_else(|| refused("attribution missing"))?;
        let history: Vec<_> = records
            .iter()
            .filter(|r| r.submission.same_generation(&record.submission))
            .cloned()
            .collect();
        let attempts = tx.gate_attempts(candidate.project)?;
        let attempt = attempts
            .iter()
            .rev()
            .find(|a| {
                a.submission
                    .matches_story(candidate.project, &candidate.story_id)
            })
            .ok_or_else(|| refused("admission missing"))?;
        if self.binding.as_ref()
            != Some(&(
                candidate.project_slug.clone(),
                record.attempt.clone(),
                candidate.verifying_generation.map_or(0, |g| g.get()),
            ))
            || record.inputs != self.gate
            || record.attempt != attempt.id
            || !record.submission.same_generation(&attempt.submission)
            || record.diagnosis_ms < self.milliseconds
            || record
                .settlement
                .as_ref()
                .is_none_or(|s| !s.cleanup_complete || s.milliseconds < self.milliseconds)
            || self.observations.len() != 4
            || self.requests.len() != 4
        {
            return Err(refused(
                "native identity, duration or complete contrast differs from retained record",
            ));
        }
        let components: Vec<_> = record
            .components
            .iter()
            .filter(|c| c.check == retained::check(&self.case))
            .collect();
        let [component] = components.as_slice() else {
            return Err(refused("native component missing or ambiguous"));
        };
        let index = record
            .plans
            .iter()
            .position(|p| p.component == component.id)
            .ok_or_else(|| refused("native plan missing"))?;
        let probes: Vec<_> = record.probes.iter().filter(|p| p.plan == index).collect();
        let mut plan = self.plan.clone();
        plan.component = component.id.clone();
        if record.plans[index] != plan
            || probes.len() != 4
            || component.check != retained::check(&self.case)
            || classify(record, component) != cause
            || probes.iter().zip(&self.observations).any(|(p, (side, r))| {
                p.side != *side || p.completed.as_ref() != Some(r) || !self.requests.contains(&p.id)
            })
        {
            return Err(refused(
                "retained contrast is not the complete native causal finding",
            ));
        }
        retained::history(&history, record, component, elapsed(self.started))?;
        retained::executions(attempt, record, index)?;
        let original =
            retained::original(attempt, original_execution, record, component, &self.case)?;
        let proof = NativeFinding {
            cause,
            candidate: candidate.clone(),
            plan: index,
            record: record.clone(),
            history,
            executions: attempt.executions.clone(),
            control: attempt
                .control_revision
                .ok_or_else(|| refused("legacy control epoch"))?,
            original,
            archives: self.archives.clone(),
            started: self.started,
            deadline: self.deadline,
            cancellation: self.cancellation.clone(),
        };
        if !proof.validate(tx, candidate)? {
            return Err(refused(
                "current submission authority or retained evidence changed",
            ));
        }
        Ok(proof)
    }
}

impl CausalReturnEvidence {
    /// Submitted source head established by this native proof.
    pub fn submitted_head(&self) -> &str {
        self.finding
            .record
            .inputs
            .head
            .as_deref()
            .expect("proved head")
    }
    /// Admission whose failed gate and diagnosis establish this proof.
    pub(in crate::service) fn attempt(&self) -> &str {
        &self.finding.record.attempt
    }
    /// Original failed merge tree established by this native proof.
    pub(in crate::service) fn failed_tree(&self) -> &str {
        &self.finding.record.plans[self.finding.plan].candidate_tree
    }

    /// Reuse the same authority fence while the owner's own attribution hold is active.
    pub(crate) fn permits_diagnosis(
        tx: &impl ReadOps,
        candidate: &VerificationCandidate,
        attempt: &str,
        control: i64,
    ) -> Result<bool, StoreError> {
        retained::authority(tx, candidate, attempt, control)
    }

    /// Retire only this proved component in the transaction that validates and returns it.
    pub(in crate::service) fn retire(
        &self,
        tx: &mut impl crate::store::WriteOps,
    ) -> Result<(), StoreError> {
        if !self.live_budget() {
            return Err(refused(
                "authority was cancelled or diagnosis allowance expired before commit",
            ));
        }
        let mut next = self.finding.record.clone();
        next.revision += 1;
        next.diagnosis_ms = next.diagnosis_ms.max(elapsed(self.finding.started));
        next.assessments.push(AttributionAssessment {
            component: next.plans[self.finding.plan].component.clone(),
            evidence_revision: self.finding.record.revision,
            cause: FailureCause::CandidateCaused,
            probes: next
                .probes
                .iter()
                .filter(|p| p.plan == self.finding.plan)
                .map(|p| p.id.clone())
                .collect(),
            detail: format!(
                "Native causal return; original sha256 {}; retained probe digests: {}",
                self.finding.original.digest,
                serde_json::to_string(&self.finding.archives)
                    .map_err(|e| refused(&format!("encode retained digests: {e}")))?
            ),
        });
        if next.components.len() == 1 {
            next.held = false;
            next.retired = Some("proved component returned for repair".into());
        }
        if !tx.update_attribution(&next, self.finding.record.revision)? {
            return Err(refused("attribution changed before return"));
        }
        Ok(())
    }

    /// Recheck the full snapshot in the transaction that applies the repair return.
    pub fn validate(
        &self,
        tx: &impl ReadOps,
        candidate: &VerificationCandidate,
    ) -> Result<bool, StoreError> {
        self.finding.validate(tx, candidate)
    }

    fn live_budget(&self) -> bool {
        self.finding.live_budget()
    }

    /// Evidence-based instructions for only the proved component, excluding other failures.
    pub fn diagnosis(&self) -> String {
        let plan = &self.finding.record.plans[self.finding.plan];
        let component = self
            .finding
            .record
            .components
            .iter()
            .find(|c| c.id == plan.component)
            .expect("proved component");
        format!(
            "CENTRAL VERIFICATION CAUSAL RETURN — {}. Repair only {}. Candidate tree {}; pinned base {}; control tree {}. Two native candidate failures match the original assertion and two control executions pass under equivalent supported conditions. Evidence {} revision {}; original {} (sha256 {}). Exact reproduction arguments: {:?}. Probe outputs: {}. Other held components are not assigned for repair. {IMPLEMENTER_TEST_SCOPE} {FAILED_GATE_RERUN_SCOPE} Commit, then move {} back to verifying. {}. The central verifier owns certification.",
            self.finding.candidate.story_id,
            component.check,
            plan.candidate_tree,
            plan.base,
            plan.control_tree,
            self.finding.record.id,
            self.finding.record.revision,
            component.log,
            self.finding.original.digest,
            plan.argv,
            self.finding
                .record
                .probes
                .iter()
                .filter(|p| p.plan == self.finding.plan)
                .filter_map(|p| p.completed.as_ref().map(|r| r.log.as_str()))
                .collect::<Vec<_>>()
                .join(", "),
            self.finding.candidate.story_id,
            crate::service::verification::push_promise(
                self.finding.candidate.cleanup_lease.is_some(),
                true
            )
        )
    }
}

impl SharedRecoveryEvidence {
    /// The recovery enrollment transaction must recheck this live capability.
    pub fn validate(
        &self,
        tx: &impl ReadOps,
        candidate: &VerificationCandidate,
    ) -> Result<bool, StoreError> {
        self.finding.validate(tx, candidate)
    }

    /// Digest-only restart evidence; it cannot construct an enrollment capability.
    pub(in crate::service) fn retained(&self) -> RetainedNativeEvidence {
        RetainedNativeEvidence {
            original: self.finding.original.clone(),
            probes: self.finding.archives.clone(),
        }
    }

    /// Immutable provenance retained by the managed recovery enrollment.
    pub(in crate::service) fn record(&self) -> &AttributionRecord {
        &self.finding.record
    }

    pub(in crate::service) fn plan(&self) -> &ContrastPlan {
        &self.finding.record.plans[self.finding.plan]
    }
}

impl NativeFinding {
    /// Recheck native custody and the precise expected classification at application.
    pub fn validate(
        &self,
        tx: &impl ReadOps,
        candidate: &VerificationCandidate,
    ) -> Result<bool, StoreError> {
        if &self.candidate != candidate
            || self.cancellation.is_cancelled()
            || Instant::now() >= self.deadline
            || !retained::authority(tx, candidate, &self.record.attempt, self.control)?
        {
            return Ok(false);
        }
        let history: Vec<_> = tx
            .attributions(candidate.project)?
            .into_iter()
            .filter(|r| r.submission.same_generation(&self.record.submission))
            .collect();
        let attempts = tx.gate_attempts(candidate.project)?;
        let Some(attempt) = attempts.iter().find(|a| a.id == self.record.attempt) else {
            return Ok(false);
        };
        let component = self
            .record
            .components
            .iter()
            .find(|c| c.id == self.record.plans[self.plan].component)
            .expect("proved component");
        if classify(&self.record, component) != self.cause
            || history != self.history
            || attempt.executions != self.executions
            || retained::history(&history, &self.record, component, elapsed(self.started)).is_err()
        {
            return Ok(false);
        }
        self.original.verify()?;
        for archive in &self.archives {
            archive.verify()?;
        }
        Ok(self.live_budget())
    }

    fn live_budget(&self) -> bool {
        !self.cancellation.is_cancelled()
            && Instant::now() < self.deadline
            && retained::history(
                &self.history,
                &self.record,
                self.record
                    .components
                    .iter()
                    .find(|c| c.id == self.record.plans[self.plan].component)
                    .expect("proved component"),
                elapsed(self.started),
            )
            .is_ok()
    }
}

fn elapsed(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}
fn refused(detail: &str) -> StoreError {
    StoreError::Validation(format!("causal repair proof: {detail}"))
}
