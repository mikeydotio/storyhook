//! SH-130: a story's superstate must agree with the superstate of the state it
//! names, and the schema — not application code — is what says so.
//!
//! The story's own preventative clause sets the bar for this file:
//!
//! > Whatever enforces the invariant must fail a test that constructs the
//! > illegal pair **directly**, not merely one that exercises `story delete`.
//! > The defect class is "two columns that must agree, stored independently";
//! > the test should be written against the columns.
//!
//! So the tests that matter most here bypass every service and write the two
//! columns by hand, the way `store::test_support::corrupt_snapshot` does — a
//! raw `rusqlite::Connection` and an `UPDATE`. A rule enforced only by the
//! layer that happens to call it is not enforced.
//!
//! **Foreign keys are enabled explicitly on every raw connection**, and that is
//! not incidental. SQLite enforces them per connection and defaults them
//! *off*, so a test that forgot would prove nothing at all: the write would
//! succeed and the file would read as evidence that the constraint is missing.
//! Production sets the pragma (`store/sqlite/mod.rs`), so a test that sets it
//! is asking the question production asks.

use rusqlite::Connection;
use storyhook_test_support::TestEnv;

/// A connection configured the way the store configures its own.
///
/// Deliberately a free function rather than a closure at each site: every raw
/// write in this file has to go through the same setup, or a test could pass
/// for the wrong reason.
fn production_shaped_connection(path: &std::path::Path) -> Connection {
    let conn = Connection::open(path).expect("opening the store");
    conn.busy_timeout(std::time::Duration::from_secs(5))
        .expect("setting a busy timeout");
    conn.pragma_update(None, "foreign_keys", true)
        .expect("enabling foreign keys");
    conn
}

// ---------------------------------------------------------------------------
// The columns themselves
// ---------------------------------------------------------------------------

#[test]
fn a_closed_story_cannot_be_written_into_an_open_state() {
    // The defect exactly, and the shape SH-20 held: superstate CLOSED while the
    // slug is `todo`, which is OPEN. Written straight to the columns.
    let env = TestEnv::shared();
    let project = env.project().seed_story("A story").build();
    let store = project.open_store();
    let conn = production_shaped_connection(store.path());

    let result = conn.execute(
        "UPDATE stories SET superstate = 'CLOSED', archived = 1, \
         closed_at = '2026-01-01T00:00:00Z' WHERE state = 'todo'",
        [],
    );

    let error = result.expect_err("the schema must refuse a CLOSED story in an OPEN state");
    assert!(
        error.to_string().to_lowercase().contains("foreign key"),
        "the refusal should come from the composite foreign key, got: {error}"
    );
}

#[test]
fn an_open_story_cannot_be_written_into_a_closed_state() {
    // The mirror image. `superstate` is not a free-text column that happens to
    // agree by convention; both directions are constrained.
    let env = TestEnv::shared();
    let project = env.project().seed_story("A story").build();
    let store = project.open_store();
    let conn = production_shaped_connection(store.path());

    let error = conn
        .execute("UPDATE stories SET state = 'done' WHERE state = 'todo'", [])
        .expect_err("an OPEN story must not be written into a CLOSED state");
    assert!(
        error.to_string().to_lowercase().contains("foreign key"),
        "got: {error}"
    );
}

#[test]
fn a_story_cannot_name_a_state_the_project_does_not_define() {
    // Falls out of the same constraint rather than needing its own. Before
    // SH-130 nothing stopped a row naming a slug that was never in the catalog.
    let env = TestEnv::shared();
    let project = env.project().seed_story("A story").build();
    let store = project.open_store();
    let conn = production_shaped_connection(store.path());

    let error = conn
        .execute("UPDATE stories SET state = 'invented'", [])
        .expect_err("a story must not name a state that does not exist");
    assert!(
        error.to_string().to_lowercase().contains("foreign key"),
        "got: {error}"
    );
}

#[test]
fn a_closed_story_must_carry_a_close_timestamp() {
    // The second instance of the defect class, closed in the same rebuild:
    // `superstate` and `archived` are the same fact told twice.
    let env = TestEnv::shared();
    let project = env.project().seed_story("A story").build();
    let store = project.open_store();
    let conn = production_shaped_connection(store.path());

    let error = conn
        .execute(
            "UPDATE stories SET state = 'done', superstate = 'CLOSED' WHERE state = 'todo'",
            [],
        )
        .expect_err("a CLOSED story with no close timestamp must be refused");
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "the refusal should come from the archived CHECK, got: {error}"
    );
}

// ---------------------------------------------------------------------------
// Route 2 — the parent side
// ---------------------------------------------------------------------------

#[test]
fn reclassifying_a_closed_state_open_reopens_the_stories_left_in_it() {
    // The route that is *not* in SH-130 as filed, and the reason the parent
    // side of this constraint has to exist at all.
    //
    // `state_usage` counts only undeleted stories and splits them into open and
    // archived; `resolve_migration` reads the open count alone and returns
    // early when it is zero. So a CLOSED state whose occupants are all archived
    // looks empty to the service, and the superstate flip proceeds with no
    // occupant migration — the stories are left in a state that has changed
    // underneath them. The write lands on `project_states`, which is why a
    // trigger on `stories` could never see it.
    //
    // Reclassification is supported, not refused: the rows re-derive. What
    // SH-130 fixes is that they now re-derive *coherently*. Before, the story's
    // superstate followed the slug to OPEN while `closed_at` stayed set, so an
    // archived story reported an OPEN superstate — the same defect class,
    // reached through `story state update` instead of `story delete`.
    let env = TestEnv::shared();
    let project = env.project().seed_story("A story").build();
    project
        .run(&["state", "add", "shipped", "--super", "CLOSED"])
        .success();
    project.run(&["move", "SH-1", "shipped"]).success();

    let closed = project.json(&["show", "SH-1"]);
    assert_eq!(closed["story"]["story"]["superstate"], "CLOSED");
    assert_ne!(
        closed["story"]["story"]["closed_at"],
        serde_json::Value::Null
    );

    project
        .run(&["state", "set", "shipped", "--super", "OPEN"])
        .success();

    let reopened = project.json(&["show", "SH-1"]);
    assert_eq!(reopened["story"]["story"]["state"], "shipped");
    assert_eq!(
        reopened["story"]["story"]["superstate"], "OPEN",
        "the row re-derives from the state's new definition"
    );
    assert_eq!(
        reopened["story"]["story"]["closed_at"],
        serde_json::Value::Null,
        "and the closure is retracted with it — an archived story reporting an \
         OPEN superstate is the defect, not the fix"
    );
}

// ---------------------------------------------------------------------------
// The delete path, at its origin
// ---------------------------------------------------------------------------

#[test]
fn a_deleted_story_comes_to_rest_in_a_closed_state() {
    let env = TestEnv::shared();
    let project = env.project().seed_story("A mistake").build();

    project
        .run(&["delete", "SH-1", "created in error"])
        .success();

    let shown = project.json(&["show", "SH-1"]);
    assert_eq!(shown["story"]["story"]["state"], "done");
    assert_eq!(shown["story"]["story"]["superstate"], "CLOSED");
    assert_eq!(shown["story"]["story"]["deleted"], true);
}

#[test]
fn listing_an_open_state_never_returns_a_closed_story() {
    // The user-visible symptom SH-130 was filed about: `story list --state todo`
    // returned SH-20, which was closed *and* soft-deleted.
    let env = TestEnv::shared();
    let project = env
        .project()
        .seed_story("Still open")
        .seed_story("Deleted")
        .build();

    project
        .run(&["delete", "SH-2", "created in error"])
        .success();

    let listed = project.json(&["list", "--state", "todo"]);
    let ids: Vec<&str> = listed["stories"]
        .as_array()
        .expect("a story array")
        .iter()
        .map(|entry| entry["story"]["id"].as_str().expect("an id"))
        .collect();

    assert_eq!(
        ids,
        vec!["SH-1"],
        "a closed story must not appear under an open state"
    );
}

#[test]
fn reopening_a_deleted_story_puts_it_back_in_an_open_state() {
    // `reopen` is checked for the mirror defect. It appends a real
    // `StoryStateChanged` into the project's default open state and the fold
    // retracts the closure markers, so the slug and the superstate move
    // together — it never had the defect, and this pins that.
    let env = TestEnv::shared();
    let project = env.project().seed_story("A mistake").build();
    project
        .run(&["delete", "SH-1", "created in error"])
        .success();

    project.run(&["reopen", "SH-1", "--force"]).success();

    let shown = project.json(&["show", "SH-1"]);
    assert_eq!(shown["story"]["story"]["state"], "todo");
    assert_eq!(shown["story"]["story"]["superstate"], "OPEN");
    assert_ne!(shown["story"]["story"]["deleted"], true);
    assert_eq!(
        shown["story"]["story"]["closed_at"],
        serde_json::Value::Null
    );
}

#[test]
fn an_ordinarily_closed_story_is_untouched_by_the_delete_rule() {
    // The resting state is for *deletion*. A story closed the ordinary way
    // stays exactly where the user put it, including in a non-default CLOSED
    // state — the fix must not quietly herd every closed story into `done`.
    let env = TestEnv::shared();
    let project = env.project().seed_story("Shipped it").build();
    project
        .run(&["state", "add", "shipped", "--super", "CLOSED"])
        .success();

    project.run(&["move", "SH-1", "shipped"]).success();

    let shown = project.json(&["show", "SH-1"]);
    assert_eq!(shown["story"]["story"]["state"], "shipped");
    assert_eq!(shown["story"]["story"]["superstate"], "CLOSED");
    assert_ne!(shown["story"]["story"]["deleted"], true);
}
