//! Every read the store performs, as free functions over a `&Connection`.
//!
//! Written once here and delegated to by both [`super::SqliteReadTx`] and
//! [`super::SqliteWriteTx`], so that a read issued inside a write transaction
//! is the *same* read — a writer that could observe a different world than a
//! reader is how a read model and its events drift apart.
//!
//! Every story and project-catalog function takes a [`ProjectId`]. There is no
//! unscoped story read in this file and there must never be one: in a single
//! global database where every repository defaults to the prefix `SH`, an
//! unscoped story read does not fail, it silently returns another project's
//! story. Full Auto operational reads instead use globally unique run ids or
//! the project slug stored on a run; the one machine-wide live-run query is the
//! input to restart reconciliation and lane-budget accounting.

use std::collections::BTreeMap;
use std::path::PathBuf;

use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use crate::domain::provenance::{ActorLabel, Provenance};
use crate::domain::remote::RemoteUrl;
use crate::domain::{Member, StateDef, StoryEvent, TypeDef};
use crate::store::error::StoreError;
use crate::store::ids::{EventSeq, GlobalSeq, ProjectId, StoryNo};
use crate::store::types::{
    AttachmentBlobRow, EngineAgent, EngineLaneRecord, EngineLaneState, EngineRunRecord,
    EngineRunState, EngineScope, FeedEvent, PrLink, ProjectRecord, ProjectRemoteRecord,
    ProjectSettings, RelationEdge, StoredEvent, StoredPayload, StoryQuery, StoryRow, StorySort,
    parse_priority, parse_superstate,
};

const PROJECT_COLUMNS: &str =
    "id, uuid, slug, name, prefix, created_at, next_story_no, next_global_seq";

// `head_global_seq` is appended at the end rather than beside `head_seq`, so
// the eighteen positional `row.get(n)` calls in `raw_story_from_row` keep
// their indices — inserting it earlier would renumber every one of them for
// no reason beyond cosmetics.
/// The `stories` columns [`raw_story_from_row`] reads, by position.
///
/// `pub(super)` since SH-530: [`super::SqliteStore::resolve_access`] prepares a
/// `SELECT` over exactly this list as its capability probe, so that "can this
/// build still read a newer store?" is answered by production's own column
/// list rather than by a second copy of it that could drift.
pub(super) const STORY_COLUMNS: &str = "story_no, head_seq, title, state, superstate, priority, story_type, \
     assignee, awaiting, archived, created_at, updated_at, closed_at, description, \
     hidden_at, draft, snapshot, head_global_seq";

fn sql<T>(result: Result<T, rusqlite::Error>, context: &str) -> Result<T, StoreError> {
    result.map_err(|e| StoreError::from_sqlite(e, context))
}

fn collect<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&Row<'_>) -> Result<T, rusqlite::Error>>,
    context: &str,
) -> Result<Vec<T>, StoreError> {
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| StoreError::from_sqlite(e, context))?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

fn project_from_row(row: &Row<'_>) -> Result<ProjectRecord, rusqlite::Error> {
    Ok(ProjectRecord {
        id: ProjectId::new(row.get(0)?),
        uuid: row.get(1)?,
        slug: row.get(2)?,
        name: row.get(3)?,
        prefix: row.get(4)?,
        created_at: row.get(5)?,
        next_story_no: row.get(6)?,
        next_global_seq: row.get(7)?,
    })
}

pub(super) fn project(
    conn: &Connection,
    project: ProjectId,
) -> Result<Option<ProjectRecord>, StoreError> {
    one(
        conn,
        &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE id = ?1"),
        params![project.get()],
        project_from_row,
        "reading a project",
    )
}

pub(super) fn project_by_uuid(
    conn: &Connection,
    uuid: &str,
) -> Result<Option<ProjectRecord>, StoreError> {
    one(
        conn,
        &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE uuid = ?1"),
        params![uuid],
        project_from_row,
        "reading a project by uuid",
    )
}

pub(super) fn project_by_slug(
    conn: &Connection,
    slug: &str,
) -> Result<Option<ProjectRecord>, StoreError> {
    one(
        conn,
        &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE slug = ?1"),
        params![slug],
        project_from_row,
        "reading a project by slug",
    )
}

pub(super) fn project_by_prefix(
    conn: &Connection,
    prefix: &str,
) -> Result<Option<ProjectRecord>, StoreError> {
    one(
        conn,
        &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE prefix = ?1"),
        params![prefix],
        project_from_row,
        "reading a project by prefix",
    )
}

/// The project that registered this git origin.
///
/// Matched on the normalized key alone. `idx_project_remotes_normalized`
/// guarantees at most one row can match, so there is no ordering to choose and
/// no ambiguity to resolve — which is the entire point of putting the
/// uniqueness in the schema.
pub(super) fn project_by_remote(
    conn: &Connection,
    remote: &RemoteUrl,
) -> Result<Option<ProjectRecord>, StoreError> {
    let columns = PROJECT_COLUMNS
        .split(", ")
        .map(|c| format!("p.{c}"))
        .collect::<Vec<_>>()
        .join(", ");
    one(
        conn,
        &format!(
            "SELECT {columns} FROM projects p \
             JOIN project_remotes pr ON pr.project_id = p.id WHERE pr.normalized = ?1"
        ),
        params![remote.key()],
        project_from_row,
        "reading a project by remote",
    )
}

pub(super) fn project_remotes(
    conn: &Connection,
    project: ProjectId,
) -> Result<Vec<ProjectRemoteRecord>, StoreError> {
    let mut stmt = sql(
        conn.prepare_cached(
            "SELECT normalized, raw, registered_at FROM project_remotes \
             WHERE project_id = ?1 ORDER BY normalized",
        ),
        "preparing project_remotes",
    )?;
    let rows = sql(
        stmt.query_map(params![project.get()], |row| {
            Ok(ProjectRemoteRecord {
                normalized: row.get(0)?,
                raw: row.get(1)?,
                registered_at: row.get(2)?,
            })
        }),
        "reading project remotes",
    )?;
    collect(rows, "reading project remotes")
}

pub(super) fn checkout_path(
    conn: &Connection,
    project: ProjectId,
) -> Result<Option<PathBuf>, StoreError> {
    let mut stmt = sql(
        conn.prepare_cached("SELECT checkout_path FROM projects WHERE id = ?1"),
        "preparing checkout_path",
    )?;
    let path: Option<Option<String>> = sql(
        stmt.query_row(params![project.get()], |row| row.get(0))
            .optional(),
        "reading a project checkout",
    )?;
    Ok(path.flatten().map(PathBuf::from))
}

pub(super) fn commit_scan_at(
    conn: &Connection,
    project: ProjectId,
) -> Result<Option<String>, StoreError> {
    let mut stmt = sql(
        conn.prepare_cached("SELECT commit_scan_at FROM projects WHERE id = ?1"),
        "preparing commit_scan_at",
    )?;
    let at: Option<Option<String>> = sql(
        stmt.query_row(params![project.get()], |row| row.get(0))
            .optional(),
        "reading a project commit scan",
    )?;
    Ok(at.flatten())
}

pub(super) fn projects(conn: &Connection) -> Result<Vec<ProjectRecord>, StoreError> {
    let mut stmt = sql(
        conn.prepare_cached(&format!(
            "SELECT {PROJECT_COLUMNS} FROM projects ORDER BY slug"
        )),
        "preparing projects",
    )?;
    let rows = sql(stmt.query_map([], project_from_row), "reading projects")?;
    collect(rows, "reading projects")
}

// ---------------------------------------------------------------------------
// Full Auto engine operational state
// ---------------------------------------------------------------------------

const ENGINE_RUN_COLUMNS: &str = "id, project_slug, scope_kind, scope_story_id, lanes, agent, \
    state, consecutive_hard_stops, stop_reason, acknowledged_at, created_at, updated_at";

#[derive(Debug)]
struct RawEngineRun {
    id: String,
    project_slug: String,
    scope_kind: String,
    scope_story_id: Option<String>,
    lanes: i64,
    agent: String,
    state: String,
    consecutive_hard_stops: i64,
    stop_reason: Option<String>,
    acknowledged_at: Option<String>,
    created_at: String,
    updated_at: String,
}

fn raw_engine_run(row: &Row<'_>) -> Result<RawEngineRun, rusqlite::Error> {
    Ok(RawEngineRun {
        id: row.get(0)?,
        project_slug: row.get(1)?,
        scope_kind: row.get(2)?,
        scope_story_id: row.get(3)?,
        lanes: row.get(4)?,
        agent: row.get(5)?,
        state: row.get(6)?,
        consecutive_hard_stops: row.get(7)?,
        stop_reason: row.get(8)?,
        acknowledged_at: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn stored_u32(value: i64, column: &str) -> Result<u32, StoreError> {
    u32::try_from(value)
        .map_err(|_| StoreError::Corrupt(format!("{column} holds out-of-range value `{value}`")))
}

fn hydrate_engine_run(raw: RawEngineRun) -> Result<EngineRunRecord, StoreError> {
    let scope = match (raw.scope_kind.as_str(), raw.scope_story_id) {
        ("project", None) => EngineScope::Project,
        ("epic", Some(story_id)) => EngineScope::Epic(story_id),
        (kind, story_id) => {
            return Err(StoreError::Corrupt(format!(
                "engine_runs scope is inconsistent: kind `{kind}`, story id {story_id:?}"
            )));
        }
    };
    let agent = EngineAgent::parse(&raw.agent).ok_or_else(|| {
        StoreError::Corrupt(format!(
            "engine_runs.agent holds unknown value `{}`",
            raw.agent
        ))
    })?;
    let state = EngineRunState::parse(&raw.state).ok_or_else(|| {
        StoreError::Corrupt(format!(
            "engine_runs.state holds unknown value `{}`",
            raw.state
        ))
    })?;
    Ok(EngineRunRecord {
        id: raw.id,
        project_slug: raw.project_slug,
        scope,
        lanes: stored_u32(raw.lanes, "engine_runs.lanes")?,
        agent,
        state,
        consecutive_hard_stops: stored_u32(
            raw.consecutive_hard_stops,
            "engine_runs.consecutive_hard_stops",
        )?,
        stop_reason: raw.stop_reason,
        acknowledged_at: raw.acknowledged_at,
        created_at: raw.created_at,
        updated_at: raw.updated_at,
    })
}

pub(super) fn engine_run(
    conn: &Connection,
    run_id: &str,
) -> Result<Option<EngineRunRecord>, StoreError> {
    one(
        conn,
        &format!("SELECT {ENGINE_RUN_COLUMNS} FROM engine_runs WHERE id = ?1"),
        &[&run_id],
        raw_engine_run,
        "reading an engine run",
    )?
    .map(hydrate_engine_run)
    .transpose()
}

fn engine_run_list(
    conn: &Connection,
    text: &str,
    binds: &[&dyn rusqlite::ToSql],
    context: &str,
) -> Result<Vec<EngineRunRecord>, StoreError> {
    let mut stmt = sql(conn.prepare_cached(text), context)?;
    let rows = sql(stmt.query_map(binds, raw_engine_run), context)?;
    collect(rows, context)?
        .into_iter()
        .map(hydrate_engine_run)
        .collect()
}

pub(super) fn engine_runs(
    conn: &Connection,
    project_slug: &str,
) -> Result<Vec<EngineRunRecord>, StoreError> {
    engine_run_list(
        conn,
        &format!(
            "SELECT {ENGINE_RUN_COLUMNS} FROM engine_runs \
             WHERE project_slug = ?1 ORDER BY created_at, id"
        ),
        &[&project_slug],
        "reading a project's engine runs",
    )
}

pub(super) fn live_engine_runs(conn: &Connection) -> Result<Vec<EngineRunRecord>, StoreError> {
    engine_run_list(
        conn,
        &format!(
            "SELECT {ENGINE_RUN_COLUMNS} FROM engine_runs \
             WHERE state IN ('running','paused','draining') \
             ORDER BY project_slug, created_at, id"
        ),
        &[],
        "reading live engine runs",
    )
}

pub(super) fn engine_lanes(
    conn: &Connection,
    run_id: &str,
) -> Result<Vec<EngineLaneRecord>, StoreError> {
    let mut stmt = sql(
        conn.prepare_cached(
            // `last_progress_seq`/`last_progress_at` are appended at the end
            // rather than beside `last_observed_at`, so every existing
            // positional `row.get(N)` below keeps its index (SH-365: this
            // struct is filled BY POSITION, and swapping an adjacent pair
            // names every field correctly and fills every one wrong).
            "SELECT run_id, lane_index, state, story_id, window_name, worktree_path, \
                    dispatched_at, last_observed_at, outcome, outcome_detail, \
                    last_progress_seq, last_progress_at, pane_id \
             FROM engine_lanes WHERE run_id = ?1 ORDER BY lane_index",
        ),
        "preparing engine lanes",
    )?;
    let rows = sql(
        stmt.query_map(params![run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<String>>(12)?,
            ))
        }),
        "reading engine lanes",
    )?;
    let raw = collect(rows, "reading engine lanes")?;
    raw.into_iter()
        .map(
            |(
                run_id,
                lane_index,
                state,
                story_id,
                window_name,
                worktree_path,
                dispatched_at,
                last_observed_at,
                outcome,
                outcome_detail,
                last_progress_seq,
                last_progress_at,
                pane_id,
            )| {
                let state = EngineLaneState::parse(&state).ok_or_else(|| {
                    StoreError::Corrupt(format!("engine_lanes.state holds unknown value `{state}`"))
                })?;
                Ok(EngineLaneRecord {
                    run_id,
                    lane_index: stored_u32(lane_index, "engine_lanes.lane_index")?,
                    state,
                    story_id,
                    pane_id,
                    window_name,
                    worktree_path,
                    dispatched_at,
                    last_observed_at,
                    last_progress_seq: last_progress_seq.map(GlobalSeq::new),
                    last_progress_at,
                    outcome,
                    outcome_detail,
                })
            },
        )
        .collect()
}

pub(super) fn event_count(conn: &Connection, project: ProjectId) -> Result<usize, StoreError> {
    let count: i64 = sql(
        conn.query_row(
            "SELECT count(*) FROM events WHERE project_id = ?1",
            params![project.get()],
            |row| row.get(0),
        ),
        "counting a project's events",
    )?;
    Ok(usize::try_from(count).unwrap_or(usize::MAX))
}

/// A project's story-id prefix, needed wherever a `StorySnapshot`'s textual id
/// has to be turned into a number or back.
pub(super) fn prefix(conn: &Connection, project: ProjectId) -> Result<String, StoreError> {
    one(
        conn,
        "SELECT prefix FROM projects WHERE id = ?1",
        params![project.get()],
        |row| row.get::<_, String>(0),
        "reading a project prefix",
    )?
    .ok_or_else(|| StoreError::NotFound(format!("project {project} does not exist")))
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

pub(super) fn states(conn: &Connection, project: ProjectId) -> Result<Vec<StateDef>, StoreError> {
    let mut stmt = sql(
        conn.prepare_cached(
            "SELECT slug, superstate, role, description FROM project_states \
             WHERE project_id = ?1 ORDER BY position",
        ),
        "preparing states",
    )?;
    let rows = sql(
        stmt.query_map(params![project.get()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        }),
        "reading states",
    )?;
    let raw: Vec<(String, String, Option<String>, Option<String>)> =
        collect(rows, "reading states")?;
    raw.into_iter()
        .map(|(slug, superstate, role, description)| {
            Ok(StateDef {
                slug,
                super_state: parse_superstate(&superstate)?,
                role,
                description,
            })
        })
        .collect()
}

/// A project's states keyed by slug — the shape [`crate::domain::fold_story`]
/// wants.
pub(super) fn state_map(
    conn: &Connection,
    project: ProjectId,
) -> Result<BTreeMap<String, StateDef>, StoreError> {
    Ok(states(conn, project)?
        .into_iter()
        .map(|state| (state.slug.clone(), state))
        .collect())
}

pub(super) fn types(conn: &Connection, project: ProjectId) -> Result<Vec<TypeDef>, StoreError> {
    let mut stmt = sql(
        conn.prepare_cached(
            "SELECT slug, description, emoji FROM project_types WHERE project_id = ?1 \
             ORDER BY position",
        ),
        "preparing types",
    )?;
    let rows = sql(
        stmt.query_map(params![project.get()], |row| {
            Ok(TypeDef {
                slug: row.get(0)?,
                description: row.get(1)?,
                emoji: row.get(2)?,
            })
        }),
        "reading types",
    )?;
    collect(rows, "reading types")
}

pub(super) fn members(conn: &Connection, project: ProjectId) -> Result<Vec<Member>, StoreError> {
    let mut stmt = sql(
        conn.prepare_cached(
            "SELECT member_id, display_name, email, github, created_at FROM project_members \
             WHERE project_id = ?1 ORDER BY member_id",
        ),
        "preparing members",
    )?;
    let rows = sql(
        stmt.query_map(params![project.get()], |row| {
            Ok(Member {
                id: row.get(0)?,
                display_name: row.get(1)?,
                email: row.get(2)?,
                github: row.get(3)?,
                created_at: row.get(4)?,
            })
        }),
        "reading members",
    )?;
    collect(rows, "reading members")
}

pub(super) fn settings(
    conn: &Connection,
    project: ProjectId,
) -> Result<ProjectSettings, StoreError> {
    let row = one(
        conn,
        "SELECT sync_auto_transition, doctor_stale_threshold \
         FROM project_settings WHERE project_id = ?1",
        params![project.get()],
        |row| {
            Ok((
                row.get::<_, Option<bool>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        },
        "reading settings",
    )?;
    // A project with no settings row has no settings — not an error. What a
    // default means belongs to the caller, which is the layer that has one.
    let Some((sync_auto_transition, doctor_stale_threshold)) = row else {
        return Ok(ProjectSettings::default());
    };
    Ok(ProjectSettings {
        sync_auto_transition,
        doctor_stale_threshold,
    })
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Decodes one stored event, keeping an unrecognised payload verbatim.
///
/// This is the SH-54 contract in one function: a kind this binary has never
/// heard of produces a [`StoredPayload::Unknown`] carrying the original JSON,
/// not an error that fails the whole read.
fn decode(
    kind: String,
    at: String,
    seq: i64,
    global_seq: i64,
    payload: String,
    provenance: Provenance,
) -> StoredEvent {
    let decoded = match serde_json::from_str::<StoryEvent>(&payload) {
        Ok(event) => StoredPayload::Known(event),
        Err(_) => StoredPayload::Unknown {
            kind: kind.clone(),
            json: payload,
        },
    };
    StoredEvent {
        seq: EventSeq::new(seq),
        global_seq: GlobalSeq::new(global_seq),
        kind,
        at,
        payload: decoded,
        provenance,
    }
}

/// Rebuilds a row's provenance from its two nullable columns (SH-246).
///
/// A stored `actor` is trusted back verbatim rather than re-parsed: it passed
/// [`ActorLabel::parse`] on the way in, and a read is the wrong place to start
/// failing over bytes that are already durable. A value that somehow violates
/// the constraint — hand-edited into the file, say — is still bounded by what
/// the renderer will do with it, and refusing the whole read would take a
/// story's history away over a label.
fn provenance_of(command: Option<String>, actor: Option<String>) -> Provenance {
    Provenance {
        command,
        actor: actor.map(ActorLabel::trusted),
    }
}

pub(super) fn events_for(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
) -> Result<Vec<StoredEvent>, StoreError> {
    let mut stmt = sql(
        conn.prepare_cached(
            "SELECT seq, global_seq, kind, at, payload, command, actor FROM events \
             WHERE project_id = ?1 AND story_no = ?2 ORDER BY seq",
        ),
        "preparing events_for",
    )?;
    let rows = sql(
        stmt.query_map(params![project.get(), story.get()], |row| {
            Ok(decode(
                row.get(2)?,
                row.get(3)?,
                row.get(0)?,
                row.get(1)?,
                row.get(4)?,
                provenance_of(row.get(5)?, row.get(6)?),
            ))
        }),
        "reading a story's events",
    )?;
    collect(rows, "reading a story's events")
}

pub(super) fn head_seq(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
) -> Result<EventSeq, StoreError> {
    let mut stmt = sql(
        conn.prepare_cached(
            "SELECT COALESCE(MAX(seq), 0) FROM events WHERE project_id = ?1 AND story_no = ?2",
        ),
        "preparing head_seq",
    )?;
    let head: i64 = sql(
        stmt.query_row(params![project.get(), story.get()], |row| row.get(0)),
        "reading a story head",
    )?;
    Ok(EventSeq::new(head))
}

pub(super) fn events_since(
    conn: &Connection,
    project: ProjectId,
    after: GlobalSeq,
    limit: u32,
) -> Result<Vec<FeedEvent>, StoreError> {
    let mut stmt = sql(
        conn.prepare_cached(
            "SELECT story_no, seq, global_seq, kind, at, payload, command, actor FROM events \
             WHERE project_id = ?1 AND global_seq > ?2 ORDER BY global_seq LIMIT ?3",
        ),
        "preparing events_since",
    )?;
    let rows = sql(
        stmt.query_map(params![project.get(), after.get(), limit], |row| {
            Ok(FeedEvent {
                story_no: StoryNo::new(row.get(0)?),
                event: decode(
                    row.get(3)?,
                    row.get(4)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(5)?,
                    provenance_of(row.get(6)?, row.get(7)?),
                ),
            })
        }),
        "reading the change feed",
    )?;
    collect(rows, "reading the change feed")
}

pub(super) fn max_global_seq(
    conn: &Connection,
    project: ProjectId,
) -> Result<GlobalSeq, StoreError> {
    let mut stmt = sql(
        conn.prepare_cached(
            "SELECT COALESCE(MAX(global_seq), 0) FROM events WHERE project_id = ?1",
        ),
        "preparing max_global_seq",
    )?;
    let max: i64 = sql(
        stmt.query_row(params![project.get()], |row| row.get(0)),
        "reading the change feed head",
    )?;
    Ok(GlobalSeq::new(max))
}

// ---------------------------------------------------------------------------
// Stories
// ---------------------------------------------------------------------------

/// The `stories` row exactly as SQLite hands it over.
///
/// Kept separate from the public [`StoryRow`] so that a column holding a value
/// the schema should have made impossible is reported as
/// [`StoreError::Corrupt`] naming the column, rather than as a rusqlite type
/// conversion that cannot say which column it was.
struct RawStoryRow {
    story_no: i64,
    head_seq: i64,
    title: String,
    state: String,
    superstate: String,
    priority: String,
    story_type: Option<String>,
    assignee: Option<String>,
    awaiting: Option<String>,
    archived: bool,
    created_at: String,
    updated_at: String,
    closed_at: Option<String>,
    description: Option<String>,
    hidden_at: Option<String>,
    draft: bool,
    snapshot: String,
    head_global_seq: i64,
}

fn raw_story_from_row(row: &Row<'_>) -> Result<RawStoryRow, rusqlite::Error> {
    Ok(RawStoryRow {
        story_no: row.get(0)?,
        head_seq: row.get(1)?,
        title: row.get(2)?,
        state: row.get(3)?,
        superstate: row.get(4)?,
        priority: row.get(5)?,
        story_type: row.get(6)?,
        assignee: row.get(7)?,
        awaiting: row.get(8)?,
        archived: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
        closed_at: row.get(12)?,
        description: row.get(13)?,
        hidden_at: row.get(14)?,
        draft: row.get(15)?,
        snapshot: row.get(16)?,
        head_global_seq: row.get(17)?,
    })
}

fn hydrate(raw: RawStoryRow, labels: Vec<String>) -> Result<StoryRow, StoreError> {
    Ok(StoryRow {
        story_no: StoryNo::new(raw.story_no),
        head_seq: EventSeq::new(raw.head_seq),
        head_global_seq: GlobalSeq::new(raw.head_global_seq),
        title: raw.title,
        state: raw.state,
        superstate: parse_superstate(&raw.superstate)?,
        priority: parse_priority(&raw.priority)?,
        story_type: raw.story_type,
        assignee: raw.assignee,
        awaiting: raw.awaiting,
        archived: raw.archived,
        created_at: raw.created_at,
        updated_at: raw.updated_at,
        closed_at: raw.closed_at,
        description: raw.description,
        hidden_at: raw.hidden_at,
        draft: raw.draft,
        labels,
        snapshot: serde_json::from_str(&raw.snapshot)?,
    })
}

/// Whether `(project, story, sha)` is already in `story_commit_links`.
///
/// An indexed primary-key probe. `EXISTS` rather than a count: the question is
/// membership, and the answer is available from the index alone.
pub(super) fn commit_linked(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
    sha: &str,
) -> Result<bool, StoreError> {
    let found: i64 = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM story_commit_links \
             WHERE project_id = ?1 AND story_no = ?2 AND sha = ?3)",
            params![project.get(), story.get(), sha],
            |row| row.get(0),
        )
        .map_err(|e| StoreError::from_sqlite(e, "reading a commit link"))?;
    Ok(found != 0)
}

/// Every `story_commit_links` row with no backing `StoryCommitLinked` event.
/// See [`ReadOps::unbacked_commit_links`](crate::store::ReadOps::unbacked_commit_links).
///
/// A row backed by a real kind-18 event always carries that event's own
/// `sha` field verbatim (`project_commit_link`'s `is_link_record` branch never
/// truncates it), so an exact match against the event's `payload` is the
/// whole test — no length heuristic needed.
pub(super) fn unbacked_commit_links(
    conn: &Connection,
    project: ProjectId,
) -> Result<Vec<(StoryNo, String)>, StoreError> {
    let mut stmt = sql(
        conn.prepare_cached(
            "SELECT scl.story_no, scl.sha FROM story_commit_links scl \
             WHERE scl.project_id = ?1 \
               AND NOT EXISTS ( \
                 SELECT 1 FROM events e \
                 WHERE e.project_id = scl.project_id \
                   AND e.story_no = scl.story_no \
                   AND e.kind = ?2 \
                   AND json_extract(e.payload, '$.sha') = scl.sha \
               ) \
             ORDER BY scl.story_no, scl.sha",
        ),
        "preparing unbacked commit links",
    )?;
    let rows = sql(
        stmt.query_map(
            params![project.get(), crate::domain::KIND_STORY_COMMIT_LINKED],
            |row| Ok((StoryNo::new(row.get(0)?), row.get::<_, String>(1)?)),
        ),
        "reading unbacked commit links",
    )?;
    collect(rows, "reading unbacked commit links")
}

const PR_LINK_COLUMNS: &str =
    "owner, repo, number, url, close_on_merge, status, linked_at, last_checked_at";

fn pr_link_from_row(row: &Row<'_>) -> Result<PrLink, rusqlite::Error> {
    Ok(PrLink {
        owner: row.get(0)?,
        repo: row.get(1)?,
        number: row.get(2)?,
        url: row.get(3)?,
        close_on_merge: row.get::<_, i64>(4)? != 0,
        status: row.get(5)?,
        linked_at: row.get(6)?,
        last_checked_at: row.get(7)?,
    })
}

/// This story's still-`open` linked pull requests. See
/// [`ReadOps::open_pr_links_for_story`](crate::store::ReadOps::open_pr_links_for_story).
pub(super) fn open_pr_links_for_story(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
) -> Result<Vec<PrLink>, StoreError> {
    let mut stmt = sql(
        conn.prepare(&format!(
            "SELECT {PR_LINK_COLUMNS} FROM story_pr_links \
             WHERE project_id = ?1 AND story_no = ?2 AND status = 'open' \
             ORDER BY owner, repo, number"
        )),
        "preparing an open-PR-links-for-story read",
    )?;
    let rows = sql(
        stmt.query_map(params![project.get(), story.get()], pr_link_from_row),
        "reading open PR links for a story",
    )?;
    collect(rows, "reading open PR links for a story")
}

/// Every still-`open` linked pull request across the project. See
/// [`ReadOps::open_pr_links`](crate::store::ReadOps::open_pr_links).
pub(super) fn open_pr_links(
    conn: &Connection,
    project: ProjectId,
) -> Result<Vec<(StoryNo, PrLink)>, StoreError> {
    let mut stmt = sql(
        conn.prepare(&format!(
            "SELECT story_no, {PR_LINK_COLUMNS} FROM story_pr_links \
             WHERE project_id = ?1 AND status = 'open' \
             ORDER BY story_no, owner, repo, number"
        )),
        "preparing an open-PR-links read",
    )?;
    let rows = sql(
        stmt.query_map(params![project.get()], |row| {
            let story_no: i64 = row.get(0)?;
            Ok((StoryNo::new(story_no), pr_link_from_row_offset(row, 1)?))
        }),
        "reading open PR links",
    )?;
    collect(rows, "reading open PR links")
}

/// Every linked pull request across the project regardless of status. See
/// [`ReadOps::pr_links`](crate::store::ReadOps::pr_links).
pub(super) fn pr_links(
    conn: &Connection,
    project: ProjectId,
) -> Result<Vec<(StoryNo, PrLink)>, StoreError> {
    let mut stmt = sql(
        conn.prepare(&format!(
            "SELECT story_no, {PR_LINK_COLUMNS} FROM story_pr_links \
             WHERE project_id = ?1 \
             ORDER BY story_no, owner, repo, number"
        )),
        "preparing a PR-links read",
    )?;
    let rows = sql(
        stmt.query_map(params![project.get()], |row| {
            let story_no: i64 = row.get(0)?;
            Ok((StoryNo::new(story_no), pr_link_from_row_offset(row, 1)?))
        }),
        "reading PR links",
    )?;
    collect(rows, "reading PR links")
}

/// [`pr_link_from_row`], for a query whose columns start at `offset` rather
/// than `0` — [`open_pr_links`] prepends `story_no`.
fn pr_link_from_row_offset(row: &Row<'_>, offset: usize) -> Result<PrLink, rusqlite::Error> {
    Ok(PrLink {
        owner: row.get(offset)?,
        repo: row.get(offset + 1)?,
        number: row.get(offset + 2)?,
        url: row.get(offset + 3)?,
        close_on_merge: row.get::<_, i64>(offset + 4)? != 0,
        status: row.get(offset + 5)?,
        linked_at: row.get(offset + 6)?,
        last_checked_at: row.get(offset + 7)?,
    })
}

/// One attachment's stored bytes (SH-315). See
/// [`ReadOps::attachment_blob`](crate::store::ReadOps::attachment_blob).
pub(super) fn attachment_blob(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
    attachment_id: u32,
) -> Result<Option<Vec<u8>>, StoreError> {
    sql(
        conn.query_row(
            "SELECT bytes FROM story_attachment_blobs \
             WHERE project_id = ?1 AND story_no = ?2 AND attachment_id = ?3",
            params![project.get(), story.get(), attachment_id],
            |row| row.get(0),
        )
        .optional(),
        "reading an attachment blob",
    )
}

/// Every attachment blob's metadata across the project (SH-315). See
/// [`ReadOps::attachment_blobs`](crate::store::ReadOps::attachment_blobs).
pub(super) fn attachment_blobs(
    conn: &Connection,
    project: ProjectId,
) -> Result<Vec<(StoryNo, AttachmentBlobRow)>, StoreError> {
    let mut stmt = sql(
        conn.prepare(
            "SELECT story_no, attachment_id, byte_len, sha256 FROM story_attachment_blobs \
             WHERE project_id = ?1 \
             ORDER BY story_no, attachment_id",
        ),
        "preparing an attachment-blobs read",
    )?;
    let rows = sql(
        stmt.query_map(params![project.get()], |row| {
            let story_no: i64 = row.get(0)?;
            Ok((
                StoryNo::new(story_no),
                AttachmentBlobRow {
                    attachment_id: row.get(1)?,
                    byte_len: row.get(2)?,
                    sha256: row.get(3)?,
                },
            ))
        }),
        "reading attachment blobs",
    )?;
    collect(rows, "reading attachment blobs")
}

pub(super) fn story(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
) -> Result<Option<StoryRow>, StoreError> {
    let raw = one(
        conn,
        &format!("SELECT {STORY_COLUMNS} FROM stories WHERE project_id = ?1 AND story_no = ?2"),
        params![project.get(), story.get()],
        raw_story_from_row,
        "reading a story",
    )?;
    match raw {
        None => Ok(None),
        Some(raw) => Ok(Some(hydrate(raw, labels_for(conn, project, story)?)?)),
    }
}

pub(super) fn stories(
    conn: &Connection,
    project: ProjectId,
    query: &StoryQuery,
) -> Result<Vec<StoryRow>, StoreError> {
    let mut text = format!("SELECT {STORY_COLUMNS} FROM stories WHERE project_id = ?1");
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(project.get())];

    macro_rules! filter {
        ($value:expr, $fragment:literal) => {{
            binds.push(Box::new($value));
            text.push_str(&format!($fragment, binds.len()));
        }};
    }

    if let Some(superstate) = &query.superstate {
        filter!(superstate.as_str().to_string(), " AND superstate = ?{}");
    }
    if let Some(state) = &query.state {
        filter!(state.clone(), " AND state = ?{}");
    }
    if let Some(priority) = &query.priority {
        filter!(priority.as_str().to_string(), " AND priority = ?{}");
    }
    if let Some(assignee) = &query.assignee {
        filter!(assignee.clone(), " AND assignee = ?{}");
    }
    if let Some(story_type) = &query.story_type {
        filter!(story_type.clone(), " AND story_type = ?{}");
    }
    if let Some(archived) = query.archived {
        filter!(archived, " AND archived = ?{}");
    }
    if let Some(hidden) = query.hidden {
        text.push_str(if hidden {
            " AND hidden_at IS NOT NULL"
        } else {
            " AND hidden_at IS NULL"
        });
    }
    if let Some(draft) = query.draft {
        filter!(draft, " AND draft = ?{}");
    }
    if let Some(label) = &query.label {
        filter!(
            label.clone(),
            " AND EXISTS (SELECT 1 FROM story_labels l WHERE l.project_id = stories.project_id \
             AND l.story_no = stories.story_no AND l.label = ?{})"
        );
    }

    // Every order ends in `story_no`, which is unique within a project, so each
    // of them is a *total* order. Identical input cannot produce two different
    // orderings — which the legacy `priority ASC, created_at ASC` comparator
    // could and did, `created_at` having only one-second precision.
    //
    // `UpdatedAt` breaks a same-second tie on `head_global_seq` (SH-336) before
    // falling back to `story_no`: `updated_at` has the same one-second
    // precision `Priority`'s own comment already names, but `head_global_seq`
    // is the exact position of each row's head event in the project's write
    // order and cannot tie the way a timestamp can (writes are serialized
    // behind one process-wide write mutex). `story_no` stays as the final key
    // rather than being dropped — it is the only thing that keeps the order
    // total for the `extra_rows` case, where a row's `head_global_seq` is 0
    // because no event backs it.
    text.push_str(match query.sort {
        StorySort::StoryNo => " ORDER BY story_no",
        StorySort::Priority => " ORDER BY priority_rank, story_no",
        StorySort::UpdatedAt => " ORDER BY updated_at DESC, head_global_seq DESC, story_no",
    });
    if let Some(limit) = query.limit {
        filter!(limit, " LIMIT ?{}");
    }

    let mut stmt = sql(conn.prepare_cached(&text), "preparing stories")?;
    let rows = sql(
        stmt.query_map(params_from_iter(binds.iter()), raw_story_from_row),
        "reading stories",
    )?;
    let raw: Vec<RawStoryRow> = collect(rows, "reading stories")?;

    // One query for the whole matched set rather than one per row.
    let mut index = labels_index(conn, project)?;
    raw.into_iter()
        .map(|row| {
            let labels = index
                .remove(&StoryNo::new(row.story_no))
                .unwrap_or_default();
            hydrate(row, labels)
        })
        .collect()
}

fn labels_for(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
) -> Result<Vec<String>, StoreError> {
    let mut stmt = sql(
        conn.prepare_cached(
            "SELECT label FROM story_labels WHERE project_id = ?1 AND story_no = ?2 ORDER BY label",
        ),
        "preparing labels_for",
    )?;
    let rows = sql(
        stmt.query_map(params![project.get(), story.get()], |row| row.get(0)),
        "reading story labels",
    )?;
    collect(rows, "reading story labels")
}

fn labels_index(
    conn: &Connection,
    project: ProjectId,
) -> Result<BTreeMap<StoryNo, Vec<String>>, StoreError> {
    let mut stmt = sql(
        conn.prepare_cached(
            "SELECT story_no, label FROM story_labels WHERE project_id = ?1 \
             ORDER BY story_no, label",
        ),
        "preparing labels_index",
    )?;
    let rows = sql(
        stmt.query_map(params![project.get()], |row| {
            Ok((StoryNo::new(row.get(0)?), row.get::<_, String>(1)?))
        }),
        "reading project labels",
    )?;
    let pairs: Vec<(StoryNo, String)> = collect(rows, "reading project labels")?;
    let mut index: BTreeMap<StoryNo, Vec<String>> = BTreeMap::new();
    for (story_no, label) in pairs {
        index.entry(story_no).or_default().push(label);
    }
    Ok(index)
}

// ---------------------------------------------------------------------------
// Relations
// ---------------------------------------------------------------------------

fn edge_from_row(row: &Row<'_>) -> Result<RelationEdge, rusqlite::Error> {
    Ok(RelationEdge {
        story_no: StoryNo::new(row.get(0)?),
        relation: row.get(1)?,
        other_no: StoryNo::new(row.get(2)?),
    })
}

pub(super) fn relations_from(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
) -> Result<Vec<RelationEdge>, StoreError> {
    let mut stmt = sql(
        conn.prepare_cached(
            "SELECT story_no, relation, other_no FROM story_relations \
             WHERE project_id = ?1 AND story_no = ?2 ORDER BY relation, other_no",
        ),
        "preparing relations_from",
    )?;
    let rows = sql(
        stmt.query_map(params![project.get(), story.get()], edge_from_row),
        "reading outbound relations",
    )?;
    collect(rows, "reading outbound relations")
}

pub(super) fn relations_to(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
) -> Result<Vec<RelationEdge>, StoreError> {
    let mut stmt = sql(
        conn.prepare_cached(
            "SELECT story_no, relation, other_no FROM story_relations \
             WHERE project_id = ?1 AND other_no = ?2 ORDER BY relation, story_no",
        ),
        "preparing relations_to",
    )?;
    let rows = sql(
        stmt.query_map(params![project.get(), story.get()], edge_from_row),
        "reading inbound relations",
    )?;
    collect(rows, "reading inbound relations")
}

// ---------------------------------------------------------------------------

/// Runs a statement expected to match at most one row.
fn one<T>(
    conn: &Connection,
    text: &str,
    binds: &[&dyn rusqlite::ToSql],
    map: impl FnMut(&Row<'_>) -> Result<T, rusqlite::Error>,
    context: &str,
) -> Result<Option<T>, StoreError> {
    let mut stmt = sql(conn.prepare_cached(text), context)?;
    let mut rows = sql(stmt.query_map(binds, map), context)?;
    rows.next()
        .transpose()
        .map_err(|e| StoreError::from_sqlite(e, context))
}
