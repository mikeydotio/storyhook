//! `story delete` — permanent story removal (SH-498).
//!
//! Almost everything here is about the gate and the two consequences, rather
//! than about the deletion. The deletion is one store call, covered by twelve
//! conformance cases; what needs a CLI-level test is what a person is told
//! before it happens, what happens to the stories left behind, and that
//! narrowing the append-only guard did not lower it for anything else.
//!
//! Two of these tests open the store file directly, with a connection shaped the
//! way production shapes its own. That is a *second* connection to a database a
//! daemon is holding, which SQLite's write-ahead log is built for — but it means
//! the questions they ask must be ones a second connection can answer: whether a
//! statement is rejected, not what some cache believes.

use rusqlite::Connection;
use storyhook::domain::StoryEvent;
use storyhook::store::test_support::inject_events;
use storyhook::store::{ReadOps, Store};
use storyhook_test_support::{Project, TestEnv};

fn project() -> Project<'static> {
    TestEnv::shared().project().build()
}

/// Two stories that claim each other before one is permanently deleted.
fn claimed_story(project: &Project<'_>) -> (String, String) {
    let doomed = project.new_story("Created in error");
    let kept = project.new_story("Real work");
    project.run(&["relate", &kept, "blocks", &doomed]).success();
    (doomed, kept)
}

/// One story's event kinds, oldest first, read straight from the store.
///
/// The event log rather than the folded snapshot: a retraction that only
/// reached the read model would leave the claimant's own history still
/// asserting an edge into a story that no longer exists, which is the exact
/// divergence the retraction exists to prevent.
fn event_kinds(project: &Project<'_>, id: &str) -> Vec<String> {
    let store = project.open_store();
    let project_id = project.project_id(&store);
    let story = project.story_no(&store, id);
    store
        .read(|tx| tx.events_for(project_id, story))
        .expect("reading a story's events")
        .into_iter()
        .map(|event| event.kind)
        .collect()
}

/// Every surviving story id `story export` reports.
fn exported_ids(project: &Project<'_>) -> Vec<String> {
    project.json(&["export"])["stories"]
        .as_array()
        .expect("export carries a stories array")
        .iter()
        .map(|story| story["id"].as_str().expect("an id").to_string())
        .collect()
}

/// A connection configured the way the store configures its own — foreign keys
/// on, which SQLite defaults *off* per connection. A raw test that forgot would
/// prove nothing, because the write it expects to be refused would succeed.
fn production_shaped_connection(path: &std::path::Path) -> Connection {
    let conn = Connection::open(path).expect("opening the store");
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .expect("setting a busy timeout");
    conn.pragma_update(None, "foreign_keys", true)
        .expect("enabling foreign keys");
    conn
}

#[test]
fn deleting_a_story_that_does_not_exist_says_so() {
    let project = project();
    project.new_story("Real work");

    let out = project
        .story()
        .args(["delete", "SH-404", "--force"])
        .output()
        .expect("running story delete");

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("SH-404") && stderr.contains("not found"),
        "expected a not-found naming the id: {stderr}"
    );
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

#[test]
fn without_force_and_without_a_terminal_it_refuses_and_names_the_flag() {
    // Every test process has stdin closed, which is the same shape as a script,
    // a CI job, or an agent. Defaulting to "no" there would be safe and silent
    // — how a script appears to work for months while never doing its job.
    let project = project();
    let id = project.new_story("Created in error");

    let out = project
        .story()
        .args(["delete", &id])
        .output()
        .expect("running story delete");

    assert_eq!(
        out.status.code(),
        Some(2),
        "a refusal is a usage error; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--force"),
        "the refusal must name the way past it: {stderr}"
    );
    assert!(
        exported_ids(&project).contains(&id),
        "a refused delete destroys nothing"
    );
}

#[test]
fn the_refusal_says_what_would_have_been_destroyed() {
    // A flag named without the stakes is a flag people pass reflexively.
    let project = project();
    let id = project.new_story("Created in error");

    let out = project
        .story()
        .args(["delete", &id])
        .output()
        .expect("running story delete");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(stderr.contains(&id), "the story is named: {stderr}");
    assert!(
        stderr.contains("Created in error"),
        "and titled, so the reader can tell it is the right one: {stderr}"
    );
    assert!(
        stderr.contains("event"),
        "the irreversible number is stated: {stderr}"
    );
}

#[test]
fn json_without_force_refuses_rather_than_prompting_into_the_stream() {
    // `--json` promises one self-describing document on stdout. A prompt
    // written there would corrupt it for every scripted caller.
    let project = project();
    let id = project.new_story("Created in error");

    let out = project
        .story()
        .args(["delete", &id, "--json"])
        .output()
        .expect("running story delete");

    assert_eq!(out.status.code(), Some(2));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim().is_empty() || serde_json::from_str::<serde_json::Value>(&stdout).is_ok(),
        "stdout must stay machine-readable: {stdout}"
    );
    assert!(exported_ids(&project).contains(&id));
}

// ---------------------------------------------------------------------------
// The deletion
// ---------------------------------------------------------------------------

#[test]
fn a_deleted_story_leaves_nothing_behind() {
    let project = project();
    let doomed = project.new_story("Created in error");
    let kept = project.new_story("Real work");

    project.run(&["delete", &doomed, "--force"]).success();

    assert_eq!(
        exported_ids(&project),
        vec![kept.clone()],
        "the export contains only surviving stories"
    );
    let shown = project
        .story()
        .args(["show", &doomed])
        .output()
        .expect("running story show");
    assert!(!shown.status.success(), "`show` must not find it");
    project.run(&["show", &kept]).success();
}

#[test]
fn a_deleted_story_leaves_the_store_without_a_divergence() {
    // Stronger than "the row is gone", and the reason the retraction exists:
    // `story doctor` rebuilds the read model from the events and compares. A
    // delete that took the story but left a surviving claim pointing at it would
    // report a `relations` divergence that `--fix` could never repair, because
    // re-folding the claimant re-derives the same dead edge and the foreign key
    // refuses to write it.
    let project = project();
    let (doomed, kept) = claimed_story(&project);

    project.run(&["delete", &doomed, "--force"]).success();

    project.run(&["doctor"]).success();
    assert_eq!(
        exported_ids(&project),
        vec![kept],
        "and the survivor is still there — a doctor that passes over an empty \
         project would pass for the wrong reason"
    );
}

#[test]
fn a_deleted_story_number_is_never_reissued() {
    // Reusing it would point every commit message, branch name and external
    // link naming the old story at a new and unrelated one. A gap is the
    // cheaper failure.
    let project = project();
    let doomed = project.new_story("Created in error");

    project.run(&["delete", &doomed, "--force"]).success();
    let next = project.new_story("The story after");

    assert_ne!(next, doomed, "the deleted id must never come back");
    assert_eq!(next, "SH-2", "and the counter carries on rather than back");
}

// ---------------------------------------------------------------------------
// The stories left behind
// ---------------------------------------------------------------------------

#[test]
fn a_surviving_claim_is_retracted_before_the_story_goes() {
    let project = project();
    let (doomed, kept) = claimed_story(&project);

    project.run(&["delete", &doomed, "--force"]).success();

    let shown = project.json(&["show", &kept]);
    let relationships = shown["story"]["story"]["relationships"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        relationships.is_empty(),
        "the survivor must not still claim an edge into a story that is gone: {relationships:?}"
    );
}

#[test]
fn the_retraction_is_a_real_event_on_the_surviving_story() {
    // Not a silent table edit. The claimant's history has to be *true*: the
    // edge genuinely was removed, at this moment, by this act — which is also
    // what makes the rebuild oracle agree with the read model afterwards.
    let project = project();
    let (doomed, kept) = claimed_story(&project);

    project.run(&["delete", &doomed, "--force"]).success();

    let kinds = event_kinds(&project, &kept);
    assert!(
        kinds.iter().any(|kind| kind == "StoryRelationshipRemoved"),
        "the survivor's own log must record the retraction: {kinds:?}"
    );
}

#[test]
fn the_confirmation_names_the_claims_it_would_retract() {
    // The one part of a delete that reaches beyond the story being deleted, so it
    // is stated up front rather than left as a surprise.
    let project = project();
    let (doomed, kept) = claimed_story(&project);

    let out = project
        .story()
        .args(["delete", &doomed])
        .output()
        .expect("running story delete");
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        stderr.contains(&kept) && stderr.contains("blocks"),
        "the warning must name the surviving claim and its relation: {stderr}"
    );
}

#[test]
fn a_story_that_never_claimed_the_edge_gets_no_retraction() {
    // `relations_to` answers from the relation *table*, which materializes the
    // mirror of every edge — so the far end of a one-sided claim has a row it
    // never asserted. Retracting from it would append an event annulling a
    // claim it never made: a fabricated history, which is the thing the council
    // refused everywhere else in SH-130.
    //
    // A one-sided claim cannot be made through `story relate`, which writes
    // both ends, so it is injected. That is what this back door is for.
    let project = project();
    let doomed = project.new_story("Created in error");
    let bystander = project.new_story("Real work");
    let store = project.open_store();
    inject_events(
        &store,
        project.project_id(&store),
        project.story_no(&store, &doomed),
        &[StoryEvent::StoryRelationshipAdded {
            at: "2026-01-01T00:00:00Z".to_string(),
            other_id: bystander.clone(),
            relation: "blocks".to_string(),
        }],
    )
    .expect("injecting a one-sided claim");
    let before = event_kinds(&project, &bystander);

    project.run(&["delete", &doomed, "--force"]).success();

    assert_eq!(
        event_kinds(&project, &bystander),
        before,
        "a story that never asserted the edge must not have a retraction \
         appended to its history"
    );
    project.run(&["doctor"]).success();
}

// ---------------------------------------------------------------------------
// The guard the delete narrowed
// ---------------------------------------------------------------------------

#[test]
fn the_append_only_guard_still_refuses_an_ordinary_deletion() {
    // Migration 5 narrows `events_reject_delete` by AND-ing a story clause onto
    // migration 3's project clause. Without this test a migration could widen
    // the guard while looking like it narrowed it, and every other test here
    // would still pass — they all go through the delete, which is the one caller
    // the predicate is supposed to admit.
    let project = project();
    let id = project.new_story("Real work");
    let store = project.open_store();
    let conn = production_shaped_connection(store.path());

    let error = conn
        .execute("DELETE FROM events", [])
        .expect_err("erasing a live story's history must still abort");

    assert!(
        error.to_string().contains("append-only"),
        "the abort must still say why: {error}"
    );
    project.run(&["show", &id]).success();
}

#[test]
fn the_append_only_guard_still_refuses_a_deletion_scoped_to_one_live_story() {
    // The narrower shape, which the blanket DELETE above would not catch if the
    // predicate were ever inverted: one story's events, named exactly, while
    // that story is still there.
    let project = project();
    let id = project.new_story("Real work");
    let store = project.open_store();
    let conn = production_shaped_connection(store.path());

    let error = conn
        .execute("DELETE FROM events WHERE story_no = 1", [])
        .expect_err("one live story's history is history too");

    assert!(error.to_string().contains("append-only"), "got: {error}");
    project.run(&["show", &id]).success();
}
