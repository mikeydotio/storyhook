//! Native host fault ownership and immutable-submission recovery.
//! Native host pressure coordination. Enrollment and release have distinct proofs.
mod native;
mod owner;
pub use native::{HostFaultEvidence, HostRestorationEvidence, observe_fault, observe_restoration};
pub(crate) use owner::status_snapshot;
pub use owner::{HostRecoveryService, HostRecoveryStatus, HostRecoveryView};
pub(crate) use owner::{blocks_admission, check_input, expected_head};

pub(crate) use native::{pending_subjects, retain_failed_pressure};
