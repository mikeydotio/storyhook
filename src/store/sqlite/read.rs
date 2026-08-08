//! Every read the store performs, as free functions over a `&Connection`.
//!
//! Written once here and delegated to by both [`super::SqliteReadTx`] and
//! [`super::SqliteWriteTx`], so that a read issued inside a write transaction
//! is the *same* read — a writer that could observe a different world than a
//! reader is how a read model and its events drift apart.
//!
//! Every function takes a [`ProjectId`]. There is no unscoped read in this
//! file and there must never be one: in a single global database where every
//! repository defaults to the prefix `SH`, an unscoped query does not fail, it
//! silently returns another project's story.

use std::collections::BTreeMap;
use std::path::PathBuf;

use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use crate::domain::remote::RemoteUrl;
use crate::domain::{Member, StateDef, StoryEvent, StorySnapshot, TypeDef};
use crate::store::error::StoreError;
use crate::store::ids::{EventSeq, GlobalSeq, ProjectId, StoryNo};
use crate::store::types::{
    FeedEvent, ProjectRecord, ProjectRemoteRecord, ProjectSettings, RelationEdge, StoredEvent,
    StoredPayload, StoryQuery, StoryRow, StorySort, parse_priority, parse_superstate,
};

const PROJECT_COLUMNS: &str =
    "id, uuid, slug, name, prefix, created_at, next_story_no, next_global_seq";

const STORY_COLUMNS: &str = "story_no, head_seq, title, state, superstate, priority, story_type, \
     assignee, awaiting, deleted, archived, created_at, updated_at, closed_at, description, \
     hidden_at, snapshot";

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
        "SELECT sync_auto_transition, doctor_stale_threshold, github_sync \
         FROM project_settings WHERE project_id = ?1",
        params![project.get()],
        |row| {
            Ok((
                row.get::<_, Option<bool>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        },
        "reading settings",
    )?;
    // A project with no settings row has no settings — not an error. What a
    // default means belongs to the caller, which is the layer that has one.
    let Some((sync_auto_transition, doctor_stale_threshold, github_sync)) = row else {
        return Ok(ProjectSettings::default());
    };
    Ok(ProjectSettings {
        sync_auto_transition,
        doctor_stale_threshold,
        github_sync: github_sync
            .map(|text| serde_json::from_str(&text))
            .transpose()?,
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
fn decode(kind: String, at: String, seq: i64, global_seq: i64, payload: String) -> StoredEvent {
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
    }
}

pub(super) fn events_for(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
) -> Result<Vec<StoredEvent>, StoreError> {
    let mut stmt = sql(
        conn.prepare_cached(
            "SELECT seq, global_seq, kind, at, payload FROM events \
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
            "SELECT story_no, seq, global_seq, kind, at, payload FROM events \
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
    deleted: bool,
    archived: bool,
    created_at: String,
    updated_at: String,
    closed_at: Option<String>,
    description: Option<String>,
    hidden_at: Option<String>,
    snapshot: String,
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
        deleted: row.get(9)?,
        archived: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        closed_at: row.get(13)?,
        description: row.get(14)?,
        hidden_at: row.get(15)?,
        snapshot: row.get(16)?,
    })
}

fn hydrate(raw: RawStoryRow, labels: Vec<String>) -> Result<StoryRow, StoreError> {
    Ok(StoryRow {
        story_no: StoryNo::new(raw.story_no),
        head_seq: EventSeq::new(raw.head_seq),
        title: raw.title,
        state: raw.state,
        superstate: parse_superstate(&raw.superstate)?,
        priority: parse_priority(&raw.priority)?,
        story_type: raw.story_type,
        assignee: raw.assignee,
        awaiting: raw.awaiting,
        deleted: raw.deleted,
        archived: raw.archived,
        created_at: raw.created_at,
        updated_at: raw.updated_at,
        closed_at: raw.closed_at,
        description: raw.description,
        hidden_at: raw.hidden_at,
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
    if let Some(deleted) = query.deleted {
        filter!(deleted, " AND deleted = ?{}");
    }
    if let Some(hidden) = query.hidden {
        text.push_str(if hidden {
            " AND hidden_at IS NOT NULL"
        } else {
            " AND hidden_at IS NULL"
        });
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
    text.push_str(match query.sort {
        StorySort::StoryNo => " ORDER BY story_no",
        StorySort::Priority => " ORDER BY priority_rank, story_no",
        StorySort::UpdatedAt => " ORDER BY updated_at DESC, story_no",
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
// GitHub bases
// ---------------------------------------------------------------------------

pub(super) fn github_base(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
) -> Result<Option<StorySnapshot>, StoreError> {
    let json = one(
        conn,
        "SELECT snapshot FROM github_bases WHERE project_id = ?1 AND story_no = ?2",
        params![project.get(), story.get()],
        |row| row.get::<_, String>(0),
        "reading a github base",
    )?;
    json.map(|text| serde_json::from_str(&text))
        .transpose()
        .map_err(StoreError::from)
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
