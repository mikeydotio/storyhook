//! SH-497/SH-498 — a permanently deleted child is not a child. Hard deletion
//! retracts the `parent-of` edge before removing the row, so epic state and
//! progress are derived only from survivors.
//!
//! User determination, 2026-08-27 (recorded on SH-497): when an epic has no
//! children its state is `todo`, and it must either be deleted or given
//! children before it can move anywhere else.
//!
//! The fixtures type their parent `epic` on purpose (SH-499): today every gate
//! infers epic-ness from the `parent-of` edge, which is itself a defect, so
//! naming the type keeps these tests asserting the intended design rather than
//! the accident they would otherwise depend on.

#![allow(clippy::disallowed_methods)]

use assert_cmd::Command;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

/// The `story show --json` document for `id`, parsed.
fn show(dir: &std::path::Path, id: &str) -> serde_json::Value {
    let out = story(dir).args(["show", id, "--json"]).output().unwrap();
    assert!(out.status.success(), "story show {id} failed");
    serde_json::from_slice(&out.stdout).expect("show --json is a document")
}

/// A project with one epic (`SH-1`) and `children` children, all related.
fn epic_with_children(children: usize) -> tempfile::TempDir {
    let dir = tempdir().unwrap();
    let p = dir.path();
    story(p)
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    // Typed `epic` explicitly, not left to acquire epic-ness from its edge.
    // SH-499: "is an epic because it has children" was never the intended
    // design -- all epics are folders, but not every story with children is
    // one -- and every behavioural gate currently asks `has_children` anyway.
    // Typing the parent states the design these tests mean to assert, and
    // keeps them green through that correction instead of pinning the
    // accident.
    story(p)
        .args(["new", "epic", "--type", "epic"])
        .assert()
        .success();
    for n in 0..children {
        story(p)
            .args(["new", &format!("child {n}")])
            .assert()
            .success();
        story(p)
            .args(["relate", "SH-1", "parent-of", &format!("SH-{}", n + 2)])
            .assert()
            .success();
    }
    dir
}

#[test]
fn an_epic_whose_only_child_is_deleted_reads_todo_rather_than_done() {
    // The reported defect, reduced. Before the fix this asserted `done`.
    let dir = epic_with_children(1);
    let p = dir.path();

    let before = show(p, "SH-1");
    assert_eq!(before["story"]["story"]["state"], "todo");

    story(p)
        .args(["delete", "SH-2", "--force"])
        .assert()
        .success();

    let after = show(p, "SH-1");
    assert_eq!(
        after["story"]["story"]["state"], "todo",
        "a deleted child is not a finished child: {after:#}"
    );
    assert_eq!(
        after["story"]["story"]["superstate"], "OPEN",
        "and the epic must not close itself: {after:#}"
    );
}

#[test]
fn a_deleted_child_is_not_counted_in_progress() {
    // The same misreading, on the surface a human actually looks at. An epic
    // reporting "1 of 1 done" for a child that was deleted is a lie told in the
    // one place a reader goes for the truth.
    let dir = epic_with_children(1);
    let p = dir.path();

    story(p)
        .args(["delete", "SH-2", "--force"])
        .assert()
        .success();

    let after = show(p, "SH-1");
    assert!(
        after["story"]["progress"].is_null(),
        "an epic with no live children has no progress to report: {after:#}"
    );
}

#[test]
fn surviving_children_still_drive_the_state_after_a_sibling_is_deleted() {
    // The other direction, so the fix cannot be "ignore children entirely".
    // Two children, one deleted, one moved to in-progress: the epic follows the
    // survivor.
    let dir = epic_with_children(2);
    let p = dir.path();

    story(p)
        .args(["delete", "SH-2", "--force"])
        .assert()
        .success();
    story(p)
        .args(["move", "SH-3", "in-progress"])
        .assert()
        .success();

    let after = show(p, "SH-1");
    assert_eq!(
        after["story"]["story"]["state"], "in-progress",
        "the surviving child still drives the epic: {after:#}"
    );
    let progress = &after["story"]["progress"];
    assert_eq!(progress["children_total"], 1, "only the survivor counts");
    assert_eq!(progress["children_done"], 0);
}

#[test]
fn an_epic_with_no_live_children_still_refuses_a_direct_move() {
    // The parent remains an epic by type even though deletion removed its last
    // parent-of edge, so its state remains computed and cannot be moved.
    let dir = epic_with_children(1);
    let p = dir.path();

    story(p)
        .args(["delete", "SH-2", "--force"])
        .assert()
        .success();

    story(p)
        .args(["move", "SH-1", "in-progress"])
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            // SH-499 rewrote this message: children are no longer given as the
            // cause, because they never were the cause. Anchored on the part
            // that is invariant under that correction.
            "is an epic",
        ));
}

#[test]
fn an_epic_with_no_live_children_can_still_be_deleted() {
    // The other escape the determination names, and the one the browser
    // suite's own teardown needs: `cleanUpCreatedStories` removes fixture
    // stories children-first, and could not remove the parent while the parent
    // insisted it was closed.
    let dir = epic_with_children(1);
    let p = dir.path();

    story(p)
        .args(["delete", "SH-2", "--force"])
        .assert()
        .success();
    story(p)
        .args(["delete", "SH-1", "--force"])
        .assert()
        .success();
}

#[test]
fn giving_a_childless_epic_a_new_child_makes_its_state_follow_again() {
    // The third escape, asserted so "todo forever" cannot be mistaken for the
    // rule. A replacement child drives the state exactly as the deleted one did.
    let dir = epic_with_children(1);
    let p = dir.path();

    story(p)
        .args(["delete", "SH-2", "--force"])
        .assert()
        .success();
    story(p).args(["new", "replacement"]).assert().success();
    story(p)
        .args(["relate", "SH-1", "parent-of", "SH-3"])
        .assert()
        .success();
    story(p)
        .args(["move", "SH-3", "in-progress"])
        .assert()
        .success();

    let after = show(p, "SH-1");
    assert_eq!(
        after["story"]["story"]["state"], "in-progress",
        "a replacement child drives the epic again: {after:#}"
    );
}
