//! Every write the store performs, as free functions over a `&Connection`
//! already inside a write transaction.
//!
//! Two rules govern this file.
//!
//! **Counters are allocated here, not by the caller.** `next_story_no` and
//! `next_global_seq` move by `UPDATE … RETURNING` inside the transaction that
//! consumes them. That single statement is the whole of the ID-collision fix:
//! under the old design the counter lived in a file inside the repository, so
//! two checkouts each read `49`, each wrote `50`, and both created `SH-49`.
//!
//! **The read model is written in the same transaction as the events it was
//! folded from.** Nothing here writes one without the other being possible in
//! the same `write` closure, and the schema's `head_seq` column records which
//! event a row was folded from so that a stale row can be told from a wrong one.

use std::collections::BTreeSet;
use std::path::Path;

use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, params};

use crate::domain::{Member, StateDef, StoryEvent, StorySnapshot, TypeDef};
use crate::store::error::StoreError;
use crate::store::fault::{FaultPoint, fire};
use crate::store::ids::{EventSeq, ExpectedSeq, PathKind, ProjectId, StoryNo};
use crate::store::sqlite::read;
use crate::store::types::{NewProject, ProjectSettings, RawEvent, priority_rank};

fn sql<T>(result: Result<T, rusqlite::Error>, context: &str) -> Result<T, StoreError> {
    result.map_err(|e| StoreError::from_sqlite(e, context))
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

pub(super) fn create_project(
    conn: &Connection,
    project: &NewProject,
) -> Result<ProjectId, StoreError> {
    if project.prefix.is_empty() {
        return Err(StoreError::Validation(
            "a project's story-id prefix cannot be empty".to_string(),
        ));
    }
    // Checked here rather than left to the UNIQUE constraint so the message
    // names which of the two identities collided. Safe from a race: this runs
    // inside `BEGIN IMMEDIATE`, which is exclusive among writers.
    if read::project_by_uuid(conn, &project.uuid)?.is_some() {
        return Err(StoreError::Validation(format!(
            "a project with uuid `{}` already exists",
            project.uuid
        )));
    }
    if read::project_by_slug(conn, &project.slug)?.is_some() {
        return Err(StoreError::Validation(format!(
            "a project with slug `{}` already exists",
            project.slug
        )));
    }

    let id: i64 = sql(
        conn.query_row(
            "INSERT INTO projects (uuid, slug, name, prefix, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) RETURNING id",
            params![
                project.uuid,
                project.slug,
                project.name,
                project.prefix,
                project.created_at
            ],
            |row| row.get(0),
        ),
        "creating a project",
    )?;
    Ok(ProjectId::new(id))
}

/// Records that storyhook has just been used in `path`.
///
/// A project has many checkouts, and every one of them resolves to the same
/// project — that plurality is what ends SH-46. The unique index on `path`
/// means a directory already claimed by another project is rejected rather
/// than silently re-pointed.
pub(super) fn touch_project_path(
    conn: &Connection,
    project: ProjectId,
    path: &Path,
    kind: PathKind,
) -> Result<(), StoreError> {
    sql(
        conn.execute(
            "INSERT INTO project_paths (project_id, path, kind, last_seen_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (project_id, path) DO UPDATE SET \
                 kind = excluded.kind, last_seen_at = excluded.last_seen_at",
            params![project.get(), path.to_string_lossy(), kind.as_str(), now()],
        ),
        "recording a project path",
    )?;
    Ok(())
}

/// Forgets one checkout of a project, reporting whether there was one.
///
/// The project itself survives: a checkout that is deleted, moved, or removed
/// from the dashboard is not a reason to lose its stories.
pub(super) fn forget_project_path(
    conn: &Connection,
    project: ProjectId,
    path: &Path,
) -> Result<bool, StoreError> {
    let removed = sql(
        conn.execute(
            "DELETE FROM project_paths WHERE project_id = ?1 AND path = ?2",
            params![project.get(), path.to_string_lossy()],
        ),
        "forgetting a project path",
    )?;
    Ok(removed > 0)
}

// ---------------------------------------------------------------------------
// Allocation
// ---------------------------------------------------------------------------

/// Hands out the next story number for a project.
///
/// The number is consumed by the same transaction that allocates it: if that
/// transaction rolls back, the counter rolls back with it and the number is
/// handed out again. No gaps, no duplicates — and, unlike a counter file in a
/// repository, no second copy that a branch can disagree with.
pub(super) fn allocate_story_no(
    conn: &Connection,
    project: ProjectId,
) -> Result<StoryNo, StoreError> {
    let allocated: Option<i64> = sql(
        conn.query_row(
            "UPDATE projects SET next_story_no = next_story_no + 1 \
             WHERE id = ?1 RETURNING next_story_no - 1",
            params![project.get()],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        }),
        "allocating a story number",
    )?;
    allocated
        .map(StoryNo::new)
        .ok_or_else(|| StoreError::NotFound(format!("project {project} does not exist")))
}

/// Raises a project's story-number counter so that nothing at or below
/// `highest` is ever handed out.
///
/// `MAX`, never assignment: a caller that has just written story 40 into a
/// project whose counter already stands at 100 must not walk it backwards, or
/// the next `story new` mints an id that already exists.
pub(super) fn reserve_story_no(
    conn: &Connection,
    project: ProjectId,
    highest: StoryNo,
) -> Result<(), StoreError> {
    let updated = sql(
        conn.execute(
            "UPDATE projects SET next_story_no = MAX(next_story_no, ?2) WHERE id = ?1",
            params![project.get(), highest.get() + 1],
        ),
        "reserving a story number",
    )?;
    if updated == 0 {
        return Err(StoreError::NotFound(format!(
            "project {project} does not exist"
        )));
    }
    Ok(())
}

/// Reserves `count` consecutive change-feed positions.
fn allocate_global_seqs(
    conn: &Connection,
    project: ProjectId,
    count: usize,
) -> Result<i64, StoreError> {
    let count = i64::try_from(count).map_err(|_| {
        StoreError::Validation("that is more events than one append can carry".to_string())
    })?;
    let first: Option<i64> = sql(
        conn.query_row(
            "UPDATE projects SET next_global_seq = next_global_seq + ?2 \
             WHERE id = ?1 RETURNING next_global_seq - ?2",
            params![project.get(), count],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        }),
        "allocating change-feed positions",
    )?;
    first.ok_or_else(|| StoreError::NotFound(format!("project {project} does not exist")))
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Appends events to a story, refusing if its head is not where the caller
/// expected.
///
/// The compare-and-swap that `--if-state` always claimed and the
/// per-directory file lock never actually provided: that lock protected one
/// checkout's copy of the data, so a claim could succeed in two checkouts at
/// once. Here the precondition is evaluated inside the same exclusive
/// transaction as the insert.
pub(super) fn append_events(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
    expected: ExpectedSeq,
    events: &[StoryEvent],
) -> Result<EventSeq, StoreError> {
    let raw = events.iter().map(encode).collect::<Result<Vec<_>, _>>()?;
    append_raw_events(conn, project, story, expected, &raw)
}

/// [`append_events`], for callers holding bytes rather than a decoded event.
pub(super) fn append_raw_events(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
    expected: ExpectedSeq,
    events: &[RawEvent],
) -> Result<EventSeq, StoreError> {
    let head = read::head_seq(conn, project, story)?;
    if let ExpectedSeq::Exact(required) = expected
        && required != head
    {
        // The precondition is evaluated and the insert performed inside one
        // exclusive transaction, so nothing can land between them. Reporting
        // the actual head as well as the expected one is what lets a caller
        // say what happened without re-reading.
        return Err(StoreError::Conflict {
            expected,
            actual: head,
        });
    }
    if events.is_empty() {
        return Ok(head);
    }

    let first_global = allocate_global_seqs(conn, project, events.len())?;
    let mut stmt = sql(
        conn.prepare_cached(
            "INSERT INTO events (project_id, story_no, seq, global_seq, kind, at, payload) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        ),
        "preparing an event append",
    )?;
    for (offset, event) in events.iter().enumerate() {
        let offset = i64::try_from(offset).expect("an append fits in i64");
        sql(
            stmt.execute(params![
                project.get(),
                story.get(),
                head.get() + offset + 1,
                first_global + offset,
                event.kind,
                event.at,
                event.payload
            ]),
            "appending an event",
        )?;
    }
    Ok(EventSeq::new(
        head.get() + i64::try_from(events.len()).expect("an append fits in i64"),
    ))
}

/// Serializes an event and lifts its `kind` and `at` out of the payload.
///
/// The denormalization is what lets a storyhook that has never heard of a kind
/// still read, report, and retain the row (SH-54): those two columns are
/// readable without understanding the payload at all.
fn encode(event: &StoryEvent) -> Result<RawEvent, StoreError> {
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
    Ok(RawEvent {
        kind: field("kind")?,
        at: field("at")?,
        payload: serde_json::to_string(&value)?,
    })
}

// ---------------------------------------------------------------------------
// Read model
// ---------------------------------------------------------------------------

/// Writes a folded snapshot into the read model.
///
/// `head` records which event the snapshot was folded from. `archived` is
/// *derived* from the snapshot rather than passed in — a story is archived
/// exactly when it has a close timestamp — and a schema CHECK holds the two
/// together, so the flag that replaced the legacy open/archive split cannot
/// drift from the fact it stands for.
///
/// Relations are derived from the snapshot too, and only this story's own end
/// of each edge is written: the schema's mirror triggers materialize the other
/// end. Writing half of a bidirectional relation is therefore not an operation
/// this API offers.
pub(super) fn put_story(
    conn: &Connection,
    project: ProjectId,
    snapshot: &StorySnapshot,
    head: EventSeq,
) -> Result<(), StoreError> {
    let prefix = read::prefix(conn, project)?;
    let story = StoryNo::parse_id(&prefix, &snapshot.id)?;
    let archived = snapshot.closed_at.is_some();

    sql(
        conn.execute(
            "INSERT INTO stories (project_id, story_no, head_seq, title, state, superstate, \
                 priority, priority_rank, story_type, assignee, awaiting, deleted, archived, \
                 created_at, updated_at, closed_at, description, snapshot) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18) \
             ON CONFLICT (project_id, story_no) DO UPDATE SET \
                 head_seq = excluded.head_seq, title = excluded.title, state = excluded.state, \
                 superstate = excluded.superstate, priority = excluded.priority, \
                 priority_rank = excluded.priority_rank, story_type = excluded.story_type, \
                 assignee = excluded.assignee, awaiting = excluded.awaiting, \
                 deleted = excluded.deleted, archived = excluded.archived, \
                 created_at = excluded.created_at, updated_at = excluded.updated_at, \
                 closed_at = excluded.closed_at, description = excluded.description, \
                 snapshot = excluded.snapshot",
            params![
                project.get(),
                story.get(),
                head.get(),
                snapshot.title,
                snapshot.state,
                snapshot.superstate.as_str(),
                snapshot.priority.as_str(),
                priority_rank(&snapshot.priority),
                snapshot.story_type,
                snapshot.assignee,
                snapshot.awaiting,
                snapshot.deleted,
                archived,
                snapshot.created_at,
                snapshot.updated_at,
                snapshot.closed_at,
                snapshot.description,
                serde_json::to_string(snapshot)?,
            ],
        ),
        "writing a story row",
    )?;

    fire(FaultPoint::MidReadModelUpdate)?;

    put_labels(conn, project, story, &snapshot.labels)?;
    put_relations(conn, project, story, &prefix, snapshot)?;
    Ok(())
}

fn put_labels(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
    labels: &[String],
) -> Result<(), StoreError> {
    sql(
        conn.execute(
            "DELETE FROM story_labels WHERE project_id = ?1 AND story_no = ?2",
            params![project.get(), story.get()],
        ),
        "clearing story labels",
    )?;
    let mut stmt = sql(
        conn.prepare_cached(
            "INSERT INTO story_labels (project_id, story_no, label) VALUES (?1, ?2, ?3)",
        ),
        "preparing a label write",
    )?;
    // De-duplicated because the snapshot's labels are a `Vec`, and the table's
    // primary key is not a place to discover that twice.
    for label in labels.iter().collect::<BTreeSet<_>>() {
        sql(
            stmt.execute(params![project.get(), story.get(), label]),
            "writing a story label",
        )?;
    }
    Ok(())
}

/// Reconciles one story's end of the relation graph with what its snapshot
/// claims.
///
/// An edge is a fact about a *pair*, not about one story, and the schema
/// materializes both directions from whichever one is written. So a row
/// attributed to this story may be there because this story claims it, or
/// because the story at the other end does. Deleting the second kind on the
/// grounds that this snapshot does not mention it would silently retract an
/// edge its owner still asserts — and would make the outcome depend on which
/// story happened to be written first.
///
/// The rule is therefore: remove a row only when *neither* end claims it. When
/// the other end has no row yet — a project being written for the first time —
/// removal is still safe, because that story's own write will restore anything
/// it claims.
fn put_relations(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
    prefix: &str,
    snapshot: &StorySnapshot,
) -> Result<(), StoreError> {
    let desired: BTreeSet<(String, i64)> = snapshot
        .relationships
        .iter()
        .map(|relation| {
            // A relation naming a story outside this project is rejected here
            // rather than stored and puzzled over later. In a shared database
            // that is a real possibility for the first time.
            let other = StoryNo::parse_id(prefix, &relation.other_id)?;
            Ok((relation.relation.clone(), other.get()))
        })
        .collect::<Result<_, StoreError>>()?;

    let existing: BTreeSet<(String, i64)> = read::relations_from(conn, project, story)?
        .into_iter()
        .map(|edge| (edge.relation, edge.other_no.get()))
        .collect();

    let mut stale = Vec::new();
    for (relation, other) in existing.difference(&desired) {
        if !claimed_by_other_end(conn, project, prefix, story, relation, StoryNo::new(*other))? {
            stale.push((relation.clone(), *other));
        }
    }

    for (relation, other) in &stale {
        sql(
            conn.execute(
                "DELETE FROM story_relations \
                 WHERE project_id = ?1 AND story_no = ?2 AND relation = ?3 AND other_no = ?4",
                params![project.get(), story.get(), relation, other],
            ),
            "removing a relation",
        )?;
    }
    for (relation, other) in desired.difference(&existing) {
        sql(
            conn.execute(
                "INSERT INTO story_relations (project_id, story_no, relation, other_no) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![project.get(), story.get(), relation, other],
            ),
            "adding a relation",
        )?;
    }
    Ok(())
}

/// Whether the story at the far end of an edge asserts it, in the snapshot the
/// read model currently holds for it.
///
/// A relation this binary has no inverse for cannot be mirrored at all, so
/// nothing at the far end could be asserting it — the schema rejects such a
/// relation on insert, and this answers `false` rather than pretending.
fn claimed_by_other_end(
    conn: &Connection,
    project: ProjectId,
    prefix: &str,
    story: StoryNo,
    relation: &str,
    other: StoryNo,
) -> Result<bool, StoreError> {
    let Some(inverse) = crate::domain::inverse_relation(relation) else {
        return Ok(false);
    };
    let Some(row) = read::story(conn, project, other)? else {
        return Ok(false);
    };
    Ok(row.snapshot.relationships.iter().any(|claim| {
        claim.relation == inverse
            && StoryNo::parse_id(prefix, &claim.other_id).is_ok_and(|target| target == story)
    }))
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

/// Replaces a project's state set.
///
/// Whole-list replacement rather than a merge, because `position` is what
/// gives the set its order and a partial write cannot express a reorder.
pub(super) fn put_states(
    conn: &Connection,
    project: ProjectId,
    states: &[StateDef],
) -> Result<(), StoreError> {
    sql(
        conn.execute(
            "DELETE FROM project_states WHERE project_id = ?1",
            params![project.get()],
        ),
        "clearing project states",
    )?;
    let mut stmt = sql(
        conn.prepare_cached(
            "INSERT INTO project_states (project_id, position, slug, superstate, role, description) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        ),
        "preparing a state write",
    )?;
    for (position, state) in states.iter().enumerate() {
        sql(
            stmt.execute(params![
                project.get(),
                i64::try_from(position).expect("a state set fits in i64"),
                state.slug,
                state.super_state.as_str(),
                state.role,
                state.description,
            ]),
            "writing a project state",
        )?;
    }
    Ok(())
}

/// Replaces a project's type set. See [`put_states`].
pub(super) fn put_types(
    conn: &Connection,
    project: ProjectId,
    types: &[TypeDef],
) -> Result<(), StoreError> {
    sql(
        conn.execute(
            "DELETE FROM project_types WHERE project_id = ?1",
            params![project.get()],
        ),
        "clearing project types",
    )?;
    let mut stmt = sql(
        conn.prepare_cached(
            "INSERT INTO project_types (project_id, position, slug, description) \
             VALUES (?1, ?2, ?3, ?4)",
        ),
        "preparing a type write",
    )?;
    for (position, story_type) in types.iter().enumerate() {
        sql(
            stmt.execute(params![
                project.get(),
                i64::try_from(position).expect("a type set fits in i64"),
                story_type.slug,
                story_type.description,
            ]),
            "writing a project type",
        )?;
    }
    Ok(())
}

pub(super) fn put_member(
    conn: &Connection,
    project: ProjectId,
    member: &Member,
) -> Result<(), StoreError> {
    sql(
        conn.execute(
            "INSERT INTO project_members (project_id, member_id, display_name, email, github, \
                 created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT (project_id, member_id) DO UPDATE SET \
                 display_name = excluded.display_name, email = excluded.email, \
                 github = excluded.github, created_at = excluded.created_at",
            params![
                project.get(),
                member.id,
                member.display_name,
                member.email,
                member.github,
                member.created_at
            ],
        ),
        "writing a project member",
    )?;
    Ok(())
}

pub(super) fn remove_member(
    conn: &Connection,
    project: ProjectId,
    member_id: &str,
) -> Result<bool, StoreError> {
    let removed = sql(
        conn.execute(
            "DELETE FROM project_members WHERE project_id = ?1 AND member_id = ?2",
            params![project.get(), member_id],
        ),
        "removing a project member",
    )?;
    Ok(removed > 0)
}

/// Replaces a project's settings.
///
/// Every column is written every time, from the value the caller passed. There
/// is no read-modify-write of a serialized document anywhere in this path —
/// that pattern is how SH-49 destroyed a state's `description`: the struct in
/// memory did not carry the field, so neither did the write.
pub(super) fn put_settings(
    conn: &Connection,
    project: ProjectId,
    settings: &ProjectSettings,
) -> Result<(), StoreError> {
    let github_sync = settings
        .github_sync
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    sql(
        conn.execute(
            "INSERT INTO project_settings (project_id, sync_auto_transition, \
                 doctor_stale_threshold, github_sync) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (project_id) DO UPDATE SET \
                 sync_auto_transition = excluded.sync_auto_transition, \
                 doctor_stale_threshold = excluded.doctor_stale_threshold, \
                 github_sync = excluded.github_sync",
            params![
                project.get(),
                settings.sync_auto_transition,
                settings.doctor_stale_threshold,
                github_sync
            ],
        ),
        "writing project settings",
    )?;
    Ok(())
}

pub(super) fn put_github_base(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
    snapshot: &StorySnapshot,
) -> Result<(), StoreError> {
    sql(
        conn.execute(
            "INSERT INTO github_bases (project_id, story_no, snapshot) VALUES (?1, ?2, ?3) \
             ON CONFLICT (project_id, story_no) DO UPDATE SET snapshot = excluded.snapshot",
            params![project.get(), story.get(), serde_json::to_string(snapshot)?],
        ),
        "writing a github base",
    )?;
    Ok(())
}

/// The store's only two clock reads are `project_paths.last_seen_at` and
/// `schema_migrations.applied_at`: bookkeeping nothing asserts on. Every
/// timestamp that a user can see arrives as a parameter, so the injectable
/// clock this design calls for can live in the caller's `Environment` rather
/// than being threaded through the storage layer.
fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}
