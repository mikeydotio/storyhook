//! Persisted overrides. Missing fields inherit independently.
use serde::{Deserialize, Serialize};

/// One stored model/effort override; absence means inherit, never clear a peer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DispatchPolicyOverride {
    /// Explicit automatic model override.
    pub model: Option<String>,
    /// Explicit reasoning effort override.
    pub effort: Option<String>,
}
