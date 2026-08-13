//! Fabricating states the store refuses to produce.
//!
//! Eight tests in storyhook's integration suite exist to answer *what happens
//! when the storage is already wrong* — a relation only one end claims, a
//! parent cycle, an event log whose bytes do not decode, a story that vanishes
//! mid-read. Under `.storyhook/` they fabricated those states by writing JSONL
//! straight into the tree. The store deliberately makes each of them
//! unreachable through the public API, so without a back door the flip would
//! not migrate that coverage — it would delete it, and the tracker would lose
//! its only tests of its own failure handling.
//!
//! This module is that back door, and it is a back door on purpose:
//!
//! * [`inject_events`] writes events **bypassing the service layer**, so a
//!   relation can be claimed by one end alone. It does not bypass the
//!   *schema* — a shape SQLite refuses is still refused, because those are the
//!   invariants the rearchitecture bought.
//! * [`inject_raw_events`] writes payload bytes verbatim, so a log can hold
//!   something no `StoryEvent` deserializes from.
//! * [`forget_events`] and [`forget_story`] delete, which the append-only
//!   trigger otherwise makes impossible. They drop that trigger, delete, and
//!   put it back **from the database's own definition of it**, so the guard
//!   cannot be weakened by a typo here.
//!
//! # Why it cannot reach a release binary
//!
//! The whole module is behind the `fault-injection` feature, which is never in
//! `default` and which only `storyhook-test-support` — a dev-dependency —
//! requests. `cargo build` and `cargo build --release` compile none of it.

use rusqlite::Connection;

use crate::domain::{StoryEvent, fold_story};
use crate::store::{
    EventSeq, ExpectedSeq, LinkSource, ProjectId, RawEvent, ReadOps, SqliteStore, Store,
    StoreError, StoryNo, WriteOps, partition_known,
};

/// Appends `events` to a story's log and re-folds its read-model row, bypassing
/// every service-level rule.
///
/// This is how a test fabricates a shape the services refuse to write — a
/// `blocks` edge the other end never claimed, a `StoryTypeSet` naming a type the
/// project does not define. The read model is still updated from the events, so
/// the fabricated state is *coherent*: `story show` reports what was injected
/// rather than a stale row that happens to disagree for a second reason.
///
/// Fails loudly if the resulting log does not fold. Fabricating an unfoldable
/// story is [`inject_raw_events`]'s job, and doing it here by accident would
/// leave a test asserting on a read model that silently stopped being updated.
pub fn inject_events(
    store: &SqliteStore,
    project: ProjectId,
    story: StoryNo,
    events: &[StoryEvent],
) -> Result<EventSeq, StoreError> {
    store.write(|tx| {
        // Unrecorded provenance (SH-246): an injected event was performed by no
        // command, and saying so is what these columns are for.
        let head = tx.append_events(
            project,
            story,
            ExpectedSeq::Any,
            events,
            &crate::domain::provenance::Provenance::unrecorded(),
        )?;
        refold(tx, project, story)?;
        Ok(head)
    })
}

/// Appends payload bytes to a story's log **verbatim**, decodable or not.
///
/// The read model is re-folded from whatever still decodes; if nothing does —
/// a log with no `StoryCreated` in it — the row is left exactly as it was.
/// That divergence between a row and its events is itself one of the shapes
/// under test, and it is the store's honest analogue of the legacy "unparseable
/// bytes in the event log" fixture.
pub fn inject_raw_events(
    store: &SqliteStore,
    project: ProjectId,
    story: StoryNo,
    events: &[RawEvent],
) -> Result<EventSeq, StoreError> {
    store.write(|tx| {
        let head = tx.append_raw_events(
            project,
            story,
            ExpectedSeq::Any,
            events,
            LinkSource::Replayed,
            &crate::domain::provenance::Provenance::unrecorded(),
        )?;
        // A fold failure here is the point of the call, not a problem with it.
        let _ = refold(tx, project, story);
        Ok(head)
    })
}

/// Deletes every event a story has, leaving its read-model row behind.
///
/// The store-side shape of "the story's log was truncated or removed out from
/// under a live reader": the row still claims a story exists and nothing
/// supports it. Returns how many events were removed.
pub fn forget_events(
    store: &SqliteStore,
    project: ProjectId,
    story: StoryNo,
) -> Result<usize, StoreError> {
    with_append_guard_down(store, |conn| {
        let removed = conn.execute(
            "DELETE FROM events WHERE project_id = ?1 AND story_no = ?2",
            rusqlite::params![project.get(), story.get()],
        )?;
        Ok(removed)
    })
}

/// Deletes a story outright — its events, its labels, its relations and its
/// read-model row.
///
/// Fabricates the race the TUI has to survive: a story that existed when a view
/// was rendered and does not exist by the time the user acts on it.
pub fn forget_story(
    store: &SqliteStore,
    project: ProjectId,
    story: StoryNo,
) -> Result<(), StoreError> {
    with_append_guard_down(store, |conn| {
        let args = rusqlite::params![project.get(), story.get()];
        // Relations first, in both directions: both ends are foreign keys into
        // `stories`, so the row cannot go while an edge still names it.
        conn.execute(
            "DELETE FROM story_relations WHERE project_id = ?1 \
             AND (story_no = ?2 OR other_no = ?2)",
            args,
        )?;
        conn.execute(
            "DELETE FROM story_labels WHERE project_id = ?1 AND story_no = ?2",
            args,
        )?;
        conn.execute(
            "DELETE FROM stories WHERE project_id = ?1 AND story_no = ?2",
            args,
        )?;
        conn.execute(
            "DELETE FROM events WHERE project_id = ?1 AND story_no = ?2",
            args,
        )?;
        Ok(())
    })
}

/// Deletes a story's read-model row, leaving its events, its labels and its
/// relations exactly where they are.
///
/// The store-side shape of "the read model lost a row its own history still
/// supports" — the drift [`crate::store::repair_read_model`] heals by
/// re-folding, and the one [`forget_story`] cannot fabricate: that helper
/// deletes the relation rows first, because it works with foreign keys *on*,
/// so the edges naming the story go with it. Here they stay. That difference is
/// the fixture SH-271 needed: an edge that is perfectly valid in the events
/// reads as *dangling* to every caller that resolves ids through the read
/// model, so `doctor` advises retracting it — and the same run's read-model
/// repair then puts the row back and makes the edge whole again.
///
/// Foreign keys are disabled for this connection only, the idiom
/// [`replace_states`] and [`forget_state`] use; every other reader and writer
/// keeps the constraint.
pub fn forget_read_model_row(
    store: &SqliteStore,
    project: ProjectId,
    story: StoryNo,
) -> Result<(), StoreError> {
    let conn = Connection::open(store.path())
        .map_err(|e| StoreError::from_sqlite(e, "opening the store for injection"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| StoreError::from_sqlite(e, "setting the injection busy timeout"))?;
    conn.pragma_update(None, "foreign_keys", false)
        .map_err(|e| StoreError::from_sqlite(e, "lowering foreign keys for injection"))?;
    conn.execute(
        "DELETE FROM stories WHERE project_id = ?1 AND story_no = ?2",
        rusqlite::params![project.get(), story.get()],
    )
    .map_err(|e| StoreError::from_sqlite(e, "forgetting a read-model row"))?;
    Ok(())
}

/// Replaces a project's whole state catalog, stories be damned.
///
/// The catalog-wide sibling of [`forget_state`], for the fixtures that need a
/// project with no OPEN state at all, or one that has dropped a state a story
/// still sits in. Both shapes are refused by `story state …`, by the service
/// layer, and — since SH-130 — by the composite foreign key on `stories`, which
/// is what makes this back door necessary rather than merely convenient.
///
/// Foreign keys are disabled for this connection only, so the constraint is
/// back in force for every other reader and writer.
pub fn replace_states(
    store: &SqliteStore,
    project: ProjectId,
    states: &[crate::domain::StateDef],
) -> Result<(), StoreError> {
    let conn = Connection::open(store.path())
        .map_err(|e| StoreError::from_sqlite(e, "opening the store for injection"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| StoreError::from_sqlite(e, "setting the injection busy timeout"))?;
    conn.pragma_update(None, "foreign_keys", false)
        .map_err(|e| StoreError::from_sqlite(e, "lowering foreign keys for injection"))?;

    conn.execute(
        "DELETE FROM project_states WHERE project_id = ?1",
        rusqlite::params![project.get()],
    )
    .map_err(|e| StoreError::from_sqlite(e, "clearing the state catalog"))?;
    for (position, state) in states.iter().enumerate() {
        conn.execute(
            "INSERT INTO project_states (project_id, position, slug, superstate, role, description) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                project.get(),
                i64::try_from(position).expect("a short catalog"),
                state.slug,
                state.super_state.as_str(),
                state.role,
                state.description,
            ],
        )
        .map_err(|e| StoreError::from_sqlite(e, "writing a fabricated state"))?;
    }
    Ok(())
}

/// Retires a state from a project's catalog while stories still name it.
///
/// Since SH-130 `stories` carries a composite foreign key into
/// `project_states`, so this is refused through every ordinary path — which is
/// the point of the constraint, and the reason a test that wants the shape has
/// to come through here. `ConfigService::remove_state` migrates or refuses;
/// `put_states` hits the foreign key at COMMIT.
///
/// The shape is still worth fabricating: a database written by an older
/// storyhook can hold it, so the rebuild oracle has to keep *reporting* it
/// rather than propagating it, and `doctor --fix` has to refuse to guess at it.
///
/// Foreign keys are disabled for this connection only. The store's own
/// connections are unaffected, so the constraint is back in force the moment
/// anything reads or writes normally.
pub fn forget_state(store: &SqliteStore, project: ProjectId, slug: &str) -> Result<(), StoreError> {
    let conn = Connection::open(store.path())
        .map_err(|e| StoreError::from_sqlite(e, "opening the store for injection"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| StoreError::from_sqlite(e, "setting the injection busy timeout"))?;
    conn.pragma_update(None, "foreign_keys", false)
        .map_err(|e| StoreError::from_sqlite(e, "lowering foreign keys for injection"))?;
    conn.execute(
        "DELETE FROM project_states WHERE project_id = ?1 AND slug = ?2",
        rusqlite::params![project.get(), slug],
    )
    .map_err(|e| StoreError::from_sqlite(e, "retiring a state"))?;
    Ok(())
}

/// Rewrites a story's read-model row to a snapshot its own events do not
/// support.
///
/// The store writes the read model inside the transaction that appends the
/// events it was folded from, so a row that disagrees with its history is
/// unreachable through any public path — and *that* is the state `story doctor
/// --fix` exists to heal. The edit is applied to the stored `StorySnapshot`
/// JSON, then written back through a second connection, so the columns the
/// query surface reads are updated from the edited snapshot exactly as
/// `put_story` would have written them.
pub fn corrupt_snapshot(
    store: &SqliteStore,
    project: ProjectId,
    story: StoryNo,
    edit: impl FnOnce(&mut serde_json::Value),
) -> Result<(), StoreError> {
    let row = store
        .read(|tx| tx.story(project, story))?
        .ok_or_else(|| StoreError::Invariant("corrupting a story that does not exist".into()))?;
    let mut value = serde_json::to_value(&row.snapshot)
        .map_err(|e| StoreError::Invariant(format!("encoding a snapshot: {e}")))?;
    edit(&mut value);
    let snapshot: crate::domain::StorySnapshot = serde_json::from_value(value)
        .map_err(|e| StoreError::Invariant(format!("the edited snapshot is not one: {e}")))?;

    let conn = Connection::open(store.path())
        .map_err(|e| StoreError::from_sqlite(e, "opening the store for corruption"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| StoreError::from_sqlite(e, "setting the corruption busy timeout"))?;
    let encoded = serde_json::to_string(&snapshot)
        .map_err(|e| StoreError::Invariant(format!("encoding a snapshot: {e}")))?;
    // `closed_at` and `archived` travel with the edit, not just `state` and
    // `superstate`. Two CHECKs tie them to each other and to the superstate
    // (`archived = (closed_at IS NOT NULL)` from schema 1, and SH-130's
    // `(superstate = 'CLOSED') = archived`), so writing half the row produces a
    // constraint failure rather than the fabricated corruption the caller
    // asked for. The fabricated row still disagrees with its *events* — which
    // is the shape under test — while remaining internally consistent, which is
    // all the schema ever claimed to guarantee.
    conn.execute(
        "UPDATE stories SET snapshot = ?3, superstate = ?4, state = ?5, \
         closed_at = ?6, archived = ?7 \
         WHERE project_id = ?1 AND story_no = ?2",
        rusqlite::params![
            project.get(),
            story.get(),
            encoded,
            snapshot.superstate.as_str(),
            snapshot.state,
            snapshot.closed_at,
            i64::from(snapshot.closed_at.is_some()),
        ],
    )
    .map_err(|e| StoreError::from_sqlite(e, "corrupting a read-model row"))?;
    Ok(())
}

/// Re-folds a story from its own events and writes the result back.
fn refold(tx: &mut impl WriteOps, project: ProjectId, story: StoryNo) -> Result<(), StoreError> {
    let prefix = tx
        .project(project)?
        .ok_or_else(|| {
            StoreError::Invariant("injecting into a project that does not exist".into())
        })?
        .prefix;
    let stored = tx.events_for(project, story)?;
    let head = stored.last().map_or(EventSeq::ZERO, |event| event.seq);
    let (known, _unknown) = partition_known(story, &stored);
    let states = tx.state_map(project)?;
    let snapshot = fold_story(&story.to_id(&prefix), &known, &states)
        .map_err(|e| StoreError::Invariant(e.to_string()))?;
    tx.put_story(project, &snapshot, head)?;
    Ok(())
}

/// Runs `f` against a raw connection with the append-only trigger removed, then
/// puts the trigger back.
///
/// The trigger is re-created from `sqlite_master`'s own copy of its definition
/// rather than from a literal here, so a test helper can never leave the store
/// with a *weaker* guard than the schema declares — the text it restores is by
/// construction the text the schema installed.
fn with_append_guard_down<T>(
    store: &SqliteStore,
    f: impl FnOnce(&Connection) -> Result<T, rusqlite::Error>,
) -> Result<T, StoreError> {
    let conn = Connection::open(store.path())
        .map_err(|e| StoreError::from_sqlite(e, "opening the store for injection"))?;
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| StoreError::from_sqlite(e, "setting the injection busy timeout"))?;
    conn.pragma_update(None, "foreign_keys", true)
        .map_err(|e| StoreError::from_sqlite(e, "enabling foreign keys for injection"))?;

    let definition: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = 'events_reject_delete'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| StoreError::from_sqlite(e, "reading the append-only trigger"))?;

    conn.execute_batch("DROP TRIGGER events_reject_delete")
        .map_err(|e| StoreError::from_sqlite(e, "lowering the append-only trigger"))?;
    let result = f(&conn);
    // Restored whatever happened: a helper that left the guard down on the
    // failure path would silently disarm the invariant for the rest of the run.
    conn.execute_batch(&definition)
        .map_err(|e| StoreError::from_sqlite(e, "restoring the append-only trigger"))?;

    result.map_err(|e| StoreError::from_sqlite(e, "injecting into the store"))
}
