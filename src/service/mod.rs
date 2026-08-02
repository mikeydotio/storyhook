//! The layer where storyhook's invariants live.
//!
//! Between [`crate::invoke::dispatch`], which knows the shape of a CLI
//! command, and [`crate::store`], which knows how to persist bytes, sits this:
//! the rules about what a story *is*. A story cannot be modified once it is
//! closed; moving into a closed state clears what the story was awaiting and
//! archives it; a relation is asserted by both of its ends or by neither.
//!
//! Under the previous design those rules lived wherever a command happened to
//! need them, which is why the state-transition batch was written out four
//! separate times and why a relation could be recorded on one story and lost
//! on the other. Here each rule is written once, and each of them is enforced
//! inside a single store transaction.
//!
//! # The shape every service method takes
//!
//! ```text
//! store.write(|tx| {          // one transaction …
//!     validate;               // … reading the project's ground truth
//!     append_events;          // … appending the batch
//!     fold + put_story;       // … and folding the result back in, atomically
//! })?;
//! fire hooks;                 // AFTER the commit, never inside it
//! read the view;              // a fresh read, so hooks' own writes are visible
//! ```
//!
//! Hooks firing after the commit is not an optimisation: a hook shells out to
//! `story`, which needs a second connection to the same database, and holding
//! a write transaction open across that is a deadlock with a five-second fuse.

pub mod catalog;
pub mod config;
pub mod git;
#[cfg(feature = "github-sync")]
pub mod github;
pub mod grouping;
pub mod history;
pub mod integrity;
pub mod migrate;
pub mod project;
pub mod query;
pub mod relation;
pub mod session;
pub mod settings;
mod state_set;
pub mod story;
pub mod system;
pub mod templates;
pub mod transfer;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::domain::{StateDef, StoryEvent, StorySnapshot, fold_story};
use crate::env::Environment;
use crate::error::AppError;
use crate::event_hooks::{self, HookEventType};
use crate::store::{
    ExpectedSeq, ProjectId, ReadOps, Store, StoreError, StoryNo, StoryRow, WriteOps,
    partition_known,
};

pub use catalog::{
    CatalogEntry, CatalogService, OrphanedRegistration, RegistryAdoption, adopt_legacy_registry,
    preferred_checkout,
};
pub use config::{ConfigService, StateEdit, StateListing};
pub use git::GitService;
#[cfg(feature = "github-sync")]
pub use github::{GithubSyncService, StoreSyncStorage};
pub use grouping::{GroupingService, PhaseCleared};
pub use integrity::IntegrityService;
pub use migrate::{MigrationPlan, MigrationReport};
pub use project::{
    DeinitOutcome, DeinitTarget, InitOptions, InitOutcome, ProjectPointer, ProjectService,
};
pub use query::{ListFilters, QueryService};
pub use relation::{RelationOutcome, RelationService};
pub use session::SessionService;
pub use settings::{SettingSpec, SettingsService, registry as settings_registry};
pub use story::{FieldEdits, NewStoryInput, ReopenOutcome, StoryService};
pub use system::SystemService;
pub use transfer::{ImportBatch, TransferService};

/// Where a service reads "now" from.
///
/// Every user-visible timestamp storyhook writes arrives as a parameter — the
/// store deliberately holds no clock — so this is the one place a service asks
/// what time it is. It exists as a value rather than a call to
/// [`chrono::Utc::now`] so that a test can pin it, which is what makes a
/// service's output comparable at all.
///
/// It lives on [`Environment`], which every [`Ctx`] carries — so pinning a
/// context's clock and pinning its data directory are the same gesture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Clock {
    /// The system clock, RFC3339 at second precision — the format every
    /// storyhook timestamp has always used.
    System,
    /// A fixed instant, for tests that assert on timestamps.
    Fixed(String),
}

impl Clock {
    /// The current time in storyhook's timestamp format.
    #[must_use]
    pub fn now(&self) -> String {
        match self {
            Self::System => chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            Self::Fixed(at) => at.clone(),
        }
    }
}

/// Everything a service needs beyond its arguments: which store, which
/// project, and the execution context the invocation arrived with.
///
/// Borrowing the store rather than owning it is what lets one long-lived store
/// serve a context per request. `Ctx` is cheap to build and is expected to be
/// rebuilt per invocation.
pub struct Ctx<'a, S: Store> {
    store: &'a S,
    project: ProjectId,
    no_hooks: bool,
    hook_depth: u32,
    cwd: PathBuf,
    env: Environment,
    stdin: Option<String>,
}

impl<'a, S: Store> Ctx<'a, S> {
    /// A context for `project` in `store`, run from `cwd` under `env`, with
    /// hooks enabled at depth zero.
    ///
    /// The environment is a parameter rather than something this constructor
    /// resolves, and that is the whole point of it: a service that reads a
    /// global path from the process environment cannot be redirected by an
    /// in-process caller, which is how two waves of this program wrote into the
    /// developer's real home directory.
    pub fn new(
        store: &'a S,
        project: ProjectId,
        cwd: impl Into<PathBuf>,
        env: Environment,
    ) -> Self {
        Self {
            store,
            project,
            no_hooks: false,
            hook_depth: 0,
            cwd: cwd.into(),
            env,
            stdin: None,
        }
    }

    /// Supplies the standard input this invocation should read, instead of this
    /// process's own.
    ///
    /// The daemon does not have the client's terminal, so a command that reads
    /// stdin has it read on the client and carried in the request envelope. A
    /// `--local` run leaves this unset and reads its own.
    #[must_use]
    pub fn with_stdin(mut self, stdin: Option<String>) -> Self {
        self.stdin = stdin;
        self
    }

    /// Suppresses the project's event hooks, as `--no-hooks` does.
    #[must_use]
    pub fn no_hooks(mut self, no_hooks: bool) -> Self {
        self.no_hooks = no_hooks;
        self
    }

    /// Sets how deep inside a hook this invocation is running.
    ///
    /// At depth 1 or more no further hooks fire. That is the whole loop
    /// prevention: a hook that shells out to `story` must not fire the hook
    /// that spawned it. The CLI reads the depth from `STORYHOOK_HOOK_DEPTH`;
    /// carrying it here rather than reading the environment inside the service
    /// is what makes the guard testable, because tests in one binary share a
    /// process and therefore share its environment.
    #[must_use]
    pub fn hook_depth(mut self, hook_depth: u32) -> Self {
        self.hook_depth = hook_depth;
        self
    }

    /// Sets the clock this context's timestamps come from.
    #[must_use]
    pub fn clock(mut self, clock: Clock) -> Self {
        self.env = self.env.clock(clock);
        self
    }

    /// The environment this invocation runs under — where the store, the state
    /// home and the backups are, and what time it is.
    #[must_use]
    pub fn env(&self) -> &Environment {
        &self.env
    }

    /// The store this context works against.
    pub fn store(&self) -> &'a S {
        self.store
    }

    /// The project every operation is scoped to.
    #[must_use]
    pub fn project(&self) -> ProjectId {
        self.project
    }

    /// The directory the invocation was made from.
    ///
    /// Still the project root as far as hooks are concerned: `hooks.toml` and
    /// the hook's own working directory both come from here.
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// How deep inside a hook this invocation is running.
    #[must_use]
    pub fn depth(&self) -> u32 {
        self.hook_depth
    }

    /// The standard input this invocation should read, if the caller supplied
    /// it rather than leaving this process to read its own.
    #[must_use]
    pub fn stdin(&self) -> Option<&str> {
        self.stdin.as_deref()
    }

    /// The current time, from this context's [`Clock`].
    #[must_use]
    pub fn now(&self) -> String {
        self.env.now()
    }

    /// Whether this invocation may fire event hooks at all.
    #[must_use]
    pub fn hooks_enabled(&self) -> bool {
        !self.no_hooks && self.hook_depth == 0
    }

    /// Fires one event hook, if this invocation is allowed to and the project
    /// configures one.
    ///
    /// Call this only after the transaction that produced the event has
    /// committed: the hook is an arbitrary shell command that frequently calls
    /// back into `story`, and a hook running inside the write transaction that
    /// spawned it would wait on a lock its own parent holds.
    pub(crate) fn fire_hook(&self, event: HookEventType, payload: &serde_json::Value) {
        if !self.hooks_enabled() {
            return;
        }
        let Some(config) = event_hooks::load_hooks_config(&self.cwd) else {
            return;
        };
        event_hooks::fire_hook(
            &self.cwd,
            &config,
            event,
            &payload.to_string(),
            self.hook_depth,
        );
    }

    /// The full [`crate::output::StoryView`] response for one story, read
    /// fresh.
    ///
    /// Deliberately its own read transaction, taken after any hooks have run:
    /// a hook may itself have written to the story, and the legacy path built
    /// its view after firing for exactly that reason.
    pub fn story_view(&self, id: &str) -> Result<crate::output::Response, AppError> {
        self.store
            .read(|tx| Ok(query::story_view(tx, self.project, id)))?
    }
}

/// A story's number and its read-model row, resolved from the id a user typed.
///
/// A story id that does not parse under the project's prefix is reported as
/// *not found* rather than as invalid input, because that is what it is from
/// the user's point of view and what the legacy path has always said.
pub(crate) fn resolve_story(
    tx: &impl ReadOps,
    project: ProjectId,
    prefix: &str,
    id: &str,
) -> Result<(StoryNo, StoryRow), AppError> {
    let not_found = || AppError::NotFound(format!("story `{id}` not found"));
    let story_no = StoryNo::parse_id(prefix, id).map_err(|_| not_found())?;
    let row = tx.story(project, story_no)?.ok_or_else(not_found)?;
    Ok((story_no, row))
}

/// [`resolve_story`], rejecting a story that has been closed.
///
/// The archived flag is the test, not the superstate: a story whose state slug
/// happens to belong to a closed state but which was never archived is still
/// editable, which is the behaviour the open-directory-versus-archive split
/// used to provide by accident.
pub(crate) fn resolve_open_story(
    tx: &impl ReadOps,
    project: ProjectId,
    prefix: &str,
    id: &str,
) -> Result<(StoryNo, StoryRow), AppError> {
    let (story_no, row) = resolve_story(tx, project, prefix, id)?;
    if row.archived {
        return Err(AppError::Validation(format!(
            "story `{id}` is closed and cannot be modified"
        )));
    }
    Ok((story_no, row))
}

/// A project's story-id prefix.
pub(crate) fn project_prefix(tx: &impl ReadOps, project: ProjectId) -> Result<String, StoreError> {
    Ok(tx
        .project(project)?
        .ok_or_else(|| StoreError::NotFound(format!("project {project} does not exist")))?
        .prefix)
}

/// Appends `events` to a story and folds the result into the read model,
/// **inside the caller's transaction**.
///
/// This is the store's mandated write pattern, written once. The store does
/// not fold on a caller's behalf — there is one definition of what a story is,
/// and it is [`fold_story`] — so every writer has to append, re-read, fold,
/// and put the snapshot back. What must never vary is that all four happen in
/// one transaction; the caller supplies that transaction, so the atomicity is
/// still visible at every call site.
pub(crate) fn append_and_fold(
    tx: &mut impl WriteOps,
    project: ProjectId,
    story: StoryNo,
    prefix: &str,
    states: &BTreeMap<String, StateDef>,
    expected: ExpectedSeq,
    events: &[StoryEvent],
) -> Result<StorySnapshot, AppError> {
    let head = tx.append_events(project, story, expected, events)?;
    let stored = tx.events_for(project, story)?;
    let (known, _unknown) = partition_known(story, &stored);
    let snapshot = fold_story(&story.to_id(prefix), &known, states)?;
    tx.put_story(project, &snapshot, head)?;
    Ok(snapshot)
}

/// Re-derives one story's read model from the history it already has.
///
/// [`append_and_fold`] without the append. There is exactly one reason to
/// need it: a story's row is a fold of its events *against the project's
/// state definitions*, so changing a definition can invalidate a row without
/// anything having happened to that story. Nothing else may use it — a fold
/// that is not preceded by an append is otherwise a sign that a caller is
/// papering over a row it should have written correctly the first time.
pub(crate) fn refold_story(
    tx: &mut impl WriteOps,
    project: ProjectId,
    story: StoryNo,
    prefix: &str,
    states: &BTreeMap<String, StateDef>,
) -> Result<StorySnapshot, AppError> {
    let head = tx.head_seq(project, story)?;
    let stored = tx.events_for(project, story)?;
    let (known, _unknown) = partition_known(story, &stored);
    let snapshot = fold_story(&story.to_id(prefix), &known, states)?;
    tx.put_story(project, &snapshot, head)?;
    Ok(snapshot)
}
