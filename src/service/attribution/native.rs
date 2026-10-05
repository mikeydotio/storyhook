//! Native executions, distinct from serializable observations and return authority.

mod pipeline;
#[cfg(test)]
mod tests;

use super::*;
use crate::{env::Environment, error::AppError, process::Cancellation};
use std::{
    path::Path,
    time::{Duration, Instant},
};

/// One owned, syntax-checked comparison. Only native subprocesses can add observations.
pub struct NativeRustComparison {
    trees: PreparedTrees,
    candidate: PreparedDirectory,
    control: PreparedDirectory,
    inputs: RustInputs,
    gate: crate::store::GateInputs,
    case: RustCase,
    environment: Environment,
    deadline: Instant,
    cancellation: Cancellation,
    observations: Vec<(ProbeSide, ProbeResult)>,
    binding: Option<(String, String, i64)>,
    requests: std::collections::BTreeSet<String>,
    #[cfg(test)]
    fixture: Option<std::path::PathBuf>,
}

/// Identity reserved by the durable owner before a physical diagnostic launch.
pub struct NativeProbeBinding<'a> {
    /// Registered project identity for resource accounting.
    pub project: &'a str,
    /// Owning admission, already persisted.
    pub attempt: &'a str,
    /// Physical execution identity, already persisted.
    pub execution: &'a str,
    /// Exact submitted generation.
    pub generation: i64,
    /// Unique broker request identity, already reserved.
    pub request: &'a str,
    /// Owned cost journal with its durable run marker.
    pub journal: &'a Path,
    /// Fresh retained output directory; must not exist before this execution.
    pub output: &'a Path,
    /// Supervisor cleanup allowance after cancellation; never extends diagnosis validity.
    pub termination_grace: Duration,
}

impl NativeRustComparison {
    /// Recompute pinned trees and validate the complete closed detector input set.
    pub fn prepare(
        checkout: &Path,
        gate: &crate::store::GateInputs,
        case: RustCase,
        intervention: TreeIntervention,
        environment: Environment,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<Self, AppError> {
        let deadline = deadline.min(Instant::now() + Duration::from_millis(MAX_DIAGNOSIS_MS));
        let RustTarget::Integration(target) = &case.target else {
            return Err(invalid("native comparison requires an integration case"));
        };
        let protected = vec![
            "Cargo.toml".into(),
            "Cargo.lock".into(),
            format!("tests/{target}.rs"),
        ];
        let trees = PreparedTrees::prepare(
            checkout,
            gate,
            &protected,
            intervention,
            deadline,
            cancellation,
        )?;
        let candidate = trees.materialize(ProbeSide::Candidate)?;
        let control = trees.materialize(ProbeSide::Control)?;
        let inputs = RustInputs::validate(&candidate, &control, &case).map_err(|e| invalid(&e))?;
        Ok(Self {
            trees,
            candidate,
            control,
            inputs,
            gate: gate.clone(),
            case,
            environment,
            deadline,
            cancellation: cancellation.clone(),
            observations: vec![],
            binding: None,
            requests: Default::default(),
            #[cfg(test)]
            fixture: None,
        })
    }

    /// Immutable contrast inputs for the owner's durable reservation.
    pub fn plan(&self, component: &str) -> ContrastPlan {
        ContrastPlan {
            component: component.into(),
            base: self
                .gate
                .base
                .clone()
                .expect("native preparation validated base"),
            candidate_tree: self.trees.trees().0.into(),
            control_tree: self.trees.trees().1.into(),
            detector: self.inputs.detector().into(),
            relation: self.trees.relation(),
            argv: self.case.run_arguments(),
        }
    }

    /// Retained exact intervention; a caller archives this before launching probes.
    pub fn patch(&self) -> &[u8] {
        self.trees.patch()
    }

    /// Execute the next C-B-B-C probe under real admission; no caller-supplied verdict is accepted.
    pub fn execute(
        &mut self,
        side: ProbeSide,
        binding: &NativeProbeBinding<'_>,
    ) -> Result<ProbeResult, AppError> {
        let order = [
            ProbeSide::Candidate,
            ProbeSide::Control,
            ProbeSide::Control,
            ProbeSide::Candidate,
        ];
        let identity = (
            binding.project.to_string(),
            binding.attempt.to_string(),
            binding.generation,
        );
        if order.get(self.observations.len()) != Some(&side)
            || self.cancellation.is_cancelled()
            || Instant::now() >= self.deadline
            || binding.generation <= 0
            || [
                binding.project,
                binding.attempt,
                binding.execution,
                binding.request,
            ]
            .iter()
            .any(|s| s.trim().is_empty())
            || self.binding.as_ref().is_some_and(|old| old != &identity)
            || self.observations.iter().any(|(_, r)| {
                r.execution_id == binding.execution || r.executions != 1 || !r.cleanup_complete
            })
            || !binding.output.is_absolute()
            || !binding.journal.is_absolute()
            || !self.requests.insert(binding.request.into())
        {
            return Err(invalid(
                "probe order, authority, unique identity or active allowance is invalid",
            ));
        }
        self.candidate.verify_unchanged()?;
        self.control.verify_unchanged()?;
        self.binding = Some(identity);
        let result = pipeline::execute(self, side, binding)?;
        self.observations.push((side, result.clone()));
        Ok(result)
    }

    /// Explicitly settle both materializations and private Git state, retaining every error.
    pub fn close(self) -> Result<(), AppError> {
        let errors: Vec<_> = [
            self.candidate.close(),
            self.control.close(),
            self.trees.close(),
        ]
        .into_iter()
        .filter_map(Result::err)
        .map(|e| e.to_string())
        .collect();
        if errors.is_empty() {
            Ok(())
        } else {
            Err(invalid(&errors.join("; ")))
        }
    }
}

fn invalid(detail: &str) -> AppError {
    AppError::Validation(format!("native Rust comparison: {detail}"))
}
