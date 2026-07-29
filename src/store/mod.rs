//! The event-sourced store: one global database, project-scoped rows.
//!
//! # What this replaces
//!
//! Story data used to live inside each repository, in `.storyhook/`: per-story
//! JSONL event logs, a SQLite archive of closed stories, a `next-id` counter
//! file, and TOML configuration — all version-controlled, all branched. Three
//! consequences followed, and all three are answered here rather than worked
//! around:
//!
//! - every checkout was an independent database, so a worktree silently
//!   diverged from the repository it came from → a project now has *many*
//!   paths and one identity;
//! - the id counter was a file two branches could each read and each
//!   increment, which twice minted the same story number for different
//!   stories → numbers are now allocated by `UPDATE … RETURNING` inside the
//!   transaction that uses them;
//! - invariants were checked by a `doctor` command after the fact → the
//!   schema now rejects the writes that used to create them.
//!
//! # Shape
//!
//! [`Store`] hands out transactions through closures. A read closure receives
//! something implementing [`ReadOps`]; a write closure receives something
//! implementing [`WriteOps`], which is a superset. Both are generic associated
//! types, so no allocation and no `dyn` is involved and the concrete engine
//! stays statically known:
//!
//! ```no_run
//! use storyhook::store::{ReadOps, SqliteStore, Store, StoryQuery};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let store = SqliteStore::open("/tmp/example/store.db")?;
//! store.migrate()?;
//! let projects = store.read(|tx| tx.projects())?;
//! for project in projects {
//!     let stories = store.read(|tx| tx.stories(project.id, &StoryQuery::all()))?;
//!     println!("{} has {} stories", project.slug, stories.len());
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Callers are generic over `S: Store`, never `dyn Store`: there is exactly one
//! implementation live in a process, and a second engine (the design admits
//! Postgres later) would be selected by an enum that delegates, not by dynamic
//! dispatch. No type in any signature here is a rusqlite type — that rule is
//! what keeps the second engine possible.
//!
//! # What deliberately is *not* here
//!
//! **The fold.** [`ReadOps::events_for`] returns events;
//! [`crate::domain::fold_story`] turns them into a snapshot; the caller writes
//! the result back with [`WriteOps::put_story`]. Keeping the fold out of the
//! store means there is exactly one definition of what a story is, and it means
//! the "read model updated in the same transaction as its events" rule is
//! visible at the call site rather than hidden behind a storage method. The one
//! exception is [`diff_read_model`], whose entire job is to fold independently
//! and disagree.

pub mod conformance;
pub mod error;
pub mod fault;
pub mod ids;
pub mod migrate;
pub mod rebuild;
pub mod sqlite;
#[cfg(feature = "fault-injection")]
pub mod test_support;
pub mod types;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::domain::{Member, StateDef, StoryEvent, StorySnapshot, TypeDef};

pub use conformance::ConformanceFixture;
pub use error::StoreError;
pub use fault::FaultPoint;
pub use ids::{EventSeq, ExpectedSeq, GlobalSeq, PathKind, ProjectId, StoryNo};
pub use migrate::{MIGRATIONS, Migration, current_schema_version};
pub use rebuild::{
    Divergence, ReadModelDiff, RebuiltStory, RepairReport, diff_read_model, rebuild_read_model,
    repair_read_model,
};
pub use sqlite::{SqliteReadTx, SqliteStore, SqliteWriteTx, StoreConfig};
pub use types::{
    FeedEvent, MigrationReport, NewProject, ProjectPathRecord, ProjectRecord, ProjectSettings,
    RawEvent, RelationEdge, StoredEvent, StoredPayload, StoryQuery, StoryRow, StorySort,
    UnknownEventDiagnostic, partition_known,
};

/// A transactional store of projects, events, and the read model folded from
/// them.
///
/// Transactions are handed to closures rather than returned, so that a caller
/// cannot hold one open across an await point, a user prompt, or a network
/// call — and so that commit and rollback are decided by this trait rather than
/// by whether every path through a caller remembered.
pub trait Store: Send + Sync + 'static {
    /// The read transaction this store hands out.
    type ReadTx<'a>: ReadOps
    where
        Self: 'a;

    /// The write transaction this store hands out.
    type WriteTx<'a>: WriteOps
    where
        Self: 'a;

    /// Runs `f` inside a read transaction — one consistent snapshot for the
    /// duration, however many statements it issues.
    fn read<T>(
        &self,
        f: impl FnOnce(&Self::ReadTx<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError>;

    /// Runs `f` inside a write transaction, committing if it returns `Ok` and
    /// rolling back otherwise.
    ///
    /// Rollback is by construction rather than by convention: the transaction
    /// rolls itself back when dropped, so an early return, an error, or a panic
    /// all leave the database untouched.
    fn write<T>(
        &self,
        f: impl FnOnce(&mut Self::WriteTx<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError>;

    /// Brings the database up to the schema version this binary understands,
    /// taking a verified backup first if anything is pending.
    fn migrate(&self) -> Result<MigrationReport, StoreError>;

    /// A value that changes whenever *another* connection commits.
    ///
    /// The daemon polls this to notice writes it did not make itself — a git
    /// hook running `story --local`, a second machine, a developer with a
    /// `sqlite3` prompt open. Comparing two tokens answers "has anything
    /// changed"; the value itself means nothing and must not be persisted or
    /// compared across process restarts.
    ///
    /// Engine-neutral by design: SQLite answers with `PRAGMA data_version`, and
    /// an eventual Postgres implementation would answer with a transaction id or
    /// a notification counter.
    fn change_token(&self) -> Result<u64, StoreError>;

    /// Writes a verified copy of this store into `dir` and returns its path.
    ///
    /// Verified means the copy is reopened and `integrity_check`ed before this
    /// returns: a backup that is only discovered to be corrupt when it is needed
    /// is worse than no backup, because it was believed in.
    fn snapshot(&self, dir: &Path) -> Result<PathBuf, StoreError>;
}

/// Everything that can be read inside a transaction.
///
/// Every method takes a [`ProjectId`]. That is a deliberate ergonomic cost: in
/// a single global database where every repository defaults to the prefix
/// `SH`, an unscoped query does not fail — it quietly returns a different
/// project's story with the same number. Making the scope a required argument
/// turns that whole class of bug into a compile error.
pub trait ReadOps {
    /// The project with this id.
    fn project(&self, project: ProjectId) -> Result<Option<ProjectRecord>, StoreError>;

    /// The project with this uuid — the identity a repository's pointer file
    /// carries.
    fn project_by_uuid(&self, uuid: &str) -> Result<Option<ProjectRecord>, StoreError>;

    /// The project with this slug — the identity the legacy registry called an
    /// id.
    fn project_by_slug(&self, slug: &str) -> Result<Option<ProjectRecord>, StoreError>;

    /// The project registered at this checkout directory.
    ///
    /// `path` is matched as stored: canonicalization belongs to the caller,
    /// which is the layer that knows whether the directory still exists.
    fn project_by_path(&self, path: &Path) -> Result<Option<ProjectRecord>, StoreError>;

    /// Every project, ordered by slug.
    fn projects(&self) -> Result<Vec<ProjectRecord>, StoreError>;

    /// Every checkout of a project, ordered by path.
    fn project_paths(&self, project: ProjectId) -> Result<Vec<ProjectPathRecord>, StoreError>;

    /// A project's states, in configured order.
    fn states(&self, project: ProjectId) -> Result<Vec<StateDef>, StoreError>;

    /// A project's states keyed by slug — the shape
    /// [`crate::domain::fold_story`] takes.
    fn state_map(&self, project: ProjectId) -> Result<BTreeMap<String, StateDef>, StoreError>;

    /// A project's story types, in configured order.
    fn types(&self, project: ProjectId) -> Result<Vec<TypeDef>, StoreError>;

    /// A project's members, ordered by member id.
    fn members(&self, project: ProjectId) -> Result<Vec<Member>, StoreError>;

    /// A project's settings. A project that has never had settings written
    /// reads back as [`ProjectSettings::default`].
    fn settings(&self, project: ProjectId) -> Result<ProjectSettings, StoreError>;

    /// A story's events, in order.
    ///
    /// Events whose kind this binary does not recognise come back as
    /// [`StoredPayload::Unknown`] carrying their original JSON, never as an
    /// error: an unknown kind must not be able to fail a read (SH-54).
    fn events_for(
        &self,
        project: ProjectId,
        story: StoryNo,
    ) -> Result<Vec<StoredEvent>, StoreError>;

    /// A story's current head sequence, or [`EventSeq::ZERO`] if it has no
    /// events.
    fn head_seq(&self, project: ProjectId, story: StoryNo) -> Result<EventSeq, StoreError>;

    /// The project's change feed after `after`, up to `limit` events.
    fn events_since(
        &self,
        project: ProjectId,
        after: GlobalSeq,
        limit: u32,
    ) -> Result<Vec<FeedEvent>, StoreError>;

    /// The project's newest change-feed position, or [`GlobalSeq::ZERO`].
    fn max_global_seq(&self, project: ProjectId) -> Result<GlobalSeq, StoreError>;

    /// One story's read-model row.
    fn story(&self, project: ProjectId, story: StoryNo) -> Result<Option<StoryRow>, StoreError>;

    /// The stories matching `query`.
    fn stories(&self, project: ProjectId, query: &StoryQuery) -> Result<Vec<StoryRow>, StoreError>;

    /// The edges this story owns.
    fn relations_from(
        &self,
        project: ProjectId,
        story: StoryNo,
    ) -> Result<Vec<RelationEdge>, StoreError>;

    /// The edges pointing at this story.
    ///
    /// Backed by its own index, so "what blocks this?" and "what are this
    /// story's children?" are lookups rather than the full scans that
    /// `is_ready`, `graph`, and `next` used to perform.
    fn relations_to(
        &self,
        project: ProjectId,
        story: StoryNo,
    ) -> Result<Vec<RelationEdge>, StoreError>;

    /// The last snapshot github-sync merged against, if any.
    fn github_base(
        &self,
        project: ProjectId,
        story: StoryNo,
    ) -> Result<Option<StorySnapshot>, StoreError>;
}

/// Everything that can be written inside a transaction.
pub trait WriteOps: ReadOps {
    /// Creates a project and returns its id.
    fn create_project(&mut self, project: &NewProject) -> Result<ProjectId, StoreError>;

    /// Records that this checkout belongs to this project, refreshing its
    /// last-seen timestamp.
    fn touch_project_path(
        &mut self,
        project: ProjectId,
        path: &Path,
        kind: PathKind,
    ) -> Result<(), StoreError>;

    /// Forgets one checkout of a project, reporting whether there was one.
    ///
    /// The project row and its stories survive. A checkout that has been
    /// deleted, moved, or taken off the dashboard is not a reason to lose the
    /// work recorded against it — which is exactly the mistake the legacy
    /// registry made impossible to make only because it held no data.
    fn forget_project_path(&mut self, project: ProjectId, path: &Path) -> Result<bool, StoreError>;

    /// Sets a project's display name.
    ///
    /// The catalog *is* the projects table, so `story web register --name` has
    /// nowhere else to record what the user called this project. Without it the
    /// flag is accepted and silently dropped — which is what the legacy
    /// registry, a file with a `name` field per repo, did not do.
    fn rename_project(&mut self, project: ProjectId, name: &str) -> Result<(), StoreError>;

    /// Allocates the next story number for a project.
    ///
    /// The counter moves inside this transaction, so a rollback returns the
    /// number to the pool and two concurrent writers cannot receive the same
    /// one. This single operation is what ends the id collisions that twice
    /// corrupted this repository's own tracker.
    fn allocate_story_no(&mut self, project: ProjectId) -> Result<StoryNo, StoreError>;

    /// Raises a project's story-number counter so that nothing at or below
    /// `highest` will ever be allocated.
    ///
    /// The importer is why this exists: a project restored from an export
    /// document has its story numbers dictated by the document, so nothing
    /// allocated them, and without this the next `story new` would mint an id
    /// that already exists. It only ever moves the counter *up* — a caller
    /// writing an old story into a live project must not walk it backwards.
    fn reserve_story_no(&mut self, project: ProjectId, highest: StoryNo) -> Result<(), StoreError>;

    /// Appends events to a story, failing with [`StoreError::Conflict`] if its
    /// head is not what `expected` requires.
    ///
    /// Returns the story's new head.
    fn append_events(
        &mut self,
        project: ProjectId,
        story: StoryNo,
        expected: ExpectedSeq,
        events: &[StoryEvent],
    ) -> Result<EventSeq, StoreError>;

    /// [`append_events`](Self::append_events), for a caller holding an event's
    /// bytes rather than a decoded [`StoryEvent`].
    ///
    /// Same compare-and-swap, same sequencing; the difference is that the
    /// payload is written exactly as given. The legacy importer needs it — a
    /// byte-identical round trip has to preserve event kinds this binary does
    /// not know — and it is how the unknown-kind path is tested. Prefer
    /// `append_events` everywhere else: it derives `kind` and `at` from the
    /// payload, so the three cannot disagree.
    fn append_raw_events(
        &mut self,
        project: ProjectId,
        story: StoryNo,
        expected: ExpectedSeq,
        events: &[RawEvent],
    ) -> Result<EventSeq, StoreError>;

    /// Writes a folded snapshot into the read model, recording the event `head`
    /// it was folded from.
    ///
    /// The snapshot's `id` decides which story is written — and is validated
    /// against the project's prefix, so a snapshot cannot be filed under the
    /// wrong project. Labels and relations are derived from it; the mirror of
    /// each relation is materialized by the schema, so writing one side of a
    /// bidirectional relation without the other is not an operation this API
    /// offers.
    fn put_story(
        &mut self,
        project: ProjectId,
        snapshot: &StorySnapshot,
        head: EventSeq,
    ) -> Result<(), StoreError>;

    /// Replaces a project's state set, in the order given.
    fn put_states(&mut self, project: ProjectId, states: &[StateDef]) -> Result<(), StoreError>;

    /// Replaces a project's type set, in the order given.
    fn put_types(&mut self, project: ProjectId, types: &[TypeDef]) -> Result<(), StoreError>;

    /// Adds or updates one member.
    fn put_member(&mut self, project: ProjectId, member: &Member) -> Result<(), StoreError>;

    /// Removes a member, reporting whether there was one.
    fn remove_member(&mut self, project: ProjectId, member_id: &str) -> Result<bool, StoreError>;

    /// Replaces a project's settings.
    ///
    /// Every field is written from the value given — the store never reads a
    /// settings document, mutates one field, and writes it back, which is the
    /// pattern that destroyed a state's `description` in SH-49.
    fn put_settings(
        &mut self,
        project: ProjectId,
        settings: &ProjectSettings,
    ) -> Result<(), StoreError>;

    /// Records the snapshot github-sync should merge against next time.
    fn put_github_base(
        &mut self,
        project: ProjectId,
        story: StoryNo,
        snapshot: &StorySnapshot,
    ) -> Result<(), StoreError>;
}
