//! The values the store exchanges with its callers.
//!
//! None of these are rusqlite types. That is a hard rule of the design: the
//! `Store` trait is meant to admit a Postgres implementation later, and a trait
//! whose signatures mention `rusqlite::Row` has already chosen its engine.

use serde::{Deserialize, Serialize};

use crate::domain::provenance::Provenance;
use crate::domain::{Priority, StoryEvent, StorySnapshot, SuperState};
use crate::store::error::StoreError;
use crate::store::ids::{EventSeq, GlobalSeq, ProjectId, StoryNo};

/// A project as it is stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRecord {
    /// The database identity.
    pub id: ProjectId,
    /// The portable identity, committed to the repository's pointer file.
    pub uuid: String,
    /// The human-facing handle — what the legacy registry called an id.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// The story-id prefix, `SH` by default.
    pub prefix: String,
    /// RFC3339, seconds precision.
    pub created_at: String,
    /// The number the next [`crate::store::WriteOps::allocate_story_no`] hands
    /// out. Exposed for diagnostics; never for a caller to allocate from.
    pub next_story_no: i64,
    /// The next change-feed position to be assigned.
    pub next_global_seq: i64,
}

/// Everything needed to create a project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewProject {
    /// The portable identity. Must be unique across the store.
    pub uuid: String,
    /// The human-facing handle. Must be unique across the store.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// The story-id prefix.
    pub prefix: String,
    /// RFC3339 creation timestamp, supplied by the caller.
    ///
    /// Taken as a parameter rather than read from the system clock so that the
    /// store holds no clock of its own — the injectable `Clock` this design
    /// calls for lives in the caller's `Environment`.
    pub created_at: String,
}

/// The scope of one Full Auto engine run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EngineScope {
    /// The whole project's ready queue.
    Project,
    /// The descendant subtree of one epic story id.
    Epic(String),
}

impl EngineScope {
    /// The stored `engine_runs.scope_kind` vocabulary value.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Epic(_) => "epic",
        }
    }

    /// The stored epic id, absent for project-wide scope.
    #[must_use]
    pub fn story_id(&self) -> Option<&str> {
        match self {
            Self::Project => None,
            Self::Epic(story_id) => Some(story_id),
        }
    }
}

/// The agent host used by an engine run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineAgent {
    /// Claude Code.
    Claude,
    /// Codex.
    Codex,
}

impl EngineAgent {
    /// The stored vocabulary value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// Parses a constrained stored value.
    #[must_use]
    pub const fn parse(raw: &str) -> Option<Self> {
        match raw.as_bytes() {
            b"claude" => Some(Self::Claude),
            b"codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

/// The durable lifecycle state of an engine run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineRunState {
    /// Claims may fill idle lanes.
    Running,
    /// Paused by an operator and eligible to resume.
    Paused,
    /// Existing lanes may finish, but no new work is claimed.
    Draining,
    /// The hard-stop breaker requires acknowledgement.
    Halted,
    /// The run has ended.
    Finished,
}

impl EngineRunState {
    /// The stored vocabulary value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Draining => "draining",
            Self::Halted => "halted",
            Self::Finished => "finished",
        }
    }

    /// Parses a constrained stored value.
    #[must_use]
    pub const fn parse(raw: &str) -> Option<Self> {
        match raw.as_bytes() {
            b"running" => Some(Self::Running),
            b"paused" => Some(Self::Paused),
            b"draining" => Some(Self::Draining),
            b"halted" => Some(Self::Halted),
            b"finished" => Some(Self::Finished),
            _ => None,
        }
    }

    /// Whether the one-live-run partial index covers this state.
    #[must_use]
    pub const fn is_live(self) -> bool {
        matches!(self, Self::Running | Self::Paused | Self::Draining)
    }
}

/// The durable lifecycle state of one engine lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineLaneState {
    /// Holds no story and may be filled.
    Idle,
    /// A story has been claimed and dispatch is in flight.
    Dispatching,
    /// The lane's agent is working.
    Working,
    /// A hard stop has preserved the lane's diagnostic state.
    Quarantined,
}

impl EngineLaneState {
    /// The stored vocabulary value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Dispatching => "dispatching",
            Self::Working => "working",
            Self::Quarantined => "quarantined",
        }
    }

    /// Parses a constrained stored value.
    #[must_use]
    pub const fn parse(raw: &str) -> Option<Self> {
        match raw.as_bytes() {
            b"idle" => Some(Self::Idle),
            b"dispatching" => Some(Self::Dispatching),
            b"working" => Some(Self::Working),
            b"quarantined" => Some(Self::Quarantined),
            _ => None,
        }
    }
}

/// One row of durable Full Auto run state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineRunRecord {
    /// Stable run identity.
    pub id: String,
    /// The project this run operates on.
    pub project_slug: String,
    /// Project-wide or epic-subtree scope.
    pub scope: EngineScope,
    /// Configured lane count.
    pub lanes: u32,
    /// Agent host launched for each lane.
    pub agent: EngineAgent,
    /// Current lifecycle state.
    pub state: EngineRunState,
    /// Consecutive hard stops seen by the breaker.
    pub consecutive_hard_stops: u32,
    /// Machine-readable or human-readable stop classification.
    pub stop_reason: Option<String>,
    /// When the current halt/drain notification was acknowledged.
    pub acknowledged_at: Option<String>,
    /// RFC3339 creation timestamp supplied by the caller.
    pub created_at: String,
    /// RFC3339 last-update timestamp supplied by the caller.
    pub updated_at: String,
}

/// One row of durable Full Auto lane state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineLaneRecord {
    /// The run that owns the lane.
    pub run_id: String,
    /// Zero-based lane position within the run.
    pub lane_index: u32,
    /// Current lifecycle state.
    pub state: EngineLaneState,
    /// Claimed story id, absent exactly while idle.
    pub story_id: Option<String>,
    /// The tmux window identity returned by dispatch.
    pub window_name: Option<String>,
    /// The lane's preserved worktree path.
    pub worktree_path: Option<String>,
    /// When dispatch began.
    pub dispatched_at: Option<String>,
    /// Most recent observation time.
    pub last_observed_at: String,
    /// Completion, skip, or hard-stop classification.
    pub outcome: Option<String>,
    /// Diagnostic detail accompanying the outcome.
    pub outcome_detail: Option<String>,
}

/// A git origin registered against a project.
///
/// `normalized` is the identity key project selection matches on, produced by
/// [`RemoteUrl`](crate::domain::remote::RemoteUrl). `raw` is what the user
/// actually supplied — kept beside it because the normalizer is lossy on
/// purpose and will improve later, and a registration must not lose the string
/// it was made from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectRemoteRecord {
    /// The identity key: `host[:port]/owner/repo`, or `local:<path>` for a
    /// filesystem remote.
    pub normalized: String,
    /// The URL exactly as it was given at registration.
    pub raw: String,
    /// RFC3339 timestamp of the registration, supplied by the caller — the
    /// store holds no clock.
    pub registered_at: String,
}

/// What one [`WriteOps::delete_project`](crate::store::WriteOps::delete_project)
/// destroyed.
///
/// Counted inside the deleting transaction rather than by a read taken before
/// it, so the report is of what actually went. A caller has just told a user
/// what it was about to destroy; telling them afterwards that it destroyed
/// something else would be worse than saying nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeletedProject {
    /// Stories removed, deleted and archived ones included.
    pub stories: usize,
    /// Events removed — the whole log, which is the irreversible part.
    pub events: usize,
    /// Origin registrations removed, freeing those identities for another
    /// project to claim.
    pub remotes: usize,
}

/// What one [`WriteOps::purge_story`](crate::store::WriteOps::purge_story)
/// destroyed.
///
/// Counted inside the purging transaction, for the same reason
/// [`DeletedProject`] is: the caller has just told a user what it was about to
/// destroy, and the honest report is of what actually went.
/// One field, deliberately. The other tables a purge clears are counted by
/// SQLite's `changes()`, which **excludes rows deleted by triggers** — and
/// `story_relations` carries a mirror trigger, so a count taken there reports
/// one edge where two rows went. A number that is quietly half the truth is
/// worse than no number, and nothing needs it: the retracted claims a user
/// cares about are reported by `StoryService::purge`, which knows which
/// stories made them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PurgedStory {
    /// Events removed — the story's whole log, which is the irreversible part.
    pub events: usize,
}

/// An event as it was stored, with its position and its decoded payload.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredEvent {
    /// Position within the story.
    pub seq: EventSeq,
    /// Position within the project's change feed.
    pub global_seq: GlobalSeq,
    /// The event's `kind` discriminant, denormalized out of the payload so it
    /// can be read, filtered, and reported without parsing.
    pub kind: String,
    /// The event's own timestamp, denormalized for the same reason.
    pub at: String,
    /// The payload, decoded if this binary understands the kind.
    pub payload: StoredPayload,
    /// Who performed the write (SH-246).
    ///
    /// [`Provenance::unrecorded`] for every event written before the columns
    /// existed, and for the replay paths that copy a history rather than
    /// perform it. Not part of [`payload`](Self::payload) on purpose: the
    /// payload is the domain event, and who wrote it is a fact about the write.
    pub provenance: Provenance,
}

impl StoredEvent {
    /// The decoded event, or `None` when this binary does not recognise its
    /// kind.
    #[must_use]
    pub fn known(&self) -> Option<&StoryEvent> {
        match &self.payload {
            StoredPayload::Known(event) => Some(event),
            StoredPayload::Unknown { .. } => None,
        }
    }
}

/// An event payload, which this binary may or may not understand.
///
/// The `Unknown` arm is the SH-54 fix expressed as a type. A storyhook that
/// meets an event kind it has never heard of *retains it verbatim* and reports
/// it; it does not fail the read. Before this, adding a `StoryEvent` variant
/// was a silent breaking change that took the dashboard down.
#[derive(Clone, Debug, PartialEq)]
pub enum StoredPayload {
    /// A payload this binary understands.
    Known(StoryEvent),
    /// A payload this binary does not understand, kept exactly as written.
    Unknown {
        /// The kind discriminant that was not recognised.
        kind: String,
        /// The payload's original JSON text, byte for byte.
        json: String,
    },
}

/// An event as bytes, for callers that must write one without understanding it.
///
/// The legacy importer needs this: replaying a `.storyhook` event log has to
/// preserve every event *verbatim*, including kinds a future storyhook added
/// and this one has never heard of, or the round trip that makes the flip a
/// two-way door stops being byte-identical. It is also how a test writes the
/// unknown-kind case that [`StoredPayload::Unknown`] exists for.
///
/// [`crate::store::WriteOps::append_events`] is the checked path and should be
/// used everywhere else: it derives all three fields from a [`StoryEvent`], so
/// they cannot disagree with the payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawEvent {
    /// The `kind` discriminant. Must match the payload's own `kind` field.
    pub kind: String,
    /// The event's timestamp, RFC3339.
    pub at: String,
    /// The payload's JSON text, written byte for byte.
    pub payload: String,
}

impl RawEvent {
    /// One decoded event as bytes, with its `kind` and `at` lifted out.
    ///
    /// The denormalization is what lets a storyhook that has never heard of a
    /// kind still read, report, and retain the row (SH-54): those two columns
    /// are readable without understanding the payload at all. Deriving them
    /// here rather than at each call site is what stops the three disagreeing.
    ///
    /// Fails only if a `StoryEvent` serializes without a string `kind` or `at`,
    /// which the derive cannot produce — hence [`StoreError::Invariant`] rather
    /// than a validation error.
    pub fn from_event(event: &StoryEvent) -> Result<Self, StoreError> {
        let value = serde_json::to_value(event)?;
        let field = |name: &str| -> Result<String, StoreError> {
            value
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| {
                    StoreError::Invariant(format!(
                        "a StoryEvent serialized without a string `{name}` field"
                    ))
                })
        };
        Ok(Self {
            kind: field("kind")?,
            at: field("at")?,
            payload: serde_json::to_string(&value)?,
        })
    }
}

/// Where an append's events came from, which decides how a `[git]` comment in
/// one is read.
///
/// The distinction is not stylistic. A `StoryCommentAdded` reading
/// `[git] <short>: <subject>` is a *pre*-#18 link record — and it is also
/// exactly what a user gets if they type that text into `story comment`. Which
/// of the two it is cannot be told from the bytes; it can be told from who is
/// speaking, and this is how the caller says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkSource {
    /// This program, now, on behalf of a command. A `[git]`-shaped comment here
    /// is a *user's comment*, and recording it as a link would let anyone
    /// suppress a real one by typing it — the exact hole the old string scan
    /// had, and the reason kind #18 exists.
    Live,
    /// A history being replayed from somewhere else: `story migrate` reading an
    /// unmigrated `.storyhook` tree, or an injector rebuilding one. Every
    /// `[git]` comment in such a history was written by `commit-sync` before
    /// kind #18 existed, so projecting them is what stops the first sync after
    /// a migration re-linking a repository's whole log.
    Replayed,
}

/// A story that carried at least one event this binary could not decode.
///
/// Produced by [`partition_known`] and by the rebuild oracle, so an unknown
/// kind is *visible* rather than merely tolerated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownEventDiagnostic {
    /// The story the unknown event belongs to.
    pub story_no: StoryNo,
    /// Where in the story it sits.
    pub seq: EventSeq,
    /// The unrecognised kind.
    pub kind: String,
}

/// Splits stored events into the ones a fold can consume and a diagnostic for
/// each one it cannot.
///
/// This is the only event processing the store does. The *fold* stays out of
/// the store on purpose: [`crate::domain::fold_story`] is the single definition
/// of what a story is, and a second one living behind a storage trait is how a
/// read model and its events drift apart.
#[must_use]
pub fn partition_known(
    story_no: StoryNo,
    events: &[StoredEvent],
) -> (Vec<StoryEvent>, Vec<UnknownEventDiagnostic>) {
    let mut known = Vec::with_capacity(events.len());
    let mut unknown = Vec::new();
    for event in events {
        match &event.payload {
            StoredPayload::Known(decoded) => known.push(decoded.clone()),
            StoredPayload::Unknown { kind, .. } => unknown.push(UnknownEventDiagnostic {
                story_no,
                seq: event.seq,
                kind: kind.clone(),
            }),
        }
    }
    (known, unknown)
}

/// One entry of a project's change feed.
#[derive(Clone, Debug, PartialEq)]
pub struct FeedEvent {
    /// The story the event belongs to.
    pub story_no: StoryNo,
    /// The event itself.
    pub event: StoredEvent,
}

/// A row of the `stories` read model.
///
/// The indexed columns are carried *beside* the folded snapshot rather than
/// derived from it on the way out, and the duplication is the point: the
/// columns are what queries filter and sort on, the snapshot is what callers
/// render, and the two agreeing is a property that has to be checkable.
/// [`crate::store::diff_read_model`] is what checks it — under the old design
/// the equivalent disagreement (an event log and its archived snapshot) was
/// SH-20, and nothing could see it.
#[derive(Clone, Debug, PartialEq)]
pub struct StoryRow {
    /// The story's number within its project.
    pub story_no: StoryNo,
    /// The event sequence this row was folded from.
    ///
    /// A row whose `head_seq` is behind the story's actual head is *stale*,
    /// which is a different fault from a row that is *wrong* — and only this
    /// column can tell them apart.
    pub head_seq: EventSeq,
    /// The change-feed position (`events.global_seq`) of the same event
    /// [`head_seq`](Self::head_seq) names (SH-336): which event within the
    /// story vs. where that event sits in the *project's* write order.
    /// `GlobalSeq::ZERO` means no event backs this row — the `extra_rows`
    /// case [`crate::store::rebuild`] already names. Exact by construction
    /// because writes are serialized behind one process-wide write mutex, so
    /// it tiebreaks a recency ordering that a one-second-precision timestamp
    /// cannot.
    pub head_global_seq: GlobalSeq,
    /// Column: title.
    pub title: String,
    /// Column: state slug.
    pub state: String,
    /// Column: superstate.
    pub superstate: SuperState,
    /// Column: priority.
    pub priority: Priority,
    /// Column: story type slug.
    pub story_type: Option<String>,
    /// Column: assignee member id.
    pub assignee: Option<String>,
    /// Column: what the story is awaiting.
    pub awaiting: Option<String>,
    /// Column: whether the story is archived.
    ///
    /// Replaces the legacy split between `open/stories/` and `archive.db`. Tied
    /// to `closed_at` by a schema CHECK, so the two cannot disagree.
    pub archived: bool,
    /// Column: creation timestamp.
    pub created_at: String,
    /// Column: last-activity timestamp.
    pub updated_at: String,
    /// Column: close timestamp, when closed.
    pub closed_at: Option<String>,
    /// Column: description.
    pub description: Option<String>,
    /// Column: when the story was hidden from the primary UI (the "Archive"
    /// action), if it currently is.
    ///
    /// No CHECK ties this to `archived`/`closed_at`/`superstate` — see
    /// `schema/0010_story_hidden.sql`. `Some` only while `superstate` is
    /// `CLOSED`; the fold and the service layer are what keep that true.
    pub hidden_at: Option<String>,
    /// Column: whether the story is a draft (SH-175) — `true` until
    /// `story publish` clears it, and never again after.
    pub draft: bool,
    /// The story's labels, joined in.
    pub labels: Vec<String>,
    /// The folded snapshot, verbatim — comments and all.
    pub snapshot: StorySnapshot,
}

/// A pull request linked to a story, as a row of `story_pr_links` (SH-49).
///
/// A projection of `StoryPrLinked`/`StoryPrMerged`/`StoryPrClosed` — see
/// `store::sqlite::write::project_pr_link` — keyed on `(owner, repo, number)`
/// rather than on the story, so it lives beside [`StoryRow`] rather than on it.
///
/// `Serialize`/`Deserialize` since SH-169: `output::ReferencedBy` puts this on
/// the wire as `referenced_by.prs`, the first path that crosses `/api/v1/invoke`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrLink {
    /// The repository owner, case-folded.
    pub owner: String,
    /// The repository name, case-folded.
    pub repo: String,
    /// The pull request number.
    pub number: u64,
    /// The pull request's URL, as linked.
    pub url: String,
    /// Whether merging this PR should close the story.
    pub close_on_merge: bool,
    /// `open`, `merged`, or `closed` — see the schema's CHECK.
    pub status: String,
    /// When this PR was linked.
    pub linked_at: String,
    /// When `story pr-check` last looked at this link, if it ever has.
    pub last_checked_at: Option<String>,
}

/// One attachment's stored bytes, as a row of `story_attachment_blobs`
/// (SH-315) — the metadata half only, never the bytes themselves.
///
/// Deliberately not the full row: `story doctor`'s project-wide pass
/// (`ReadOps::attachment_blobs`) compares this against every story's folded
/// [`crate::domain::Attachment`] list without ever loading a single image
/// into memory. Fetching the bytes themselves — `story attachment save`'s
/// job — is a separate, per-attachment call (`ReadOps::attachment_blob`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentBlobRow {
    /// The attachment this blob belongs to, matching
    /// [`crate::domain::Attachment::id`].
    pub attachment_id: u32,
    /// The stored byte count, as recorded at write time — compared against
    /// the snapshot's own copy of this fact, and against `length(bytes)`,
    /// which the schema's own `CHECK` already guarantees agree.
    pub byte_len: u64,
    /// The stored SHA-256, hex-encoded — compared against the snapshot's own
    /// copy of this fact by `story doctor`.
    pub sha256: String,
}

/// A relation edge as stored: both ends are numbers within one project.
///
/// Story *ids* (`SH-1`) are a rendering concern. Keeping the store in numbers
/// is what lets the schema declare a foreign key to `stories` — and therefore
/// what makes a dangling relation unrepresentable.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RelationEdge {
    /// The story that owns this end of the edge.
    pub story_no: StoryNo,
    /// The relation as named on this end (`blocks`, `child-of`, …).
    pub relation: String,
    /// The story at the other end.
    pub other_no: StoryNo,
}

/// Per-project settings.
///
/// Columns, not a blob: a read-modify-write round trip through a serialized
/// document is how SH-49 destroyed a `description` field that the struct in
/// memory did not know about.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectSettings {
    /// `sync.auto_transition` — whether commit-sync moves stories automatically.
    pub sync_auto_transition: Option<bool>,
    /// `doctor.stale_threshold` — a duration string such as `14d`.
    pub doctor_stale_threshold: Option<String>,
}

/// How [`crate::store::ReadOps::stories`] orders its results.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorySort {
    /// Ascending story number — the numeric order `list` and `search` use.
    #[default]
    StoryNo,
    /// Priority first (critical → none), then ascending story number.
    ///
    /// A *total* order, unlike the legacy `priority ASC, created_at ASC`
    /// comparator whose second key has one-second precision and therefore
    /// ties. `domain::ready_order` (SH-63) is the same rule, adopted by the
    /// service layer's ready-list comparators after this one had already
    /// proven it here.
    Priority,
    /// Most recently updated first; a same-second tie broken exactly by
    /// [`StoryRow::head_global_seq`](crate::store::types::StoryRow::head_global_seq)
    /// (SH-336), then ascending story number.
    UpdatedAt,
}

/// A filter over a project's stories.
///
/// Deliberately minimal: every field here backs something `ReadOps` already
/// needs, and the query surface grows with the services that consume it rather
/// than in anticipation of them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoryQuery {
    /// Restrict to `OPEN` or `CLOSED`.
    pub superstate: Option<SuperState>,
    /// Restrict to one state slug.
    pub state: Option<String>,
    /// Restrict to one priority.
    pub priority: Option<Priority>,
    /// Restrict to one assignee's member id.
    pub assignee: Option<String>,
    /// Restrict to one story type slug.
    pub story_type: Option<String>,
    /// Restrict to stories carrying this label.
    pub label: Option<String>,
    /// Restrict to archived (`true`) or unarchived (`false`) stories.
    ///
    /// This one flag replaces the legacy split between `open/stories/*.jsonl`
    /// and `archive/archive.db` — two storage media whose disagreement was
    /// SH-20.
    pub archived: Option<bool>,
    /// Restrict to hidden (`true`) or unhidden (`false`) stories — the SH-43
    /// "Archive" fact. Orthogonal to [`archived`](Self::archived): a story
    /// can be closed and not (yet) hidden, but never hidden while OPEN.
    pub hidden: Option<bool>,
    /// Restrict to draft (`true`) or live (`false`) stories (SH-175).
    pub draft: Option<bool>,
    /// Result order.
    pub sort: StorySort,
    /// Maximum rows to return.
    pub limit: Option<u32>,
}

impl StoryQuery {
    /// Every story in the project, in numeric order.
    #[must_use]
    pub fn all() -> Self {
        Self::default()
    }

    /// Restricts to a superstate.
    #[must_use]
    pub fn superstate(mut self, superstate: SuperState) -> Self {
        self.superstate = Some(superstate);
        self
    }

    /// Restricts to a state slug.
    #[must_use]
    pub fn state(mut self, state: impl Into<String>) -> Self {
        self.state = Some(state.into());
        self
    }

    /// Restricts to a priority.
    #[must_use]
    pub fn priority(mut self, priority: Priority) -> Self {
        self.priority = Some(priority);
        self
    }

    /// Restricts to an assignee.
    #[must_use]
    pub fn assignee(mut self, assignee: impl Into<String>) -> Self {
        self.assignee = Some(assignee.into());
        self
    }

    /// Restricts to a story type.
    #[must_use]
    pub fn story_type(mut self, story_type: impl Into<String>) -> Self {
        self.story_type = Some(story_type.into());
        self
    }

    /// Restricts to stories carrying a label.
    #[must_use]
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Restricts to archived or unarchived stories.
    #[must_use]
    pub fn archived(mut self, archived: bool) -> Self {
        self.archived = Some(archived);
        self
    }

    /// Restricts to hidden or unhidden stories.
    #[must_use]
    pub fn hidden(mut self, hidden: bool) -> Self {
        self.hidden = Some(hidden);
        self
    }

    /// Restricts to draft or live stories (SH-175).
    #[must_use]
    pub fn draft(mut self, draft: bool) -> Self {
        self.draft = Some(draft);
        self
    }

    /// Sets the result order.
    #[must_use]
    pub fn sort(mut self, sort: StorySort) -> Self {
        self.sort = sort;
        self
    }

    /// Caps the number of rows returned.
    #[must_use]
    pub fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }
}

/// The rank column that makes priority sortable by an index.
///
/// `priority` is stored as its slug, which is self-describing and matches the
/// snapshot JSON, but `ORDER BY` on it would be alphabetical — `critical`
/// before `high` by luck, `low` before `medium` by accident. The rank is
/// carried beside it and tied to it by a CHECK constraint, so the two cannot
/// drift.
#[must_use]
pub const fn priority_rank(priority: &Priority) -> i64 {
    match priority {
        Priority::Critical => 0,
        Priority::High => 1,
        Priority::Medium => 2,
        Priority::Low => 3,
        Priority::None => 4,
    }
}

/// Parses a stored `stories.priority` slug.
pub fn parse_priority(raw: &str) -> Result<Priority, StoreError> {
    Priority::parse(raw)
        .ok_or_else(|| StoreError::Corrupt(format!("stories.priority holds unknown value `{raw}`")))
}

/// Parses a stored `stories.superstate` value.
pub fn parse_superstate(raw: &str) -> Result<SuperState, StoreError> {
    SuperState::parse(raw).ok_or_else(|| {
        StoreError::Corrupt(format!("stories.superstate holds unknown value `{raw}`"))
    })
}

/// The outcome of [`crate::store::Store::migrate`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationReport {
    /// The schema version the database was at before this call.
    pub from_version: u32,
    /// The schema version it is at now.
    pub to_version: u32,
    /// The migrations applied, in order, by name.
    pub applied: Vec<String>,
    /// Where the pre-migration backup was written, when one was taken.
    ///
    /// `None` when nothing was pending, or when the database was empty — there
    /// is nothing to lose backing up a database with no schema at all.
    pub backup: Option<std::path::PathBuf>,
}

impl MigrationReport {
    /// Whether this call changed anything.
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.applied.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_ranks_order_critical_first_and_none_last() {
        let mut ranked = [
            Priority::None,
            Priority::Critical,
            Priority::Low,
            Priority::High,
            Priority::Medium,
        ];
        ranked.sort_by_key(priority_rank);
        assert_eq!(
            ranked.iter().map(Priority::as_str).collect::<Vec<_>>(),
            ["critical", "high", "medium", "low", "none"]
        );
    }

    #[test]
    fn every_priority_slug_round_trips_through_its_stored_form() {
        for priority in [
            Priority::Critical,
            Priority::High,
            Priority::Medium,
            Priority::Low,
            Priority::None,
        ] {
            assert_eq!(parse_priority(priority.as_str()).unwrap(), priority);
        }
        assert!(parse_priority("urgent").is_err());
    }

    #[test]
    fn partition_known_keeps_order_and_reports_each_unknown() {
        let events = vec![
            StoredEvent {
                seq: EventSeq::new(1),
                global_seq: GlobalSeq::new(1),
                kind: "StoryCreated".into(),
                at: "2026-01-01T00:00:00Z".into(),
                payload: StoredPayload::Known(StoryEvent::StoryCreated {
                    at: "2026-01-01T00:00:00Z".into(),
                    title: "t".into(),
                    state: "todo".into(),
                }),
                provenance: Provenance::unrecorded(),
            },
            StoredEvent {
                seq: EventSeq::new(2),
                global_seq: GlobalSeq::new(2),
                kind: "StoryTeleported".into(),
                at: "2026-01-01T00:00:01Z".into(),
                payload: StoredPayload::Unknown {
                    kind: "StoryTeleported".into(),
                    json: "{\"kind\":\"StoryTeleported\"}".into(),
                },
                provenance: Provenance::unrecorded(),
            },
        ];
        let (known, unknown) = partition_known(StoryNo::new(1), &events);
        assert_eq!(known.len(), 1);
        assert_eq!(
            unknown,
            vec![UnknownEventDiagnostic {
                story_no: StoryNo::new(1),
                seq: EventSeq::new(2),
                kind: "StoryTeleported".into(),
            }]
        );
    }
}
