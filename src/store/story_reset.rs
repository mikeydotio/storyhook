//! Durable ownership of a card reset, independent of an engine run.
use super::{ProjectId, StoryNo};
use crate::service::resources::ResourceReport;
use serde::{Deserialize, Serialize};

/// A reset's durable operation identity and cleanup progress.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoryReset {
    /// Project owning the story.
    pub project: ProjectId,
    /// Story number within the project.
    pub story: StoryNo,
    /// Canonical public story identifier.
    pub story_id: String,
    /// Unique operation handle, retained during retry.
    pub token: String,
    /// State before reset; readiness is restored only after cleanup.
    pub original_state: String,
    /// Exact engine owners observed when reserving.
    pub lanes: Vec<ResetLane>,
    /// Pinned resource identity after outstanding dispatch has settled.
    pub resources: Option<ResourceReport>,
    /// Filesystem objects pinned with the resource report.
    #[serde(default)]
    pub paths: Vec<ResetPathIdentity>,
    /// Whether absence has been proved and finalization committed.
    pub completed: bool,
    /// Latest failed attempt, retained until an explicit retry.
    pub failure: Option<String>,
}

/// Identity of the one engine lane reserved with a story reset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetLane {
    /// Owning engine run.
    pub run_id: String,
    /// Lane index within the run.
    pub lane_index: u32,
}

/// A filesystem object that must retain its identity throughout cleanup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetPathIdentity {
    /// Canonical resource path.
    pub path: std::path::PathBuf,
    /// Filesystem device at reservation.
    pub device: u64,
    /// Inode at reservation.
    pub inode: u64,
    /// Cleanup is allowed to remove this object.
    pub removable: bool,
}
