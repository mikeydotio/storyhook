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
    /// Whether the reset finished: the story is released and its receipt final.
    pub completed: bool,
    /// The latest obstacle the reset is waiting out, cleared when it finishes.
    pub failure: Option<String>,
    /// Resources the reset left in place, with the reason for each (SH-886).
    #[serde(default)]
    pub residue: Vec<ResetResidue>,
    /// What the reset discarded that can still be found, recorded before removal.
    #[serde(default)]
    pub recovery: Option<ResetRecovery>,
    /// Who asked for the reset, replayed whenever the daemon resumes it.
    #[serde(default)]
    pub origin: ResetOrigin,
}

/// Who asked for a reset and what that request protects (SH-886).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetOrigin {
    /// The requester's tmux pane and socket: reset never closes that window.
    #[serde(default)]
    pub caller: crate::service::reset::ResetCaller,
    /// The requester's working directory: never removed; hooks run from it.
    #[serde(default)]
    pub cwd: Option<std::path::PathBuf>,
    /// Whether state-change hooks fire when the reset finishes.
    #[serde(default)]
    pub fire_hooks: bool,
    /// The requester's hook nesting depth.
    #[serde(default)]
    pub hook_depth: u32,
    /// Set when the reset adopted a reservation made by `story reset` before
    /// this upgrade: that request's `--force`. Its branch is kept, and a dirty
    /// or locked worktree is removed only when this is true (council C1).
    #[serde(default)]
    pub legacy_force: Option<bool>,
}

/// One resource a reset did not remove, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetResidue {
    /// The resource, named the way a person finds it: window, path or branch.
    pub resource: String,
    /// Why the reset left it in place.
    pub reason: String,
    /// Whether the next dispatch of the story would collide with it.
    #[serde(default)]
    pub blocks_dispatch: bool,
}

/// Recovery evidence captured before a reset removes local work.
///
/// Branch deletion and worktree removal also delete their reflogs, so without
/// this record unpushed commits would be recoverable but undetectable.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetRecovery {
    /// The local branch the reset deletes.
    pub branch: Option<String>,
    /// That branch's tip commit before deletion.
    pub tip: Option<String>,
    /// Commits on that tip that no other branch, tag or remote contains.
    pub unpushed: Option<u64>,
    /// Tracked paths with uncommitted changes that the reset discards.
    pub dirty: Option<u64>,
    /// Untracked paths that the reset discards.
    pub untracked: Option<u64>,
    /// The awaiting reason the reset cleared, recorded when it finished.
    #[serde(default)]
    pub cleared_awaiting: Option<String>,
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
