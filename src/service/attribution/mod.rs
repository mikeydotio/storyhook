//! Causal diagnosis is evidence for repair ownership, never gate certification.

mod contrast;
pub(crate) mod holds;
mod model;
mod native;
mod preparation;
mod rust_cargo;
mod rust_case;
mod rust_inputs;
mod trees;
mod validation;
mod view;
pub use contrast::classify;
pub use holds::AttributionHold;
pub use model::*;
pub use native::{
    CausalReturnEvidence, NativeProbeBinding, NativeRustComparison, SettledRustComparison,
};
pub use preparation::{DiagnosticPreparation, PreparationResult};
pub use rust_cargo::{CargoTarget, RustExecutable};
pub use rust_case::{RustCase, RustCaseObservation, RustTarget};
pub use rust_inputs::{RustInputs, RustIntervention};
pub use trees::{PreparedDirectory, PreparedTrees, TreeIntervention};
pub(crate) use view::render as render_evidence;

#[cfg(test)]
mod tests;
