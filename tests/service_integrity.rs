//! What `story doctor` finds, and what `--fix` can actually put right.
//!
//! Every fixture here damages the project in a way the service API refuses to
//! produce — that is what makes them coverage of the doctor rather than of the
//! writers. Two mechanisms are used, and the difference matters:
//!
//! * **Events written for one end of a relation only**, through the store's own
//!   append path. The relations *table* stays symmetric (triggers materialize
//!   the mirror), so queries are unaffected; the asymmetry is in the histories,
//!   and only an event can fix it.
//! * **A read-model row edited through a second connection**, which no code
//!   path can do. That is drift: the row and its events disagree, and re-folding
//!   is exactly the repair.
//!
//! The two dimensions are complementary, never overlapping: the rebuild diff
//! also notices a relation only one end claims, but the story-level pass
//! already reports that in the legacy wording, so the doctor prints it once.

use rusqlite::Connection;
use storyhook::domain::{StoryEvent, TypeDef, fold_story};
use storyhook::error::AppError;
use storyhook::service::{Ctx, IntegrityService, NewStoryInput, RelationService, StoryService};
use storyhook::store::{
    ExpectedSeq, ReadOps, SqliteStore, Store, StoreError, StoryNo, WriteOps, partition_known,
};
use storyhook_test_support::{FIXTURE_NOW, ServiceFixture};

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

fn labels_of(fixture: &ServiceFixture, id: &str) -> Vec<String> {
    let no = StoryNo::parse_id("SH", id).expect("a well-formed id");
    fixture
        .store()
        .read(|tx| tx.story(fixture.project(), no))
        .expect("reading the story")
        .expect("the story exists")
        .snapshot
        .labels
}

/// Appends events to **one** story and folds only that story, bypassing the
/// services that would keep both ends of a relation in step.
fn append_to_one_end(fixture: &ServiceFixture, id: &str, events: &[StoryEvent]) {
    let project = fixture.project();
    let story = StoryNo::parse_id("SH", id).expect("a well-formed id");
    fixture
        .store()
        .write(|tx| {
            let head = tx.append_events(project, story, ExpectedSeq::Any, events)?;
            let stored = tx.events_for(project, story)?;
            let (known, _unknown) = partition_known(story, &stored);
            let states = tx.state_map(project)?;
            let snapshot = fold_story(id, &known, &states).map_err(StoreError::from)?;
            tx.put_story(project, &snapshot, head)?;
            Ok(())
        })
        .expect("appending to one end");
}

/// [`append_to_one_end`], handing back the store's refusal instead of
/// panicking on it.
fn try_append_to_one_end(
    fixture: &ServiceFixture,
    id: &str,
    events: &[StoryEvent],
) -> Result<(), StoreError> {
    let project = fixture.project();
    let story = StoryNo::parse_id("SH", id).expect("a well-formed id");
    fixture.store().write(|tx| {
        let head = tx.append_events(project, story, ExpectedSeq::Any, events)?;
        let stored = tx.events_for(project, story)?;
        let (known, _unknown) = partition_known(story, &stored);
        let states = tx.state_map(project)?;
        let snapshot = fold_story(id, &known, &states).map_err(StoreError::from)?;
        tx.put_story(project, &snapshot, head)?;
        Ok(())
    })
}

fn report(fixture: &ServiceFixture) -> Vec<String> {
    IntegrityService::new(&fixture.ctx())
        .report()
        .expect("reporting")
}

fn fix(fixture: &ServiceFixture) -> Result<String, AppError> {
    IntegrityService::new(&fixture.ctx()).fix()
}

// --- a healthy project -----------------------------------------------------

#[test]
fn a_healthy_project_reports_nothing_and_has_nothing_to_fix() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "A");
    let b = new_story(&ctx, "B");
    RelationService::new(&ctx)
        .relate(&a, "blocks", &b, false)
        .expect("relating");
    drop(ctx);

    assert!(report(&fixture).is_empty());
    assert_eq!(
        fix(&fixture).expect("fixing"),
        "doctor found nothing to fix"
    );
}

/// `obviated-by` flags a story everywhere *except* in the doctor, which has
/// always suppressed it: it is an authoring decision, not damage.
#[test]
fn an_obviated_story_is_flagged_for_list_but_not_reported_by_doctor() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "A");
    let b = new_story(&ctx, "B");
    RelationService::new(&ctx)
        .relate(&a, "obviated-by", &b, false)
        .expect("relating");
    drop(ctx);

    assert!(report(&fixture).is_empty(), "{:?}", report(&fixture));
}

// --- story-level findings --------------------------------------------------

#[test]
fn a_relation_only_one_end_claims_is_reported_and_repaired() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "A");
    let b = new_story(&ctx, "B");
    drop(ctx);

    append_to_one_end(
        &fixture,
        &a,
        &[StoryEvent::StoryRelationshipAdded {
            at: FIXTURE_NOW.to_string(),
            other_id: b.clone(),
            relation: "blocks".to_string(),
        }],
    );

    let issues = report(&fixture);
    assert!(
        issues
            .iter()
            .any(|issue| issue == "SH-1: missing inverse relation `blocked-by` on story `SH-2`"),
        "{issues:?}"
    );
    assert_eq!(
        issues.len(),
        1,
        "the rebuild diff sees the same asymmetry, and must not report it a \
         second time in a second vocabulary: {issues:?}"
    );

    assert_eq!(
        fix(&fixture).expect("fixing"),
        "doctor repaired supported integrity issues"
    );
    assert!(report(&fixture).is_empty(), "{:?}", report(&fixture));
}

/// SH-164: a label written before the write-path guard existed — `web,sse` as
/// one label, the SH-145 shape — cannot be produced by any service today, so
/// it is seeded the same way the relation asymmetry above is: straight
/// through the store's own append, bypassing every service.
#[test]
fn a_malformed_label_on_an_open_story_is_reported_and_repaired() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "A");
    drop(ctx);

    append_to_one_end(
        &fixture,
        &a,
        &[StoryEvent::StoryLabelsSet {
            at: FIXTURE_NOW.to_string(),
            labels: vec!["web,sse".to_string()],
        }],
    );
    assert_eq!(labels_of(&fixture, &a), ["web,sse"]);

    let issues = report(&fixture);
    assert!(
        issues
            .iter()
            .any(|issue| issue.starts_with("SH-1: malformed labels") && issue.contains("web,sse")),
        "{issues:?}"
    );

    assert_eq!(
        fix(&fixture).expect("fixing"),
        "doctor repaired supported integrity issues"
    );
    assert!(report(&fixture).is_empty(), "{:?}", report(&fixture));
    // Split on the way back out, and addressable again — which `web,sse` as
    // one label never was.
    assert_eq!(labels_of(&fixture, &a), ["sse", "web"]);
}

/// The counterpart of [`fix_does_not_append_to_archived_stories`]: a closed
/// story's history is closed, so a malformed label on one is a finding
/// `--fix` cannot clear, the same as any other issue a closed story cannot be
/// repaired out of (`fix_exits_non_zero_when_something_is_left_unrepaired`).
#[test]
fn a_malformed_label_on_a_closed_story_is_reported_but_not_repaired() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "A");
    StoryService::new(&ctx)
        .set_state(&a, "done", None, None)
        .expect("closing");
    drop(ctx);

    append_to_one_end(
        &fixture,
        &a,
        &[StoryEvent::StoryLabelsSet {
            at: FIXTURE_NOW.to_string(),
            labels: vec!["web,sse".to_string()],
        }],
    );

    let issues = report(&fixture);
    assert!(
        issues
            .iter()
            .any(|issue| issue.starts_with("SH-1: malformed labels")),
        "{issues:?}"
    );

    let error = fix(&fixture).expect_err("a closed story's history cannot be appended to");
    assert!(error.to_string().contains("malformed labels"), "{error}");
    assert_eq!(
        labels_of(&fixture, &a),
        ["web,sse"],
        "an archived story's history was appended to"
    );
}

/// Three of the legacy doctor's findings are **unrepresentable** in the store,
/// and this is where that is recorded.
///
/// A dangling relation needs a `story_relations` row naming a story that does
/// not exist (foreign key); a second parent needs two `child-of` rows for one
/// story (unique index); a read-model row with no events needs the events
/// deleted (append-only trigger). Each is refused by the schema rather than
/// detected by the doctor afterwards — the defect class is gone, not the
/// coverage. The checks stay in [`compute_integrity_issues`] because they are
/// also what `story show` and `list --flagged` compute, and because the
/// importer wave has to be able to report them about *legacy* data.
///
/// [`compute_integrity_issues`]: storyhook::domain::compute_integrity_issues
#[test]
fn the_shapes_doctor_used_to_find_are_now_refused_by_the_schema() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let child = new_story(&ctx, "child");
    let first = new_story(&ctx, "first parent");
    let second = new_story(&ctx, "second parent");
    RelationService::new(&ctx)
        .relate(&first, "parent-of", &child, false)
        .expect("relating");
    drop(ctx);

    let dangling = try_append_to_one_end(
        &fixture,
        &child,
        &[StoryEvent::StoryRelationshipAdded {
            at: FIXTURE_NOW.to_string(),
            other_id: "SH-99".to_string(),
            relation: "blocks".to_string(),
        }],
    )
    .expect_err("a relation to a story that does not exist");
    assert!(dangling.to_string().contains("FOREIGN KEY"), "{dangling:?}");

    let two_parents = try_append_to_one_end(
        &fixture,
        &child,
        &[StoryEvent::StoryRelationshipAdded {
            at: FIXTURE_NOW.to_string(),
            other_id: second.clone(),
            relation: "child-of".to_string(),
        }],
    )
    .expect_err("a second parent");
    assert!(
        two_parents
            .to_string()
            .contains("a story may have at most one parent"),
        "{two_parents:?}"
    );

    let connection = Connection::open(fixture.store().path()).expect("opening the database");
    let deleted = connection
        .execute("DELETE FROM events", [])
        .expect_err("deleting a history");
    assert!(
        deleted.to_string().contains("events are append-only"),
        "{deleted:?}"
    );

    assert!(report(&fixture).is_empty(), "nothing landed");
}

#[test]
fn a_parent_child_cycle_is_reported() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "A");
    let b = new_story(&ctx, "B");
    RelationService::new(&ctx)
        .relate(&a, "parent-of", &b, false)
        .expect("relating");
    drop(ctx);

    append_to_one_end(
        &fixture,
        &b,
        &[StoryEvent::StoryRelationshipAdded {
            at: FIXTURE_NOW.to_string(),
            other_id: a.clone(),
            relation: "parent-of".to_string(),
        }],
    );

    let issues = report(&fixture);
    assert!(
        issues
            .iter()
            .any(|issue| issue.contains("parent/child cycle detected")),
        "{issues:?}"
    );

    // `--fix` writes the missing inverse, which makes the histories agree —
    // and leaves the cycle, which no repair can decide how to break.
    let error = fix(&fixture).expect_err("a cycle is not repairable");
    assert!(
        error.to_string().contains("parent/child cycle detected"),
        "{error}"
    );
    assert!(
        !report(&fixture)
            .iter()
            .any(|issue| issue.contains("does not claim the inverse")),
        "the asymmetry WAS repaired"
    );
}

#[test]
fn a_story_whose_type_left_the_catalog_is_reported() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "typed");
    StoryService::new(&ctx)
        .set_fields(
            &id,
            &storyhook::service::FieldEdits {
                story_type: Some("bug".into()),
                ..storyhook::service::FieldEdits::default()
            },
        )
        .expect("typing");
    drop(ctx);

    // `ConfigService::remove_type` refuses while a story names the type, so the
    // catalog is edited under it.
    let project = fixture.project();
    fixture
        .store()
        .write(|tx| {
            tx.put_types(
                project,
                &[TypeDef {
                    slug: "feature".into(),
                    description: None,
                    emoji: None,
                }],
            )
        })
        .expect("shrinking the catalog");

    assert_eq!(report(&fixture), ["SH-1: unknown type `bug`"]);
    // Not repairable: nothing but a human knows which type the story meant.
    let error = fix(&fixture).expect_err("doctor cannot invent a type");
    assert!(matches!(error, AppError::Integrity(_)), "{error:?}");
    assert_eq!(error.exit_code(), 5);
}

// --- read-model drift ------------------------------------------------------

/// The dimension the legacy doctor could not have: the read model is a cache of
/// a fold, and until the store there was no second copy of the truth to compare
/// it against.
#[test]
fn a_row_that_disagrees_with_its_events_is_reported_and_repaired() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "the real title");
    drop(ctx);
    assert!(report(&fixture).is_empty());

    let connection = Connection::open(fixture.store().path()).expect("opening the database");
    connection
        .execute("UPDATE stories SET title = 'not what the events say'", [])
        .expect("damaging the read model");

    let issues = report(&fixture);
    assert_eq!(
        issues,
        ["story 1: title is `not what the events say` but the events say `the real title`"],
    );

    assert_eq!(
        fix(&fixture).expect("fixing"),
        "doctor repaired supported integrity issues"
    );
    assert!(report(&fixture).is_empty());
}

/// `--fix` must not claim a repair it did not make.
#[test]
fn fix_exits_non_zero_when_something_is_left_unrepaired() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "typed");
    StoryService::new(&ctx)
        .set_fields(
            &id,
            &storyhook::service::FieldEdits {
                story_type: Some("bug".into()),
                ..storyhook::service::FieldEdits::default()
            },
        )
        .expect("typing");
    drop(ctx);

    let project = fixture.project();
    fixture
        .store()
        .write(|tx| {
            tx.put_types(
                project,
                &[TypeDef {
                    slug: "feature".into(),
                    description: None,
                    emoji: None,
                }],
            )
        })
        .expect("shrinking the catalog");

    // Damage the read model too: the repairable half is repaired, the
    // unrepairable half still fails the command.
    let connection = Connection::open(fixture.store().path()).expect("opening the database");
    connection
        .execute("UPDATE stories SET title = 'wrong'", [])
        .expect("damaging the read model");

    let error = fix(&fixture).expect_err("an unknown type survives the repair");
    let message = error.to_string();
    assert!(message.contains("unknown type `bug`"), "{message}");
    assert!(
        !message.contains("the events say"),
        "the drift half WAS repaired: {message}"
    );
}

/// Repair leaves archived stories' histories alone — appending to a closed
/// story would reopen a question the project settled.
#[test]
fn fix_does_not_append_to_archived_stories() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "closed");
    StoryService::new(&ctx)
        .set_state(&id, "done", None, None)
        .expect("closing");
    drop(ctx);

    let project = fixture.project();
    let story = StoryNo::parse_id("SH", &id).expect("a well-formed id");
    let before = fixture
        .store()
        .read(|tx| tx.head_seq(project, story))
        .expect("reading the head");

    fix(&fixture).expect("fixing");

    let after = fixture
        .store()
        .read(|tx| tx.head_seq(project, story))
        .expect("reading the head");
    assert_eq!(before, after, "an archived story's history was appended to");
}
