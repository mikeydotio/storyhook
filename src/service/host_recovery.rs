//! Native host fault ownership and immutable-submission recovery.
//! Native host pressure coordination. Enrollment and release have distinct proofs.
mod native;
mod owner;
pub use native::{HostFaultEvidence, HostRestorationEvidence, observe_fault, observe_restoration};
pub use owner::{HostRecoveryService, HostRecoveryView};
pub(crate) use owner::{blocks_admission, check_input, expected_head};
