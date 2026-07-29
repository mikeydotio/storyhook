//! The store-backed half of GitHub sync.
//!
//! The merge engine in [`crate::github`] is untouched by the rearchitecture —
//! three-way merge, conflict resolution and field mapping have no opinion about
//! where a story lives. What moved is the storage underneath it, and this module
//! is the implementation of [`SyncStorage`] that puts it in the store:
//!
//! | The engine wants | Legacy | Store |
//! |---|---|---|
//! | sync configuration | `.storyhook/github-sync.toml` | `project_settings.github_sync` |
//! | per-story merge base | `.storyhook/github-sync/bases/<id>.json` | the `github_bases` table |
//! | a pre-sync backup | `.storyhook/github-sync/backups/<ts>/` | `$XDG_STATE_HOME/storyhook/github-sync/backups/<ts>/` |
//!
//! The backups leave the repository entirely, which is the point of the whole
//! exercise: a sync that rewrites a story's history should not also dirty the
//! user's working tree with a directory of `.jsonl` files they then have to
//! decide whether to commit.

use std::path::{Path, PathBuf};

use crate::domain::{Member, StateDef, StoryEvent, StorySnapshot};
use crate::error::AppError;
use crate::github::storage::SyncStorage;
use crate::github::sync_state::GithubSyncConfig;
use crate::output::Response;
use crate::store::{ExpectedSeq, ReadOps, Store, StoryNo, StoryQuery, WriteOps};

use super::story::{NewStoryInput, StoryService};
use super::{Clock, Ctx, append_and_fold, project_prefix, resolve_open_story};

/// GitHub sync over one project in one store.
pub struct GithubSyncService<'ctx, S: Store> {
    ctx: &'ctx Ctx<'ctx, S>,
}

impl<'ctx, S: Store> GithubSyncService<'ctx, S> {
    /// A sync service bound to `ctx`.
    pub fn new(ctx: &'ctx Ctx<'ctx, S>) -> Self {
        Self { ctx }
    }

    /// Runs a sync, optionally restricted to one story.
    pub fn sync(&self, story_id: Option<&str>, dry_run: bool) -> Result<Response, AppError> {
        crate::github::run_sync_with(&StoreSyncStorage::new(self.ctx), story_id, dry_run)
    }
}

/// [`SyncStorage`] backed by the store.
pub struct StoreSyncStorage<'ctx, S: Store> {
    ctx: &'ctx Ctx<'ctx, S>,
}

impl<'ctx, S: Store> StoreSyncStorage<'ctx, S> {
    /// Storage for the project `ctx` names, with backups under that context's
    /// state home.
    ///
    /// The destination comes from the context's [`crate::env::Environment`]
    /// rather than from the process environment, and that is load-bearing: an in-process
    /// caller cannot redirect a variable, and an integration test *is* an
    /// in-process caller, because `storyhook_test_support::TestEnv` isolates
    /// child processes only. Before the environment was a value, running the
    /// sync tests wrote into the developer's real `~/.local/state/storyhook`.
    pub fn new(ctx: &'ctx Ctx<'ctx, S>) -> Self {
        Self { ctx }
    }

    /// Where this storage writes its backups.
    fn resolve_backups_dir(&self) -> Result<PathBuf, AppError> {
        Ok(self.ctx.env().github_backups_dir())
    }

    /// A story's number under this project's prefix.
    fn story_no(&self, story_id: &str) -> Result<StoryNo, AppError> {
        let project = self.ctx.project();
        let prefix = self.ctx.store().read(|tx| project_prefix(tx, project))?;
        StoryNo::parse_id(&prefix, story_id)
            .map_err(|_| AppError::NotFound(format!("story `{story_id}` not found")))
    }
}

impl<S: Store> SyncStorage for StoreSyncStorage<'_, S> {
    fn root(&self) -> &Path {
        self.ctx.cwd()
    }

    fn now(&self) -> String {
        self.ctx.now()
    }

    fn load_config(&self) -> Result<Option<GithubSyncConfig>, AppError> {
        let project = self.ctx.project();
        let stored = self
            .ctx
            .store()
            .read(|tx| Ok(tx.settings(project)?.github_sync))?;
        stored
            .map(|value| {
                serde_json::from_value(value).map_err(|error| {
                    // `Storage`, matching what the legacy path's unparseable
                    // `github-sync.toml` produced: the exit code is pinned by
                    // the error-contract table.
                    AppError::Storage(format!(
                        "stored github-sync configuration is unreadable: {error}"
                    ))
                })
            })
            .transpose()
    }

    fn save_config(&self, config: &GithubSyncConfig) -> Result<(), AppError> {
        let project = self.ctx.project();
        let document = serde_json::to_value(config)?;
        // Read the whole settings row and write it back with one field
        // replaced. Safe here, unlike the pattern SH-49 punished, because
        // `ProjectSettings` is columns rather than a serialized document: there
        // are no fields this binary does not know about to drop.
        Ok(self.ctx.store().write(|tx| {
            let mut settings = tx.settings(project)?;
            settings.github_sync = Some(document);
            tx.put_settings(project, &settings)
        })?)
    }

    fn load_base(&self, story_id: &str) -> Result<Option<StorySnapshot>, AppError> {
        let project = self.ctx.project();
        let story_no = self.story_no(story_id)?;
        Ok(self
            .ctx
            .store()
            .read(|tx| tx.github_base(project, story_no))?)
    }

    fn save_base(&self, story_id: &str, snapshot: &StorySnapshot) -> Result<(), AppError> {
        let project = self.ctx.project();
        let story_no = self.story_no(story_id)?;
        Ok(self
            .ctx
            .store()
            .write(|tx| tx.put_github_base(project, story_no, snapshot))?)
    }

    /// Writes the story's event log to the state directory before the sync
    /// rewrites it.
    ///
    /// JSONL, one event per line — the same bytes the legacy backup copied out
    /// of `.storyhook/open/stories/<id>.jsonl`, so a restore is still a matter
    /// of reading the file rather than of understanding a new format.
    fn backup(&self, story_id: &str) -> Result<(), AppError> {
        let project = self.ctx.project();
        let story_no = self.story_no(story_id)?;
        let stored = self
            .ctx
            .store()
            .read(|tx| tx.events_for(project, story_no))?;
        if stored.is_empty() {
            return Err(AppError::NotFound(format!(
                "event log not found for story {story_id}"
            )));
        }

        let mut document = String::new();
        for event in &stored {
            match &event.payload {
                crate::store::StoredPayload::Known(known) => {
                    document.push_str(&serde_json::to_string(known)?);
                }
                // An event kind this binary does not understand is still the
                // user's data, and a backup that silently omitted it would be
                // worse than no backup at all.
                crate::store::StoredPayload::Unknown { json, .. } => document.push_str(json),
            }
            document.push('\n');
        }

        let directory = self
            .resolve_backups_dir()?
            .join(timestamp_dir(&self.ctx.now()));
        std::fs::create_dir_all(&directory)?;
        std::fs::write(directory.join(format!("{story_id}.jsonl")), document)?;
        Ok(())
    }

    fn states(&self) -> Result<Vec<StateDef>, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| tx.states(project))?)
    }

    fn members(&self) -> Result<Vec<Member>, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| tx.members(project))?)
    }

    fn prefix(&self) -> Result<String, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| project_prefix(tx, project))?)
    }

    fn open_stories(&self) -> Result<Vec<StorySnapshot>, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| {
            Ok(tx
                .stories(project, &StoryQuery::all().archived(false))?
                .into_iter()
                .map(|row| row.snapshot)
                .collect())
        })?)
    }

    fn story(&self, story_id: &str) -> Result<StorySnapshot, AppError> {
        let project = self.ctx.project();
        let story_no = self.story_no(story_id)?;
        Ok(self
            .ctx
            .store()
            .read(|tx| tx.story(project, story_no))?
            .ok_or_else(|| AppError::NotFound(format!("story `{story_id}` not found")))?
            .snapshot)
    }

    /// Creates a story **without firing the project's `create` hook**.
    ///
    /// The legacy path wrote the story straight through `storage::create_story`,
    /// which fired nothing; pulling fifty issues down from GitHub and firing
    /// fifty `create` hooks would be a new behaviour, not a port.
    fn create_story(&self, title: &str) -> Result<StorySnapshot, AppError> {
        let quiet = Ctx::new(
            self.ctx.store(),
            self.ctx.project(),
            self.ctx.cwd(),
            self.ctx.env().clone(),
        )
        .no_hooks(true)
        .clock(Clock::Fixed(self.ctx.now()));
        StoryService::new(&quiet).create(&NewStoryInput {
            title: title.to_string(),
            ..NewStoryInput::default()
        })
    }

    /// Appends events to a story and folds them in, in one transaction.
    ///
    /// [`ExpectedSeq::Any`], because the engine reads a story, talks to GitHub
    /// for as long as that takes, and writes back — a compare-and-swap against
    /// the head it read would fail on a hook that touched the story in between,
    /// which the legacy append could not even notice.
    fn write_events(&self, story_id: &str, events: &[StoryEvent]) -> Result<(), AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            let (story_no, _row) = resolve_open_story(&*tx, project, &prefix, story_id)?;
            append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Any,
                events,
            )?;
            Ok(())
        })?)
    }
}

/// One backup directory's name, derived from the sync's own timestamp.
///
/// `20260728T143000Z`, the shape the legacy backup used, so a directory listing
/// still sorts chronologically. Derived from the caller's clock rather than read
/// fresh, so every story backed up by one sync lands in one directory.
fn timestamp_dir(now: &str) -> String {
    now.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}
