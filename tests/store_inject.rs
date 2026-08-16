//! The corruption-fabrication API, tested against the shapes the suite needs.
//!
//! Eight tests in the integration suite fabricate states the public API refuses
//! to produce — a relation only one end claims, a parent cycle, a story whose
//! event log has vanished mid-read. They are the *only* coverage of what
//! storyhook does when its own storage is already wrong, so the flip owes them
//! a way to keep doing exactly that rather than a rewrite that makes them
//! well-behaved.
//!
//! This file is the contract for that API. Each test fabricates one of the
//! shapes `docs/rearch/flip-checklist.md` section B enumerates, and asserts
//! that the store now holds it — the *consumers* of these shapes live in
//! `tests/doctor.rs`, `tests/error_contract.rs` and friends.

mod store_support;

use store_support::{append_and_fold, new_store, seed_project};
use storyhook::domain::{StoryEvent, fold_story};
use storyhook::store::test_support::{
    forget_events, forget_story, inject_events, inject_raw_events,
};
use storyhook::store::{
    EventSeq, ExpectedSeq, RawEvent, ReadOps, Store, StoryNo, WriteOps, partition_known,
};

/// `at` for every fabricated event: fixtures assert on rendered output, so the
/// instant has to be the same on every run.
const AT: &str = "2026-01-01T00:00:00Z";

/// The `kind` every fabrication below writes, which must be one this build
/// decodes.
///
/// Both tests that use it fabricate a *known* kind carrying something wrong —
/// bytes that are not an event, and an orphan comment with no creation behind
/// it. Misspell it and both become fabrications of an *unknown* kind instead,
/// which the store retains rather than rejects (SH-54) and whose assertions
/// still pass, so the subject changes in silence (SH-364).
const DECODABLE_KIND: &str = "StoryCommentAdded";

#[test]
fn the_fabricated_kind_is_still_one_this_build_decodes() {
    assert!(
        storyhook::domain::is_known_event_kind(DECODABLE_KIND),
        "{DECODABLE_KIND} does not decode, so the fabrications below are \
         unknown-kind events wearing a known kind's name"
    );
}

/// A created story, ready to be corrupted.
fn seeded() -> (
    tempfile::TempDir,
    storyhook::store::SqliteStore,
    storyhook::store::ProjectId,
) {
    let (dir, store) = new_store();
    let project = seed_project(&store, "inject", "SH");
    (dir, store, project)
}

/// Creates story `n` in `project` with one `StoryCreated` event.
fn create(store: &storyhook::store::SqliteStore, project: storyhook::store::ProjectId, n: i64) {
    append_and_fold(
        store,
        project,
        StoryNo::new(n),
        ExpectedSeq::Exact(EventSeq::ZERO),
        &[StoryEvent::StoryCreated {
            at: AT.to_string(),
            title: format!("story {n}"),
            state: "todo".to_string(),
        }],
    )
    .expect("creating a story");
}

// ---------------------------------------------------------------------------
// 1. Typed events that bypass service invariants
// ---------------------------------------------------------------------------

#[test]
fn a_relation_only_one_end_claims_can_be_fabricated() {
    let (_dir, store, project) = seeded();
    create(&store, project, 1);
    create(&store, project, 2);

    // `RelationService::relate` writes both ends' events in one transaction, so
    // this shape is unreachable through the service layer — which is exactly
    // why the doctor's coverage of it needs an injection API.
    inject_events(
        &store,
        project,
        StoryNo::new(1),
        &[StoryEvent::StoryRelationshipAdded {
            at: AT.to_string(),
            other_id: "SH-2".to_string(),
            relation: "blocks".to_string(),
        }],
    )
    .expect("injecting a one-sided relation");

    let (one, two) = store
        .read(|tx| {
            Ok((
                tx.events_for(project, StoryNo::new(1))?,
                tx.events_for(project, StoryNo::new(2))?,
            ))
        })
        .expect("reading both logs");
    assert_eq!(
        one.len(),
        2,
        "the claiming end must carry the relationship event"
    );
    assert_eq!(
        two.len(),
        1,
        "the other end must NOT — a symmetric injection would fabricate a healthy relation"
    );
}

#[test]
fn an_injected_event_updates_the_read_model_it_folds_to() {
    let (_dir, store, project) = seeded();
    create(&store, project, 1);

    inject_events(
        &store,
        project,
        StoryNo::new(1),
        &[StoryEvent::StoryTypeSet {
            at: AT.to_string(),
            story_type: "no-such-type".to_string(),
        }],
    )
    .expect("injecting an unknown story type");

    let row = store
        .read(|tx| tx.story(project, StoryNo::new(1)))
        .expect("reading the story")
        .expect("the story exists");
    assert_eq!(
        row.snapshot.story_type.as_deref(),
        Some("no-such-type"),
        "injection must leave the read model folded from the events it wrote — a \
         fabricator that only wrote events would leave `story show` telling the truth"
    );
}

// ---------------------------------------------------------------------------
// 2. Raw bytes that are not valid events
// ---------------------------------------------------------------------------

#[test]
fn bytes_that_are_not_a_decodable_event_can_be_written_verbatim() {
    let (_dir, store, project) = seeded();
    create(&store, project, 1);

    inject_raw_events(
        &store,
        project,
        StoryNo::new(1),
        &[RawEvent {
            kind: DECODABLE_KIND.to_string(),
            at: AT.to_string(),
            payload: "{not json at all".to_string(),
        }],
    )
    .expect("injecting undecodable bytes");

    let stored = store
        .read(|tx| tx.events_for(project, StoryNo::new(1)))
        .expect("reading the log");
    let (known, unknown) = partition_known(StoryNo::new(1), &stored);
    assert_eq!(known.len(), 1, "only the creation event still decodes");
    assert_eq!(
        unknown.len(),
        1,
        "the torn payload must survive as an undecodable event rather than being rejected"
    );
    assert_eq!(unknown[0].kind, DECODABLE_KIND);
}

#[test]
fn a_story_whose_log_cannot_fold_can_be_fabricated() {
    let (_dir, store, project) = seeded();
    create(&store, project, 1);
    // Drop the creation event and leave only a comment: `fold_story` refuses a
    // log with no `StoryCreated`, which is the store-side shape of the legacy
    // "empty event log" corruption.
    forget_events(&store, project, StoryNo::new(1)).expect("forgetting the log");
    inject_raw_events(
        &store,
        project,
        StoryNo::new(1),
        &[RawEvent {
            kind: DECODABLE_KIND.to_string(),
            at: AT.to_string(),
            payload: format!(r#"{{"kind":"{DECODABLE_KIND}","at":"{AT}","text":"orphan"}}"#),
        }],
    )
    .expect("injecting an orphan comment");

    let stored = store
        .read(|tx| tx.events_for(project, StoryNo::new(1)))
        .expect("reading the log");
    let (known, _) = partition_known(StoryNo::new(1), &stored);
    let states = store
        .read(|tx| tx.state_map(project))
        .expect("reading the catalog");
    assert!(
        fold_story("SH-1", &known, &states).is_err(),
        "a log with no creation event must not fold — that is the corruption under test"
    );
}

// ---------------------------------------------------------------------------
// 3. Deletion — a story's events vanishing out from under a reader
// ---------------------------------------------------------------------------

#[test]
fn a_storys_events_can_be_deleted_leaving_its_read_model_row_behind() {
    let (_dir, store, project) = seeded();
    create(&store, project, 1);

    let removed = forget_events(&store, project, StoryNo::new(1)).expect("forgetting the log");
    assert_eq!(removed, 1, "one event was there to remove");

    let (events, row) = store
        .read(|tx| {
            Ok((
                tx.events_for(project, StoryNo::new(1))?,
                tx.story(project, StoryNo::new(1))?,
            ))
        })
        .expect("reading back");
    assert!(events.is_empty(), "the log must be gone");
    assert!(
        row.is_some(),
        "the read-model row must remain — a row with no events is the shape under test"
    );
}

#[test]
fn the_append_only_guard_is_restored_after_a_deletion() {
    let (_dir, store, project) = seeded();
    create(&store, project, 1);
    forget_events(&store, project, StoryNo::new(1)).expect("forgetting the log");

    // The guard is dropped to perform the delete and must be put back, or every
    // later assertion in the test that used it is running against a store whose
    // central invariant has been quietly switched off.
    let conn = rusqlite::Connection::open(store.path()).expect("opening the store directly");
    conn.execute(
        "INSERT INTO events (project_id, story_no, seq, global_seq, kind, at, payload) \
                  VALUES (1, 1, 1, 9999, 'StoryCreated', '2026-01-01T00:00:00Z', '{}')",
        [],
    )
    .expect("seeding a row to delete");
    let err = conn
        .execute("DELETE FROM events WHERE story_no = 1", [])
        .expect_err("the append-only trigger must be back in place");
    assert!(
        err.to_string().contains("append-only"),
        "expected the append-only guard, got: {err}"
    );
}

#[test]
fn a_whole_story_can_be_made_to_vanish() {
    let (_dir, store, project) = seeded();
    create(&store, project, 1);
    create(&store, project, 2);

    forget_story(&store, project, StoryNo::new(1)).expect("forgetting the story");

    let (gone, survivor) = store
        .read(|tx| {
            Ok((
                tx.story(project, StoryNo::new(1))?,
                tx.story(project, StoryNo::new(2))?,
            ))
        })
        .expect("reading back");
    assert!(gone.is_none(), "the story must be gone from the read model");
    assert!(
        survivor.is_some(),
        "forgetting one story must not touch its neighbours"
    );
}

// ---------------------------------------------------------------------------
// 4. The story number allocator is untouched
// ---------------------------------------------------------------------------

#[test]
fn injection_does_not_disturb_the_id_counter() {
    let (_dir, store, project) = seeded();
    create(&store, project, 1);
    inject_events(
        &store,
        project,
        StoryNo::new(1),
        &[StoryEvent::StoryCommentAdded {
            at: AT.to_string(),
            text: "injected".to_string(),
        }],
    )
    .expect("injecting a comment");

    let next = store
        .write(|tx| tx.allocate_story_no(project))
        .expect("allocating");
    assert_eq!(
        next,
        StoryNo::new(1),
        "injection writes events for a story number the caller chose; it must not \
         consume numbers from the allocator, or a fixture's ids would depend on how \
         much corruption it fabricated"
    );
}
