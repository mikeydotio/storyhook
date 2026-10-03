//! Causal diagnosis is evidence for repair ownership, never gate certification.

mod contrast;
mod model;
mod validation;
mod view;
pub use contrast::classify;
pub use model::*;
pub(crate) use view::render as render_evidence;

#[cfg(test)]
mod tests;
