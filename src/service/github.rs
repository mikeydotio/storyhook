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

use crate::cli::ConflictSide;
use crate::domain::{Member, StateDef, StoryEvent, StorySnapshot};
use crate::error::AppError;
use crate::github::api::{GithubApi, GithubApiFactory};
use crate::github::client::GithubClient;
use crate::github::conflict::Resolution;
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
    ///
    /// `resolve` arrives as the wire's [`ConflictSide`] and is translated here,
    /// on the gated side of the feature boundary: the request envelope has to
    /// carry the choice in every build, while the merge engine's own
    /// [`Resolution`] only exists when `github-sync` is compiled in.
    /// `strategy`/`mode` are translated the same way, from
    /// [`crate::cli::SetupStrategy`]/[`crate::cli::SetupMode`] to their gated
    /// twins — see [`crate::github::initial::InitialStrategy`].
    /// The credential comes from the request envelope rather than from this
    /// process's environment (SH-153): the daemon's environment is whichever
    /// client's shell happened to start it, so reading it here answered the
    /// wrong person's question — and lent one caller's token to the next.
    pub fn sync(
        &self,
        story_id: Option<&str>,
        dry_run: bool,
        resolve: Option<ConflictSide>,
        strategy: Option<crate::cli::SetupStrategy>,
        mode: Option<crate::cli::SetupMode>,
    ) -> Result<Response, AppError> {
        let resolve = resolve.map(|side| match side {
            ConflictSide::Local => Resolution::KeepLocal,
            ConflictSide::Remote => Resolution::KeepRemote,
        });
        let strategy = strategy.map(|strategy| match strategy {
            crate::cli::SetupStrategy::ImportAll => {
                crate::github::initial::InitialStrategy::ImportAll
            }
            crate::cli::SetupStrategy::MatchTitles => {
                crate::github::initial::InitialStrategy::MatchTitles
            }
            crate::cli::SetupStrategy::PushOnly => {
                crate::github::initial::InitialStrategy::PushOnly
            }
            crate::cli::SetupStrategy::FutureOnly => {
                crate::github::initial::InitialStrategy::FutureOnly
            }
        });
        let mode = mode.map(|mode| match mode {
            crate::cli::SetupMode::Manual => crate::github::sync_state::SyncMode::Manual,
            crate::cli::SetupMode::Off => crate::github::sync_state::SyncMode::Off,
        });
        crate::github::run_sync_with(
            &StoreSyncStorage::new(self.ctx),
            &RealGithubApiFactory,
            self.ctx.github_token(),
            story_id,
            dry_run,
            resolve,
            strategy,
            mode,
        )
    }
}

/// [`GithubApiFactory`] backed by the real GitHub REST client.
pub struct RealGithubApiFactory;

impl GithubApiFactory for RealGithubApiFactory {
    fn build(&self, token: String, owner: String, repo: String) -> Box<dyn GithubApi> {
        Box::new(GithubClient::new(token, owner, repo))
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
                self.ctx.provenance(),
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

/// Re-derives a just-restored project's `github.owner`/`github.repo` from the
/// destination checkout's own git remote (SH-189).
///
/// # Why this cannot live in `import_project` itself
///
/// [`crate::service::transfer::import_project`] is unconditionally compiled —
/// `story import-project` has to work under `--no-default-features` — while
/// [`crate::github::sync_state::detect_github_remote`] and everything needed to interpret the
/// `github_sync` blob live entirely behind this crate's `github-sync` cargo
/// feature. So the blob rides through the restore's transaction completely
/// opaque (a verbatim copy — see `transfer::apply_settings`), and this
/// function runs the correction as a **second, non-transactional step**
/// immediately after, from the same `story import-project` call site, once the
/// feature that can read the blob is known to be compiled in. It mirrors the
/// codebase's existing precedent of the restore's pointer file being written
/// only after the transaction commits.
///
/// # Why a restored document cannot be trusted as-is
///
/// A document's `github.owner`/`github.repo` were correct for the checkout
/// that produced it. Restored into a different checkout — a fork, a
/// relocated clone — they name the *source* project, and nothing else in the
/// program will ever re-derive them: [`crate::github::run_sync_with`] only
/// calls [`crate::github::initial::run_initial_setup`] when a project is
/// unconfigured, and a restored project is configured the moment its blob
/// lands. This is the only point that can ever correct it.
///
/// # Best-effort, not a refusal
///
/// If `detect_github_remote` cannot name a GitHub project for this checkout
/// (no `origin`, a non-GitHub remote, or simply not yet configured — restoring
/// into a fresh directory before `git remote add origin` runs is ordinary),
/// the document's original owner/repo are left exactly as the restore wrote
/// them. Refusing to carry `github_sync` at all over one stale field would be
/// a worse asymmetry than the partial-carry harm SH-133 already rejected. The
/// safety net is `story doctor`'s advisory (SH-189), which re-compares the
/// *currently configured* owner/repo against the checkout on every run — not
/// a one-shot message here — so drift is caught for as long as it exists,
/// including drift introduced after this call returns.
///
/// A blob this build cannot parse as a [`GithubSyncConfig`] is left untouched
/// rather than treated as a failure: the restore that carried it has already
/// committed, and a malformed or foreign-shaped document is exactly the
/// condition `story doctor` exists to surface, not a reason to unwind data
/// that already landed.
pub fn reconcile_restored_github_remote<S: Store>(
    store: &S,
    root: &std::path::Path,
) -> Result<(), AppError> {
    // Not threaded through from the caller: `import_project` may have adopted
    // an *existing* empty project rather than created one, and the pointer
    // file it just wrote (or deliberately left alone, if one already named
    // this checkout) is the one place that says which project this checkout
    // now identifies.
    let Some(pointer) = super::project::read_pointer(root)? else {
        return Ok(());
    };
    let Some(project) = store.read(|tx| tx.project_by_uuid(&pointer.uuid))? else {
        return Ok(());
    };
    let Some(document) = store.read(|tx| Ok(tx.settings(project.id)?.github_sync))? else {
        return Ok(());
    };
    let Ok(mut config) = serde_json::from_value::<GithubSyncConfig>(document) else {
        return Ok(());
    };
    let Some(detected) = crate::github::sync_state::detect_github_remote(root)? else {
        return Ok(());
    };
    if detected.owner == config.github.owner && detected.repo == config.github.repo {
        return Ok(());
    }
    config.github = detected;
    let updated = serde_json::to_value(&config)?;
    Ok(store.write(|tx| {
        let mut row = tx.settings(project.id)?;
        row.github_sync = Some(updated);
        tx.put_settings(project.id, &row)
    })?)
}
