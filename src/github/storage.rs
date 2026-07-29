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
//! into `crate::storage` and into files under `.storyhook/github-sync/`, which
//! made the whole feature un-portable without rewriting the merge engine beside
//! it. Here the engine takes a `&dyn SyncStorage` and does not know which side
//! of the flip it is on.
//!
//! `dyn`, not a generic parameter, on purpose: the engine's call graph is a
//! dozen functions deep and threading `S: Store` through all of them would
//! monomorphize the merge engine per store implementation for no benefit — this
//! is a network-bound command whose cost is measured in HTTP round trips.

use std::path::Path;

use crate::domain::{Member, StateDef, StoryEvent, StorySnapshot};
use crate::error::AppError;
use crate::storage;

use super::sync_state::{
    GithubSyncConfig, create_backup, load_base_snapshot, load_sync_config, save_base_snapshot,
    save_sync_config,
};

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

/// [`SyncStorage`] over a repository's `.storyhook/` directory — the
/// pre-rearchitecture layout.
///
/// Every method forwards to the function the engine used to call directly, so
/// this implementation is behaviour-preserving by construction.
pub struct LegacySyncStorage<'a> {
    root: &'a Path,
}

impl<'a> LegacySyncStorage<'a> {
    /// Storage for the project at `root`.
    pub fn new(root: &'a Path) -> Self {
        Self { root }
    }
}

impl SyncStorage for LegacySyncStorage<'_> {
    fn root(&self) -> &Path {
        self.root
    }

    fn now(&self) -> String {
        storage::now()
    }

    fn load_config(&self) -> Result<Option<GithubSyncConfig>, AppError> {
        load_sync_config(self.root)
    }

    fn save_config(&self, config: &GithubSyncConfig) -> Result<(), AppError> {
        save_sync_config(self.root, config)
    }

    fn load_base(&self, story_id: &str) -> Result<Option<StorySnapshot>, AppError> {
        load_base_snapshot(self.root, story_id)
    }

    fn save_base(&self, story_id: &str, snapshot: &StorySnapshot) -> Result<(), AppError> {
        save_base_snapshot(self.root, story_id, snapshot)
    }

    fn backup(&self, story_id: &str) -> Result<(), AppError> {
        create_backup(self.root, story_id)
    }

    fn states(&self) -> Result<Vec<StateDef>, AppError> {
        storage::load_states(self.root)
    }

    fn members(&self) -> Result<Vec<Member>, AppError> {
        storage::load_members(self.root)
    }

    fn prefix(&self) -> Result<String, AppError> {
        storage::load_project_prefix(self.root)
    }

    fn open_stories(&self) -> Result<Vec<StorySnapshot>, AppError> {
        storage::load_all_open_snapshots(self.root)
    }

    fn story(&self, story_id: &str) -> Result<StorySnapshot, AppError> {
        storage::load_open_story_snapshot(self.root, story_id)
    }

    fn create_story(&self, title: &str) -> Result<StorySnapshot, AppError> {
        storage::create_story(self.root, title, None)
    }

    fn write_events(&self, story_id: &str, events: &[StoryEvent]) -> Result<(), AppError> {
        storage::write_story_events(self.root, story_id, events)
    }
}
