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

use std::collections::BTreeSet;

use rusqlite::Connection;
use storyhook::domain::finding::{Finding, FindingCode, FindingData};
use storyhook::domain::{StateDef, StoryEvent, SuperState, TypeDef, fold_story};
use storyhook::error::AppError;
use storyhook::service::{
    Ctx, IntegrityService, NewStoryInput, QueryService, RelationService, StoryService,
};
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
            let head = tx.append_events(
                project,
                story,
                ExpectedSeq::Any,
                events,
                &storyhook::domain::provenance::Provenance::unrecorded(),
            )?;
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
        let head = tx.append_events(
            project,
            story,
            ExpectedSeq::Any,
            events,
            &storyhook::domain::provenance::Provenance::unrecorded(),
        )?;
        let stored = tx.events_for(project, story)?;
        let (known, _unknown) = partition_known(story, &stored);
        let states = tx.state_map(project)?;
        let snapshot = fold_story(id, &known, &states).map_err(StoreError::from)?;
        tx.put_story(project, &snapshot, head)?;
        Ok(())
    })
}

/// The findings' sentences, which is what most of this file asserts on.
///
/// `report()` answers typed findings since SH-244; the assertions below are
/// about the *report a person reads*, so they keep reading sentences and the
/// structured half is pinned separately, at the bottom of this file.
fn report(fixture: &ServiceFixture) -> Vec<String> {
    findings(fixture)
        .into_iter()
        .map(|finding| finding.message)
        .collect()
}

/// The findings themselves.
fn findings(fixture: &ServiceFixture) -> Vec<Finding> {
    IntegrityService::new(&fixture.ctx())
        .report()
        .expect("reporting")
}

fn fix(fixture: &ServiceFixture) -> Result<String, AppError> {
    IntegrityService::new(&fixture.ctx()).fix()
}

/// The advisories `story doctor` prints when it finds no integrity fault.
///
/// Through `dispatch` rather than the service, because advice is assembled in
/// the arm rather than by `IntegrityService` — deliberately, since exiting
/// non-zero is what `report()`'s non-empty return means and advice must not do
/// that. A fixture store lives under a temp directory, so the catalog
/// advisories are skipped and what is left is the project's own.
fn advisories(fixture: &ServiceFixture) -> Vec<String> {
    match storyhook::invoke::dispatch(
        &fixture.ctx(),
        storyhook::cli::Invocation::Doctor { fix: false },
    )
    .expect("doctor must not fail on a healthy project")
    {
        storyhook::output::Response::Issues(advice) => advice,
        other => panic!("`story doctor` must answer with Issues, got {other:?}"),
    }
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

/// `obviated-by` flags a story everywhere *except* in the doctor — and the
/// reason is that a **symmetric** obviation edge is an authoring decision that
/// no integrity check has anything to say about, not that the doctor filters
/// anything out.
///
/// This test asserted only the doctor half and named the filter in its own doc
/// comment, which made it read as the pin on `is_suppressed`. It never was:
/// `relate` writes both ends, so `compute_integrity_issues` produces nothing
/// here and the assertion held with the filter or without it — vacuously since
/// SH-244 stopped `story_issues` reading `flagged_reasons` (SH-268). It now
/// asserts the half of its own name it never checked, which is the half that
/// makes the two surfaces' disagreement deliberate: the flag is still on the
/// story, and only the doctor declines to call it damage.
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

    let flagged = fixture
        .store()
        .read(|tx| {
            Ok(QueryService::new(tx, fixture.project(), FIXTURE_NOW)
                .show(&a)?
                .flagged_reasons)
        })
        .expect("showing SH-1");
    assert!(
        flagged.contains(&"story is obviated by another story".to_string()),
        "the flag is the authoring decision, and it stays: {flagged:?}"
    );
}

/// A one-sided obviation edge is damage like any other relation's, from
/// whichever end claims it (SH-268).
///
/// This is the direction `is_suppressed` ate. The finding's sentence names the
/// **expected inverse**, `obviated-by`, so it contained the substring; its
/// mirror below spells the inverse `obviates` and was reported all along. Which
/// end of a broken pair survived decided whether `doctor` mentioned it, and it
/// was the harmful end that went unmentioned: `is_ready` excludes a story only
/// when *that story* carries `obviated-by`, so SH-2 here — declared unnecessary
/// by SH-1 — keeps being recommended by `story next` until the edge is whole.
#[test]
fn an_asymmetric_obviates_edge_is_reported_like_any_other_relation() {
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
            relation: "obviates".to_string(),
        }],
    );

    let issues = report(&fixture);
    assert_eq!(
        issues,
        ["SH-1: missing inverse relation `obviated-by` on story `SH-2`"],
        "the rebuild diff sees the same asymmetry and must not report it a \
         second time in a second vocabulary"
    );

    assert_eq!(
        fix(&fixture).expect("both ends are open"),
        "doctor repaired supported integrity issues"
    );
    assert!(report(&fixture).is_empty(), "{:?}", report(&fixture));
}

/// The mirror of the test above, and the whole point of the pair: `doctor`'s
/// answer must not depend on which end of an obviation edge survives.
///
/// This direction was reported before SH-268 and nothing pinned it, so the
/// asymmetry could have been repaired in either direction without a test
/// noticing. Both are pinned now.
#[test]
fn an_asymmetric_obviated_by_edge_is_reported_like_any_other_relation() {
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
            relation: "obviated-by".to_string(),
        }],
    );

    let issues = report(&fixture);
    assert_eq!(
        issues,
        ["SH-1: missing inverse relation `obviates` on story `SH-2`"]
    );

    // Typed, not only rendered: the sentence names the *expected inverse*
    // while the data carries the *claimed* relation, which is exactly the
    // distinction the substring test could not draw (SH-268).
    let finding = findings(&fixture).pop().expect("the one finding");
    assert_eq!(finding.code, FindingCode::MissingInverseRelation);
    assert_eq!(
        finding.data,
        Some(FindingData::Relation {
            relation: "obviated-by".to_string(),
            other: b.clone(),
        })
    );

    assert_eq!(
        fix(&fixture).expect("both ends are open"),
        "doctor repaired supported integrity issues"
    );
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
///
/// SH-225: it must also *say so*. Refusing is right; refusing silently left
/// the operator re-reading a finding `--fix` had just declined to touch, with
/// nothing to distinguish "this story is closed" from "the doctor is broken" —
/// which is what kept SH-181's eight rows invisible for a week.
#[test]
fn a_malformed_label_on_a_closed_story_is_reported_but_not_repaired() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "A");
    StoryService::new(&ctx)
        .set_state(&a, "done", None, None, None)
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
    let message = error.to_string();
    assert!(message.contains("malformed labels"), "{message}");
    assert!(
        message.contains("SH-1: normalize its labels to [\"sse\", \"web\"]"),
        "the repair `--fix` declined to make is unnamed: {message}"
    );
    assert!(
        message.contains("story reopen"),
        "nothing tells the operator how to unblock the repair: {message}"
    );
    assert_eq!(
        labels_of(&fixture, &a),
        ["web,sse"],
        "an archived story's history was appended to"
    );
}

/// SH-225, the case that misleads hardest: the story a repair must be
/// appended to is **not** the story the finding names. A missing inverse is
/// reported against the end that *has* the relation (`compute_integrity_
/// issues`) and repaired on the end that lacks it — so an operator working
/// from the finding alone reopens the wrong story.
#[test]
fn fix_names_the_closed_end_an_inverse_repair_needs_reopened() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "A");
    let b = new_story(&ctx, "B");
    StoryService::new(&ctx)
        .set_state(&b, "done", None, None, None)
        .expect("closing B");
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

    let error = fix(&fixture).expect_err("the inverse belongs on a closed story");
    let message = error.to_string();
    assert!(
        message.contains("SH-1: missing inverse relation `blocked-by` on story `SH-2`"),
        "{message}"
    );
    assert!(
        message
            .contains("SH-2: write the missing inverse relation `blocked-by` of SH-1's `blocks`"),
        "the finding names SH-1, so only this can tell the operator to reopen SH-2: {message}"
    );

    // The recipe that message prints, followed to the letter: advice that does
    // not actually clear the finding would be a worse failure than silence.
    let ctx = fixture.ctx();
    StoryService::new(&ctx).reopen(&b).expect("reopening SH-2");
    drop(ctx);
    assert_eq!(
        fix(&fixture).expect("the destination is open now"),
        "doctor repaired supported integrity issues"
    );
    assert!(report(&fixture).is_empty(), "{:?}", report(&fixture));
}

/// The other half of SH-225's blind spot, and the one that was never a
/// message problem: `--fix` visited *open* stories only, so a repair a closed
/// story's relation implied — one whose append target is the open end, and
/// therefore perfectly legal — was never even attempted, leaving a finding
/// the command could never clear.
#[test]
fn fix_writes_an_inverse_an_open_story_lacks_of_a_closed_ones_relation() {
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
    let ctx = fixture.ctx();
    StoryService::new(&ctx)
        .set_state(&a, "done", None, None, None)
        .expect("closing A");
    drop(ctx);

    assert_eq!(
        fix(&fixture).expect("the append target, SH-2, is open"),
        "doctor repaired supported integrity issues"
    );
    assert!(report(&fixture).is_empty(), "{:?}", report(&fixture));
}

/// SH-225's blocked-repair naming, on an obviation edge — which is to say, on
/// the same path as every other relation (SH-268).
///
/// This test used to be the pin on `is_suppressed`, and it was founded *on*
/// the suppression: it asserted a **clean report**, which only held because
/// `doctor` was declining to mention the very finding whose repair it had just
/// declined to make. Delete the filter and the premise goes with it — the
/// finding is reported, so the run fails, and the naming lands on the failure
/// path where it can be read beside the finding it explains.
///
/// What is left is worth keeping for a reason the old framing obscured: the
/// story a repair must be appended to is not the story the finding names, and
/// that holds for the obviation pair exactly as `fix_names_the_closed_end_an_
/// inverse_repair_needs_reopened` holds it for `blocks`. Nothing about this
/// pair is special any more, and that is the assertion.
#[test]
fn a_blocked_obviation_repair_is_named_on_the_failure_path() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "A");
    let b = new_story(&ctx, "B");
    StoryService::new(&ctx)
        .set_state(&b, "done", None, None, None)
        .expect("closing B");
    drop(ctx);

    append_to_one_end(
        &fixture,
        &a,
        &[StoryEvent::StoryRelationshipAdded {
            at: FIXTURE_NOW.to_string(),
            other_id: b.clone(),
            relation: "obviates".to_string(),
        }],
    );

    assert_eq!(
        report(&fixture),
        ["SH-1: missing inverse relation `obviated-by` on story `SH-2`"],
        "an obviation asymmetry is damage, and damage is reported"
    );

    let error = fix(&fixture).expect_err("the inverse belongs on a closed story");
    let message = error.to_string();
    assert!(
        message.contains("SH-1: missing inverse relation `obviated-by` on story `SH-2`"),
        "{message}"
    );
    assert!(
        message.contains(
            "SH-2: write the missing inverse relation `obviated-by` of SH-1's `obviates`"
        ),
        "the finding names SH-1, so only this can tell the operator to reopen SH-2: {message}"
    );

    // And the same recipe clears it — advice that does not actually clear the
    // finding would be a worse failure than silence.
    let ctx = fixture.ctx();
    StoryService::new(&ctx).reopen(&b).expect("reopening SH-2");
    drop(ctx);
    assert_eq!(
        fix(&fixture).expect("the destination is open now"),
        "doctor repaired supported integrity issues"
    );
    assert!(report(&fixture).is_empty(), "{:?}", report(&fixture));
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

/// Injects an unrecognised-kind event (a newer storyhook's data, not this
/// build's) at seq 2 of a fresh story.
fn inject_unrecognised_kind(fixture: &ServiceFixture) {
    storyhook::store::test_support::inject_raw_events(
        fixture.store(),
        fixture.project(),
        StoryNo::new(1),
        &[storyhook::store::RawEvent {
            kind: "StoryPinned".to_string(),
            at: "2030-01-01T00:00:00Z".to_string(),
            payload: r#"{"kind":"StoryPinned","at":"2030-01-01T00:00:00Z"}"#.to_string(),
        }],
    )
    .expect("injecting");
}

/// Injects a known-kind event whose payload this build cannot read (a torn
/// payload — damage) at seq 2 of a fresh story.
fn inject_torn_payload(fixture: &ServiceFixture) {
    storyhook::store::test_support::inject_raw_events(
        fixture.store(),
        fixture.project(),
        StoryNo::new(1),
        &[storyhook::store::RawEvent {
            kind: "StoryCommentAdded".to_string(),
            at: "2030-01-01T00:00:01Z".to_string(),
            payload: "{not json at all".to_string(),
        }],
    )
    .expect("injecting");
}

/// SH-67 gave `story doctor` its first line about an event this build cannot
/// decode. SH-185 settles what SH-67 left open (its council's Q3): an
/// unrecognised *kind* is a newer storyhook's data, not damage, so it must
/// never join `report()`'s health-determining vector, must not push `story
/// doctor` to a non-zero exit, and must not make `--fix` fail. It is a
/// *notice* — [`IntegrityService::notices`] — not a finding.
#[test]
fn an_unrecognised_event_kind_is_a_notice_not_a_finding() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "visited by a newer storyhook");
    drop(ctx);
    inject_unrecognised_kind(&fixture);

    assert!(
        report(&fixture).is_empty(),
        "an unrecognised kind must not be a doctor finding"
    );

    let notices = IntegrityService::new(&fixture.ctx())
        .notices()
        .expect("notices");
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].contains("event 2")
            && notices[0].contains("`StoryPinned`")
            && notices[0].contains("A newer storyhook wrote it."),
        "{notices:?}"
    );

    // A notice-only project is healthy: `doctor` answers `Issues`, not an
    // error, and the notice rides along as advice.
    let advice = advisories(&fixture);
    assert!(
        advice.iter().any(|line| line.contains("StoryPinned")),
        "the notice must still reach the user, just through the advisory channel: {advice:?}"
    );

    // Nothing to fix, by design — not a repair failure.
    let message = fix(&fixture).expect("a notice must never fail --fix");
    assert!(
        message.contains("StoryPinned"),
        "the notice must not vanish from --fix's own report either: {message}"
    );
}

/// A kind this build knows and cannot read is a torn payload: damage, exactly
/// as SH-67 left it. It still forces a non-zero exit and `--fix` still cannot
/// invent the missing bytes.
#[test]
fn a_torn_known_event_payload_is_still_a_finding_fix_cannot_repair() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "a torn payload");
    drop(ctx);
    inject_torn_payload(&fixture);

    let issues = report(&fixture);
    assert_eq!(issues.len(), 1, "{issues:?}");
    assert!(
        issues[0].contains("event 2")
            && issues[0].contains("`StoryCommentAdded`")
            && !issues[0].contains("newer storyhook"),
        "a torn payload reads differently from a notice: {issues:?}"
    );
    assert!(
        IntegrityService::new(&fixture.ctx())
            .notices()
            .expect("notices")
            .is_empty(),
        "a torn payload is damage, not a notice"
    );

    assert!(
        fix(&fixture).is_err(),
        "doctor cannot invent a payload it cannot read"
    );
}

/// SH-185's load-bearing constraint: a notice must stay visible even when the
/// same run also carries a real finding, precisely because it is no longer
/// part of the vector that decides health. Silently dropping it just because
/// something else made `doctor` unhealthy would be its own regression.
#[test]
fn a_notice_stays_visible_alongside_a_real_finding_in_the_same_run() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "both a notice and damage");
    drop(ctx);
    inject_unrecognised_kind(&fixture);
    inject_torn_payload(&fixture);

    let issues = report(&fixture);
    assert_eq!(
        issues.len(),
        1,
        "the notice must not join the health-determining vector: {issues:?}"
    );

    let error = storyhook::invoke::dispatch(
        &fixture.ctx(),
        storyhook::cli::Invocation::Doctor { fix: false },
    )
    .expect_err("real damage still fails `doctor`");
    let rendered = error.to_string();
    assert!(
        rendered.contains("StoryCommentAdded"),
        "the real finding must still be reported: {rendered}"
    );
    assert!(
        rendered.contains("StoryPinned"),
        "the notice must not silently vanish just because doctor is unhealthy: {rendered}"
    );

    let fix_error = fix(&fixture).expect_err("real damage still fails --fix");
    let fix_rendered = fix_error.to_string();
    assert!(fix_rendered.contains("StoryCommentAdded"));
    assert!(
        fix_rendered.contains("StoryPinned"),
        "the notice must not vanish from a failed --fix either: {fix_rendered}"
    );
}

/// SH-266, and the generalisation of the test above it: **advice is the same
/// list on both outcomes**.
///
/// SH-185's constraint was pinned for `notices` alone, and `notices` was the
/// one advice source the damaged branch passed on — the other seven were
/// assembled inside the healthy branch, so an orphaned registration, an
/// unregistered origin, a github remote that had drifted, an abandoned
/// command, a stale pointer or a legacy commit link was reported only while
/// nothing else was wrong. Withheld, that is, exactly when an operator is
/// reading.
///
/// Asserted as an **equality** rather than a containment, so the property is
/// "the same advice", not "some advice survived": an advice source added to
/// one branch and not the other fails here.
///
/// A stale pointer prefix (SH-190) is the provocation because it is advice no
/// feature flag can switch off and no store-location guard can suppress — the
/// catalog advisories are deliberately silent under a temporary store, which
/// every fixture here has.
#[test]
fn the_advice_a_damaged_run_carries_is_the_advice_a_healthy_one_prints() {
    use storyhook::service::project::{ProjectPointer, write_pointer};

    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "something to be told, and then some damage");
    drop(ctx);

    // The project's ids are `SH-…`; its checkout claims `OLD-…`. Hand-edited
    // or copy-pasted, which is exactly the case SH-190 added the advisory for.
    write_pointer(
        fixture.cwd(),
        &ProjectPointer::new("fixture-uuid".to_string(), "OLD".to_string()),
    )
    .expect("writing a stale pointer file");

    let healthy = advisories(&fixture);
    assert!(
        healthy.iter().any(|line| line.contains("prefix `OLD`")),
        "the fixture must actually provoke an advisory, or this test proves \
         nothing: {healthy:?}"
    );

    inject_torn_payload(&fixture);

    let error = storyhook::invoke::dispatch(
        &fixture.ctx(),
        storyhook::cli::Invocation::Doctor { fix: false },
    )
    .expect_err("a torn payload still fails `doctor`");
    let AppError::Integrity(detail) = &error else {
        panic!("damage is an integrity error: {error:?}");
    };
    assert_eq!(
        detail.advice, healthy,
        "the same run's advice must not depend on whether the project is also \
         damaged (SH-266)"
    );
    assert!(
        error.to_string().contains("prefix `OLD`"),
        "and it must reach the rendered report a person reads: {error}"
    );
}

/// A project with no github-sync configuration has nothing to be told.
#[test]
fn a_project_with_no_github_sync_has_nothing_to_be_told() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "unsynced");
    drop(ctx);
    assert!(
        report(&fixture).is_empty(),
        "a project with no github-sync has nothing to be told"
    );
    assert!(advisories(&fixture).is_empty());
}

/// A blob this build cannot parse as a `GithubSyncConfig` — a document
/// written by a wildly different shape, or hand-edited — must not crash
/// `story doctor`; it is reported by `story github-sync` itself the next time
/// it tries to use it, not duplicated here (SH-189).
#[test]
fn an_unparseable_github_sync_blob_produces_no_advisory_and_no_finding() {
    let fixture = ServiceFixture::new();
    fixture
        .store()
        .write(|tx| {
            let mut row = tx.settings(fixture.project())?;
            row.github_sync = Some(serde_json::json!({"owner": "ada", "repo": "engine"}));
            tx.put_settings(fixture.project(), &row)
        })
        .expect("configuring github-sync");

    assert!(
        advisories(&fixture).is_empty(),
        "an unparseable blob is not this advisory's failure to report"
    );
    assert!(
        report(&fixture).is_empty(),
        "a malformed github-sync document is not an integrity fault"
    );
}

/// SH-189: once a checkout's own git remote confirms the configured
/// repository, there is nothing to flag.
#[cfg(feature = "github-sync")]
#[test]
fn a_github_remote_matching_the_configured_repository_produces_no_advisory() {
    use storyhook::github::sync_state::{GithubRepo, GithubSyncConfig, SyncMode, SyncSettings};

    let fixture = ServiceFixture::new();
    fixture
        .store()
        .write(|tx| {
            let mut row = tx.settings(fixture.project())?;
            row.github_sync = Some(
                serde_json::to_value(GithubSyncConfig {
                    github: GithubRepo {
                        owner: "acme".into(),
                        repo: "widgets".into(),
                    },
                    sync: SyncSettings {
                        mode: SyncMode::Manual,
                        last_sync_at: None,
                        last_full_sync_at: None,
                    },
                    etags: Default::default(),
                    mappings: Vec::new(),
                })
                .expect("serializing"),
            );
            tx.put_settings(fixture.project(), &row)
        })
        .expect("configuring github-sync");
    git(fixture.cwd(), &["init", "-q"]);
    git(
        fixture.cwd(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widgets.git",
        ],
    );

    assert!(
        advisories(&fixture).is_empty(),
        "a matching remote has nothing to flag"
    );
}

/// SH-189: a checkout whose `origin` disagrees with the configured
/// repository is the case that matters most — a restore into a fork or a
/// relocated clone, where the next sync would otherwise silently push
/// somewhere the operator did not intend.
#[cfg(feature = "github-sync")]
#[test]
fn a_mismatched_github_remote_is_flagged_by_name() {
    use storyhook::github::sync_state::{GithubRepo, GithubSyncConfig, SyncMode, SyncSettings};

    let fixture = ServiceFixture::new();
    fixture
        .store()
        .write(|tx| {
            let mut row = tx.settings(fixture.project())?;
            row.github_sync = Some(
                serde_json::to_value(GithubSyncConfig {
                    github: GithubRepo {
                        owner: "acme".into(),
                        repo: "widgets".into(),
                    },
                    sync: SyncSettings {
                        mode: SyncMode::Manual,
                        last_sync_at: None,
                        last_full_sync_at: None,
                    },
                    etags: Default::default(),
                    mappings: Vec::new(),
                })
                .expect("serializing"),
            );
            tx.put_settings(fixture.project(), &row)
        })
        .expect("configuring github-sync");
    git(fixture.cwd(), &["init", "-q"]);
    git(
        fixture.cwd(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/a-fork-owner/widgets.git",
        ],
    );

    let advice = advisories(&fixture);
    assert_eq!(advice.len(), 1, "{advice:?}");
    assert!(
        advice[0].contains("acme/widgets") && advice[0].contains("a-fork-owner/widgets"),
        "the notice must name both the configured and the detected repository: {advice:?}"
    );

    // Advisory, not a finding: a mismatch is not an integrity fault.
    assert!(report(&fixture).is_empty());
}

/// SH-189: restoring into a fresh directory before `git remote add origin`
/// runs is ordinary, not an error — but doctor should say the configured
/// repository has not been verified yet, rather than staying silent.
#[cfg(feature = "github-sync")]
#[test]
fn a_checkout_with_no_origin_yet_is_reported_as_unverified() {
    use storyhook::github::sync_state::{GithubRepo, GithubSyncConfig, SyncMode, SyncSettings};

    let fixture = ServiceFixture::new();
    fixture
        .store()
        .write(|tx| {
            let mut row = tx.settings(fixture.project())?;
            row.github_sync = Some(
                serde_json::to_value(GithubSyncConfig {
                    github: GithubRepo {
                        owner: "acme".into(),
                        repo: "widgets".into(),
                    },
                    sync: SyncSettings {
                        mode: SyncMode::Manual,
                        last_sync_at: None,
                        last_full_sync_at: None,
                    },
                    etags: Default::default(),
                    mappings: Vec::new(),
                })
                .expect("serializing"),
            );
            tx.put_settings(fixture.project(), &row)
        })
        .expect("configuring github-sync");
    // No `git init` at all in `fixture.cwd()`.

    let advice = advisories(&fixture);
    assert_eq!(advice.len(), 1, "{advice:?}");
    assert!(
        advice[0].contains("acme/widgets") && advice[0].contains("not been verified"),
        "{advice:?}"
    );
    assert!(report(&fixture).is_empty());
}

/// Runs `git <args>` in `cwd`, asserting success — a plain subprocess call is
/// enough here since `ServiceFixture::cwd` is an isolated scratch directory,
/// unlike `TestEnv`'s heavier CLI-subprocess fixtures.
#[cfg(feature = "github-sync")]
fn git(cwd: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(args)
        .output()
        .expect("running git");
    assert!(
        output.status.success(),
        "`git {}` in {} failed: {}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// SH-266 on the `--fix` path: what the run *did* is withheld because the run
/// also failed.
///
/// Two independent damages, because that is the only shape in which the
/// receipt can go missing: a required state deleted out of the catalog, which
/// `--fix` genuinely repairs, and an unaddressable type slug, which it cannot.
/// The states are in the catalog afterwards either way — an operator who is
/// not told that reads the failure as "nothing happened" and repeats it.
#[test]
fn a_failed_fix_still_reports_the_states_it_added() {
    let fixture = ServiceFixture::new();
    let connection = Connection::open(fixture.store().path()).expect("opening the store");
    connection
        .execute("DELETE FROM project_states WHERE slug = 'blocked'", [])
        .expect("dropping a required state");
    connection
        .execute(
            "UPDATE project_types SET slug = 'in review' \
             WHERE rowid = (SELECT MIN(rowid) FROM project_types)",
            [],
        )
        .expect("making a type unaddressable");

    let error = fix(&fixture).expect_err("an unaddressable type slug survives the repair");
    let rendered = error.to_string();
    assert!(
        rendered.contains("cannot be addressed"),
        "the finding that failed the run: {rendered}"
    );
    assert!(
        rendered.contains("added 1 required state"),
        "the repair `--fix` DID make must be reported even though the run failed: {rendered}"
    );
}

/// The sharper half of the same defect, and the one that is *always* withheld.
///
/// A story the read-model repair could not rewrite is also a `FoldFailure`
/// finding, so `--fix` fails whenever there is one — which makes the failure
/// path the only place "could not be repaired" can ever be read, and it was
/// the one path that dropped it.
#[test]
fn a_failed_fix_still_names_the_story_it_could_not_repair() {
    let mut states = storyhook_test_support::default_states();
    states.push(StateDef {
        slug: "shelved".into(),
        super_state: SuperState::Closed,
        role: None,
        description: None,
    });
    let fixture = ServiceFixture::with_states(&states);
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "shelved, then orphaned by its own catalog");
    StoryService::new(&ctx)
        .set_state(&id, "shelved", None, None, None)
        .expect("shelving");
    drop(ctx);

    // Through the corruption back door: SH-130's composite foreign key refuses
    // this through every ordinary path, and a database written by an older
    // storyhook can still hold it.
    storyhook::store::test_support::forget_state(fixture.store(), fixture.project(), "shelved")
        .expect("retiring a state out from under its story");

    let error = fix(&fixture).expect_err("a story that cannot be folded is a finding");
    let rendered = error.to_string();
    assert!(
        rendered.contains("cannot be folded"),
        "the finding that failed the run: {rendered}"
    );
    assert!(
        rendered.contains("could not be repaired"),
        "the repair `--fix` declined to guess at must be named, not dropped: {rendered}"
    );

    // The catalog goes back so the fixture's drop-time drift check has
    // something to fold against; this test's damage is not its to report.
    fixture
        .store()
        .write(|tx| tx.put_states(fixture.project(), &states))
        .expect("restoring the state this test retired");
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
        .set_state(&id, "done", None, None, None)
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

// ---------------------------------------------------------------------------
// The structured half (SH-244)
// ---------------------------------------------------------------------------
//
// Everything above asserts on the report a person reads. These assert that the
// same report is readable by a machine without a regex — which is the whole of
// SH-244, and which SH-243 paid for in a hand-rolled parser over 1.68MB.

/// **The story's acceptance criterion.** SH-243 needed four values out of a
/// divergence line and had to regex them out of one flattened string; they
/// were structured in `ReadModelDiff` the whole time and thrown away a line
/// later. This reads all four off the finding.
#[test]
fn a_divergence_carries_the_four_values_sh243_had_to_regex_out() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "drifted");
    drop(ctx);

    let connection = Connection::open(fixture.store().path()).expect("opening the store");
    connection
        .execute("UPDATE stories SET title = 'wrong'", [])
        .expect("damaging the read model");

    let divergence = findings(&fixture)
        .into_iter()
        .find(|finding| finding.code == FindingCode::ReadModelDivergence)
        .expect("a damaged read-model row is a divergence");

    assert_eq!(
        divergence.subject.as_deref(),
        Some("SH-1"),
        "`subject` carries the rendered id every other surface speaks, even though \
         the sentence itself still says `story 1:`"
    );
    match divergence.data {
        Some(FindingData::Divergence {
            field,
            persisted,
            rebuilt,
        }) => {
            assert_eq!(field, "title");
            assert_eq!(persisted, "wrong");
            assert_eq!(rebuilt, "drifted");
        }
        other => panic!("a divergence must carry its three values: {other:?}"),
    }

    // `ServiceFixture` asserts the read model matches its events when it
    // drops, and this test damaged it on purpose.
    fix(&fixture).expect("repairing the damage this test did");
}

/// SH-225, as data rather than as prose an operator has to read carefully: a
/// missing inverse is reported against the end that *has* its half, while the
/// repair belongs on the end that *lacks* it. Eight closed stories sat behind
/// that distinction for a week because nothing said which was which.
#[test]
fn a_missing_inverse_names_a_remedy_its_subject_does_not() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "one");
    let b = new_story(&ctx, "two");
    drop(ctx);

    // One end claims the edge and the other's history never hears of it.
    // Appended rather than deleted from a relate(): events are append-only,
    // and a trigger refuses the DELETE.
    append_to_one_end(
        &fixture,
        &a,
        &[StoryEvent::StoryRelationshipAdded {
            at: FIXTURE_NOW.to_string(),
            other_id: b.clone(),
            relation: "blocks".to_string(),
        }],
    );

    let finding = findings(&fixture)
        .into_iter()
        .find(|finding| finding.code == FindingCode::MissingInverseRelation)
        .expect("a one-sided edge is a missing inverse");

    assert_eq!(
        finding.subject.as_deref(),
        Some(a.as_str()),
        "reported against the end that already has its half"
    );
    assert_eq!(
        finding.remedy.as_deref(),
        Some(b.as_str()),
        "but the repair is written to the end that lacks it — SH-225"
    );
    assert_ne!(
        finding.subject, finding.remedy,
        "if these were ever equal this test would be proving nothing"
    );

    // `ServiceFixture` asserts both halves of every edge when it drops.
    fix(&fixture).expect("repairing the one-sided edge this test made");
}

/// The rendered report is exactly the findings' own sentences, joined — an
/// equality, not a containment. This is what makes the structured and prose
/// forms one value rather than two that can drift apart.
#[test]
fn the_rendered_report_is_exactly_its_findings_joined() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "one");
    let b = new_story(&ctx, "two");
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

    let found = findings(&fixture);
    assert!(!found.is_empty(), "the fixture must actually be damaged");

    let detail = storyhook::error::IntegrityDetail::report(found.clone(), Vec::new())
        .expect("a finding is a report");
    assert_eq!(
        detail.to_string(),
        found
            .iter()
            .map(|finding| finding.message.clone())
            .collect::<Vec<_>>()
            .join("\n"),
    );

    fix(&fixture).expect("repairing the one-sided edge this test made");
}

/// Every code this build can emit is reachable from a fixture.
///
/// Derived over the enum rather than a hand-maintained list, so a check added
/// later without a provocation fails here instead of shipping unexercised —
/// the same shape as the scans in `tests/store_isolation.rs`.
#[test]
fn every_finding_code_is_provoked_by_some_fixture() {
    let mut seen: BTreeSet<FindingCode> = BTreeSet::new();
    for provoke in code_provocations() {
        for finding in provoke() {
            seen.insert(finding.code);
        }
    }

    let excused: BTreeSet<FindingCode> = excused().into_iter().map(|(code, _)| code).collect();
    let unexercised: Vec<FindingCode> = every_finding_code()
        .into_iter()
        .filter(|code| !seen.contains(code) && !excused.contains(code))
        .collect();
    assert!(
        unexercised.is_empty(),
        "a finding code no fixture provokes is a check nothing tests — provoke it above, \
         or excuse it with a reason: {unexercised:?}"
    );

    // And the excuses stay honest: one that a fixture now happens to provoke
    // is a stale excuse, which is how an exclusion list rots into a lie.
    let stale: Vec<FindingCode> = excused
        .iter()
        .copied()
        .filter(|code| seen.contains(code))
        .collect();
    assert!(
        stale.is_empty(),
        "these are provoked after all — delete their excuses: {stale:?}"
    );
}

/// Every [`FindingCode`] this build defines.
///
/// An exhaustive `match` rather than a list, so a code added later stops this
/// file compiling until somebody decides how it is exercised. The same guard
/// `tests/error_contract.rs` uses over `AppError`.
fn every_finding_code() -> Vec<FindingCode> {
    let named = |code: FindingCode| -> FindingCode {
        match code {
            FindingCode::RequiredStates
            | FindingCode::UnaddressableType
            | FindingCode::MultipleParents
            | FindingCode::DanglingRelation
            | FindingCode::MissingInverseRelation
            | FindingCode::MissingReciprocalRelation
            | FindingCode::ParentChildCycle
            | FindingCode::UnknownType
            | FindingCode::MalformedLabels
            | FindingCode::MissingRow
            | FindingCode::ExtraRow
            | FindingCode::FoldFailure
            | FindingCode::ReadModelDivergence
            | FindingCode::UndecodableEvent
            | FindingCode::Unstructured => code,
        }
    };
    [
        FindingCode::RequiredStates,
        FindingCode::UnaddressableType,
        FindingCode::MultipleParents,
        FindingCode::DanglingRelation,
        FindingCode::MissingInverseRelation,
        FindingCode::MissingReciprocalRelation,
        FindingCode::ParentChildCycle,
        FindingCode::UnknownType,
        FindingCode::MalformedLabels,
        FindingCode::MissingRow,
        FindingCode::ExtraRow,
        FindingCode::FoldFailure,
        FindingCode::ReadModelDivergence,
        FindingCode::UndecodableEvent,
        FindingCode::Unstructured,
    ]
    .into_iter()
    .map(named)
    .collect()
}

/// How each finding code is reached.
///
/// The graph-shaped checks are provoked directly against
/// `compute_integrity_issues`, which is a pure function of the story map — a
/// store fixture would add minutes and prove nothing extra. The rest go
/// through a real damaged store.
fn code_provocations() -> Vec<Box<dyn Fn() -> Vec<Finding>>> {
    /// A project with one story, then whatever damage the caller does to the
    /// database behind storyhook's back.
    fn damaged(sql: &'static str) -> Vec<Finding> {
        let fixture = ServiceFixture::new();
        let ctx = fixture.ctx();
        new_story(&ctx, "a story");
        drop(ctx);
        Connection::open(fixture.store().path())
            .expect("opening the store")
            .execute(sql, [])
            .expect("damaging the store");
        let found = findings(&fixture);
        // Repaired where possible, because the fixture checks the read model
        // and every edge when it drops. A fault `--fix` cannot repair — an
        // unaddressable type slug — is neither of those and drops clean.
        let _ = fix(&fixture);
        found
    }

    vec![
        Box::new(|| {
            let fixture = ServiceFixture::new();
            let ctx = fixture.ctx();
            new_story(&ctx, "a story");
            drop(ctx);
            Connection::open(fixture.store().path())
                .expect("opening the store")
                .execute("UPDATE stories SET title = 'wrong'", [])
                .expect("damaging the read model");
            let found = findings(&fixture);
            // Repaired before the fixture's own drop-time drift check.
            fix(&fixture).expect("repairing");
            found
        }),
        Box::new(|| {
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
                .expect("shrinking the catalog under the story");
            findings(&fixture)
        }),
        Box::new(|| damaged("DELETE FROM project_states WHERE slug = 'blocked'")),
        Box::new(|| {
            damaged(
                "UPDATE project_types SET slug = 'in review' \
             WHERE rowid = (SELECT MIN(rowid) FROM project_types)",
            )
        }),
        Box::new(|| {
            let fixture = ServiceFixture::new();
            let ctx = fixture.ctx();
            let id = new_story(&ctx, "labelled");
            drop(ctx);
            append_to_one_end(
                &fixture,
                &id,
                &[StoryEvent::StoryLabelsSet {
                    at: FIXTURE_NOW.to_string(),
                    labels: vec!["web,sse".to_string()],
                }],
            );
            let found = findings(&fixture);
            let _ = fix(&fixture);
            found
        }),
        Box::new(|| {
            let fixture = ServiceFixture::new();
            let ctx = fixture.ctx();
            let a = new_story(&ctx, "one");
            let b = new_story(&ctx, "two");
            drop(ctx);
            append_to_one_end(
                &fixture,
                &a,
                &[StoryEvent::StoryRelationshipAdded {
                    at: FIXTURE_NOW.to_string(),
                    other_id: b,
                    relation: "blocks".to_string(),
                }],
            );
            let found = findings(&fixture);
            let _ = fix(&fixture);
            found
        }),
    ]
}

/// Codes no fixture in this file provokes, each with the reason.
///
/// Deliberately small and deliberately explicit: an entry here is a decision,
/// not a gap that went unnoticed. The compiler forces a new code into
/// [`every_finding_code`]; this list forces somebody to say why it is not
/// exercised, exactly as `tests/error_contract.rs`'s `UNPROVOKABLE` does.
fn excused() -> Vec<(FindingCode, &'static str)> {
    vec![
        (
            FindingCode::MultipleParents,
            "the store refuses it: `append_events` raises `a story may have at most one \
             parent` at the fold, so no supported path can build one. The check survives \
             for data written before that invariant existed — see \
             `the_shapes_doctor_used_to_find_are_now_refused_by_the_schema`",
        ),
        (
            FindingCode::DanglingRelation,
            "a relation pointing at a story that never existed. `relate` refuses to \
             create one, so provoking it needs a hand-injected event — the shape \
             tests/doctor.rs owns, where `--fix` retracting it is the point",
        ),
        (
            FindingCode::MissingReciprocalRelation,
            "the mutual spelling of MissingInverseRelation, which IS provoked above; \
             both come from the same branch of `compute_integrity_issues` and differ \
             only in wording",
        ),
        (
            FindingCode::ParentChildCycle,
            "the write path refuses to create a cycle, so this needs injected events; \
             tests/relations.rs owns cycle construction",
        ),
        (
            FindingCode::MissingRow,
            "a story with events and no read-model row — store damage, owned by \
             tests/corruption_recovery.rs",
        ),
        (
            FindingCode::ExtraRow,
            "a row with no events behind it, same owner as MissingRow",
        ),
        (
            FindingCode::FoldFailure,
            "an unfoldable history, same owner as MissingRow",
        ),
        (
            FindingCode::UndecodableEvent,
            "a torn payload of a kind this build knows — the encoder cannot produce one, \
             so it needs a hand-written row",
        ),
        (
            FindingCode::Unstructured,
            "not a doctor finding at all: it is what `IntegrityDetail: From<String>` mints \
             so every raise site carries one. Pinned in src/error.rs's own unit tests",
        ),
    ]
}
