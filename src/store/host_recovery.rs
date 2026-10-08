//! One durable machine-fault coordinator shared by projects in this store.
use serde::{Deserialize, Serialize};
/// A versioned host fault envelope. Native proofs, not this JSON, grant effects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostRecovery {
    /// Unique owner identity.
    pub id: String,
    /// Immutable digest of exact native authority, host, boot, policy and episode.
    pub fault_key: String,
    /// Compare-and-swap revision.
    pub revision: i64,
    /// Host-wide admission pause until native restoration is observed.
    pub active: bool,
    /// Retained subjects and release receipts; never executable authority.
    pub state: serde_json::Value,
}

/// Immutable original gate custody retained before a live broker can be read.
/// This observation cannot pause host admissions or authorize restoration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostRecoveryPending {
    /// Original attribution identity, also the idempotency key.
    pub id: String,
    /// Owning project, never inferred from a receipt's display fields.
    pub project: crate::store::ProjectId,
    /// Original submitted story.
    pub story: crate::store::StoryNo,
    /// Original author generation.
    pub generation: crate::store::GlobalSeq,
    /// Versioned original candidate and raw evidence custody.
    pub evidence: serde_json::Value,
}
