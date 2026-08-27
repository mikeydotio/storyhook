//! SH-497 — a deleted child is not a child.
//!
//! Deletion in storyhook is SOFT: the row survives, its `state` slug is kept as
//! a truthful record and its `superstate` is forced to CLOSED (issue #18). That
//! is right for the story itself and wrong for everything that reads it as
//! somebody's child, because `stories.get(child_id)` still succeeds — so
//! `computed_epic_state` reached its "every child is closed" branch and
//! reported the parent **done**. Deleting an epic's last child did not strand
//! the epic; it silently COMPLETED it, with `state_computed: true` so the value
//! read as authoritative rather than stale.
//!
//! User determination, 2026-08-27 (recorded on SH-497): when an epic has no
//! children its state is `todo`, and it must either be deleted or given
//! children before it can move anywhere else.
//!
//! That sentence carries a deliberate ASYMMETRY, and it is the whole design:
//!
//! * the epic's **state** ignores deleted children, so a childless epic
//!   computes to `todo`/OPEN rather than inventing a completion;
//! * the epic's **identity** does not. `has_children` still tests for the
//!   `parent-of` edge, so the story is still an epic and `story move` still
//!   refuses it. A story whose children were all deleted is not thereby
//!   promoted back to an ordinary story that anyone may drag anywhere.
//!
//! Getting only the first half would close the reported failure and quietly
//! hand every abandoned epic back to the board as a movable card.

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
    story(p).args(["new", "epic"]).assert().success();
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

    story(p).args(["delete", "SH-2", "abandoned"]).assert().success();

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

    story(p).args(["delete", "SH-2", "abandoned"]).assert().success();

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

    story(p).args(["delete", "SH-2", "abandoned"]).assert().success();
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
    // The asymmetry, and the half a partial fix would drop. The epic reads
    // `todo` now -- but it is STILL an epic, because it still holds the
    // `parent-of` edge, so its state stays computed and un-driveable. The
    // determination's own words: it must be deleted, or given children.
    let dir = epic_with_children(1);
    let p = dir.path();

    story(p).args(["delete", "SH-2", "abandoned"]).assert().success();

    story(p)
        .args(["move", "SH-1", "in-progress"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("is an epic because it has children"));
}

#[test]
fn an_epic_with_no_live_children_can_still_be_deleted() {
    // The other escape the determination names, and the one the browser
    // suite's own teardown needs: `cleanUpCreatedStories` removes fixture
    // stories children-first, and could not remove the parent while the parent
    // insisted it was closed.
    let dir = epic_with_children(1);
    let p = dir.path();

    story(p).args(["delete", "SH-2", "abandoned"]).assert().success();
    story(p)
        .args(["delete", "SH-1", "abandoned too"])
        .assert()
        .success();
}

#[test]
fn giving_a_childless_epic_a_new_child_makes_its_state_follow_again() {
    // The third escape, asserted so "todo forever" cannot be mistaken for the
    // rule. A replacement child drives the state exactly as the deleted one did.
    let dir = epic_with_children(1);
    let p = dir.path();

    story(p).args(["delete", "SH-2", "abandoned"]).assert().success();
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
