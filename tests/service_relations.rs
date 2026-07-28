//! Invariants of the relation service — and the proof that the drift guard
//! every service test relies on is not vacuously green.
//!
//! A relation is one fact asserted twice. The store keeps the relations
//! *table* symmetric by construction, so the thing worth testing here is the
//! level the store cannot reach: that both stories' **histories** agree, after
//! every operation, in every order.

use storyhook::domain::{StoryRelation, StorySnapshot, SuperState};
use storyhook::error::AppError;
use storyhook::service::{Ctx, NewStoryInput, RelationOutcome, RelationService, StoryService};
use storyhook::store::{ReadOps, SqliteStore, Store, StoryNo};
use storyhook_test_support::ServiceFixture;

// --- helpers ---------------------------------------------------------------

fn new_story(ctx: &Ctx<'_, SqliteStore>, title: &str) -> String {
    StoryService::new(ctx)
        .create(&NewStoryInput {
            title: title.to_string(),
            ..NewStoryInput::default()
        })
        .expect("creating a story")
        .id
}

fn snapshot(fixture: &ServiceFixture, id: &str) -> StorySnapshot {
    let no = StoryNo::parse_id("SH", id).expect("a well-formed id");
    fixture
        .store()
        .read(|tx| tx.story(fixture.project(), no))
        .expect("reading the story")
        .expect("the story exists")
        .snapshot
}

fn relations(fixture: &ServiceFixture, id: &str) -> Vec<(String, String)> {
    let mut edges: Vec<(String, String)> = snapshot(fixture, id)
        .relationships
        .into_iter()
        .map(|StoryRelation { relation, other_id }| (relation, other_id))
        .collect();
    edges.sort();
    edges
}

fn stored_edges(fixture: &ServiceFixture, id: &str) -> Vec<(String, i64)> {
    let no = StoryNo::parse_id("SH", id).expect("a well-formed id");
    let mut edges: Vec<(String, i64)> = fixture
        .store()
        .read(|tx| tx.relations_from(fixture.project(), no))
        .expect("reading edges")
        .into_iter()
        .map(|edge| (edge.relation, edge.other_no.get()))
        .collect();
    edges.sort();
    edges
}

fn validation_message(error: AppError) -> String {
    match error {
        AppError::Validation(message) => message,
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// The pairs `relation_edges` defines, as (asked-for, a's edge, b's edge).
const RELATION_PAIRS: [(&str, &str, &str); 9] = [
    ("relates-to", "relates-to", "relates-to"),
    ("related-to", "relates-to", "relates-to"),
    ("blocks", "blocks", "blocked-by"),
    ("blocked-by", "blocked-by", "blocks"),
    ("parent-of", "parent-of", "child-of"),
    ("child-of", "child-of", "parent-of"),
    ("duplicate-of", "duplicate-of", "duplicate-of"),
    ("obviates", "obviates", "obviated-by"),
    ("obviated-by", "obviated-by", "obviates"),
];

// --- symmetry --------------------------------------------------------------

#[test]
fn every_relation_is_written_to_both_stories() {
    for (asked, a_edge, b_edge) in RELATION_PAIRS {
        let fixture = ServiceFixture::new();
        let ctx = fixture.ctx();
        let a = new_story(&ctx, "a");
        let b = new_story(&ctx, "b");

        RelationService::new(&ctx)
            .relate(&a, asked, &b, false)
            .unwrap_or_else(|e| panic!("`{asked}` failed: {e}"));

        assert_eq!(
            relations(&fixture, &a),
            [(a_edge.to_string(), b.clone())],
            "`{asked}` on the first story"
        );
        assert_eq!(
            relations(&fixture, &b),
            [(b_edge.to_string(), a.clone())],
            "`{asked}` on the second story"
        );
    }
}

#[test]
fn every_relation_is_removed_from_both_stories() {
    for (asked, _, _) in RELATION_PAIRS {
        let fixture = ServiceFixture::new();
        let ctx = fixture.ctx();
        let a = new_story(&ctx, "a");
        let b = new_story(&ctx, "b");
        let service = RelationService::new(&ctx);

        service.relate(&a, asked, &b, false).unwrap();
        service.relate(&a, asked, &b, true).unwrap();

        assert!(
            relations(&fixture, &a).is_empty(),
            "`{asked}` left a's half"
        );
        assert!(
            relations(&fixture, &b).is_empty(),
            "`{asked}` left b's half"
        );
    }
}

#[test]
fn a_relation_asked_for_from_either_end_produces_the_same_edges() {
    let forward = ServiceFixture::new();
    {
        let ctx = forward.ctx();
        let a = new_story(&ctx, "a");
        let b = new_story(&ctx, "b");
        RelationService::new(&ctx)
            .relate(&a, "blocks", &b, false)
            .unwrap();
    }
    let backward = ServiceFixture::new();
    {
        let ctx = backward.ctx();
        let a = new_story(&ctx, "a");
        let b = new_story(&ctx, "b");
        RelationService::new(&ctx)
            .relate(&b, "blocked-by", &a, false)
            .unwrap();
    }

    assert_eq!(relations(&forward, "SH-1"), relations(&backward, "SH-1"));
    assert_eq!(relations(&forward, "SH-2"), relations(&backward, "SH-2"));
}

#[test]
fn the_relations_table_mirrors_what_the_snapshots_claim() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "a");
    let b = new_story(&ctx, "b");
    RelationService::new(&ctx)
        .relate(&a, "blocks", &b, false)
        .unwrap();

    assert_eq!(stored_edges(&fixture, &a), [("blocks".to_string(), 2)]);
    assert_eq!(stored_edges(&fixture, &b), [("blocked-by".to_string(), 1)]);
}

#[test]
fn relations_stay_symmetric_under_every_order_of_operations() {
    // Add, remove and re-add across two edges in several orders; the drift
    // guard on `Drop` checks symmetry after each fixture, and the assertions
    // here check it at each step.
    let orders: [&[(&str, &str, &str, bool)]; 4] = [
        &[
            ("SH-1", "blocks", "SH-2", false),
            ("SH-1", "relates-to", "SH-3", false),
            ("SH-1", "blocks", "SH-2", true),
        ],
        &[
            ("SH-2", "blocked-by", "SH-1", false),
            ("SH-1", "blocks", "SH-2", true),
            ("SH-1", "blocks", "SH-2", false),
        ],
        &[
            ("SH-1", "parent-of", "SH-2", false),
            ("SH-2", "child-of", "SH-1", true),
            ("SH-3", "child-of", "SH-1", false),
        ],
        &[
            ("SH-1", "obviates", "SH-2", false),
            ("SH-2", "obviated-by", "SH-1", true),
            ("SH-1", "duplicate-of", "SH-3", false),
        ],
    ];

    for (index, order) in orders.iter().enumerate() {
        let fixture = ServiceFixture::new();
        let ctx = fixture.ctx();
        for title in ["a", "b", "c"] {
            new_story(&ctx, title);
        }
        let service = RelationService::new(&ctx);
        for (a, relation, b, remove) in order.iter() {
            service
                .relate(a, relation, b, *remove)
                .unwrap_or_else(|e| panic!("order {index}: {a} {relation} {b}: {e}"));
            fixture.assert_no_drift();
        }
    }
}

// --- idempotence -----------------------------------------------------------

#[test]
fn adding_a_relation_twice_reports_no_change_and_writes_nothing() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "a");
    let b = new_story(&ctx, "b");
    let service = RelationService::new(&ctx);
    service.relate(&a, "blocks", &b, false).unwrap();

    let head_before = head_seq(&fixture, &a);
    match service.relate(&a, "blocks", &b, false).unwrap() {
        RelationOutcome::Unchanged { remove } => assert!(!remove),
        RelationOutcome::Changed(_) => panic!("a repeated add must report no change"),
    }
    assert_eq!(head_seq(&fixture, &a), head_before);
}

#[test]
fn removing_a_relation_that_is_not_there_reports_no_change() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "a");
    let b = new_story(&ctx, "b");
    match RelationService::new(&ctx)
        .relate(&a, "blocks", &b, true)
        .unwrap()
    {
        RelationOutcome::Unchanged { remove } => assert!(remove),
        RelationOutcome::Changed(_) => panic!("removing an absent edge must report no change"),
    }
}

fn head_seq(fixture: &ServiceFixture, id: &str) -> i64 {
    let no = StoryNo::parse_id("SH", id).expect("a well-formed id");
    fixture
        .store()
        .read(|tx| tx.head_seq(fixture.project(), no))
        .expect("reading the head")
        .get()
}

#[test]
fn a_half_written_relation_is_completed_rather_than_reported_unchanged() {
    // If only one end is missing its half, the add writes that end alone —
    // which is how a legacy project's asymmetric relation gets repaired by
    // simply asking for the relation again.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "a");
    let b = new_story(&ctx, "b");
    let service = RelationService::new(&ctx);
    service.relate(&a, "blocks", &b, false).unwrap();
    service.relate(&b, "blocks", &a, false).unwrap();

    assert_eq!(
        relations(&fixture, &a),
        [
            ("blocked-by".to_string(), b.clone()),
            ("blocks".to_string(), b.clone())
        ]
    );
    assert_eq!(
        relations(&fixture, &b),
        [
            ("blocked-by".to_string(), a.clone()),
            ("blocks".to_string(), a.clone())
        ]
    );
}

// --- rejections ------------------------------------------------------------

#[test]
fn a_story_cannot_relate_to_itself() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "a");
    let error = RelationService::new(&ctx)
        .relate(&a, "relates-to", &a, false)
        .unwrap_err();
    assert_eq!(
        validation_message(error),
        "stories cannot relate to themselves"
    );
}

#[test]
fn a_missing_story_is_reported_before_the_self_relation_rule() {
    let fixture = ServiceFixture::new();
    let error = RelationService::new(&fixture.ctx())
        .relate("SH-9", "relates-to", "SH-9", false)
        .unwrap_err();
    match error {
        AppError::NotFound(message) => assert_eq!(message, "story `SH-9` not found"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn relating_to_a_closed_story_is_refused_from_either_side() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "a");
    let b = new_story(&ctx, "b");
    StoryService::new(&ctx)
        .set_state(&b, "done", None, None)
        .unwrap();

    for (first, second) in [(&a, &b), (&b, &a)] {
        let error = RelationService::new(&ctx)
            .relate(first, "relates-to", second, false)
            .unwrap_err();
        assert_eq!(
            validation_message(error),
            "story `SH-2` is closed and cannot be modified"
        );
    }
}

#[test]
fn an_unsupported_relation_names_itself() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "a");
    let b = new_story(&ctx, "b");
    let error = RelationService::new(&ctx)
        .relate(&a, "sort-of-like", &b, false)
        .unwrap_err();
    assert_eq!(
        validation_message(error),
        "unsupported relationship `sort-of-like`"
    );
}

#[test]
fn a_story_may_not_have_two_parents() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let parent = new_story(&ctx, "parent");
    let other_parent = new_story(&ctx, "other parent");
    let child = new_story(&ctx, "child");
    let service = RelationService::new(&ctx);
    service.relate(&parent, "parent-of", &child, false).unwrap();

    let error = service
        .relate(&other_parent, "parent-of", &child, false)
        .unwrap_err();
    assert_eq!(
        validation_message(error),
        format!("story `{child}` already has a different parent")
    );
    // Neither half of the rejected edge survives.
    assert!(relations(&fixture, &other_parent).is_empty());
    assert_eq!(relations(&fixture, &child).len(), 1);
}

#[test]
fn the_second_parent_rule_holds_when_asked_for_from_the_child() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let parent = new_story(&ctx, "parent");
    let other_parent = new_story(&ctx, "other parent");
    let child = new_story(&ctx, "child");
    let service = RelationService::new(&ctx);
    service.relate(&child, "child-of", &parent, false).unwrap();

    let error = service
        .relate(&child, "child-of", &other_parent, false)
        .unwrap_err();
    assert!(validation_message(error).contains("already has a different parent"));
}

#[test]
fn a_parent_cycle_is_refused() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "a");
    let b = new_story(&ctx, "b");
    let service = RelationService::new(&ctx);
    service.relate(&a, "parent-of", &b, false).unwrap();

    let error = service.relate(&b, "parent-of", &a, false).unwrap_err();
    let message = validation_message(error);
    assert!(message.contains("would create a cycle"), "{message}");
}

#[test]
fn a_longer_parent_cycle_is_refused() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "a");
    let b = new_story(&ctx, "b");
    let c = new_story(&ctx, "c");
    let service = RelationService::new(&ctx);
    service.relate(&a, "parent-of", &b, false).unwrap();
    service.relate(&b, "parent-of", &c, false).unwrap();

    let error = service.relate(&c, "parent-of", &a, false).unwrap_err();
    assert!(validation_message(error).contains("would create a cycle"));
}

#[test]
fn removing_a_relation_skips_the_parent_rules() {
    // A project that already violates the single-parent rule must still be
    // able to take an edge back out.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let parent = new_story(&ctx, "parent");
    let child = new_story(&ctx, "child");
    let service = RelationService::new(&ctx);
    service.relate(&parent, "parent-of", &child, false).unwrap();
    service.relate(&parent, "parent-of", &child, true).unwrap();
    assert!(relations(&fixture, &child).is_empty());
}

#[test]
fn a_rejected_relation_leaves_both_stories_exactly_as_they_were() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "a");
    let b = new_story(&ctx, "b");
    let before = (head_seq(&fixture, &a), head_seq(&fixture, &b));

    let _ = RelationService::new(&ctx).relate(&a, "sort-of-like", &b, false);

    assert_eq!((head_seq(&fixture, &a), head_seq(&fixture, &b)), before);
}

// --- the returned story ----------------------------------------------------

#[test]
fn relating_answers_with_the_first_story_including_its_new_edge() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "a");
    let b = new_story(&ctx, "b");
    match RelationService::new(&ctx)
        .relate(&a, "blocks", &b, false)
        .unwrap()
    {
        RelationOutcome::Changed(snapshot) => {
            assert_eq!(snapshot.id, a);
            assert_eq!(
                snapshot.relationships,
                [StoryRelation {
                    relation: "blocks".into(),
                    other_id: b,
                }]
            );
        }
        RelationOutcome::Unchanged { .. } => panic!("the edge was new"),
    }
}

#[test]
fn completing_a_half_written_edge_still_answers_with_the_first_story() {
    // Only `b` needs an event here, so `a` is never appended to — its answer
    // has to come from its row rather than from a write that did not happen.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "a");
    let b = new_story(&ctx, "b");
    let service = RelationService::new(&ctx);
    service.relate(&a, "relates-to", &b, false).unwrap();
    service.relate(&b, "relates-to", &a, true).unwrap();

    match service.relate(&a, "relates-to", &b, false).unwrap() {
        RelationOutcome::Changed(snapshot) => assert_eq!(snapshot.id, a),
        RelationOutcome::Unchanged { .. } => panic!("b's half was missing"),
    }
    assert_eq!(relations(&fixture, &b), [("relates-to".to_string(), a)]);
}

// --- interaction with the lifecycle ----------------------------------------

#[test]
fn closing_a_related_story_leaves_the_relation_intact() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "a");
    let b = new_story(&ctx, "b");
    RelationService::new(&ctx)
        .relate(&a, "blocks", &b, false)
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&b, "done", None, None)
        .unwrap();

    assert_eq!(snapshot(&fixture, &b).superstate, SuperState::Closed);
    assert_eq!(relations(&fixture, &a), [("blocks".to_string(), b)]);
}

#[test]
fn deleting_a_related_story_leaves_both_halves_recorded() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "a");
    let b = new_story(&ctx, "b");
    RelationService::new(&ctx)
        .relate(&a, "blocks", &b, false)
        .unwrap();
    StoryService::new(&ctx).delete(&b, "obsolete").unwrap();

    assert_eq!(relations(&fixture, &a), [("blocks".to_string(), b.clone())]);
    assert_eq!(relations(&fixture, &b), [("blocked-by".to_string(), a)]);
}

// --- the drift guard itself ------------------------------------------------

#[test]
#[should_panic(expected = "the read model has drifted from its events")]
fn the_drift_guard_fails_a_fixture_whose_read_model_was_damaged() {
    // Every other test in this wave leans on the guard, so the guard has to be
    // shown to bite. The damage is applied under the store, through a second
    // connection, because the store's own API cannot produce it — which is the
    // entire point of the store.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "about to be corrupted");
    drop(ctx);

    let connection =
        rusqlite::Connection::open(fixture.store().path()).expect("opening the database directly");
    connection
        .execute("UPDATE stories SET title = 'not what the events say'", [])
        .expect("damaging the read model");

    fixture.assert_no_drift();
}

#[test]
#[should_panic(expected = "the read model has drifted from its events")]
fn the_drift_guard_runs_on_drop() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "about to be corrupted");
    drop(ctx);

    let connection =
        rusqlite::Connection::open(fixture.store().path()).expect("opening the database directly");
    connection
        .execute("UPDATE stories SET title = 'not what the events say'", [])
        .expect("damaging the read model");

    // No explicit assertion: dropping the fixture at the end of the test is
    // what has to fail.
}
