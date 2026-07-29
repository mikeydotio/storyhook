//! The seam between the sync engine and wherever story data happens to live.
//!
//! `src/github/`'s interesting half — the three-way merge, the conflict
//! resolver, the field mapping — has no opinion about storage. It needs eight
//! things: the sync configuration, a per-story base snapshot to merge against, a
//! backup before it rewrites anything, the project's catalog, its open stories,
//! and the ability to create a story and append events to one. Everything else
//! it does is HTTP.
//!
//! Before this trait existed those eight things were twenty-four direct calls
//! into the pre-rearchitecture `crate::storage` and into files under
//! `.storyhook/github-sync/`, which made the whole feature un-portable without
//! rewriting the merge engine beside it. The seam was introduced with two
//! implementations so that the port was provably behaviour-preserving; the
//! legacy one died with the directory layout it addressed, and
//! [`crate::service::github::StoreSyncStorage`] is the only implementation
//! left.
//!
//! `dyn`, not a generic parameter, on purpose: the engine's call graph is a
//! dozen functions deep and threading `S: Store` through all of them would
//! monomorphize the merge engine per store implementation for no benefit — this
//! is a network-bound command whose cost is measured in HTTP round trips.

use std::path::Path;

use crate::domain::{Member, StateDef, StoryEvent, StorySnapshot};
use crate::error::AppError;

use super::sync_state::GithubSyncConfig;

/// Everything the GitHub sync engine needs from storage.
pub trait SyncStorage {
    /// The checkout the sync is running from.
    ///
    /// Still needed after the flip: the repository is where `git remote get-url
    /// origin` is asked which GitHub project this is.
    fn root(&self) -> &Path;

    /// The current time, in storyhook's timestamp format.
    fn now(&self) -> String;

    /// The project's github-sync configuration, or `None` if it has never been
    /// set up.
    fn load_config(&self) -> Result<Option<GithubSyncConfig>, AppError>;

    /// Replaces the project's github-sync configuration.
    fn save_config(&self, config: &GithubSyncConfig) -> Result<(), AppError>;

    /// The snapshot this story was in the last time it synced — the base of the
    /// three-way merge.
    fn load_base(&self, story_id: &str) -> Result<Option<StorySnapshot>, AppError>;

    /// Records the snapshot to merge against next time.
    fn save_base(&self, story_id: &str, snapshot: &StorySnapshot) -> Result<(), AppError>;

    /// Preserves a story's history before the sync rewrites any of it.
    fn backup(&self, story_id: &str) -> Result<(), AppError>;

    /// The project's states, in configured order.
    fn states(&self) -> Result<Vec<StateDef>, AppError>;

    /// The project's members.
    fn members(&self) -> Result<Vec<Member>, AppError>;

    /// The project's story-id prefix.
    fn prefix(&self) -> Result<String, AppError>;

    /// Every open story in the project.
    fn open_stories(&self) -> Result<Vec<StorySnapshot>, AppError>;

    /// One open story.
    fn story(&self, story_id: &str) -> Result<StorySnapshot, AppError>;

    /// Creates a story with `title` in the project's default open state,
    /// returning it.
    fn create_story(&self, title: &str) -> Result<StorySnapshot, AppError>;

    /// Appends events to a story.
    fn write_events(&self, story_id: &str, events: &[StoryEvent]) -> Result<(), AppError>;
}
