//! Durable ownership and retry evidence for abandoned workspaces.
use super::{GlobalSeq, ProjectId, ResetPathIdentity, StoryNo};
use crate::domain::StoryCleanupLease;
use crate::service::resources::ResourceReport;
use serde::{Deserialize, Serialize};

/// Progress is persisted before each irreversible resource operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DroppedCleanupPhase {
    /// Resources are pinned; no process signal has been sent.
    Prepared,
    /// Process journal owns termination; retry must reconcile it.
    Stopping,
    /// The exact window and its captured writers are absent.
    Quiescent,
    /// Git removal may have started; retry checks its exact postconditions.
    Removing,
    /// Worktree path and registration are absent; branches were retained.
    Removed,
}

/// A drop generation's exact resources and durable cleanup progress.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DroppedCleanup {
    /// Project owning the operation.
    pub project: ProjectId,
    /// Story number within that project.
    pub story: StoryNo,
    /// Unique controller and process-journal identity.
    pub token: String,
    /// Exact transition into dropped, independent of subsequent comments.
    pub generation: GlobalSeq,
    /// Original resource authority; immutable within an operation.
    pub lease: StoryCleanupLease,
    /// Resource observations pinned before any signals or removal.
    pub resources: ResourceReport,
    /// Filesystem incarnations; replacements must never inherit authority.
    pub paths: Vec<ResetPathIdentity>,
    /// Kernel process incarnation, when an owned live pane exists.
    pub process_start: Option<String>,
    /// Last durably completed resource stage.
    pub phase: DroppedCleanupPhase,
    /// No cleanup child or uncertain writer remains under this operation.
    pub released: bool,
    /// Latest diagnostic, retained across retries.
    pub failure: Option<String>,
}
