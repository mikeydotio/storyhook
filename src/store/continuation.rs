//! Durable ownership and delivery evidence for autonomous context handoffs.
use super::{ProjectId, StoryNo};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Delivery state, independent of the story's configured workflow states.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContinuationStatus {
    /// Durable intent with no external effect in progress.
    Pending,
    /// An external effect may have happened; replay requires evidence.
    Attempting,
    /// Native feedback or absent-provider resume awaits the receiving review.
    AwaitingAck,
    /// The receiving root session reviewed current evidence.
    Acknowledged,
    /// Automatic delivery stopped with a durable diagnosis.
    NeedsAttention,
    /// Current story state makes this request inapplicable.
    Superseded,
}
impl ContinuationStatus {
    /// Whether submission must wait for this request to resolve.
    pub fn outstanding(self) -> bool {
        !matches!(self, Self::Acknowledged | Self::Superseded)
    }
}
/// The single lifecycle operation currently owning a handoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ContinuationPhase {
    /// Explicit recovery is waiting for a fresh ownership observation.
    Observe,
    /// The originating Stop owns native continuation feedback; no terminal input.
    NativeContinuation,
    /// A provider proven absent is resuming in retained resources.
    Resume,
    /// The receiving review acknowledged current evidence.
    Complete,
    /// The supervisor recorded a human obviation hold, without implementation approval.
    Administrative,
}
impl ContinuationPhase {
    /// Stable operational spelling shared with the provider adapters.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::NativeContinuation => "native-continuation",
            Self::Resume => "resume",
            Self::Complete => "complete",
            Self::Administrative => "administrative",
        }
    }
}
impl std::fmt::Display for ContinuationPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
/// A generation-bound delivery record. Capture retains provider-specific evidence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Continuation {
    /// Durable UUID identity.
    pub id: String,
    /// Owning project.
    pub project_id: ProjectId,
    /// Numeric story key.
    pub story_no: StoryNo,
    /// Canonical story identifier.
    pub story_id: String,
    /// Original strict handoff envelope.
    pub handoff: Value,
    /// Immutable originating provider, session and turn for idempotency.
    pub generation: Value,
    /// Runtime-validated lease, provider, process, session and Git evidence.
    pub capture: Value,
    /// Current delivery state.
    pub status: ContinuationStatus,
    /// Typed native-feedback, recovery, acknowledgement, or administrative phase.
    pub phase: ContinuationPhase,
    /// Compare-and-swap revision, independent of timestamps.
    pub revision: i64,
    /// Number of side-effect attempts, excluding read-only observations.
    pub attempts: u32,
    /// RFC3339 request time.
    pub created_at: String,
    /// RFC3339 last transition time.
    pub updated_at: String,
    /// Diagnostic explaining the most recent transition.
    pub detail: String,
    /// Receiving review's exact story sequence, when acknowledged.
    pub reviewed_seq: Option<i64>,
    /// Exact Git HEAD reviewed by the receiver.
    pub reviewed_head: Option<String>,
}
