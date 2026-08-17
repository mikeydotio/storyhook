//! The layer where storyhook's invariants live.
//!
//! Between [`crate::invoke::dispatch`], which knows the shape of a CLI
//! command, and [`crate::store`], which knows how to persist bytes, sits this:
//! the rules about what a story *is*. A closed story's state, scope and rollups
//! cannot change, though an observation may still be appended to it (SH-261);
//! moving into a closed state clears what the story was awaiting and archives
//! it; a relation is asserted by both of its ends or by neither.
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

pub mod attachment;
pub mod catalog;
pub mod config;
pub mod git;
pub mod git_links;
#[cfg(feature = "github-sync")]
pub mod github;
pub mod github_setup;
pub mod grouping;
pub mod history;
pub mod integrity;
pub mod migrate;
#[cfg(feature = "github-sync")]
pub mod pr_check;
pub mod pr_link;
pub mod project;
pub mod query;
pub mod questionnaire;
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

use crate::domain::provenance::Provenance;
use crate::domain::{StateDef, StoryEvent, StorySnapshot, fold_story};
use crate::env::Environment;
use crate::error::AppError;
use crate::event_hooks::{self, HookEventType};
use crate::store::{
    ExpectedSeq, ProjectId, ReadOps, Store, StoreError, StoryNo, StoryRow, WriteOps,
    partition_known,
};

pub use attachment::{AttachmentService, MAX_ATTACHMENT_BYTES};
pub use catalog::{
    CatalogEntry, CatalogService, OriginFinding, OriginSweep, OrphanedRegistration, RecordedOrigin,
    UnregisteredOrigin,
};
pub use config::{ConfigService, StateEdit, StateListing};
pub use git::GitService;
pub use git_links::{CheckoutLink, GitLinkService, OriginLink, PointerOutcome};
#[cfg(feature = "github-sync")]
pub use github::{GithubSyncService, RealGithubApiFactory, StoreSyncStorage};
pub use grouping::{GroupingService, PhaseCleared};
pub use integrity::{Examination, FixOutcome, IntegrityService};
pub use migrate::{MigrationPlan, MigrationReport};
pub use pr_link::PrLinkService;
pub use project::{
    DeleteOutcome, InitOptions, InitOutcome, OriginOutcome, PointerUpdate, ProjectPointer,
    ProjectService, SetPrefixOutcome,
};
pub use query::{ListFilters, QueryService};
pub use relation::{RelationOutcome, RelationService};
pub use session::SessionService;
pub use settings::{SettingSpec, SettingsService, registry as settings_registry};
pub use story::{FieldEdits, NewStoryInput, StoryService};
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
    github_token: Option<crate::domain::secret::GithubToken>,
    provenance: Provenance,
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
            github_token: None,
            provenance: Provenance::unrecorded(),
        }
    }

    /// Supplies the standard input this invocation should read, instead of this
    /// process's own.
    ///
    /// The daemon does not have the client's terminal, so a command that reads
    /// stdin has it read on the client and carried in the request envelope. An
    /// in-process caller leaves this unset and reads its own.
    #[must_use]
    pub fn with_stdin(mut self, stdin: Option<String>) -> Self {
        self.stdin = stdin;
        self
    }

    /// Supplies the caller's GitHub credential.
    ///
    /// Here for the same reason [`with_stdin`](Self::with_stdin) is: the
    /// credential belongs to whoever ran the command, and the daemon's own
    /// environment belongs to whoever started the daemon (SH-153). An
    /// in-process caller that leaves this unset has supplied none, and a
    /// command that needs one refuses rather than looking elsewhere.
    #[must_use]
    pub fn with_github_token(
        mut self,
        github_token: Option<crate::domain::secret::GithubToken>,
    ) -> Self {
        self.github_token = github_token;
        self
    }

    /// Supplies who is performing this invocation's writes (SH-246).
    ///
    /// Here for the same reason [`with_stdin`](Self::with_stdin) and
    /// [`with_github_token`](Self::with_github_token) are: half of it — the
    /// declared actor — is a fact about the caller that the daemon's own
    /// environment cannot supply. An in-process caller that leaves this unset
    /// writes [`Provenance::unrecorded`], which is honest rather than merely
    /// permissive: a fixture appending events really was performed by nothing a
    /// user would recognise.
    #[must_use]
    pub fn with_provenance(mut self, provenance: Provenance) -> Self {
        self.provenance = provenance;
        self
    }

    /// Who is performing this invocation's writes.
    ///
    /// Every event a single command appends carries the same provenance by
    /// construction, which is why this is read off the context rather than
    /// passed from wherever an event happened to be built.
    #[must_use]
    pub fn provenance(&self) -> &Provenance {
        &self.provenance
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

    /// The caller's GitHub credential, if this invocation carried one.
    #[must_use]
    pub fn github_token(&self) -> Option<&crate::domain::secret::GithubToken> {
        self.github_token.as_ref()
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

/// Which stories a single-story write is allowed to reach.
///
/// Every write to one story states this at its call site rather than inheriting
/// it from whichever helper it happened to reach for. The distinction it draws
/// is the one the resolvers below encode, and SH-261 is where it was settled:
/// an **edit** changes what a story *is* and is refused once the story is
/// closed; an **append** records an observation about it and is not.
///
/// The line is not "which verb is this" but **what derived state can this write
/// touch** — the standard SH-207's council applied when it let a closed story be
/// a relation *target* only where the write provably could not reach
/// `compute_progress`. A comment reaches nothing but the comment list and
/// `updated_at`, which is what puts it on the append side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Intent {
    /// Changes the story's own fields, state or relationships. Refused once the
    /// story is closed — see [`resolve_open_story`].
    Edit,
    /// Records an observation about the story without changing what it is.
    /// Permitted on a closed story, refused on a deleted one — see
    /// [`resolve_appendable_story`]. Granted to two writes so far: `story
    /// comment` (SH-261 — a comment reaches only the comment list and
    /// `updated_at`) and `commit-sync`'s commit link (SH-279 —
    /// `StoryCommitLinked` reaches only `referenced_by_commits` and
    /// `updated_at`). `tests/invoker_seam.rs::only_the_comment_path_appends_to_a_closed_story`
    /// pins the exact set; a third write needs its own argument before it is
    /// added there.
    Append,
}

impl Intent {
    /// Resolves `id` under this intent, applying the guard that belongs to it.
    pub(crate) fn resolve(
        self,
        tx: &impl ReadOps,
        project: ProjectId,
        prefix: &str,
        id: &str,
    ) -> Result<(StoryNo, StoryRow), AppError> {
        match self {
            Self::Edit => resolve_open_story(tx, project, prefix, id),
            Self::Append => resolve_appendable_story(tx, project, prefix, id),
        }
    }
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
        return Err(AppError::Validation(closed_story_refusal(id)));
    }
    Ok((story_no, row))
}

/// What a closed story says when an edit is refused.
///
/// One constructor rather than a literal per site, because there were two hand-
/// copied copies of the previous sentence and they are the kind of thing that
/// drifts apart the moment one of them is corrected.
///
/// The sentence it replaced — *"story `<id>` is closed and cannot be
/// modified"* — was false, and had been since SH-43: `hide` and `unhide` modify
/// closed stories, and since SH-261 so does `comment`. A refusal that overstates
/// the rule teaches the wrong rule, and this one was quoted back as an invariant
/// (`docs/spec/dashboard-dispatch.md`) by a later design that then built around
/// it.
pub(crate) fn closed_story_refusal(id: &str) -> String {
    format!(
        "story `{id}` is closed; reopen it with `story reopen {id}` to change it — a comment needs no reopen"
    )
}

/// [`resolve_story`], rejecting a story that has been soft-deleted but
/// permitting one that is merely closed (SH-261).
///
/// **A closed story's log is already appendable**, which is what makes this the
/// smaller of the two guards rather than a hole in the larger one: `hide` and
/// `unhide` append to archived stories, `purge` appends relationship retractions
/// onto closed claimants, and `history::restore` appends compensating events to
/// anything. What was refused was not appending — it was appending *on a
/// person's behalf*.
///
/// The line stops at `deleted`, and deliberately: [`StoryService::purge`]
/// destroys every event on a soft-deleted story, so an observation recorded here
/// is evidence with an expiry date nothing warns its author about. A soft-deleted
/// story's futures are restoration and destruction, and `StoryService::delete`
/// already reports an *already*-deleted story as not found — so permitting a
/// comment on one would make a story writable that another verb insists does not
/// exist.
///
/// `hidden` is not consulted. It is a display fact layered on a closed story, so
/// a hidden story takes a comment and stays hidden.
///
/// A second caller reached this in SH-279 — `GitService::record_commit`,
/// linking a commit that names a closed story — on the identical argument: a
/// link reaches only `referenced_by_commits` and `updated_at`. Its refusal
/// message below still reads as `comment`-specific because that caller never
/// surfaces it verbatim; it re-reports the decline in `commit-sync`'s own
/// voice instead (see `NotMovedReason` and `DeclinedReason` in `git.rs`).
pub(crate) fn resolve_appendable_story(
    tx: &impl ReadOps,
    project: ProjectId,
    prefix: &str,
    id: &str,
) -> Result<(StoryNo, StoryRow), AppError> {
    let (story_no, row) = resolve_story(tx, project, prefix, id)?;
    if row.deleted {
        return Err(AppError::Validation(format!(
            "story `{id}` is deleted and cannot be commented on; \
             restore it first with `story reopen {id} --force`"
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
// Eight, since SH-246 added `provenance`. Every one of them is a distinct fact
// the caller alone holds, and the obvious bundling — a struct of
// project/prefix/states — would hide the transaction this deliberately keeps
// visible at all 29 call sites. Bundling is a refactor, and a refactor does not
// share a commit with a behaviour change.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_and_fold(
    tx: &mut impl WriteOps,
    project: ProjectId,
    story: StoryNo,
    prefix: &str,
    states: &BTreeMap<String, StateDef>,
    expected: ExpectedSeq,
    events: &[StoryEvent],
    provenance: &Provenance,
) -> Result<StorySnapshot, AppError> {
    // Every producer of a `StoryLabelsSet` is expected to normalize through
    // `domain::normalize_labels` before it gets here; this is the backstop
    // for the one that forgets, so a comma-bearing or blank label (SH-164)
    // cannot reach the store through this, the one path every service uses.
    for event in events {
        crate::domain::validate_event_for_append(event)?;
    }
    let head = tx.append_events(project, story, expected, events, provenance)?;
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
