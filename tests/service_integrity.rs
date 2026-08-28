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
    AttachmentService, Ctx, Examination, IntegrityService, NewStoryInput, QueryService,
    RelationService, StoryService,
};
use storyhook::store::{
    ExpectedSeq, ReadOps, SqliteStore, Store, StoreError, StoryNo, WriteOps, folds, partition_known,
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

/// The edges a story's snapshot claims, as `(relation, other)` pairs.
fn relations_of(fixture: &ServiceFixture, id: &str) -> Vec<(String, String)> {
    let no = StoryNo::parse_id("SH", id).expect("a well-formed id");
    fixture
        .store()
        .read(|tx| tx.story(fixture.project(), no))
        .expect("reading the story")
        .expect("the story exists")
        .snapshot
        .relationships
        .into_iter()
        .map(|relation| (relation.relation, relation.other_id))
        .collect()
}

/// The state a story's row currently reports.
fn story_state(fixture: &ServiceFixture, id: &str) -> String {
    let no = StoryNo::parse_id("SH", id).expect("a well-formed id");
    fixture
        .store()
        .read(|tx| tx.story(fixture.project(), no))
        .expect("reading the story")
        .expect("the story exists")
        .snapshot
        .state
}

/// A story's decodable events, straight from its log.
///
/// The assertion surface for a test whose question is what a repair *appended*
/// (SH-285). A rendered relation is a fold of these, and the fold is exactly
/// what is in doubt when a row is missing — so a test about an endpoint the
/// read model cannot answer for has to read the log rather than the cache.
fn events_of(fixture: &ServiceFixture, story: i64) -> Vec<StoryEvent> {
    let no = StoryNo::new(story);
    fixture
        .store()
        .read(|tx| {
            let stored = tx.events_for(fixture.project(), no)?;
            let (known, _unknown) = partition_known(no, &stored);
            Ok(known)
        })
        .expect("reading a log")
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
    examine(fixture).findings
}

/// A whole doctor read pass: the findings, and the notices that are not
/// findings. One call because it is one fold of the project (SH-267).
fn examine(fixture: &ServiceFixture) -> Examination {
    IntegrityService::new(&fixture.ctx())
        .examine()
        .expect("examining")
}

/// A `--fix` run's rendered report, or the error its remaining findings make.
///
/// The two production calls composed — `repair()` for what the run did,
/// `verdict()` for what that counts as — rather than one call that conflated
/// them. The `?` is the distinction SH-270 drew: a repair that *blew up* now
/// propagates, where it used to be indistinguishable from a repair that ran and
/// left findings.
fn fix(fixture: &ServiceFixture) -> Result<String, AppError> {
    IntegrityService::new(&fixture.ctx()).repair()?.verdict()
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

// --- what the doctor costs -------------------------------------------------
//
// A fold is every event in the project, re-folded into every story. It is also
// invisible: it produces no output of its own, so a doctor that performed four
// of them answered exactly like one that performed a single fold — which is
// how `doctor` came to fold the project twice and `--fix` four times without
// anything saying so (SH-267). These count them, through the `test-seam`
// counter inside the fold itself, because a caller can only see results.

/// A doctor read folds the project exactly once.
#[test]
fn a_doctor_read_folds_the_project_once() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "A");
    new_story(&ctx, "B");
    drop(ctx);

    let (examination, counted) = folds::counting(|| examine(&fixture));

    assert_eq!(counted, 1, "one doctor read, one fold");
    assert!(examination.findings.is_empty());
    assert!(examination.notices.is_empty());
}

/// A `--fix` folds it exactly twice, and neither is spare: once to rebuild the
/// read model from the events, once to see what the repair left behind.
///
/// The second is genuinely a different question from the first — it is asked of
/// a project the run has since written to — which is what distinguishes it from
/// the two this story removed.
#[test]
fn a_doctor_fix_folds_the_project_twice() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "A");
    new_story(&ctx, "B");
    drop(ctx);

    let (message, counted) = folds::counting(|| fix(&fixture).expect("fixing"));

    assert_eq!(counted, 2, "one fold to repair, one to re-examine");
    assert_eq!(message, "doctor found nothing to fix");
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
/// the command could never clear. `relates-to` is intentional here: SH-500
/// durably retracts a closed story's `blocks` edges, so that relation can no
/// longer be a stable closed-source fixture for this generic repair contract.
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
            relation: "relates-to".to_string(),
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
/// not exist (foreign key); a read-model row with no events needs the events
/// deleted (append-only trigger). Each is refused by the schema rather than
/// detected by the doctor afterwards — the defect class is gone, not the
/// coverage. Multiple parents are intentionally representable as of SH-446.
///
/// [`compute_integrity_issues`]: storyhook::domain::compute_integrity_issues
#[test]
fn the_shapes_doctor_used_to_find_are_now_refused_by_the_schema() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let child = new_story(&ctx, "child");
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
        ["SH-1: title is `not what the events say` but the events say `the real title`"],
    );

    assert_eq!(
        fix(&fixture).expect("fixing"),
        "doctor repaired supported integrity issues"
    );
    assert!(report(&fixture).is_empty());
}

/// The `UnexaminedStory` sentence a report carries for `id` (SH-286).
///
/// Written once here rather than at each of the assertion sites below, because
/// what those assertions are about is *which* stories the report declares
/// unexaminable — the wording is `story_issues`' to own, and six copies of it
/// in this file would make rephrasing it a six-file edit for no gain.
fn unexamined(id: &str) -> String {
    format!(
        "{id}: not examined — the events do not vouch for its read-model row, so its own label, \
         type, parent and cycle checks were skipped, and no edge naming it was called dangling or \
         one-sided. The finding naming the damage itself is elsewhere in this report"
    )
}

/// Deletes a story's read-model row while leaving its history — and every edge
/// naming it — in place. See
/// [`storyhook::store::test_support::forget_read_model_row`] for why
/// `forget_story` cannot stand in for this.
fn forget_row(fixture: &ServiceFixture, story: i64) {
    storyhook::store::test_support::forget_read_model_row(
        fixture.store(),
        fixture.project(),
        StoryNo::new(story),
    )
    .expect("forgetting a read-model row");
}

/// SH-271, the half that misdescribes the run: restoring a row a story's own
/// history supports **is** a repair, and `touched` ingested only the diff's
/// `divergences`. A run whose whole repair was a restored row therefore
/// announced that it had found nothing to fix, which is SH-266's defect class
/// verbatim — output that denies what its own run did.
#[test]
fn restoring_a_lost_row_is_a_repair_the_run_admits_to() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "A");
    drop(ctx);

    forget_row(&fixture, 1);
    assert_eq!(
        report(&fixture),
        [
            // SH-286: the row is gone, so nothing this report says about SH-1
            // was read from it.
            unexamined("SH-1"),
            "SH-1: has events but no read-model row".to_string(),
        ]
    );

    assert_eq!(
        fix(&fixture).expect("a row its events support is restorable"),
        "doctor repaired supported integrity issues",
        "the run restored a row and said it had found nothing to fix"
    );
    assert!(report(&fixture).is_empty(), "{:?}", report(&fixture));
}

/// SH-271, the half that is dangerous: `blocked` is computed *before* the
/// read-model repair and rendered *after* it, so a run can advise undoing an
/// edge it has itself just made whole.
///
/// A missing row makes a *valid* edge read as dangling — `all_stories` resolves
/// ids through the read model — and when the claiming story is closed, that
/// becomes a blocked repair: "reopen SH-1 and retract its dangling relation".
/// `repair_read_model` then restores SH-2's row from its events and the edge is
/// correct again. Following the advice at that point deletes good data, from a
/// story the operator has to reopen to do it.
///
/// The assertion is on the whole message rather than on a substring: advice is
/// spliced in after the headline, so an equality holds only if nothing was
/// spliced.
#[test]
fn a_repair_the_run_itself_dissolved_is_not_still_advised() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "A");
    let b = new_story(&ctx, "B");
    RelationService::new(&ctx)
        .relate(&a, "obviates", &b, false)
        .expect("relating");
    StoryService::new(&ctx)
        .set_state(&a, "done", None, None, None)
        .expect("closing A");
    drop(ctx);

    forget_row(&fixture, 2);
    assert_eq!(
        report(&fixture),
        [
            // Since SH-286 the report withholds SH-1's edge rather than calling
            // it dangling — SH-2 is missing from the *read model*, not from the
            // project — and says so in SH-2's place.
            unexamined("SH-2"),
            "SH-2: has events but no read-model row".to_string(),
        ],
        "the damage the fixture makes, as the pre-repair report sees it"
    );

    assert_eq!(
        fix(&fixture).expect("the row is restorable"),
        "doctor repaired supported integrity issues",
        "the run advised retracting an edge its own repair had just made whole"
    );
    assert!(report(&fixture).is_empty(), "{:?}", report(&fixture));
    assert_eq!(
        relations_of(&fixture, &a),
        [("obviates".to_string(), b.clone())],
        "the edge the advice would have retracted"
    );
    assert_eq!(
        relations_of(&fixture, &b),
        [("obviated-by".to_string(), a.clone())]
    );
}

/// SH-285: the same hazard on the path SH-271 did not close. An **open**
/// claimant is not advised about, it is *written to* — so a missing row on the
/// far end does not produce a blocked repair to reconcile, it produces a
/// `StoryRelationshipRemoved` event that destroys a correct edge.
///
/// `repair_read_model` then restores SH-2's row from its own events, SH-2 still
/// claims its half, and the run's own closing report names an asymmetry the run
/// itself created — from a store that, before the repair, held nothing worse
/// than a rebuildable cache miss.
#[test]
fn a_fix_does_not_retract_an_open_storys_edge_to_a_story_whose_row_is_missing() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "A");
    let b = new_story(&ctx, "B");
    RelationService::new(&ctx)
        .relate(&a, "blocks", &b, false)
        .expect("relating");
    drop(ctx);

    forget_row(&fixture, 2);
    assert_eq!(
        report(&fixture),
        [
            // As above (SH-286): a valid edge is not evidence of damage just
            // because the read model cannot show its far end.
            unexamined("SH-2"),
            "SH-2: has events but no read-model row".to_string(),
        ],
        "the damage the fixture makes, as the pre-repair report sees it"
    );

    assert_eq!(
        fix(&fixture).expect("a row its events support is restorable"),
        "doctor repaired supported integrity issues"
    );
    assert!(
        report(&fixture).is_empty(),
        "the run reported damage it created itself: {:?}",
        report(&fixture)
    );
    assert_eq!(
        relations_of(&fixture, &a),
        [("blocks".to_string(), b.clone())],
        "the repair retracted a valid edge"
    );
    assert_eq!(
        relations_of(&fixture, &b),
        [("blocked-by".to_string(), a.clone())]
    );
}

/// SH-285's **second** mechanism, isolated: an endpoint whose row is missing
/// *and unrestorable*, because its own events will not fold.
///
/// Re-folding the read model first cannot help here —
/// [`storyhook::store::repair_read_model`] leaves an unfoldable story's row
/// exactly as it found it, on purpose, because overwriting it with a guess
/// would destroy the evidence. So SH-2 is absent from `all_stories` however
/// early the re-fold runs, and only resolving existence from the *events*
/// keeps SH-1's valid edge.
///
/// The unfoldable shape is a story sitting in a state its catalog no longer
/// defines, which is the same back door
/// [`a_failed_fix_still_names_the_story_it_could_not_repair`] uses. A torn
/// event payload will not do: the fold skips what it cannot decode and
/// succeeds, so the row comes back and the fixture proves nothing. `shelved`
/// rather than a required state, so the catalog repair does not put it back.
///
/// The run legitimately **fails** — a torn payload is a finding no repair can
/// clear — so the assertion is on SH-1's event log, never on the verdict. The
/// log rather than the rendered relations because the row is the thing in
/// doubt: what matters is that nothing was appended.
///
/// It used to characterize SH-286 as-is: the *report* resolved endpoint
/// existence through the read model, so it called SH-1's edge dangling in the
/// same breath that `--fix` declined to retract it, and the assertion said so
/// and instructed its own deletion. SH-286 has landed, and the two assertions
/// that replaced it are its inverse — the report withholds that finding now, and
/// says in SH-2's place that it could not examine it.
#[test]
fn a_fix_does_not_retract_an_edge_to_a_story_whose_events_will_not_fold() {
    let mut states = storyhook_test_support::default_states();
    states.push(StateDef {
        slug: "shelved".into(),
        super_state: SuperState::Closed,
        role: None,
        description: None,
    });
    let fixture = ServiceFixture::with_states(&states);
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "A");
    let b = new_story(&ctx, "B");
    RelationService::new(&ctx)
        .relate(&a, "blocks", &b, false)
        .expect("relating");
    StoryService::new(&ctx)
        .set_state(&b, "shelved", None, None, None)
        .expect("shelving B");
    drop(ctx);

    storyhook::store::test_support::forget_state(fixture.store(), fixture.project(), "shelved")
        .expect("retiring a state out from under its story");
    forget_row(&fixture, 2);

    let before = events_of(&fixture, 1);
    fix(&fixture).expect_err("a story that cannot be folded is a finding no repair clears");

    assert_eq!(
        events_of(&fixture, 1),
        before,
        "the repair wrote to SH-1's history over an endpoint it could not read"
    );
    assert_eq!(
        relations_of(&fixture, &a),
        [("blocks".to_string(), b.clone())],
        "the repair retracted a valid edge"
    );

    let issues = report(&fixture);
    assert!(
        issues
            .iter()
            .any(|issue| issue.starts_with("SH-2: ") && issue.contains("cannot be folded")),
        "the fold failure is the finding the operator has to act on: {issues:?}"
    );
    assert!(
        !issues
            .iter()
            .any(|issue| issue.contains("dangling relation")),
        "SH-286: the report resolves existence from the events now, so it must not name an edge \
         `--fix` rightly refuses to retract: {issues:?}"
    );
    assert!(
        issues.contains(&unexamined("SH-2")),
        "SH-286: and it must say why it went quiet — a suppression with no disclosure beside it \
         is the SH-268 shape: {issues:?}"
    );

    // The catalog goes back so the fixture's drop-time drift check has
    // something to fold against, and then the row SH-2 lost — foldable again
    // now the state exists — goes back with it. This test's damage is not its
    // to report.
    fixture
        .store()
        .write(|tx| tx.put_states(fixture.project(), &states))
        .expect("restoring the state this test retired");
    storyhook::store::repair_read_model(fixture.store(), fixture.project())
        .expect("restoring the row the retired state made unrestorable");
}

/// SH-285's **first** mechanism, isolated: a repair the pass can only make if
/// it is reading a row the events support.
///
/// SH-1's labels are malformed (SH-164) and its row is then forgotten. With the
/// re-fold *after* the story pass, the pass sees no row for SH-1 at all, makes
/// no repair, and the restored row's malformed labels come back as a finding —
/// so the run fails and the operator has to run `--fix` a second time. With the
/// re-fold first, one run is enough.
///
/// The existence probe cannot green this: SH-1 is the story being repaired, not
/// an endpoint being resolved.
#[test]
fn one_fix_run_repairs_a_label_on_a_story_whose_row_it_had_to_restore() {
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
    forget_row(&fixture, 1);

    assert_eq!(
        fix(&fixture).expect("one run repairs a row and the labels that row carries"),
        "doctor repaired supported integrity issues"
    );
    assert_eq!(labels_of(&fixture, &a), ["sse", "web"]);
    assert!(report(&fixture).is_empty(), "{:?}", report(&fixture));
}

/// The anti-overfit pin, and the reason SH-285 is not "stop retracting".
///
/// An edge whose far end has **no row and no events** names a story that really
/// is not there, and retracting it is the repair. Without this test the
/// cheapest way to pass the two above is to stop retracting anything at all,
/// which would trade a data-loss defect for a silent-failure one — a `--fix`
/// that reports a dangling relation it will never clear, run after run.
#[test]
fn a_fix_still_retracts_an_edge_to_a_story_that_is_genuinely_gone() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "A");
    let b = new_story(&ctx, "B");
    RelationService::new(&ctx)
        .relate(&a, "blocks", &b, false)
        .expect("relating");
    drop(ctx);

    // Both halves, and in this order: the events first, because forgetting the
    // row is what makes the story invisible and forgetting the log is what
    // makes it *absent*. Either alone is a story the fix must not touch.
    storyhook::store::test_support::forget_events(
        fixture.store(),
        fixture.project(),
        StoryNo::new(2),
    )
    .expect("forgetting a history");
    forget_row(&fixture, 2);

    assert_eq!(
        report(&fixture),
        ["SH-1: dangling relation `blocks` to missing story `SH-2`"],
        "the damage the fixture makes, as the pre-repair report sees it"
    );

    assert_eq!(
        fix(&fixture).expect("retracting a genuinely dangling edge is a repair"),
        "doctor repaired supported integrity issues"
    );
    assert!(
        relations_of(&fixture, &a).is_empty(),
        "the edge to a story with neither a row nor a history was left standing: {:?}",
        relations_of(&fixture, &a)
    );
    assert!(report(&fixture).is_empty(), "{:?}", report(&fixture));
}

// --- what the doctor may assert about a story the events do not corroborate --
//
// SH-286. The doctor's story-level checks read the `stories` table, which is a
// cache of a fold of the events; where that cache cannot be believed, the
// checks are skipped and an `UnexaminedStory` finding is minted in their place.
// Each test below picks one of the four ways a row loses its standing, damages
// the project *and* leaves a second, ordinary fault on the same story, and
// asserts both halves of the swap: the ordinary finding is withheld, and the
// story is still named.
//
// The narrow reading of this story — "a story with events and no row" — passes
// only the first of them. The other three have a row.

/// A story whose events will not fold, **whose stale row survives**.
///
/// The case that decides how wide the set is drawn.
/// [`storyhook::store::diff_rebuilt`] files a story under `fold_failures` and
/// moves on *before* it looks for a row, so a row written when the story last
/// folded is still sitting there — and a doctor that resolved "can I believe
/// this?" as "is there a row?" would examine it, computing findings from a
/// snapshot the same run has just failed to reproduce and offering `--fix` a
/// repair built out of them.
///
/// The ordinary fault is a malformed label (SH-164), which is repairable, so a
/// doctor that still saw it would not merely report it — it would append.
#[test]
fn a_story_whose_events_will_not_fold_is_not_examined_through_its_stale_row() {
    let mut states = storyhook_test_support::default_states();
    states.push(StateDef {
        slug: "shelved".into(),
        super_state: SuperState::Closed,
        role: None,
        description: None,
    });
    let fixture = ServiceFixture::with_states(&states);
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
    let ctx = fixture.ctx();
    StoryService::new(&ctx)
        .set_state(&a, "shelved", None, None, None)
        .expect("shelving A");
    drop(ctx);
    assert_eq!(
        labels_of(&fixture, &a),
        ["web,sse"],
        "the fixture needs a row carrying a repairable fault"
    );

    storyhook::store::test_support::forget_state(fixture.store(), fixture.project(), "shelved")
        .expect("retiring a state out from under its story");

    let issues = report(&fixture);
    assert!(
        !issues
            .iter()
            .any(|issue| issue.contains("malformed labels")),
        "a finding was computed from a row this run could not reproduce: {issues:?}"
    );
    assert!(
        issues.contains(&unexamined("SH-1")),
        "the suppression has to say so, or the report has silently gone quiet: {issues:?}"
    );
    assert!(
        issues
            .iter()
            .any(|issue| issue.starts_with("SH-1: ") && issue.contains("cannot be folded")),
        "and the damage itself is still named: {issues:?}"
    );

    let before = events_of(&fixture, 1);
    fix(&fixture).expect_err("a story that cannot be folded is a finding no repair clears");
    assert_eq!(
        events_of(&fixture, 1),
        before,
        "`--fix` normalized labels it read from a row the events do not support"
    );

    // The catalog goes back, and with it the row — foldable again — so the
    // fixture's drop-time drift check has something to fold against.
    fixture
        .store()
        .write(|tx| tx.put_states(fixture.project(), &states))
        .expect("restoring the state this test retired");
    storyhook::store::repair_read_model(fixture.store(), fixture.project())
        .expect("restoring the row the retired state made unrestorable");
}

/// A read-model row with no events behind it — the SH-285 defect inverted.
///
/// By the authority that decides what exists, this story does not: nothing in
/// the events describes it. Yet it sits in the story map, where the checks read
/// its labels as facts and `--fix` would answer them by **appending the first
/// event this story has ever had** — fabricating a history for a story the
/// project does not have, which is the same irreversible write SH-285 stopped,
/// arriving from the other direction.
#[test]
fn a_row_with_no_events_behind_it_is_not_examined_either() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "A");
    let b = new_story(&ctx, "B");
    drop(ctx);

    append_to_one_end(
        &fixture,
        &b,
        &[StoryEvent::StoryLabelsSet {
            at: FIXTURE_NOW.to_string(),
            labels: vec!["web,sse".to_string()],
        }],
    );
    storyhook::store::test_support::forget_events(
        fixture.store(),
        fixture.project(),
        StoryNo::new(2),
    )
    .expect("forgetting a history and leaving the row");

    let issues = report(&fixture);
    assert!(
        !issues
            .iter()
            .any(|issue| issue.contains("malformed labels")),
        "a finding was computed from a row nothing in the events supports: {issues:?}"
    );
    assert!(issues.contains(&unexamined("SH-2")), "{issues:?}");
    assert!(
        issues.contains(&"SH-2: read-model row with no events".to_string()),
        "{issues:?}"
    );

    let _ = fix(&fixture);
    assert!(
        events_of(&fixture, 2).is_empty(),
        "`--fix` invented a history for a story the events do not describe: {:?}",
        events_of(&fixture, 2)
    );

    // The orphan row goes, since re-folding cannot remove it and the fixture
    // checks for drift when it drops.
    forget_row(&fixture, 2);
}

/// A row whose embedded snapshot the events contradict.
///
/// The subtlest member of the set, and the one that would be easiest to leave
/// out: `service::query::story_map` builds the story map from the `snapshot`
/// column specifically, so this is not drift in some field the checks never
/// read — it is the exact value every story-level finding is computed from,
/// disagreeing with the events in the same report that says so.
///
/// Only the `snapshot` field earns this. A `title` or `state` column the fold
/// disagrees with is drift the story checks never consult, and treating every
/// divergence as disqualifying would take a badly drifted project's whole
/// report down to `UnexaminedStory` — which is going quiet, the thing this
/// design exists to refuse.
#[test]
fn a_row_whose_snapshot_the_events_contradict_is_not_examined() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "A");
    drop(ctx);

    storyhook::store::test_support::corrupt_snapshot(
        fixture.store(),
        fixture.project(),
        StoryNo::new(1),
        |snapshot| {
            snapshot["labels"] = serde_json::json!(["web,sse"]);
        },
    )
    .expect("corrupting the column the story map is built from");

    let issues = report(&fixture);
    assert!(
        !issues
            .iter()
            .any(|issue| issue.contains("malformed labels")),
        "the doctor reported a label the same run proved the story does not have: {issues:?}"
    );
    assert!(issues.contains(&unexamined("SH-1")), "{issues:?}");
    assert!(
        issues
            .iter()
            .any(|issue| issue.starts_with("SH-1: snapshot is ")),
        "and the divergence itself is still named: {issues:?}"
    );

    assert_eq!(
        fix(&fixture).expect("re-folding is exactly the repair for a divergent row"),
        "doctor repaired supported integrity issues"
    );
    assert!(report(&fixture).is_empty(), "{:?}", report(&fixture));
}

/// A divergence in a column the story checks never read leaves the story
/// examinable — the anti-overfit pin for the test above.
///
/// Without it, the cheapest way to pass that one is to disqualify a story on
/// any divergence at all, which would make a project whose rows have drifted in
/// one ordinary field report `UnexaminedStory` for every story in it and
/// nothing else. That is SH-268's shape: a report that went quiet about real
/// damage.
#[test]
fn a_divergence_in_a_column_the_checks_do_not_read_leaves_the_story_examinable() {
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
    Connection::open(fixture.store().path())
        .expect("opening the store")
        .execute("UPDATE stories SET title = 'not what the events say'", [])
        .expect("damaging one column of the read model");

    let issues = report(&fixture);
    assert!(
        issues
            .iter()
            .any(|issue| issue.contains("malformed labels")),
        "the story is still examinable: its labels come from a snapshot column the events \
         agree with: {issues:?}"
    );
    assert!(
        !issues.iter().any(|issue| issue == &unexamined("SH-1")),
        "a title that drifted is not a reason to stop examining the story: {issues:?}"
    );

    assert_eq!(
        fix(&fixture).expect("both the row and the label are repairable"),
        "doctor repaired supported integrity issues"
    );
    assert!(report(&fixture).is_empty(), "{:?}", report(&fixture));
}

/// The rule reaches `story show` and `story list`, which carry the same checks
/// through `StoryView::flagged_reasons` and have no `MissingRow` finding beside
/// them to explain a false one.
///
/// `story_views` resolves the question more cheaply than the doctor does — one
/// indexed probe per endpoint the map cannot show, none at all on a healthy
/// project — so its answer is a strict *subset* of the doctor's. This pins the
/// direction that matters: whatever it may still flag, it never flags an edge
/// the doctor would withhold.
#[test]
fn story_views_never_flags_an_edge_the_doctor_would_withhold() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "A");
    let b = new_story(&ctx, "B");
    RelationService::new(&ctx)
        .relate(&a, "blocks", &b, false)
        .expect("relating");
    drop(ctx);

    forget_row(&fixture, 2);

    let project = fixture.project();
    let flagged: Vec<String> = fixture
        .store()
        .read(|tx| {
            Ok(QueryService::new(tx, project, FIXTURE_NOW)
                .story_views(false)?
                .into_iter()
                .flat_map(|view| view.flagged_reasons)
                .collect::<Vec<_>>())
        })
        .expect("reading views");
    assert!(
        flagged.is_empty(),
        "`story show {a}` called a valid edge dangling because the far end's row is missing: \
         {flagged:?}"
    );

    // And the anti-overfit half, in the same fixture's shape: an endpoint with
    // neither a row nor a history really is gone, and `story show` still says
    // so.
    storyhook::store::test_support::forget_events(fixture.store(), project, StoryNo::new(2))
        .expect("forgetting a history");
    let flagged: Vec<String> = fixture
        .store()
        .read(|tx| {
            Ok(QueryService::new(tx, project, FIXTURE_NOW)
                .story_views(false)?
                .into_iter()
                .flat_map(|view| view.flagged_reasons)
                .collect::<Vec<_>>())
        })
        .expect("reading views");
    assert_eq!(
        flagged,
        [format!("dangling relation `blocks` to missing story `{b}`")],
        "an edge to a story with neither a row nor a history is genuinely dangling"
    );

    fix(&fixture).expect("retracting a genuinely dangling edge is a repair");
}

/// The disclosure is conservation, not decoration: for every way a row can lose
/// its standing, the report still names the story.
///
/// Asserted over all four at once, in one project, because the property is
/// about the *set* — a report that named three of them and dropped the fourth
/// would pass each of the tests above and still have gone quiet about a story.
#[test]
fn a_report_names_a_story_it_could_not_examine() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    for title in ["A", "B", "C", "D"] {
        new_story(&ctx, title);
    }
    drop(ctx);

    // Four stories, four ways to lose standing. SH-4 is left healthy, so the
    // assertion below cannot pass by naming everything.
    forget_row(&fixture, 1);
    storyhook::store::test_support::forget_events(
        fixture.store(),
        fixture.project(),
        StoryNo::new(2),
    )
    .expect("forgetting a history and leaving the row");
    storyhook::store::test_support::corrupt_snapshot(
        fixture.store(),
        fixture.project(),
        StoryNo::new(3),
        |snapshot| {
            snapshot["description"] = serde_json::json!("not what the events say");
        },
    )
    .expect("corrupting the column the story map is built from");

    let unexamined_subjects: BTreeSet<String> = findings(&fixture)
        .into_iter()
        .filter(|finding| finding.code == FindingCode::UnexaminedStory)
        .filter_map(|finding| finding.subject)
        .collect();
    assert_eq!(
        unexamined_subjects,
        ["SH-1", "SH-2", "SH-3"]
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<_>>(),
        "every story the events do not corroborate is named once, and a healthy one is not"
    );

    forget_row(&fixture, 2);
    storyhook::store::repair_read_model(fixture.store(), fixture.project())
        .expect("restoring the rows this test damaged");
}

// --- report and fix are one contract ---------------------------------------

/// Whether a story's row puts it in an OPEN state — the test-side spelling of
/// the set `--fix` is allowed to append to.
///
/// Production asks `StoryQuery::all().archived(false)`, which also excludes a
/// deleted story; no fixture below has one, so the two agree. A story with no
/// row at all is not open, which is the right answer for one that has been
/// erased.
fn is_open(fixture: &ServiceFixture, id: &str) -> bool {
    let no = StoryNo::parse_id("SH", id).expect("a well-formed id");
    fixture
        .store()
        .read(|tx| tx.story(fixture.project(), no))
        .expect("reading the story")
        .is_some_and(|row| row.snapshot.superstate == SuperState::Open)
}

/// `--fix` appends to exactly the stories its own findings name as remedies,
/// and to no others.
///
/// Both halves of one contract, asserted in both directions over a project
/// damaged four ways at once:
///
/// * **No invention.** Every story the run appended to was named by the
///   `remedy` of a finding the *pre-repair report* produced. A repair with no
///   finding behind it is SH-268 — a filter applied to `report()` and not to
///   `fix()`, so `--fix` repaired what the report called healthy.
/// * **No omission.** Every finding whose remedy is *reachable* — an open
///   story — is gone afterwards. `remedy` is documented as a pointer rather
///   than a promise, and this is the extent of the promise it does make:
///   [`storyhook::domain::finding`] says the one thing that can defeat it is a
///   destination that is closed.
///
/// The expected set is derived from the findings rather than written out, so
/// the assertion cannot be satisfied by a repair loop and a test that drifted
/// together; the literal beside it is what makes a *derivation* that quietly
/// collapsed to nothing still fail.
///
/// Three shapes make the directions bite. The missing inverse is reported
/// against SH-1 and repaired on SH-2, so a run that appended to its findings'
/// *subjects* fails (SH-225). SH-6's unknown type carries no remedy, so a run
/// that treats every finding as repairable fails. SH-7's remedy is itself and
/// it is closed, so a run that ignores reachability fails.
///
/// One documented exception is deliberately absent from the fixture: a
/// `DanglingRelation` whose endpoint has events but no foldable row is
/// reported and rightly *not* repaired (SH-286), which is the one case where a
/// reachable remedy survives a run. It has its own test —
/// [`a_fix_does_not_retract_an_edge_to_a_story_whose_events_will_not_fold`] —
/// and putting it here too would blunt this one into a tautology.
#[test]
fn a_fix_appends_exactly_where_its_findings_name_a_reachable_remedy() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "A");
    let b = new_story(&ctx, "B");
    let c = new_story(&ctx, "C");
    let d = new_story(&ctx, "D");
    let e = new_story(&ctx, "E");
    let f = new_story(&ctx, "F");
    let g = new_story(&ctx, "G");
    StoryService::new(&ctx)
        .set_fields(
            &f,
            &storyhook::service::FieldEdits {
                story_type: Some("bug".into()),
                ..storyhook::service::FieldEdits::default()
            },
        )
        .expect("typing F");
    RelationService::new(&ctx)
        .relate(&d, "blocks", &e, false)
        .expect("relating D to the story that is about to vanish");
    StoryService::new(&ctx)
        .set_state(&g, "done", None, None, None)
        .expect("closing G");
    drop(ctx);

    // A missing inverse: reported against SH-1, repaired on SH-2.
    append_to_one_end(
        &fixture,
        &a,
        &[StoryEvent::StoryRelationshipAdded {
            at: FIXTURE_NOW.to_string(),
            other_id: b.clone(),
            relation: "blocks".to_string(),
        }],
    );
    // Malformed labels on an open story, and on a closed one.
    for story in [&c, &g] {
        append_to_one_end(
            &fixture,
            story,
            &[StoryEvent::StoryLabelsSet {
                at: FIXTURE_NOW.to_string(),
                labels: vec!["web,sse".to_string()],
            }],
        );
    }
    // A genuinely dangling edge: SH-5 loses both its row and its history, so
    // it is absent rather than merely uncached.
    storyhook::store::test_support::forget_events(
        fixture.store(),
        fixture.project(),
        StoryNo::new(5),
    )
    .expect("forgetting a history");
    forget_row(&fixture, 5);
    // And a type the catalog no longer defines, which nothing but a human can
    // repair.
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
        .expect("shrinking the catalog under F");

    let stories: Vec<String> = (1..=7).map(|no| format!("SH-{no}")).collect();
    let before: Vec<usize> = (1..=7).map(|no| events_of(&fixture, no).len()).collect();
    let reachable: BTreeSet<String> = findings(&fixture)
        .into_iter()
        .filter_map(|finding| finding.remedy)
        .filter(|remedy| is_open(&fixture, remedy))
        .collect();

    let outcome = IntegrityService::new(&fixture.ctx())
        .repair()
        .expect("a repair that leaves findings is still a repair");

    let appended: BTreeSet<String> = stories
        .iter()
        .zip(&before)
        .filter(|(story, count)| {
            let no = StoryNo::parse_id("SH", story).expect("a well-formed id");
            events_of(&fixture, no.get()).len() > **count
        })
        .map(|(story, _)| story.clone())
        .collect();

    assert_eq!(
        appended, reachable,
        "the run appended somewhere its own findings did not send it, or skipped somewhere they \
         did"
    );
    assert_eq!(
        appended,
        ["SH-2", "SH-3", "SH-4"]
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<_>>(),
        "the inverse lands on SH-2 (not SH-1, which reported it), the labels on SH-3, the \
         retraction on SH-4 — and nothing lands on SH-6, whose finding names no remedy, or on \
         SH-7, whose remedy is closed"
    );

    assert_eq!(
        report(&fixture),
        [
            "SH-6: unknown type `bug`",
            "SH-7: malformed labels [\"web,sse\"] — a label cannot contain a comma or be blank",
        ],
        "what is left must be exactly the findings with no reachable remedy"
    );
    assert!(
        outcome
            .advice
            .iter()
            .any(|entry| entry.contains("SH-7: normalize its labels to [\"sse\", \"web\"]")),
        "the one repair the run could not reach went unnamed (SH-225): {:?}",
        outcome.advice
    );
    assert_eq!(
        relations_of(&fixture, &b),
        [("blocked-by".to_string(), a.clone())],
        "the inverse was written to the end that lacked it"
    );
}

/// The re-fold moved to the front of `repair`, but it must not move past the
/// catalog write: [`storyhook::store::repair_read_model`] folds every story
/// against the project's state definitions, so a project below the
/// required-state floor cannot fold the stories sitting in the state it is
/// missing.
///
/// One run has to do both — add `blocked` back to the catalog, then restore the
/// row of the story sitting in it. Ordering the re-fold ahead of the catalog
/// write makes this red, which is the failure the reorder invites.
#[test]
fn the_read_model_repair_runs_after_the_catalog_repair() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "A");
    StoryService::new(&ctx)
        .set_state(&a, "blocked", Some("waiting"), None, None)
        .expect("blocking A");
    drop(ctx);

    storyhook::store::test_support::forget_state(fixture.store(), fixture.project(), "blocked")
        .expect("retiring a state a story sits in");
    forget_row(&fixture, 1);

    let message =
        fix(&fixture).expect("one run repairs the catalog and then the rows that need it");
    assert!(
        message.starts_with("doctor repaired supported integrity issues")
            && message.contains("added 1 required state this project was missing"),
        "{message}"
    );
    assert!(report(&fixture).is_empty(), "{:?}", report(&fixture));
    assert_eq!(
        story_state(&fixture, &a),
        "blocked",
        "the restored row must fold against the state the same run put back"
    );
}

/// The other direction, and the reason the reconciliation is per-entry rather
/// than "a run that repaired something drops its whole blocked list": a repair
/// the run did *not* dissolve is still named, in the same run that dissolved
/// another one.
///
/// SH-3's labels are malformed and SH-3 is closed, so that repair is out of
/// reach however much else the run puts right — and SH-225's whole point is
/// that the operator is told which story to reopen. Meanwhile SH-1's "dangling"
/// edge is dissolved by the same run's row restoration.
#[test]
fn a_blocked_repair_the_run_did_not_dissolve_is_still_named() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "A");
    let b = new_story(&ctx, "B");
    let c = new_story(&ctx, "C");
    RelationService::new(&ctx)
        .relate(&a, "obviates", &b, false)
        .expect("relating");
    StoryService::new(&ctx)
        .set_state(&a, "done", None, None, None)
        .expect("closing A");
    StoryService::new(&ctx)
        .set_state(&c, "done", None, None, None)
        .expect("closing C");
    drop(ctx);

    append_to_one_end(
        &fixture,
        &c,
        &[StoryEvent::StoryLabelsSet {
            at: FIXTURE_NOW.to_string(),
            labels: vec!["web,sse".to_string()],
        }],
    );
    forget_row(&fixture, 2);

    let message = fix(&fixture)
        .expect_err("a closed story's malformed labels cannot be repaired")
        .to_string();
    assert!(
        message.contains("SH-3: normalize its labels to [\"sse\", \"web\"]"),
        "a blocked repair the run left standing went unnamed: {message}"
    );
    assert!(
        !message.contains("retract its dangling relation"),
        "the dissolved repair was still advised: {message}"
    );
}

/// A `kind` this build must **not** be able to decode — a newer storyhook's
/// vocabulary, arriving in an older one's store.
const UNRECOGNISED_KIND: &str = "StoryPinned";

/// A `kind` this build **does** decode, so an unreadable payload underneath it
/// reads as damage rather than as data from the future.
const DECODABLE_KIND: &str = "StoryCommentAdded";

/// The two fixtures below differ in exactly one thing — whether the kind is one
/// this build knows — and that is the whole distinction `story doctor` draws
/// between *damage* and *data from the future*. A typo in either spelling
/// collapses it silently, because an unknown kind is retained rather than
/// rejected (SH-54) and every assertion downstream still holds (SH-364).
#[test]
fn the_two_injected_kinds_still_mean_what_these_fixtures_need_them_to_mean() {
    assert!(
        !storyhook::domain::is_known_event_kind(UNRECOGNISED_KIND),
        "{UNRECOGNISED_KIND} decodes now, so `inject_unrecognised_kind` injects \
         a recognised kind and every test built on it has changed subject"
    );
    assert!(
        storyhook::domain::is_known_event_kind(DECODABLE_KIND),
        "{DECODABLE_KIND} does not decode, so `inject_torn_payload` injects a \
         kind from the future rather than the torn payload it promises"
    );
}

/// Injects an unrecognised-kind event (a newer storyhook's data, not this
/// build's) after the three required creation events of a fresh story.
fn inject_unrecognised_kind(fixture: &ServiceFixture) {
    storyhook::store::test_support::inject_raw_events(
        fixture.store(),
        fixture.project(),
        StoryNo::new(1),
        &[storyhook::store::RawEvent {
            kind: UNRECOGNISED_KIND.to_string(),
            at: "2030-01-01T00:00:00Z".to_string(),
            payload: format!(r#"{{"kind":"{UNRECOGNISED_KIND}","at":"2030-01-01T00:00:00Z"}}"#),
        }],
    )
    .expect("injecting");
}

/// Injects a known-kind event whose payload this build cannot read (a torn
/// payload — damage) after the three required creation events of a fresh story.
fn inject_torn_payload(fixture: &ServiceFixture) {
    storyhook::store::test_support::inject_raw_events(
        fixture.store(),
        fixture.project(),
        StoryNo::new(1),
        &[storyhook::store::RawEvent {
            kind: DECODABLE_KIND.to_string(),
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

    let notices = examine(&fixture).notices;
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].contains("event 4")
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
        issues[0].starts_with("SH-1: ")
            && issues[0].contains("event 4")
            && issues[0].contains("`StoryCommentAdded`")
            && !issues[0].contains("newer storyhook"),
        "a torn payload reads differently from a notice: {issues:?}"
    );
    assert!(
        examine(&fixture).notices.is_empty(),
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
/// one advice source the damaged branch passed on — the other six were
/// assembled inside the healthy branch, so an orphaned registration, an
/// unregistered origin, an abandoned command, a stale pointer or a legacy
/// commit link was reported only while nothing else was wrong. Withheld,
/// that is, exactly when an operator is reading.
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
        rendered.contains("SH-1: cannot be folded"),
        "the finding that failed the run: {rendered}"
    );
    assert!(
        rendered.contains("could not be repaired"),
        "the repair `--fix` declined to guess at must be named, not dropped: {rendered}"
    );
    // SH-269: the advice block names the story the way the finding above it
    // does. It used to say `1: <reason>` — not even the word "story".
    assert!(
        rendered
            .lines()
            .any(|line| line.starts_with("SH-1: ") && !line.contains("cannot be folded:")),
        "the unrepairable list spells its id like every other line: {rendered}"
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
        "`subject` carries the rendered id every other surface speaks"
    );
    assert!(
        divergence.message.starts_with("SH-1: "),
        "and since SH-269 the sentence spells it the same way: {}",
        divergence.message
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

// --- one report, one name per story (SH-269) -------------------------------

/// **SH-269's acceptance criterion.** `story doctor` named one story two ways
/// in one report: `SH-41:` from the story-level pass and `story 41:` from the
/// read-model pass. A reader of a mixed report could not tell whether the two
/// lines were about the same story — the ids differ in *both* halves of the
/// spelling, so neither one is a substring of the other.
///
/// Derived rather than enumerated: a finding that identifies a story must
/// *lead* with the id [`Finding::subject`] carries, whichever pass produced
/// it. A check added later inherits the rule instead of getting to spell its
/// own id, which is how the two spellings diverged in the first place.
#[test]
fn every_finding_that_names_a_story_leads_with_the_rendered_id() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let a = new_story(&ctx, "one");
    new_story(&ctx, "two");
    drop(ctx);

    // Damage from both passes, in one report: a malformed label on SH-1
    // (story-level), SH-2's row deleted out from under its own history and
    // SH-1's title rewritten behind the events' back (read-model).
    //
    // Deliberately *no* relation between the two. An edge naming a story whose
    // row is missing reads as dangling to the repair pass, which retracts it —
    // SH-285, a live data-loss defect this test found and must not depend on.
    append_to_one_end(
        &fixture,
        &a,
        &[StoryEvent::StoryLabelsSet {
            at: FIXTURE_NOW.to_string(),
            labels: vec!["web,sse".to_string()],
        }],
    );
    forget_row(&fixture, 2);
    let connection = Connection::open(fixture.store().path()).expect("opening the store");
    connection
        .execute("UPDATE stories SET title = 'wrong'", [])
        .expect("damaging the read model");

    let found = findings(&fixture);
    let codes: BTreeSet<FindingCode> = found.iter().map(|finding| finding.code).collect();
    assert!(
        codes.contains(&FindingCode::MalformedLabels),
        "the fixture must reach the story-level pass: {found:?}"
    );
    assert!(
        codes.contains(&FindingCode::MissingRow)
            && codes.contains(&FindingCode::ReadModelDivergence),
        "and the read-model pass, or this test proves nothing: {found:?}"
    );

    for finding in &found {
        let Some(subject) = finding.subject.as_deref() else {
            // A property of the *project* rather than of a story — the catalog
            // findings, which name no story and must not invent one.
            continue;
        };
        assert!(
            finding.message.starts_with(&format!("{subject}: ")),
            "a finding must lead with the id its `subject` carries, and `{}` does not \
             lead with `{subject}`",
            finding.message
        );
    }

    assert_eq!(
        fix(&fixture).expect("every fault this test made is repairable"),
        "doctor repaired supported integrity issues"
    );
    assert!(report(&fixture).is_empty(), "{:?}", report(&fixture));
}

/// The same rule for the notice channel, which is rendered beside the findings
/// and had the same bare number (SH-269). `Finding::subject` never covered
/// this half: a notice is a bare `String`.
#[test]
fn a_notice_names_its_story_the_way_the_findings_do() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "visited by a newer storyhook");
    drop(ctx);
    inject_unrecognised_kind(&fixture);

    let notices = examine(&fixture).notices;
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].starts_with("SH-1: "),
        "a notice sits in the same report as the findings and spells ids the same way: \
         {notices:?}"
    );
}

/// `story doctor`'s detection layer for SH-398: a story whose `awaiting`
/// reason names another open story with no `blocked-by` edge recording it.
/// The authoring-time nudge (`block_notice::warnings`) only fires when the
/// reason is *written*; this is what still finds a reason typed before that
/// nudge existed, or edited by hand.
#[test]
fn a_prose_reason_naming_an_unlinked_open_story_is_a_notice() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let worker = new_story(&ctx, "worker");
    let mentioned = new_story(&ctx, "mentioned but not linked");
    StoryService::new(&ctx)
        .set_awaiting(&worker, &format!("flake filed as {mentioned}"))
        .unwrap();

    let notices = examine(&fixture).notices;
    assert!(
        notices
            .iter()
            .any(|n| n.starts_with(&format!("{worker}'s reason names {mentioned}"))),
        "{notices:?}"
    );
}

/// The sibling positive control: naming the blocker through `--on` (i.e.
/// `RelationService::block_on`) instead of prose leaves nothing for the
/// sweep to report.
#[test]
fn a_reason_naming_its_own_recorded_blocker_is_not_a_notice() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let worker = new_story(&ctx, "worker");
    let blocker = new_story(&ctx, "the recorded blocker");
    RelationService::new(&ctx)
        .block_on(
            &worker,
            std::slice::from_ref(&blocker),
            Some(&format!("waiting on {blocker}")),
        )
        .unwrap();

    let notices = examine(&fixture).notices;
    assert!(
        notices.iter().all(|n| !n.contains("reason names")),
        "{notices:?}"
    );
}

/// A reason naming a story that has already closed is not a live gap -- the
/// mention no longer needs an edge that would clear itself, because there is
/// nothing left for it to clear against.
#[test]
fn a_reason_naming_a_closed_story_is_not_a_notice() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let worker = new_story(&ctx, "worker");
    let closed = new_story(&ctx, "already closed");
    StoryService::new(&ctx)
        .set_state(&closed, "done", None, None, None)
        .unwrap();
    StoryService::new(&ctx)
        .set_awaiting(&worker, &format!("resolved once {closed} lands"))
        .unwrap();

    let notices = examine(&fixture).notices;
    assert!(
        notices.iter().all(|n| !n.contains("reason names")),
        "{notices:?}"
    );
}

/// A healthy project -- no prose reason mentioning any story at all --
/// contributes nothing, which is the negative control every derived scan in
/// this project's own doctrine needs (SH-364's own lesson: an oracle that
/// can only ever say yes is not an oracle).
#[test]
fn a_reason_that_mentions_no_story_at_all_is_not_a_notice() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let worker = new_story(&ctx, "worker");
    StoryService::new(&ctx)
        .set_awaiting(&worker, "waiting on legal sign-off")
        .unwrap();

    let notices = examine(&fixture).notices;
    assert!(
        notices.iter().all(|n| !n.contains("reason names")),
        "{notices:?}"
    );
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
            | FindingCode::UnexaminedStory
            | FindingCode::Unstructured
            | FindingCode::MissingAttachmentBlob
            | FindingCode::OrphanedAttachmentBlob
            | FindingCode::AttachmentBlobMismatch => code,
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
        FindingCode::UnexaminedStory,
        FindingCode::Unstructured,
        FindingCode::MissingAttachmentBlob,
        FindingCode::OrphanedAttachmentBlob,
        FindingCode::AttachmentBlobMismatch,
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
        // SH-315: a real attachment, its blob row deleted out from under it —
        // the snapshot still claims it, the bytes are gone.
        Box::new(|| {
            const PNG_BYTES: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];
            let fixture = ServiceFixture::new();
            let ctx = fixture.ctx();
            let id = new_story(&ctx, "with an attachment");
            AttachmentService::new(&ctx)
                .add(&id, PNG_BYTES, "shot.png", None)
                .expect("attaching");
            drop(ctx);
            Connection::open(fixture.store().path())
                .expect("opening the store")
                .execute("DELETE FROM story_attachment_blobs", [])
                .expect("damaging the store");
            let found = findings(&fixture);
            let _ = fix(&fixture);
            found
        }),
        // SH-315: a blob row with no attachment in any snapshot naming it —
        // written directly, since the service never leaves one behind.
        Box::new(|| {
            let fixture = ServiceFixture::new();
            let ctx = fixture.ctx();
            new_story(&ctx, "with an orphaned blob");
            drop(ctx);
            let placeholder_sha = "0".repeat(64);
            Connection::open(fixture.store().path())
                .expect("opening the store")
                .execute(
                    &format!(
                        "INSERT INTO story_attachment_blobs \
                             (project_id, story_no, attachment_id, bytes, byte_len, sha256, \
                              added_at) \
                         SELECT project_id, story_no, 1, X'89504e470d0a1a0a', 8, \
                             '{placeholder_sha}', '2026-01-01T00:00:00Z' \
                         FROM stories LIMIT 1"
                    ),
                    [],
                )
                .expect("orphaning a blob");
            let found = findings(&fixture);
            let _ = fix(&fixture);
            found
        }),
        // SH-315: a real attachment whose stored sha256 was altered after the
        // fact — the snapshot's recorded hash no longer matches the row.
        // Not `byte_len`: the schema's own `CHECK (byte_len = length(bytes))`
        // would refuse a length that disagrees with the actual blob, so only
        // `sha256` — which carries no such constraint against the bytes — can
        // be damaged this way without the damage SQL itself failing.
        Box::new(|| {
            const PNG_BYTES: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0];
            let fixture = ServiceFixture::new();
            let ctx = fixture.ctx();
            let id = new_story(&ctx, "with a mismatched attachment");
            AttachmentService::new(&ctx)
                .add(&id, PNG_BYTES, "shot.png", None)
                .expect("attaching");
            drop(ctx);
            let wrong_sha = "1".repeat(64);
            Connection::open(fixture.store().path())
                .expect("opening the store")
                .execute(
                    &format!("UPDATE story_attachment_blobs SET sha256 = '{wrong_sha}'"),
                    [],
                )
                .expect("damaging the store");
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
            "legacy wire code retained for compatibility; SH-446 made multiple parents \
             intentional, so the domain no longer emits this finding",
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
            FindingCode::UnexaminedStory,
            "the disclosure minted for exactly the stories MissingRow, ExtraRow, FoldFailure \
             and a `snapshot` divergence name (SH-286), so it is provoked wherever they are \
             and excused for the same reason — with one addition of its own, \
             `a_report_names_a_story_it_could_not_examine` below, which is where the swap \
             itself is pinned",
        ),
        (
            FindingCode::Unstructured,
            "not a doctor finding at all: it is what `IntegrityDetail: From<String>` mints \
             so every raise site carries one. Pinned in src/error.rs's own unit tests",
        ),
    ]
}

/// The state a store is in once a **newer** storyhook has written to it: an
/// event kind this build cannot decode sits in the log, and the read-model row
/// reflects that event's contribution — because the binary that wrote it could
/// fold it.
///
/// `inject_unrecognised_kind` alone does not produce this: it re-folds from
/// what *this* build understands, which is the state after an older binary has
/// already run over the store. The `put_story` below is the newer binary's own
/// fold, and it is what this build must not destroy.
fn visited_by_a_newer_storyhook(fixture: &ServiceFixture, id: &str) {
    inject_unrecognised_kind(fixture);
    let project = fixture.project();
    let story = StoryNo::parse_id("SH", id).expect("a well-formed id");
    fixture
        .store()
        .write(|tx| {
            let stored = tx.events_for(project, story)?;
            let head = stored.last().expect("the story has events").seq;
            let (known, _unknown) = partition_known(story, &stored);
            let states = tx.state_map(project)?;
            let mut snapshot = fold_story(id, &known, &states).map_err(StoreError::from)?;
            snapshot.title = NEWER_TITLE.to_string();
            tx.put_story(project, &snapshot, head)?;
            Ok(())
        })
        .expect("writing the newer binary's fold");
}

/// What the unrecognised event contributed, as the newer binary folded it.
const NEWER_TITLE: &str = "renamed by a newer storyhook";

/// A story's persisted title, straight from the row.
fn story_title(fixture: &ServiceFixture, id: &str) -> String {
    let no = StoryNo::parse_id("SH", id).expect("a well-formed id");
    fixture
        .store()
        .read(|tx| tx.story(fixture.project(), no))
        .expect("reading the story")
        .expect("the story exists")
        .snapshot
        .title
}

// --- SH-410: an incomplete fold is not an oracle ---------------------------
//
// A story carrying an event this build cannot decode folds to `Ok` — the fold
// succeeded on what it could read — and that snapshot silently omits whatever
// the undecoded events contributed. Comparing a row against it says nothing
// true, and rewriting the row from it destroys the newer storyhook's own fold.
//
// The fixture below is the state that reproduces it, and it is not the state
// `inject_unrecognised_kind` produces on its own: that helper re-folds from
// what *this* build knows, which is the store as it stands *after* an older
// binary has already run over it. The `put_story` in
// `visited_by_a_newer_storyhook` is the newer binary's own fold, which is what
// must survive.

/// The row a newer storyhook wrote is still there after `--fix` runs.
///
/// The defect as filed: `--fix` reverted the title to this build's partial
/// fold, reported `Ok`, and headlined it as a repair.
#[test]
fn a_newer_storyhooks_row_survives_doctor_fix() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "the title this build can fold");
    drop(ctx);
    visited_by_a_newer_storyhook(&fixture, &id);

    fix(&fixture).expect("a row this build cannot vouch for must not fail --fix");

    assert_eq!(
        story_title(&fixture, &id),
        NEWER_TITLE,
        "--fix overwrote a row it cannot fold whole"
    );
}

/// A store a newer storyhook wrote is healthy, and `story doctor` must say so.
///
/// The half that was not in the filing, and the larger one: the read-only pass
/// reported two `ReadModelDivergence` findings plus the `UnexaminedStory` they
/// cascade into, so a plain `story doctor` exited non-zero against an
/// undamaged store — which is what would send an operator to `--fix` in the
/// first place.
#[test]
fn a_row_this_build_cannot_vouch_for_is_not_a_divergence_finding() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "the title this build can fold");
    drop(ctx);
    visited_by_a_newer_storyhook(&fixture, &id);

    assert!(
        report(&fixture).is_empty(),
        "a disagreement with a fold this build knows is partial is not damage: {:?}",
        report(&fixture)
    );
}

/// The withholding is reported, and reported as a withholding.
///
/// A silent no-op would be its own defect: `--fix` is the command an operator
/// runs when they already suspect something, so "checked, fine" is the one
/// answer it must not give for a row it declined to touch.
#[test]
fn the_notice_names_the_withheld_fields_and_the_remedy() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "the title this build can fold");
    drop(ctx);
    visited_by_a_newer_storyhook(&fixture, &id);

    let notices = examine(&fixture).notices;
    let row_notice = notices
        .iter()
        .find(|notice| notice.contains("left exactly as found"))
        .unwrap_or_else(|| panic!("no notice about the row itself: {notices:?}"));
    assert!(
        row_notice.contains("SH-1")
            && row_notice.contains("title")
            && row_notice.contains("StoryPinned")
            && row_notice.contains("re-run"),
        "the notice must name the story, the withheld field, the cause and the remedy: \
         {row_notice}"
    );

    let message = fix(&fixture).expect("a withheld row must not fail --fix");
    assert!(
        message.contains("left exactly as found"),
        "--fix must account for the row it declined to write: {message}"
    );
}

/// `--fix` does not claim a repair it did not make.
///
/// Measured before the fix: the headline read "doctor repaired supported
/// integrity issues" for a run whose only action was overwriting a row it
/// should not have touched.
#[test]
fn fix_does_not_claim_a_repair_it_did_not_make() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "the title this build can fold");
    drop(ctx);
    visited_by_a_newer_storyhook(&fixture, &id);

    let message = fix(&fixture).expect("a withheld row must not fail --fix");
    assert!(
        message.starts_with("doctor found nothing to fix"),
        "nothing was repaired, so nothing may be claimed: {message}"
    );
}

/// The anti-blinding control: staleness is still damage.
///
/// `head_seq`/`head_global_seq` are read from the raw event list *before*
/// `partition_known` runs, so a row behind them is stale whatever this build
/// can decode. This is the assertion that fails if someone later widens the
/// withholding to every column, and it must be green both before and after the
/// SH-410 change.
#[test]
fn a_stale_row_on_a_story_with_a_future_event_is_still_a_finding() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "the title this build can fold");
    drop(ctx);
    inject_unrecognised_kind(&fixture);
    // Irreparable by construction: the story is withheld from
    // `repair_read_model`, so `--fix` cannot put this head back however often
    // it runs. That is the subject of the test, not an omission.
    fixture.expects_drift();
    // Put the row's head back where it stood before the injected event, which
    // is what a row nobody re-folded looks like.
    Connection::open(fixture.store().path())
        .expect("opening the store")
        .execute("UPDATE stories SET head_seq = 1", [])
        .expect("staling the row");

    let issues = report(&fixture);
    assert!(
        issues.iter().any(|issue| issue.contains("head_seq")),
        "a stale head is fold-independent and stays damage: {issues:?}"
    );
}

/// A story with no read-model row is reported, never invented.
///
/// `--fix` must not write the incomplete fold in order to make the story
/// visible: `put_story` stamps the row at the **full** `head_seq`, taken from
/// the very event that did not decode, so the row would claim to be a fold up
/// to head and the one column that distinguishes *stale* from *wrong* would be
/// spent saying something false.
#[test]
fn a_missing_row_on_a_story_with_a_future_event_is_reported_and_not_invented() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "the title this build can fold");
    drop(ctx);
    inject_unrecognised_kind(&fixture);
    // Irreparable by construction — see the sibling test above.
    fixture.expects_drift();
    Connection::open(fixture.store().path())
        .expect("opening the store")
        .execute("DELETE FROM stories", [])
        .expect("removing the row");

    let error = fix(&fixture).expect_err("a story with no row is damage --fix cannot repair");
    assert!(
        error
            .to_string()
            .contains("has events but no read-model row"),
        "the missing row must be named: {error}"
    );
    assert!(
        fixture
            .store()
            .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
            .expect("reading")
            .is_none(),
        "--fix invented a row it cannot fold whole"
    );
}
