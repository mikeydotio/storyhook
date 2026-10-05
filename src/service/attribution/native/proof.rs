//! Only a settled native owner can grant a causal return witness.
mod retained;
use super::*;
use crate::service::VerificationCandidate;
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
            || classify(record, component) != FailureCause::CandidateCaused
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
        let proof = CausalReturnEvidence {
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
        let mut next = self.record.clone();
        next.revision += 1;
        next.diagnosis_ms = next.diagnosis_ms.max(elapsed(self.started));
        next.assessments.push(AttributionAssessment {
            component: next.plans[self.plan].component.clone(),
            evidence_revision: self.record.revision,
            cause: FailureCause::CandidateCaused,
            probes: next
                .probes
                .iter()
                .filter(|p| p.plan == self.plan)
                .map(|p| p.id.clone())
                .collect(),
            detail: format!(
                "Native causal return; original sha256 {}; retained probe digests: {}",
                self.original.digest,
                serde_json::to_string(&self.archives)
                    .map_err(|e| refused(&format!("encode retained digests: {e}")))?
            ),
        });
        if next.components.len() == 1 {
            next.held = false;
            next.retired = Some("proved component returned for repair".into());
        }
        if !tx.update_attribution(&next, self.record.revision)? {
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
        if history != self.history
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

    /// Evidence-based instructions for only the proved component, excluding other failures.
    pub fn diagnosis(&self) -> String {
        let plan = &self.record.plans[self.plan];
        let component = self
            .record
            .components
            .iter()
            .find(|c| c.id == plan.component)
            .expect("proved component");
        format!(
            "CENTRAL VERIFICATION CAUSAL RETURN — {}. Repair only {}. Candidate tree {}; pinned base {}; control tree {}. Two native candidate failures match the original assertion and two control executions pass under equivalent supported conditions. Evidence {} revision {}; original {} (sha256 {}). Exact reproduction arguments: {:?}. Probe outputs: {}. Other held components are not assigned for repair. {} {} Commit, then move {} back to verifying. The central verifier owns submission and certification.",
            self.candidate.story_id,
            component.check,
            plan.candidate_tree,
            plan.base,
            plan.control_tree,
            self.record.id,
            self.record.revision,
            component.log,
            self.original.digest,
            plan.argv,
            self.record
                .probes
                .iter()
                .filter(|p| p.plan == self.plan)
                .filter_map(|p| p.completed.as_ref().map(|r| r.log.as_str()))
                .collect::<Vec<_>>()
                .join(", "),
            crate::service::verification::IMPLEMENTER_TEST_SCOPE,
            crate::service::verification::FAILED_GATE_RERUN_SCOPE,
            self.candidate.story_id
        )
    }
}

fn elapsed(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}
fn refused(detail: &str) -> StoreError {
    StoreError::Validation(format!("causal repair proof: {detail}"))
}
