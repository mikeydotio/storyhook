//! Durable coordination of native causal comparisons; no certification authority.
use super::*;
use crate::service::attribution::*;
use crate::store::{GateExecution, GateExecutionPurpose, StoreError};
mod execute;
mod record;

/// A proposed exact reproduction. Native validation establishes whether it is supported.
pub struct RustDiagnosisRequest {
    /// Retained original physical gate execution.
    pub execution: String,
    /// Raw original output already named by that execution.
    pub log: PathBuf,
    /// Exact selected package, integration target and test.
    pub case: RustCase,
    /// Detector-preserving control proposal, validated against pinned Git inputs.
    pub intervention: TreeIntervention,
    #[cfg(test)]
    fixture: Option<PathBuf>,
}

/// Diagnosis grants a repair capability only after native execution and cleanup.
pub enum RustDiagnosisResult {
    /// The exact attempt or submission no longer permits diagnosis.
    Superseded,
    /// Evidence is retained, but does not establish candidate responsibility.
    Held {
        /// Durable evidence identifier.
        evidence: String,
        /// Why automatic diagnosis stopped.
        detail: String,
    },
    /// A settled native contrast bound to the retained original gate failure.
    Proven(Box<CausalReturnEvidence>),
}

impl VerificationGuard {
    /// Retain, reserve, execute and settle an exact native comparison under this owner.
    pub fn diagnose_rust_failure<S: Store>(
        &self,
        ctx: &Ctx<'_, S>,
        candidate: &VerificationCandidate,
        request: RustDiagnosisRequest,
    ) -> Result<RustDiagnosisResult, AppError> {
        let started = Instant::now();
        if ctx.project() != candidate.project
            || self.active.project != candidate.project
            || self.active.story_id != candidate.story_id
            || self.is_cancelled()
        {
            return Ok(RustDiagnosisResult::Superseded);
        }
        let subscription = self.registry.bus.subscribe();
        let Some((original, control)) = record::original(ctx.store(), candidate, self)? else {
            return Ok(RustDiagnosisResult::Superseded);
        };
        let pending = self.reserve(ReservationReason::Attribution, ctx.now());
        let selection = record::select(&original, &request);
        let Some((mut record, fresh)) = record::begin(
            ctx, candidate, self, control, &original, &request, &selection,
        )?
        else {
            return Ok(RustDiagnosisResult::Superseded);
        };
        // An interrupted or already assessed record cannot be silently replayed.
        if !fresh {
            pending.retire();
            return Ok(held(
                &record,
                "original diagnosis already retained; inspect its evidence before retry",
            ));
        }
        let (result, observed) = observation::observe_during(
            &subscription,
            &self.cancellation,
            &self.cancellation,
            &candidate.project_slug,
            || {
                Ok(ctx.store().read(|tx| {
                    CausalReturnEvidence::permits_diagnosis(
                        tx,
                        candidate,
                        &self.active.attempt_id,
                        control,
                    )
                })?)
            },
            || {
                execute::run(
                    ctx,
                    candidate,
                    self,
                    &request,
                    &selection,
                    &mut record,
                    started,
                    control,
                )
            },
        );
        let result = result?;
        if !observed? || self.is_cancelled() {
            pending.retire();
            return Ok(RustDiagnosisResult::Superseded);
        }
        if let RustDiagnosisResult::Held { evidence, detail } = &result {
            StoryService::new(ctx).comment(&candidate.story_id, &format!(
                "CENTRAL VERIFICATION ATTRIBUTION HELD — evidence {evidence}, attempt {}. No repair is assigned. Inspect `story verifier evidence {} --json`.\n\n{}",
                self.active.attempt_id, candidate.story_id, crate::text_lint::quote_evidence(detail)))?;
        }
        pending.retire();
        Ok(result)
    }
}

fn held(record: &AttributionRecord, detail: &str) -> RustDiagnosisResult {
    RustDiagnosisResult::Held {
        evidence: record.id.clone(),
        detail: detail.into(),
    }
}

fn elapsed(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
