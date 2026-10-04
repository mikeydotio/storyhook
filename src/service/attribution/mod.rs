//! Causal diagnosis is evidence for repair ownership, never gate certification.

mod contrast;
pub(crate) mod holds;
mod model;
mod preparation;
mod rust_case;
mod validation;
mod view;
pub use contrast::classify;
pub use holds::AttributionHold;
pub use model::*;
pub use preparation::{DiagnosticPreparation, PreparationResult};
pub use rust_case::{RustCase, RustCaseObservation, RustTarget};
pub(crate) use view::render as render_evidence;

#[cfg(test)]
mod tests;
