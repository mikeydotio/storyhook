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

use rusqlite::{Connection, params};

use crate::domain::provenance::{ActorLabel, Provenance};
use crate::domain::remote::RemoteUrl;
use crate::domain::{
    KIND_STORY_COMMIT_LINKED, Member, StateDef, StoryEvent, StorySnapshot, TypeDef,
};
use crate::store::error::StoreError;
use crate::store::fault::{FaultPoint, fire};
use crate::store::ids::{EventSeq, ExpectedSeq, ProjectId, StoryNo};
use crate::store::sqlite::read;
use crate::store::types::{
    DeletedProject, LinkSource, NewProject, ProjectSettings, PurgedStory, RawEvent, priority_rank,
};

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

/// Renames a project, leaving every other column alone.
pub(super) fn rename_project(
    conn: &Connection,
    project: ProjectId,
    name: &str,
) -> Result<(), StoreError> {
    let updated = sql(
        conn.execute(
            "UPDATE projects SET name = ?2 WHERE id = ?1",
            params![project.get(), name],
        ),
        "renaming a project",
    )?;
    if updated == 0 {
        return Err(StoreError::Invariant(format!(
            "renaming a project that does not exist (id {})",
            project.get()
        )));
    }
    Ok(())
}

/// Sets a project's story-id prefix, leaving every other column alone. See
/// [`crate::store::WriteOps::set_prefix`] for what a caller must do around
/// this call for the result to be consistent.
pub(super) fn set_prefix(
    conn: &Connection,
    project: ProjectId,
    new_prefix: &str,
) -> Result<(), StoreError> {
    let updated = sql(
        conn.execute(
            "UPDATE projects SET prefix = ?2 WHERE id = ?1",
            params![project.get(), new_prefix],
        ),
        "setting a project's prefix",
    )?;
    if updated == 0 {
        return Err(StoreError::Invariant(format!(
            "setting the prefix of a project that does not exist (id {})",
            project.get()
        )));
    }
    Ok(())
}

/// Every table holding rows scoped to a single project, in the order they must
/// be cleared, paired with the column naming the project.
///
/// The order is a dependency order, and two positions in it are load-bearing:
///
/// * `story_commit_links` has **no foreign key at all** (0002 explains why: it
///   is a projection of an event, and events are legitimately written before
///   the read-model row exists). No cascade will ever clean it, so it has to be
///   here explicitly or a deleted project leaves its git links behind.
/// * `projects` comes **before** `events`, which is what makes migration 3's
///   rewritten `events_reject_delete` abstain. Reversing these two turns a
///   working teardown into an aborted transaction.
///
/// Every other entry would be handled by `ON DELETE CASCADE`. They are listed
/// anyway, and the cascade demoted to a backstop, so that
/// [`verify_project_is_gone`] can prove the teardown was complete rather than
/// trusting that it was.
const PROJECT_SCOPED_TABLES: &[(&str, &str)] = &[
    ("github_bases", "project_id"),
    ("story_commit_links", "project_id"),
    ("story_pr_links", "project_id"),
    ("story_relations", "project_id"),
    ("story_labels", "project_id"),
    ("stories", "project_id"),
    ("project_settings", "project_id"),
    ("project_members", "project_id"),
    ("project_types", "project_id"),
    ("project_states", "project_id"),
    ("project_remotes", "project_id"),
    ("projects", "id"),
    ("events", "project_id"),
];

/// Removes a project and everything recorded against it. See
/// [`WriteOps::delete_project`](crate::store::WriteOps::delete_project) for the
/// contract and for why the event log may be deleted here and nowhere else.
pub(super) fn delete_project(
    conn: &Connection,
    project: ProjectId,
) -> Result<DeletedProject, StoreError> {
    if read::project(conn, project)?.is_none() {
        return Err(StoreError::NotFound(format!(
            "deleting a project that does not exist (id {})",
            project.get()
        )));
    }

    let mut removed = DeletedProject::default();
    for (table, column) in PROJECT_SCOPED_TABLES {
        // The table name is a compile-time constant from the list above, never
        // caller input, so interpolating it cannot be an injection. The project
        // id — the only value — is still bound.
        let count = sql(
            conn.execute(
                &format!("DELETE FROM {table} WHERE {column} = ?1"),
                params![project.get()],
            ),
            "deleting a project",
        )?;
        match *table {
            "stories" => removed.stories = count,
            "project_remotes" => removed.remotes = count,
            "events" => removed.events = count,
            _ => {}
        }
    }

    verify_project_is_gone(conn, project)?;
    Ok(removed)
}

/// Fails unless every project-scoped table is empty of this project.
///
/// The teardown above would be complete without this today. It exists for the
/// migration that adds the thirteenth table and does not add it to
/// [`PROJECT_SCOPED_TABLES`]: the alternative to failing here is a store that
/// quietly accumulates rows belonging to projects that no longer exist, keyed
/// by an id SQLite will hand to somebody else.
fn verify_project_is_gone(conn: &Connection, project: ProjectId) -> Result<(), StoreError> {
    for (table, column) in PROJECT_SCOPED_TABLES {
        let left: i64 = sql(
            conn.query_row(
                &format!("SELECT count(*) FROM {table} WHERE {column} = ?1"),
                params![project.get()],
                |row| row.get(0),
            ),
            "verifying a project deletion",
        )?;
        if left > 0 {
            return Err(StoreError::Invariant(format!(
                "deleting project {} left {left} row(s) in `{table}`",
                project.get()
            )));
        }
    }
    Ok(())
}

/// Every table holding rows scoped to a single story, in the order they must be
/// cleared, paired with the predicate that selects that story's rows.
///
/// The predicate is a fragment rather than a column name because one entry is
/// not shaped like the others: a story's relations live under **both** ends of
/// each edge, so `story_relations` has to be cleared by `story_no` *or*
/// `other_no`. Clearing only one end would leave the mirror row behind, its
/// foreign key pointing at a story that no longer exists.
///
/// Two positions are load-bearing, and they are the same two that matter in
/// [`PROJECT_SCOPED_TABLES`]:
///
/// * `story_commit_links` has no foreign key at all (0002 explains why), so no
///   cascade will ever clean it and it has to be here explicitly.
/// * `stories` comes **before** `events`, which is what makes migration 5's
///   rewritten `events_reject_delete` abstain. Reversing these two turns a
///   working purge into an aborted transaction.
///
/// `github_bases`, `story_labels` and `story_relations` would each be handled
/// by `ON DELETE CASCADE`. They are listed anyway, and the cascade demoted to a
/// backstop, so that [`verify_story_is_gone`] can prove the purge was complete
/// rather than trusting that it was.
const STORY_SCOPED_TABLES: &[(&str, &str)] = &[
    ("github_bases", "story_no = ?2"),
    ("story_commit_links", "story_no = ?2"),
    ("story_pr_links", "story_no = ?2"),
    ("story_relations", "(story_no = ?2 OR other_no = ?2)"),
    ("story_labels", "story_no = ?2"),
    ("stories", "story_no = ?2"),
    ("events", "story_no = ?2"),
];

/// Removes one story and everything recorded against it. See
/// [`WriteOps::purge_story`](crate::store::WriteOps::purge_story) for the
/// contract, for why the event log may be deleted here, and for what this
/// deliberately leaves to its caller.
pub(super) fn purge_story(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
) -> Result<PurgedStory, StoreError> {
    if read::story(conn, project, story)?.is_none() {
        return Err(StoreError::NotFound(format!(
            "purging a story that does not exist ({project}, story {story})"
        )));
    }

    let mut removed = PurgedStory::default();
    for (table, predicate) in STORY_SCOPED_TABLES {
        // Both interpolated fragments are compile-time constants from the list
        // above, never caller input, so this cannot be an injection. The two
        // values — the project id and the story number — are still bound.
        let count = sql(
            conn.execute(
                &format!("DELETE FROM {table} WHERE project_id = ?1 AND {predicate}"),
                params![project.get(), story.get()],
            ),
            "purging a story",
        )?;
        if *table == "events" {
            removed.events = count;
        }
    }

    verify_story_is_gone(conn, project, story)?;
    Ok(removed)
}

/// Fails unless every story-scoped table is empty of this story.
///
/// The purge above would be complete without this today. It exists for the
/// migration that adds a seventh story-scoped table and does not add it to
/// [`STORY_SCOPED_TABLES`]: the alternative to failing here is a store that
/// quietly keeps rows belonging to a story that no longer exists, under a
/// number nothing will ever hand out again.
fn verify_story_is_gone(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
) -> Result<(), StoreError> {
    for (table, predicate) in STORY_SCOPED_TABLES {
        let left: i64 = sql(
            conn.query_row(
                &format!("SELECT count(*) FROM {table} WHERE project_id = ?1 AND {predicate}"),
                params![project.get(), story.get()],
                |row| row.get(0),
            ),
            "verifying a story purge",
        )?;
        if left > 0 {
            return Err(StoreError::Invariant(format!(
                "purging story {story} of project {project} left {left} row(s) in `{table}`"
            )));
        }
    }
    Ok(())
}

/// Registers a git origin as belonging to a project.
///
/// The holder is read *before* the insert rather than after a constraint
/// failure. That ordering is what lets the refusal name the project that
/// already holds the origin — SQLite reports the column a `UNIQUE` index covers
/// and has no way to report a project — and it is safe from a race because a
/// write transaction is `BEGIN IMMEDIATE`, exclusive among writers. The index
/// stays as the backstop for anything that reaches this table without coming
/// through here.
pub(super) fn link_remote(
    conn: &Connection,
    project: ProjectId,
    remote: &RemoteUrl,
    registered_at: &str,
) -> Result<(), StoreError> {
    if let Some(holder) = read::project_by_remote(conn, remote)?
        && holder.id != project
    {
        return Err(StoreError::Invariant(format!(
            "the origin `{}` is already registered to project `{}`",
            remote.key(),
            holder.slug
        )));
    }

    sql(
        conn.execute(
            "INSERT INTO project_remotes (project_id, normalized, raw, registered_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (project_id, normalized) DO UPDATE SET \
                 raw = excluded.raw, registered_at = excluded.registered_at",
            params![project.get(), remote.key(), remote.raw(), registered_at],
        ),
        "registering a project origin",
    )?;
    Ok(())
}

/// Forgets one git origin of a project, reporting whether there was one.
pub(super) fn unlink_remote(
    conn: &Connection,
    project: ProjectId,
    remote: &RemoteUrl,
) -> Result<bool, StoreError> {
    let removed = sql(
        conn.execute(
            "DELETE FROM project_remotes WHERE project_id = ?1 AND normalized = ?2",
            params![project.get(), remote.key()],
        ),
        "forgetting a project origin",
    )?;
    Ok(removed > 0)
}

/// Sets or clears `projects.checkout_path`.
///
/// An overwrite rather than an insert, because a project has at most one
/// checkout — see the trait method and migration 0007's header.
pub(super) fn set_checkout_path(
    conn: &Connection,
    project: ProjectId,
    path: Option<&Path>,
) -> Result<(), StoreError> {
    sql(
        conn.execute(
            "UPDATE projects SET checkout_path = ?2 WHERE id = ?1",
            params![project.get(), path.map(|p| p.to_string_lossy().to_string())],
        ),
        "recording a project checkout",
    )?;
    Ok(())
}

/// Records that this store scanned this project's commits at `at`.
///
/// One column, by a targeted `UPDATE` — never through `put_settings`, whose
/// unconditional full-row rewrite is SH-129's hazard and would sit on a path
/// taken once per commit. See migration 0014's header.
pub(super) fn record_commit_scan(
    conn: &Connection,
    project: ProjectId,
    at: &str,
) -> Result<(), StoreError> {
    sql(
        conn.execute(
            "UPDATE projects SET commit_scan_at = ?2 WHERE id = ?1",
            params![project.get(), at],
        ),
        "recording a project commit scan",
    )?;
    Ok(())
}

/// Sets the receipt only if this project has never had one, answering whether
/// it did.
///
/// `WHERE commit_scan_at IS NULL` rather than a read-then-write, so the "arm
/// once, never re-arm" rule is the database's and not a caller's: two installs
/// racing cannot both observe NULL and both write, and the second's `changes()`
/// of zero is what tells it so.
pub(super) fn arm_commit_scan(
    conn: &Connection,
    project: ProjectId,
    at: &str,
) -> Result<bool, StoreError> {
    let armed = sql(
        conn.execute(
            "UPDATE projects SET commit_scan_at = ?2 \
             WHERE id = ?1 AND commit_scan_at IS NULL",
            params![project.get(), at],
        ),
        "arming a project commit scan",
    )?;
    Ok(armed > 0)
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
    provenance: &Provenance,
) -> Result<EventSeq, StoreError> {
    let raw = events
        .iter()
        .map(RawEvent::from_event)
        .collect::<Result<Vec<_>, _>>()?;
    append(
        conn,
        project,
        story,
        expected,
        &raw,
        LinkSource::Live,
        provenance,
    )
}

/// [`append_events`], for callers holding bytes rather than a decoded event.
///
/// `source` is the caller's, because it is a fact about the caller — see
/// [`LinkSource`]. `append_events` does not take one for the same reason: a
/// decoded [`StoryEvent`] can only have come from this program, now.
pub(super) fn append_raw_events(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
    expected: ExpectedSeq,
    events: &[RawEvent],
    source: LinkSource,
    provenance: &Provenance,
) -> Result<EventSeq, StoreError> {
    append(conn, project, story, expected, events, source, provenance)
}

/// Appends events, stamping each with the provenance of the write.
///
/// `provenance` is an argument for exactly the reason [`LinkSource`] is: it is a
/// fact about the *caller*, and a caller cannot state a fact it is never asked
/// for. A [`Provenance::unrecorded`] is a legitimate answer — a fixture, a
/// replay, a path with genuinely nothing to declare — and it writes NULLs, which
/// is what every pre-SH-246 row already holds.
fn append(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
    expected: ExpectedSeq,
    events: &[RawEvent],
    source: LinkSource,
    provenance: &Provenance,
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
    let command = provenance.command.as_deref();
    let actor = provenance.actor.as_ref().map(ActorLabel::as_str);
    let mut stmt = sql(
        conn.prepare_cached(
            "INSERT INTO events \
             (project_id, story_no, seq, global_seq, kind, at, payload, command, actor) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
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
                event.payload,
                command,
                actor
            ]),
            "appending an event",
        )?;
        project_commit_link(conn, project, story, event, source)?;
        project_pr_link(conn, project, story, event)?;
    }
    Ok(EventSeq::new(
        head.get() + i64::try_from(events.len()).expect("an append fits in i64"),
    ))
}

/// Records a link record's commit hash in `story_commit_links`.
///
/// The projection that makes "one link record per commit per story" a
/// **database constraint** rather than a caller's discipline: the table's
/// primary key rejects a second insert, in the same transaction as the event
/// that would have been the second copy, so the two cannot end up disagreeing.
///
/// **Two kinds feed it, and only one of them unconditionally.**
/// `StoryCommitLinked` (kind #18) carries the full hash in a field and is
/// always projected. A `StoryCommentAdded` reading `[git] <short>: <subject>`
/// is the *pre*-#18 shape, and it is projected only when the append is a
/// [`LinkSource::Replayed`] history — because the same text is what a user gets
/// by typing it into `story comment`, and projecting that would let anyone
/// suppress a real link. That hole is precisely what the old string scan had.
///
/// The two are not inserted the same way either. A duplicate
/// `StoryCommitLinked` **fails the append** — that is the invariant. A
/// duplicate legacy comment does not: a human can write the same `[git]`
/// comment twice, and refusing to import a story because of it would be a
/// migration that rejects real data.
///
/// # A payload it cannot read is not an error
///
/// The store's governing rule is that an event it does not understand must be
/// *storable* — SH-54, and `store_inject.rs::bytes_that_are_not_a_decodable_
/// event_can_be_written_verbatim` is the test that says so. A projection that
/// refused an append because a payload would not parse would break that rule
/// from the inside, which is exactly what the first draft of this function did.
/// So every read below is fallible-and-skipped: no readable sha means no row,
/// never a rejected write. Nothing is lost by it — a record nothing can name
/// cannot be a duplicate of anything.
///
/// Works on [`RawEvent`] rather than a decoded [`StoryEvent`] so that
/// `append_raw_events` — the path `story migrate` and `story import-project`
/// take — projects the link too. *Which* of the two shapes it projects for them
/// is the caller's [`LinkSource`]: `migrate` replays a legacy tree and always
/// passes `Replayed`, because its input is legacy by construction.
/// `import-project` restores a document through the same primitive and passes
/// `Replayed` only when the operator's `--legacy-links` flag asserts this
/// specific document predates kind #18 — `Live` otherwise (SH-70), since an
/// export document, unlike a legacy tree, carries no such guarantee on its own.
fn project_commit_link(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
    event: &RawEvent,
    source: LinkSource,
) -> Result<(), StoreError> {
    let is_link_record = event.kind == KIND_STORY_COMMIT_LINKED;
    let legacy_shape = event.kind == "StoryCommentAdded" && source == LinkSource::Replayed;
    if !is_link_record && !legacy_shape {
        // Every other kind, and every kind this binary has never heard of —
        // untouched by construction, because this reads the `kind` column
        // rather than the payload's meaning.
        return Ok(());
    }
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&event.payload) else {
        return Ok(());
    };

    let sha = if is_link_record {
        payload
            .get("sha")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    } else {
        payload
            .get("text")
            .and_then(serde_json::Value::as_str)
            .and_then(crate::domain::git_link_sha)
            .map(str::to_string)
    };
    let Some(sha) = sha else {
        return Ok(());
    };

    let statement = if is_link_record {
        "INSERT INTO story_commit_links (project_id, story_no, sha) VALUES (?1, ?2, ?3)"
    } else {
        "INSERT INTO story_commit_links (project_id, story_no, sha) VALUES (?1, ?2, ?3) \
         ON CONFLICT DO NOTHING"
    };
    sql(
        conn.execute(statement, params![project.get(), story.get(), sha]),
        "recording a commit link",
    )?;
    Ok(())
}

/// Projects a `StoryPrLinked`/`StoryPrUnlinked`/`StoryPrMerged`/`StoryPrClosed`
/// event into `story_pr_links` (SH-49).
///
/// Reads the raw event's `kind` and JSON payload directly, like
/// [`project_commit_link`], so unknown-kind replay still projects correctly and
/// so `append_raw_events` — the path `story migrate` and `story import-project`
/// take — projects the link too.
///
/// **Every other kind, and every kind this binary has never heard of, is
/// untouched** — this reads the `kind` column rather than the payload's
/// meaning, and a payload that fails to parse or is missing a field is skipped
/// rather than rejected, for the same "a payload it cannot read is not an
/// error" reason [`project_commit_link`] documents.
///
/// Three different write shapes for the four kinds: `StoryPrLinked` **upserts**
/// (a user may re-link the same PR to toggle `close_on_merge`, which
/// `story_commit_links`' pure-insert model has no need for), `StoryPrUnlinked`
/// **deletes** by `url`, and `StoryPrMerged`/`StoryPrClosed` **update** the
/// existing row's `status`.
fn project_pr_link(
    conn: &Connection,
    project: ProjectId,
    story: StoryNo,
    event: &RawEvent,
) -> Result<(), StoreError> {
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&event.payload) else {
        return Ok(());
    };
    let str_field = |name: &str| {
        payload
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let bool_field = |name: &str| payload.get(name).and_then(serde_json::Value::as_bool);
    let number_field = |name: &str| {
        payload
            .get(name)
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| i64::try_from(n).ok())
    };

    match event.kind.as_str() {
        "StoryPrLinked" => {
            let (Some(owner), Some(repo), Some(number), Some(url), Some(close_on_merge)) = (
                str_field("owner"),
                str_field("repo"),
                number_field("number"),
                str_field("url"),
                bool_field("close_on_merge"),
            ) else {
                return Ok(());
            };
            sql(
                conn.execute(
                    "INSERT INTO story_pr_links \
                         (project_id, story_no, owner, repo, number, url, close_on_merge, \
                          status, linked_at, last_checked_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'open', ?8, NULL) \
                     ON CONFLICT (project_id, story_no, owner, repo, number) DO UPDATE SET \
                         url = excluded.url, \
                         close_on_merge = excluded.close_on_merge, \
                         status = 'open', \
                         linked_at = excluded.linked_at, \
                         last_checked_at = NULL",
                    params![
                        project.get(),
                        story.get(),
                        owner,
                        repo,
                        number,
                        url,
                        i64::from(close_on_merge),
                        event.at
                    ],
                ),
                "linking a pull request",
            )?;
        }
        "StoryPrUnlinked" => {
            let Some(url) = str_field("url") else {
                return Ok(());
            };
            sql(
                conn.execute(
                    "DELETE FROM story_pr_links \
                     WHERE project_id = ?1 AND story_no = ?2 AND url = ?3",
                    params![project.get(), story.get(), url],
                ),
                "unlinking a pull request",
            )?;
        }
        "StoryPrMerged" => {
            let Some(url) = str_field("url") else {
                return Ok(());
            };
            sql(
                conn.execute(
                    "UPDATE story_pr_links SET status = 'merged', last_checked_at = ?4 \
                     WHERE project_id = ?1 AND story_no = ?2 AND url = ?3",
                    params![project.get(), story.get(), url, event.at],
                ),
                "recording a merged pull request",
            )?;
        }
        "StoryPrClosed" => {
            let Some(url) = str_field("url") else {
                return Ok(());
            };
            sql(
                conn.execute(
                    "UPDATE story_pr_links SET status = 'closed', last_checked_at = ?4 \
                     WHERE project_id = ?1 AND story_no = ?2 AND url = ?3",
                    params![project.get(), story.get(), url, event.at],
                ),
                "recording a closed pull request",
            )?;
        }
        _ => {}
    }
    Ok(())
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
/// `head_global_seq` (SH-336) is derived the same way, by a scalar subquery
/// against `events` rather than a parameter, so it cannot drift from the
/// event `head` actually names: `COALESCE((SELECT global_seq FROM events
/// WHERE ... AND seq = head), 0)`. This makes every caller's obligation the
/// same one `head` already implies — the event this row is folded from must
/// already be committed in this transaction — and both callers that fold with
/// no new event (`refold_story`, `rebuild::repair_read_model`) get a correct
/// value with no special case, since the event `head` names is already on
/// disk from an earlier write.
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
            "INSERT INTO stories (project_id, story_no, head_seq, head_global_seq, title, \
                 state, superstate, priority, priority_rank, story_type, assignee, awaiting, \
                 deleted, archived, created_at, updated_at, closed_at, description, hidden_at, \
                 draft, snapshot) \
             VALUES (?1, ?2, ?3, \
                 COALESCE((SELECT e.global_seq FROM events e \
                            WHERE e.project_id = ?1 AND e.story_no = ?2 AND e.seq = ?3), 0), \
                 ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
                 ?18, ?19, ?20) \
             ON CONFLICT (project_id, story_no) DO UPDATE SET \
                 head_seq = excluded.head_seq, head_global_seq = excluded.head_global_seq, \
                 title = excluded.title, state = excluded.state, \
                 superstate = excluded.superstate, priority = excluded.priority, \
                 priority_rank = excluded.priority_rank, story_type = excluded.story_type, \
                 assignee = excluded.assignee, awaiting = excluded.awaiting, \
                 deleted = excluded.deleted, archived = excluded.archived, \
                 created_at = excluded.created_at, updated_at = excluded.updated_at, \
                 closed_at = excluded.closed_at, description = excluded.description, \
                 hidden_at = excluded.hidden_at, draft = excluded.draft, \
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
                snapshot.hidden_at,
                snapshot.draft,
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
            "INSERT INTO project_types (project_id, position, slug, description, emoji) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
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
                story_type.emoji,
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
