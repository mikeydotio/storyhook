//! Causal diagnosis is evidence for repair ownership, never gate certification.

mod contrast;
mod model;
mod validation;
pub use contrast::classify;
pub use model::*;

#[cfg(test)]
mod tests;
