//! Preparation owns allowance before the control tree and contrast plan exist.
use serde::{Deserialize, Serialize};

/// One durable reservation for control-tree and detector preparation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticPreparation {
    /// UTC reservation time for diagnostics; never a monotonic duration substitute.
    pub started_at: String,
    /// Absent while preparation or its cleanup remains in flight.
    pub completed: Option<PreparationResult>,
}

/// Immutable preparation outcome, including work that failed to produce a control.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparationResult {
    /// Active monotonic time including owned process and workspace cleanup.
    pub milliseconds: u64,
    /// Retained raw preparation output.
    pub log: String,
    /// Established inputs or the originating failure with context.
    pub detail: String,
    /// Whether all owned preparation processes and resources are proved settled.
    pub cleanup_complete: bool,
}

impl DiagnosticPreparation {
    /// Preserve the reservation identity and every completed observation.
    pub(super) fn preserved_by(&self, next: &Self) -> bool {
        self.started_at == next.started_at
            && (self.completed.is_none() || self.completed == next.completed)
    }

    /// Execution completion alone cannot prove that owned resources were released.
    pub(super) fn unsettled(&self) -> bool {
        self.completed.as_ref().is_none_or(|r| !r.cleanup_complete)
    }
}

/// Immutable completion of the whole comparison's retained native resources.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticSettlement {
    /// UTC completion time; retained evidence, never deadline authority.
    pub completed_at: String,
    /// Full active diagnosis through cleanup, including all prior operations.
    pub milliseconds: u64,
    /// Explicit native cleanup result or originating errors.
    pub detail: String,
    /// Whether native resource settlement succeeded.
    pub cleanup_complete: bool,
}
