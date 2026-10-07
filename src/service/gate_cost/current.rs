//! Compact durable projections, independent of progress freshness.
use super::{Elapsed, view::EvidenceView};
use crate::store::{GateAttempt, GlobalSeq};
use serde::{Deserialize, Serialize};

/// Last retained admission checkpoint; absent evidence is never a zero cost.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CurrentCost {
    /// Admission that owns this observation, including shared executions.
    pub admission_id: String,
    /// Durable monotonic lower bound and its uncertainty.
    pub elapsed: Elapsed,
    /// Independent process-budget outcome.
    pub budget: String,
    /// Gate's own result; no budget observation grants certification.
    pub gate_result: Option<String>,
}

impl CurrentCost {
    /// Projects an admission without sampling time or changing any state.
    pub fn new(attempt: &GateAttempt) -> Self {
        Self {
            admission_id: attempt.id.clone(),
            elapsed: attempt.elapsed.clone(),
            budget: attempt.budget_status().into(),
            gate_result: attempt.verdict.clone(),
        }
    }

    /// Separates gate authority from the process defect in human output.
    pub fn render(&self) -> String {
        format!(
            "Admission cost: {} ms{}; budget {} (900000 ms, observation only); gate result {}. Checkpoint {}.{}\n",
            self.elapsed.milliseconds,
            if self.elapsed.estimated {
                " (estimated)"
            } else {
                ""
            },
            self.budget,
            self.gate_result.as_deref().unwrap_or("unknown"),
            self.elapsed.checkpoint_at,
            self.elapsed
                .diagnostic
                .as_ref()
                .map_or_else(String::new, |d| format!(" {d}")),
        )
    }
}

/// Summary of the requested generation only, retaining earlier retry breaches.
pub fn progress(view: &EvidenceView, generation: Option<GlobalSeq>) -> Option<String> {
    let cost = view
        .submissions
        .iter()
        .find(|s| s.submission.generation == generation)?;
    let admission = view
        .attempts
        .iter()
        .rev()
        .find(|a| cost.attempts.contains(&a.id))?;
    let number = |n: Option<u64>| n.map_or_else(|| "unknown".into(), |n| n.to_string());
    Some(format!(
        "\n{}Cumulative submission cost: wall {} ms (UTC estimate), admission {} ms, physical gate {} ms, diagnosis {} ms; {} process-budget-breach(es). Shared execution cost is not divided. Observed through {}.\n",
        CurrentCost::new(admission).render(),
        number(cost.wall_milliseconds),
        number(cost.admission_milliseconds),
        number(cost.execution_milliseconds),
        number(cost.diagnosis_milliseconds),
        cost.breaches.len(),
        cost.observed_through.as_deref().unwrap_or("unknown"),
    ))
}
