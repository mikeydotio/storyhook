//! Persistence envelopes for project-fault coordination and immutable observations.

use super::{GlobalSeq, ProjectId, StoryNo};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One durable coordinator; the service owns the versioned state schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRecovery {
    /// Stable recovery identity, independent of changing submissions.
    pub id: String,
    /// Proven owning project.
    pub project: ProjectId,
    /// Machine-readable fault class.
    pub code: String,
    /// Repository-relative configuration or command locus.
    pub locus: String,
    /// Compare-and-swap revision, starting at zero.
    pub revision: i64,
    /// Whether this coordinator still owns its fault identity.
    pub active: bool,
    /// Versioned service state, including decision authority and pending effects.
    pub state: Value,
}

/// An immutable unjudged submission and its original project-fault evidence.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectRecoveryObservation {
    /// Coordinator retaining this observation.
    pub recovery_id: String,
    /// Proven owning project, also used to scope attempt deduplication.
    pub project: ProjectId,
    /// Story whose submission was inspected.
    pub story: StoryNo,
    /// Exact transition into verification.
    pub generation: GlobalSeq,
    /// Verifier attempt identity; identical delivery is idempotent.
    pub attempt_id: String,
    /// RFC3339 observation time.
    pub observed_at: String,
    /// Versioned service evidence, including typed fault and source authority.
    pub evidence: Value,
}
