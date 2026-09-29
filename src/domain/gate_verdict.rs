//! How one verification gate ended (SH-830, SH-831).
//!
//! The verifier records a gate's verdict beside the batch preview that
//! preceded it and on the batch record whose tree it judged. Both records are
//! read back by people and later analysis, so the verdict has one set of
//! stable wire slugs.

use serde::{Deserialize, Serialize};

/// How a gate ended, including the ways it can end without judging anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateVerdict {
    /// The exact merge tree passed the gate.
    Certified,
    /// The exact merge tree failed the gate.
    TestsFailed,
    /// The pull request does not merge into its base.
    Conflict,
    /// The submission cannot be acted on.
    InvalidSubmission,
    /// The project's own gate configuration is at fault.
    ProjectFault,
    /// GitHub, Git, credentials or the verifier failed independently of the code.
    InfrastructureFailure,
    /// The gate was cancelled before it judged anything.
    Cancelled,
    /// A repair admission was refused before any gate operation.
    RepairDeferred,
    /// The gate answered, but its cleanup could not establish quiescence.
    CleanupFailed,
    /// The attempt lost its authority (a resubmission or a withdrawal).
    Withdrawn,
    /// An operator stopped the attempt while its gate ran.
    Interrupted,
    /// The verifier could not observe the gate's outcome.
    Error,
}
