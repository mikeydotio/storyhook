//! Durable single-submission integration ownership, independent of batches.
use super::{GlobalSeq, ProjectId, StoryNo};
use serde::{Deserialize, Serialize};

/// Immutable submission envelope with guarded versioned lifecycle state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationRecovery {
    /// Global owner identity, also naming its private integration branch.
    pub id: String,
    /// Explicit owning project.
    pub project: ProjectId,
    /// Original submitted story.
    pub story: StoryNo,
    /// Original submitted verification generation.
    pub generation: GlobalSeq,
    /// Monotonic compare-and-swap revision.
    pub revision: i64,
    /// Retains exclusive ownership until all effects/resources are reconciled.
    pub active: bool,
    /// Strict service lifecycle; serialized evidence cannot mint native proof.
    pub state: serde_json::Value,
}

/// Immutable original conflict custody awaiting native policy inspection.
/// This record is an observation, never permission to assemble or publish.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationPending {
    /// Exact original attribution identity.
    pub id: String,
    /// Owning project.
    pub project: ProjectId,
    /// Original submitted story.
    pub story: StoryNo,
    /// Original submitted generation.
    pub generation: GlobalSeq,
    /// Strict retained candidate/operator/evidence observation.
    pub evidence: serde_json::Value,
}

/// Immutable native clean-input readmission of an original held generation.
/// The receipt constrains later gates; it is neither a certificate nor an effect capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntegrationReadmission {
    /// Exact retained attribution identity; one release per original diagnostic.
    pub id: String,
    /// Original owning project.
    pub project: ProjectId,
    /// Original submitted story.
    pub story: StoryNo,
    /// Original submitted generation, never renewed by readmission.
    pub generation: GlobalSeq,
    /// Strict service evidence retained with the original attribution CAS.
    pub evidence: serde_json::Value,
}
