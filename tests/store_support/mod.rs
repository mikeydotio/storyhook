//! Fixture helpers shared by the store's integration tests.
//!
//! Deliberately *not* used by `tests/store_sqlite_conformance.rs`: the
//! conformance suite is a self-contained macro so that a second `Store`
//! implementation can be put under it with one line and no shared scaffolding
//! to port. These helpers exist for the tests that are about this engine in
//! particular — the oracle, the property tests — and they build fixtures the
//! way a service is expected to: append events, fold, write the result back, in
//! one transaction.

#![allow(dead_code)]

use std::path::Path;

use storyhook::domain::{StateDef, StoryEvent, SuperState, TypeDef, fold_story};
use storyhook::store::{
    EventSeq, ExpectedSeq, NewProject, ProjectId, ReadOps, SqliteStore, Store, StoreError, StoryNo,
    WriteOps,
};
use storyhook_test_support::scratch_dir;
use tempfile::TempDir;

/// A migrated store in its own scratch directory.
///
/// The `TempDir` is returned alongside because dropping it deletes the
/// database — a fixture that outlives its directory would be reading a file
/// that is no longer there.
#[must_use]
pub fn new_store() -> (TempDir, SqliteStore) {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).expect("opening the store");
    store.migrate().expect("migrating the store");
    (dir, store)
}

/// The default state set: three open states and one closed, with an active
/// role.
///
/// Carries `blocked` because [`storyhook::domain::REQUIRED_STATES`] requires it
/// of every project (SH-125). A fixture seeded below that floor is not a
/// smaller version of a real project — it is one that cannot have its catalog
/// edited at all, so every state test written against it would be testing the
/// refusal.
#[must_use]
pub fn default_states() -> Vec<StateDef> {
    vec![
        StateDef {
            slug: "todo".into(),
            super_state: SuperState::Open,
            role: None,
            description: Some("Not started".into()),
        },
        StateDef {
            slug: "in-progress".into(),
            super_state: SuperState::Open,
            role: Some("active".into()),
            description: None,
        },
        StateDef {
            slug: "blocked".into(),
            super_state: SuperState::Open,
            role: None,
            description: None,
        },
        StateDef {
            slug: "done".into(),
            super_state: SuperState::Closed,
            role: None,
            description: None,
        },
    ]
}

/// The default type set.
#[must_use]
pub fn default_types() -> Vec<TypeDef> {
    vec![
        TypeDef {
            slug: "feature".into(),
            description: None,
        },
        TypeDef {
            slug: "bug".into(),
            description: Some("Something is broken".into()),
        },
    ]
}

/// Creates a project with the default catalog, and one registered checkout.
pub fn seed_project(store: &SqliteStore, slug: &str, prefix: &str) -> ProjectId {
    store
        .write(|tx| {
            let project = tx.create_project(&NewProject {
                uuid: format!("uuid-{slug}"),
                slug: slug.to_string(),
                name: slug.to_string(),
                prefix: prefix.to_string(),
                created_at: "2026-01-01T00:00:00Z".into(),
            })?;
            tx.touch_project_path(
                project,
                Path::new(&format!("/checkouts/{slug}")),
                storyhook::store::PathKind::Main,
            )?;
            tx.put_states(project, &default_states())?;
            tx.put_types(project, &default_types())?;
            Ok(project)
        })
        .expect("seeding a project")
}

/// Appends events to a story and writes the folded result back, in one
/// transaction — the pattern every service is expected to follow.
///
/// The fold happens *here*, in the caller, because the store deliberately does
/// not fold. That the read model is written in the same transaction as the
/// events it came from is therefore visible, which is the whole reason the
/// store's API is shaped this way.
pub fn append_and_fold(
    store: &SqliteStore,
    project: ProjectId,
    story: StoryNo,
    expected: ExpectedSeq,
    events: &[StoryEvent],
) -> Result<EventSeq, StoreError> {
    store.write(|tx| {
        let head = tx.append_events(project, story, expected, events)?;
        let states = tx.state_map(project)?;
        let prefix = tx.project(project)?.expect("the project exists").prefix;
        let stored = tx.events_for(project, story)?;
        let (known, _unknown) = storyhook::store::partition_known(story, &stored);
        let snapshot = fold_story(&story.to_id(&prefix), &known, &states)
            .map_err(|e| StoreError::Invariant(e.to_string()))?;
        tx.put_story(project, &snapshot, head)?;
        Ok(head)
    })
}

/// Allocates a number, creates the story, and returns the number.
pub fn create_story(store: &SqliteStore, project: ProjectId, title: &str, at: &str) -> StoryNo {
    let story = store
        .write(|tx| tx.allocate_story_no(project))
        .expect("allocating a story number");
    append_and_fold(
        store,
        project,
        story,
        ExpectedSeq::Exact(EventSeq::ZERO),
        &[StoryEvent::StoryCreated {
            at: at.to_string(),
            title: title.to_string(),
            state: "todo".into(),
        }],
    )
    .expect("creating a story");
    story
}

/// Adds a relation, writing both stories' events and both folded snapshots in
/// **one** transaction.
///
/// This is the shape the relation service is meant to take: under the old
/// design the two halves were two separate operations, so a failure between
/// them left a half-written relation that survived indefinitely (SH-60). Here
/// a rejected edge — a second parent, a relation to a story that does not
/// exist — rolls back both halves together.
pub fn link_atomic(
    store: &SqliteStore,
    project: ProjectId,
    from: StoryNo,
    relation: &str,
    to: StoryNo,
) -> Result<(), StoreError> {
    let inverse = storyhook::domain::inverse_relation(relation)
        .ok_or_else(|| StoreError::Validation(format!("unknown relation `{relation}`")))?;
    store.write(|tx| {
        let prefix = tx.project(project)?.expect("the project exists").prefix;
        let states = tx.state_map(project)?;
        for (story, other, relation) in [(from, to, relation), (to, from, inverse)] {
            let head = tx.append_events(
                project,
                story,
                ExpectedSeq::Any,
                &[StoryEvent::StoryRelationshipAdded {
                    at: "2026-01-01T00:10:00Z".to_string(),
                    other_id: other.to_id(&prefix),
                    relation: relation.to_string(),
                }],
            )?;
            let stored = tx.events_for(project, story)?;
            let (known, _) = storyhook::store::partition_known(story, &stored);
            let snapshot = fold_story(&story.to_id(&prefix), &known, &states)
                .map_err(|e| StoreError::Invariant(e.to_string()))?;
            tx.put_story(project, &snapshot, head)?;
        }
        Ok(())
    })
}

/// Opens a second connection straight to the database file, for tests that need
/// to *damage* the read model in ways the store's own API forbids.
///
/// The corruption the oracle exists to detect cannot be produced through the
/// store — that is the point of the store. It has to be injected underneath it.
#[must_use]
pub fn raw(store: &SqliteStore) -> rusqlite::Connection {
    let conn = rusqlite::Connection::open(store.path()).expect("opening the database directly");
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    conn
}
