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

use crate::domain::provenance::Provenance;
use crate::domain::remote::RemoteUrl;
use crate::domain::{Member, StateDef, StoryEvent, StorySnapshot, TypeDef};

pub use conformance::ConformanceFixture;
pub use error::StoreError;
pub use fault::{DELIVERY_BACKSTOP, FaultPoint};
pub use ids::{EventSeq, ExpectedSeq, GlobalSeq, ProjectId, StoryNo, StoryRef};
pub use migrate::{MIGRATIONS, Migration, current_schema_version};
#[cfg(feature = "test-seam")]
pub use rebuild::folds;
pub use rebuild::{
    Divergence, ReadModelDiff, RebuiltStory, RepairReport, diff, diff_read_model,
    rebuild_read_model, repair_read_model,
};
pub use sqlite::{Access, SqliteReadTx, SqliteStore, SqliteWriteTx, StoreConfig};
pub use types::{
    AttachmentBlobRow, DeletedProject, EngineAgent, EngineLaneRecord, EngineLaneState,
    EngineQuarantineRecord, EngineRunRecord, EngineRunState, EngineScope, FeedEvent, LinkSource,
    MigrationReport, NewProject, PrLink, ProjectRecord, ProjectRemoteRecord, ProjectSettings,
    PurgedStory, RawEvent, RelationEdge, StoredEvent, StoredPayload, StoryQuery, StoryRow,
    StorySort, UnknownEventDiagnostic, VerificationFailureDisposition, VerificationIncident,
    partition_known,
};

/// A transactional store of projects, events, and the read model folded from
/// them.
///
/// Transactions are handed to closures rather than returned, so that a caller
/// cannot hold one open across an await point, a user prompt, or a network
/// call — and so that commit and rollback are decided by this trait rather than
/// by whether every path through a caller remembered.
pub trait Store: Send + Sync + 'static {
    /// How this store may be used (SH-530).
    ///
    /// Required rather than defaulted to [`Access::ReadWrite`], and the
    /// difference is not stylistic. Everything that warns a user their store is
    /// degraded reads this one answer, so an implementation that inherited a
    /// cheerful default — a wrapper, a cache, a decorator that forgot to
    /// delegate — would not fail, it would silently report a degraded store as
    /// healthy and switch the whole warning off. Requiring it makes that a
    /// compile error instead, which is the SH-365 shape: let the compiler hold
    /// the invariant a reviewer would have to notice.
    fn access(&self) -> Access;
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
    /// The daemon polls this to notice writes it did not make itself — a
    /// `story tui` session, a second machine, a developer with a `sqlite3`
    /// prompt open. Comparing two tokens answers "has anything
    /// changed"; the value itself means nothing and must not be persisted or
    /// compared across process restarts.
    ///
    /// Engine-neutral by design: SQLite answers with `PRAGMA data_version`, and
    /// an eventual Postgres implementation would answer with a transaction id or
    /// a notification counter.
    fn change_token(&self) -> Result<u64, StoreError>;

    /// Writes a verified copy of this store into `dir` and returns its path.
    ///
    /// Verified means the copy is reopened, `integrity_check`ed, and shown to
    /// hold at least one page before this returns: a backup that is only
    /// discovered to be corrupt when it is needed is worse than no backup,
    /// because it was believed in. Soundness alone does not carry that weight,
    /// and an engine implementing this trait has to know it — an empty database
    /// passes an integrity check, so a check that asks nothing else cannot tell
    /// a copy from a file that never received one (SH-296).
    ///
    /// `label` distinguishes what the copy is *for* — a directory holding both
    /// a daily schedule's output and a maintenance operation's own safety net
    /// still says which is which. It becomes a filename component, so a caller
    /// that took it from outside the process must validate it first; this
    /// method trusts its caller rather than re-checking, the same division of
    /// responsibility [`crate::domain::prefix::validate`] documents for a
    /// project's prefix.
    ///
    /// This promises only that the copy is *a* recent committed state. A
    /// caller that needs the copy to be the state a particular write begins
    /// from wants [`Self::write_with_snapshot`] instead.
    fn snapshot(&self, dir: &Path, label: &str) -> Result<PathBuf, StoreError>;

    /// Runs `f` inside a write transaction, having first taken a verified copy
    /// of the state that transaction begins from — one critical section, not
    /// two.
    ///
    /// The contract is two clauses, and an implementation must honour them
    /// together:
    ///
    /// 1. The copy is the **committed state this transaction begins from**,
    ///    not merely a recent one.
    /// 2. **No writer commits between that copy and this transaction's
    ///    commit** — not another thread, and not another process.
    ///
    /// An implementation must therefore **not** satisfy this by calling
    /// [`Self::snapshot`] and then [`Self::write`]. Those are two critical
    /// sections; any writer fits between them; and restoring the copy would
    /// then discard work that was never part of this operation and whose
    /// author never asked for it to be undone (SH-297). Exclusion has to span
    /// both, which means holding whatever lock excludes *every* writer across
    /// the copy as well as the write — a deliberate price, paid only by the
    /// rare, confirmed maintenance operations that need a way back.
    ///
    /// The clauses name no mechanism on purpose. SQLite honours them by
    /// holding its write lock across both; an engine with no such lock would
    /// honour them with an exclusive advisory lock every writer takes, plus a
    /// dump taken from this transaction's own exported snapshot. That the
    /// honest implementation is expensive is information this contract should
    /// convey rather than hide.
    ///
    /// There is deliberately **no default implementation**: the only one that
    /// could be written here is `snapshot()` then `write()`, which is the
    /// defect, and a defaulted method would ship it silently to the next
    /// engine.
    fn write_with_snapshot<T>(
        &self,
        dir: &Path,
        label: &str,
        f: impl FnOnce(&mut Self::WriteTx<'_>) -> Result<T, StoreError>,
    ) -> Result<WriteWithSnapshot<T>, StoreError>;
}

/// A write, and the verified copy of the state it began from.
///
/// Named rather than a bare `(PathBuf, T)` so that neither half can be read as
/// the other at a call site.
#[derive(Debug)]
pub struct WriteWithSnapshot<T> {
    /// The copy, in the directory the caller named.
    ///
    /// Restoring it undoes this write **and nothing else** — see
    /// [`Store::write_with_snapshot`] for the two clauses that guarantee it.
    pub snapshot: PathBuf,
    /// Whatever the closure returned.
    pub value: T,
}

/// Everything that can be read inside a transaction.
///
/// Every story or project-catalog method takes a [`ProjectId`]. That is a
/// deliberate ergonomic cost: in a single global database where every
/// repository defaults to the prefix `SH`, an unscoped story query does not
/// fail — it quietly returns a different project's story with the same number.
/// Making the scope a required argument turns that whole class of bug into a
/// compile error. Engine operations use their globally unique run id or the
/// project slug stored on the run; [`Self::live_engine_runs`] is deliberately
/// machine-wide for restart reconciliation and lane-budget accounting.
pub trait ReadOps {
    /// The project with this id.
    fn project(&self, project: ProjectId) -> Result<Option<ProjectRecord>, StoreError>;

    /// The project with this uuid — the identity a repository's pointer file
    /// carries.
    fn project_by_uuid(&self, uuid: &str) -> Result<Option<ProjectRecord>, StoreError>;

    /// The project with this slug — the identity the legacy registry called an
    /// id.
    fn project_by_slug(&self, slug: &str) -> Result<Option<ProjectRecord>, StoreError>;

    /// The project whose story-id prefix this is, if any project holds it.
    ///
    /// Matched exactly, case included: a stored prefix is always canonical
    /// uppercase. [`WriteOps::set_prefix`] is the one caller that must ask
    /// before writing, so that two projects never mint colliding ids.
    fn project_by_prefix(&self, prefix: &str) -> Result<Option<ProjectRecord>, StoreError>;

    /// Every project, ordered by slug.
    fn projects(&self) -> Result<Vec<ProjectRecord>, StoreError>;

    /// One Full Auto engine run by its stable id.
    fn engine_run(&self, run_id: &str) -> Result<Option<EngineRunRecord>, StoreError>;

    /// Every engine run for one project, oldest first with id as a total-order
    /// tiebreaker.
    fn engine_runs(&self, project_slug: &str) -> Result<Vec<EngineRunRecord>, StoreError>;

    /// Every live engine run across the store, ordered by project then age.
    ///
    /// This is intentionally the one machine-wide operational read in the
    /// trait: restart reconciliation and the machine lane budget must see all
    /// projects before either can make a safe decision.
    fn live_engine_runs(&self) -> Result<Vec<EngineRunRecord>, StoreError>;

    /// The machine-wide verifier incident, if infrastructure owns the queue.
    fn verification_incident(&self) -> Result<Option<VerificationIncident>, StoreError>;

    /// Every lane belonging to a run, ordered by lane index.
    fn engine_lanes(&self, run_id: &str) -> Result<Vec<EngineLaneRecord>, StoreError>;

    /// The project that registered this git origin, if any.
    ///
    /// The lookup half of project identity, and the reason
    /// [`RemoteUrl`](crate::domain::remote::RemoteUrl) is a type rather than a
    /// string: the *only* way to obtain one is through the normalizer, so this
    /// and [`link_remote`](WriteOps::link_remote) cannot key on different
    /// grammars however far apart their callers drift.
    ///
    /// Unlike every other method here this takes no [`ProjectId`] — deciding
    /// which project you are in is its whole job. An origin nobody registered
    /// is `Ok(None)`, never an error: project selection consults this on every
    /// command, and "nothing is registered here" is an ordinary answer.
    fn project_by_remote(&self, remote: &RemoteUrl) -> Result<Option<ProjectRecord>, StoreError>;

    /// Every git origin registered to a project, ordered by identity key.
    fn project_remotes(&self, project: ProjectId) -> Result<Vec<ProjectRemoteRecord>, StoreError>;

    /// Where this project's repo-side operations run, if a checkout was linked.
    ///
    /// **Never an answer to "which project is this?"** — that is
    /// [`project_by_remote`](Self::project_by_remote) and the committed pointer
    /// file, which storyhook reads rather than the store. This is the reverse
    /// direction and the only direction: a project names a directory, and
    /// nothing looks a project up by it. If anything ever does, the epic's
    /// invariant — nothing about the filesystem is ever *required* to answer
    /// which project this is — is gone.
    fn checkout_path(&self, project: ProjectId) -> Result<Option<PathBuf>, StoreError>;

    /// When this store last scanned this project's commits (SH-316).
    ///
    /// **Never an answer to "is a git hook installed?"** — see migration 0014's
    /// header. It records that a `story commit-sync` ran, whoever ran it, and a
    /// reader may compare it against git and say what git says. `None` means no
    /// scan has ever been recorded here, which is unknowable rather than absent.
    fn commit_scan_at(&self, project: ProjectId) -> Result<Option<String>, StoreError>;

    /// How many events a project holds.
    ///
    /// A count rather than `events_since(..).len()`: `story project delete`
    /// has to state the size of what it is about to destroy, and paging a
    /// whole change feed to say "4,812" would make describing the deletion
    /// cost what performing it costs.
    fn event_count(&self, project: ProjectId) -> Result<usize, StoreError>;

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

    /// Whether a git commit has already been linked to this story.
    ///
    /// The idempotency key `story commit-sync` used to answer by scanning every
    /// event on the story for a comment beginning `[git] <short-hash>:`. That
    /// was O(events) per commit per story and a user comment could satisfy it;
    /// this is an indexed lookup against a primary key.
    ///
    /// **`sha` is matched exactly.** Records this binary writes carry the full
    /// forty characters; rows backfilled by schema migration 2 carry the
    /// seven-character abbreviation, because that is all the `[git]` comment
    /// they were recovered from preserved. A caller that must find either asks
    /// twice — see `GitService::already_linked`, which is the only such caller
    /// and says why there.
    fn commit_linked(
        &self,
        project: ProjectId,
        story: StoryNo,
        sha: &str,
    ) -> Result<bool, StoreError>;

    /// Every `story_commit_links` row in this project with no backing
    /// `StoryCommitLinked` (kind #18) event — i.e. every link recovered from a
    /// `[git]`-shaped comment (schema migration 2's backfill, `story migrate`,
    /// or an `import-project --legacy-links` restore) rather than from the
    /// event itself.
    ///
    /// The store cannot tell whether such a row is a genuine pre-#18 link or a
    /// live comment misclassified by a `--legacy-links` restore — that is
    /// exactly the ambiguity [`LinkSource`] exists to close at write time, and
    /// this reads what already landed rather than reopening it. `story
    /// doctor`'s advisory (SH-70) is the only caller; it reports, and neither
    /// it nor this can auto-fix a row it did not write.
    fn unbacked_commit_links(
        &self,
        project: ProjectId,
    ) -> Result<Vec<(StoryNo, String)>, StoreError>;

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

    /// This story's still-`open` linked pull requests (SH-49), in
    /// `(owner, repo, number)` order.
    ///
    /// `story pr-check <id>`'s read: only `open` links are worth asking
    /// GitHub about again, so `merged`/`closed` rows are excluded rather than
    /// filtered by the caller.
    fn open_pr_links_for_story(
        &self,
        project: ProjectId,
        story: StoryNo,
    ) -> Result<Vec<PrLink>, StoreError>;

    /// Every still-`open` linked pull request across the whole project,
    /// paired with the story number that owns it — `story pr-check`'s
    /// project-wide read, when no story id is given.
    fn open_pr_links(&self, project: ProjectId) -> Result<Vec<(StoryNo, PrLink)>, StoreError>;

    /// Every linked pull request across the whole project regardless of
    /// status, paired with the story number that owns it (SH-169).
    ///
    /// Unlike [`open_pr_links`](Self::open_pr_links), a merged or closed link
    /// belongs here too — `referenced_by.prs` is a history of what pointed at
    /// a story, not a work queue of what still needs asking about, so the
    /// exact filter that serves `pr-check` would hide the PR a reader is most
    /// likely to want to see (the one that already shipped the fix).
    fn pr_links(&self, project: ProjectId) -> Result<Vec<(StoryNo, PrLink)>, StoreError>;

    /// One attachment's stored bytes (SH-315), or `None` if no blob row backs
    /// it — which `story doctor` reports as damage, not this call.
    ///
    /// `story attachment save`'s read. Loads the whole blob into memory,
    /// which is why every other consumer — `story attachment list`,
    /// `story doctor` — reaches for [`Self::attachment_blobs`] instead.
    fn attachment_blob(
        &self,
        project: ProjectId,
        story: StoryNo,
        attachment_id: u32,
    ) -> Result<Option<Vec<u8>>, StoreError>;

    /// Every attachment blob's metadata across the whole project, paired with
    /// the story number that owns it (SH-315) — `story doctor`'s read.
    ///
    /// One project-wide query rather than one call per story: the doctor's
    /// per-story pass runs over every story regardless of whether it holds
    /// any attachments, and reaching for [`Self::attachment_blob`] there would
    /// make that an N+1 query loading full image bytes it never uses. This
    /// returns metadata only, following [`Self::pr_links`]'s own precedent for
    /// the same shape of project-wide read.
    fn attachment_blobs(
        &self,
        project: ProjectId,
    ) -> Result<Vec<(StoryNo, AttachmentBlobRow)>, StoreError>;
}

/// Everything that can be written inside a transaction.
pub trait WriteOps: ReadOps {
    /// Creates a project and returns its id.
    fn create_project(&mut self, project: &NewProject) -> Result<ProjectId, StoreError>;

    /// Creates a Full Auto engine run.
    ///
    /// Insert-only by design. In particular, callers must not read for a live
    /// run first: `engine_runs_one_live_per_project` is the authority that
    /// settles two starts racing across processes.
    fn create_engine_run(&mut self, run: &EngineRunRecord) -> Result<(), StoreError>;

    /// Replaces the mutable state of an existing engine run.
    ///
    /// A missing id is an error rather than an implicit insert, keeping run
    /// creation on the constraint-arbitrated path above.
    fn update_engine_run(&mut self, run: &EngineRunRecord) -> Result<(), StoreError>;

    /// Inserts or replaces one lane under its `(run_id, lane_index)` identity.
    fn put_engine_lane(&mut self, lane: &EngineLaneRecord) -> Result<(), StoreError>;

    /// Creates or replaces the one machine-wide verifier incident.
    fn put_verification_incident(
        &mut self,
        incident: &VerificationIncident,
    ) -> Result<(), StoreError>;

    /// Clears the incident only when its identity still matches `incident_id`.
    fn clear_verification_incident(&mut self, incident_id: &str) -> Result<bool, StoreError>;

    /// Registers a git origin as belonging to this project.
    ///
    /// A project may hold many origins — a repository that moved, a second
    /// canonical remote — but an origin belongs to at most one project. That
    /// second half is a `UNIQUE` index on the normalized key rather than an
    /// application rule, so ambiguous resolution is *unrepresentable* instead of
    /// merely handled.
    ///
    /// The holder is looked up **before** the insert, so a refusal can name the
    /// project that already holds the origin — SQLite's constraint error names a
    /// column and has no way to name a project. Catching the constraint and
    /// looking up afterwards would leave the racing writer's message anonymous,
    /// which is the one case where naming the holder matters most. The lookup is
    /// safe from a race because a write transaction is `BEGIN IMMEDIATE` and so
    /// exclusive among writers, the same reasoning
    /// [`create_project`](Self::create_project) uses for uuid and slug.
    ///
    /// Registering an origin this project already holds **updates** the raw form
    /// and the timestamp rather than failing, so `link origin` is safely
    /// re-runnable. Registering one another project holds is
    /// [`StoreError::Invariant`].
    ///
    /// `registered_at` is a parameter because the store holds no clock.
    fn link_remote(
        &mut self,
        project: ProjectId,
        remote: &RemoteUrl,
        registered_at: &str,
    ) -> Result<(), StoreError>;

    /// Forgets one git origin of a project, reporting whether there was one.
    ///
    /// The project and its stories survive. A repository being transferred,
    /// renamed, or replaced by a new canonical remote is not a reason to lose
    /// the work recorded against it. The freed
    /// identity becomes available to another project immediately.
    fn unlink_remote(&mut self, project: ProjectId, remote: &RemoteUrl)
    -> Result<bool, StoreError>;

    /// Sets — or with `None`, clears — where this project's repo-side work runs.
    ///
    /// A plain overwrite, because a project has **at most one** checkout: a
    /// second call replaces the first, and there is nothing to disambiguate on
    /// the way out.
    ///
    /// The path is not checked for existence, uniqueness or gitness. It is
    /// consulted by exactly one consumer, which fails loudly on its own if the
    /// directory is wrong, and two projects sharing one is a monorepo rather
    /// than an ambiguity — see the migration's header.
    fn set_checkout_path(
        &mut self,
        project: ProjectId,
        path: Option<&Path>,
    ) -> Result<(), StoreError>;

    /// Records that this store scanned this project's commits at `at`.
    ///
    /// A plain overwrite: the receipt is the *latest* scan, and the history of
    /// scans is not a thing anything asks for. Written by `commit-sync` on every
    /// invocation and by nothing else — see [`ReadOps::commit_scan_at`] and
    /// migration 0014 for what it may and may not be read to mean.
    fn record_commit_scan(&mut self, project: ProjectId, at: &str) -> Result<(), StoreError>;

    /// Sets the receipt to `at` **only if this project has never had one**, and
    /// answers whether it did.
    ///
    /// The conditional is the whole method. `story hooks install` arms a project
    /// that has never recorded a scan, so that a hook which never fires once
    /// still has a baseline to be measured against; re-arming a receipt that is
    /// already set would overwrite the evidence a re-run exists to surface,
    /// which is the single action that destroys the signal. Expressed as `WHERE
    /// commit_scan_at IS NULL` rather than as a read-then-write, so two installs
    /// racing cannot both see NULL and both write.
    fn arm_commit_scan(&mut self, project: ProjectId, at: &str) -> Result<bool, StoreError>;

    /// Sets a project's display name.
    ///
    /// The catalog *is* the projects table, so `story web register --name` has
    /// nowhere else to record what the user called this project. Without it the
    /// flag is accepted and silently dropped — which is what the legacy
    /// registry, a file with a `name` field per repo, did not do.
    fn rename_project(&mut self, project: ProjectId, name: &str) -> Result<(), StoreError>;

    /// Sets a project's story-id prefix, leaving every other column alone.
    ///
    /// **Not, by itself, what makes a rename correct.** A story's rendered
    /// `id` is derived fresh from this column every time it is folded, so
    /// changing it here is enough for `id` on its own — but a relationship's
    /// `other_id` is folded verbatim from the event that set it and does not
    /// re-derive, so a caller that changes this without also appending
    /// compensating `StoryRelationshipRemoved`/`StoryRelationshipAdded`
    /// events for every relation in the project leaves the read model
    /// internally inconsistent the moment anything is refolded: every
    /// `other_id` still names the old prefix, `StoryNo::parse_id` rejects it
    /// against the new one, and the story becomes unwritable. [`ProjectService::set_prefix`](crate::service::project::ProjectService::set_prefix)
    /// is the one caller and does both, in the order this note describes —
    /// this column first, so that its own compensating writes validate
    /// against the prefix they are written under.
    ///
    /// Called after [`ReadOps::project_by_prefix`] has confirmed no other
    /// project already holds `new_prefix`; this method does not check.
    fn set_prefix(&mut self, project: ProjectId, new_prefix: &str) -> Result<(), StoreError>;

    /// Removes a project and every row recorded against it, permanently.
    ///
    /// The **only** operation permitted to delete events, and the reason
    /// schema 3 exists. Schema 1 guarded `events` with an unconditional
    /// `events_reject_delete` trigger and deliberately withheld
    /// `ON DELETE CASCADE` from `events.project_id`, saying that a future
    /// `delete_project` "has to drop these guards inside its own migration and
    /// say so — it cannot happen by accident". Migration 3 does exactly that,
    /// and narrowly: the trigger now abstains only for an event whose project
    /// no longer exists, which is a state nothing but this operation's own
    /// last step can produce. `events_reject_update` is untouched — an event
    /// is still never rewritten, under any circumstance.
    ///
    /// Every project-scoped table is cleared explicitly, in dependency order,
    /// even where a cascade would have done it. The cascade is demoted to a
    /// backstop so that a table added by a later migration and forgotten here
    /// is caught by the verification sweep that ends this transaction, rather
    /// than silently orphaning its rows.
    ///
    /// Ordering is load-bearing in one place: `projects` is deleted *before*
    /// `events`, which is what makes the rewritten trigger abstain. That works
    /// only because a write transaction runs under `defer_foreign_keys`, which
    /// holds `events.project_id`'s check until COMMIT. If that deferral were
    /// ever removed the first statement fails with a foreign-key violation and
    /// nothing is written — loudly, never silently.
    ///
    /// A project that does not exist is [`StoreError::NotFound`], never a
    /// silent no-op: the caller has just told a user what it was about to
    /// destroy, and "nothing, actually" is an answer they need.
    ///
    /// # Identity reuse
    ///
    /// `projects.id` is a plain `INTEGER PRIMARY KEY` with no `AUTOINCREMENT`,
    /// so the id this frees is handed to the next project created. A
    /// [`ProjectId`] held across a deletion therefore names a *different*
    /// project rather than a missing one. Nothing in storyhook holds one that
    /// long — every request resolves the project it operates on — but a caller
    /// that caches ids must not.
    fn delete_project(&mut self, project: ProjectId) -> Result<DeletedProject, StoreError>;

    /// Removes one story and every row recorded against it, permanently.
    ///
    /// The second — and last — operation permitted to delete events, and the
    /// reason schema 5 exists. Its migration narrows `events_reject_delete` by
    /// **adding** a conjunct to the one schema 3 installed rather than
    /// replacing it, so a project teardown fails the project clause, a purge
    /// fails the story clause, and every other DELETE against `events` still
    /// aborts. Neither operation can satisfy the other's escape.
    ///
    /// Ordering is load-bearing in the same place it is for
    /// [`delete_project`](Self::delete_project): the `stories` row is deleted
    /// *before* the events, which is what makes the trigger abstain.
    ///
    /// A story that does not exist is [`StoreError::NotFound`].
    ///
    /// # What this deliberately does not do
    ///
    /// **It does not roll `next_story_no` back.** A purged number is never
    /// reissued. Reusing it would point every commit message, branch name,
    /// review comment and external link that names the old story at a new and
    /// unrelated one, which is a worse failure than a gap in the sequence.
    ///
    /// **It does not touch any other story's history**, and cannot: a surviving
    /// story whose own events claim an edge into this one still claims it, and
    /// the rebuild oracle will report the divergence. Retracting those claims
    /// with real `StoryRelationshipRemoved` events, before this is called, is
    /// the caller's job — `StoryService::purge` is the only caller and does it.
    fn purge_story(
        &mut self,
        project: ProjectId,
        story: StoryNo,
    ) -> Result<PurgedStory, StoreError>;

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
    ///
    /// `provenance` stamps every appended row with who performed the write
    /// (SH-246), and is an argument for the same reason `source` is on
    /// [`append_raw_events`](Self::append_raw_events): it is a fact about the
    /// *caller*. [`Provenance::unrecorded`] is a legitimate answer and writes
    /// NULLs — the value every pre-SH-246 row already holds, rendered
    /// `(unrecorded)` rather than confused with an unset field.
    fn append_events(
        &mut self,
        project: ProjectId,
        story: StoryNo,
        expected: ExpectedSeq,
        events: &[StoryEvent],
        provenance: &Provenance,
    ) -> Result<EventSeq, StoreError>;

    /// [`append_events`](Self::append_events), for a caller holding an event's
    /// bytes rather than a decoded [`StoryEvent`].
    ///
    /// Same compare-and-swap, same sequencing; the difference is that the
    /// payload is written exactly as given. A caller that must preserve an
    /// event kind this binary has never heard of needs it — `story migrate`,
    /// `story import-project` and the injectors — and it is how the
    /// unknown-kind path is tested. Prefer `append_events` when every event is
    /// a decoded [`StoryEvent`]: it derives `kind` and `at` from the payload,
    /// so the three cannot disagree.
    ///
    /// `source` says whose history this is, and it is not a formality:
    /// [`LinkSource`] decides whether a `[git]`-shaped comment in the batch is
    /// read as a pre-#18 commit link. It is an argument rather than a property
    /// of the method because the distinction is a fact about the *caller*, and
    /// a caller cannot state a fact it is never asked for.
    fn append_raw_events(
        &mut self,
        project: ProjectId,
        story: StoryNo,
        expected: ExpectedSeq,
        events: &[RawEvent],
        source: LinkSource,
        provenance: &Provenance,
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

    /// Writes one attachment's bytes (SH-315), replacing any blob already
    /// stored under this `(project, story, attachment_id)`.
    ///
    /// Called by [`crate::service::attachment::AttachmentService::add`] in the
    /// same transaction as the [`StoryEvent::StoryAttachmentAdded`] that names
    /// this attachment — not from `write::append`'s per-kind projection loop
    /// the way `story_pr_links` is, because the bytes are never present in
    /// that event's own JSON payload for a projection to read out of it. An
    /// upsert rather than a pure insert for the same reason
    /// `story_pr_links`' `StoryPrLinked` write is: a caller cannot reach this
    /// under an id that already holds bytes through the ordinary CLI path
    /// (ids are never reused), but replay is not ordinary — `story
    /// import-project` restores an id-numbered document that may already
    /// have written this row once, and an upsert makes that restore
    /// idempotent rather than a duplicate-key failure.
    fn put_attachment_blob(
        &mut self,
        project: ProjectId,
        story: StoryNo,
        attachment_id: u32,
        bytes: &[u8],
        sha256: &str,
        added_at: &str,
    ) -> Result<(), StoreError>;

    /// Deletes one attachment's stored bytes (SH-315), reporting whether
    /// there was a row to delete.
    ///
    /// Called by
    /// [`crate::service::attachment::AttachmentService::remove`] in the same
    /// transaction as the [`StoryEvent::StoryAttachmentRemoved`] that retracts
    /// this attachment — removal deletes the bytes rather than tombstoning
    /// them, per that event's own doc comment.
    fn delete_attachment_blob(
        &mut self,
        project: ProjectId,
        story: StoryNo,
        attachment_id: u32,
    ) -> Result<bool, StoreError>;
}
