//! Regression tests for #18: `story delete` left `state`/`superstate`
//! unchanged, so a deleted story kept counting as OPEN — it still showed up
//! in open counts, `story next`, `--ready` filters, and kept blocking any
//! story that was `blocked-by` it.
//!
//! The fix forces a deleted story's folded `superstate` to CLOSED (while
//! preserving its `state` slug as a truthful record), surfaces `deleted` /
//! `deleted_reason` on the snapshot, and makes `story reopen` a guarded
//! undelete for soft-deleted stories.

use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn delete_excludes_story_from_open_counts() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Created in error"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Still open"])
        .assert()
        .success();

    story(dir.path())
        .args(["delete", "SH-1", "created in error"])
        .assert()
        .success();

    story(dir.path())
        .arg("summary")
        .assert()
        .success()
        .stdout(predicate::str::contains("stories: 2 (1 open, 1 closed)"));

    story(dir.path())
        .args(["--json", "summary"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_open\": 1"))
        .stdout(predicate::str::contains("\"total_closed\": 1"));
}

#[test]
fn delete_excludes_story_from_report_open_count() {
    // `report` computes open/closed via `build_report_data`, a separate code
    // path from `summary` — exercise it directly rather than relying on
    // `summary`'s text happening to match.
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Task A"]).assert().success();

    story(dir.path())
        .args(["delete", "SH-1", "no longer needed"])
        .assert()
        .success();

    story(dir.path())
        .args(["--json", "report"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_open\": 0"))
        .stdout(predicate::str::contains("\"total_closed\": 1"));
}

#[test]
fn delete_excludes_story_from_next_and_list_ready() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Only story"])
        .assert()
        .success();

    story(dir.path())
        .args(["delete", "SH-1", "created in error"])
        .assert()
        .success();

    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("no ready stories"));

    let output = story(dir.path())
        .args(["list", "--ready"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(!stdout.contains("SH-1"));
}

#[test]
fn delete_marks_deleted_story_in_plain_list() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Duplicate story"])
        .assert()
        .success();

    story(dir.path())
        .args(["delete", "SH-1", "duplicate"])
        .assert()
        .success();

    // Like any closed story, a deleted one still appears in the default
    // `list` (there is no default open-only filter) — but it must be
    // clearly marked so it doesn't read as live work.
    story(dir.path())
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-1"))
        .stdout(predicate::str::contains("[deleted]"));
}

#[test]
fn deleted_blocker_no_longer_blocks_dependent() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Blocker"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Dependent"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-2", "blocked-by", "SH-1"])
        .assert()
        .success();

    // Sanity check: SH-2 is genuinely blocked before the delete (only the
    // blocker SH-1 itself is ready). Assert on the distinct titles rather
    // than raw IDs — SH-1's own rendered relationships legitimately mention
    // "SH-2" (its derived inverse `blocks` edge).
    let output = story(dir.path()).arg("next").assert().success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("Blocker"));
    assert!(!stdout.contains("Dependent"));

    story(dir.path())
        .args(["delete", "SH-1", "no longer needed"])
        .assert()
        .success();

    // SH-1 being deleted (not just closed) must still clear the block.
    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("Dependent"));
}

#[test]
fn delete_show_json_exposes_superstate_and_deleted_fields() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Test story"])
        .assert()
        .success();

    story(dir.path())
        .args(["delete", "SH-1", "created in error"])
        .assert()
        .success();

    story(dir.path())
        .args(["--json", "show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"superstate\": \"CLOSED\""))
        .stdout(predicate::str::contains("\"deleted\": true"))
        .stdout(predicate::str::contains(
            "\"deleted_reason\": \"created in error\"",
        ))
        // The state slug itself is preserved as a truthful record of what
        // the story was when it was deleted.
        .stdout(predicate::str::contains("\"state\": \"todo\""));

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: todo (CLOSED, deleted)"))
        .stdout(predicate::str::contains("deleted_reason: created in error"));
}

#[test]
fn delete_archives_and_removes_open_jsonl() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Test story"])
        .assert()
        .success();

    story(dir.path())
        .args(["delete", "SH-1", "created in error"])
        .assert()
        .success();

    assert!(
        !dir.path()
            .join(".storyhook/open/stories/SH-1.jsonl")
            .exists()
    );

    let connection = Connection::open(dir.path().join(".storyhook/archive/archive.db")).unwrap();
    let state: String = connection
        .query_row(
            "SELECT state FROM closed_stories WHERE id = 'SH-1'",
            [],
            |row| row.get(0),
        )
        .expect("SH-1 should be archived after delete");
    assert_eq!(state, "todo");
}

#[test]
fn reopen_deleted_story_without_force_fails_and_stays_closed() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Test story"])
        .assert()
        .success();
    story(dir.path())
        .args(["delete", "SH-1", "created in error"])
        .assert()
        .success();

    // assert_cmd runs with piped (non-TTY) stdin, so this must fail outright
    // rather than hang on a confirmation prompt that can never be answered.
    story(dir.path())
        .args(["reopen", "SH-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--force"));

    // Still closed/deleted — the failed attempt must not have partially
    // mutated anything.
    story(dir.path())
        .args(["--json", "show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"superstate\": \"CLOSED\""))
        .stdout(predicate::str::contains("\"deleted\": true"));
}

#[test]
fn reopen_deleted_story_with_force_undeletes() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Test story"])
        .assert()
        .success();
    story(dir.path())
        .args(["comment", "SH-1", "important context"])
        .assert()
        .success();
    story(dir.path())
        .args(["delete", "SH-1", "created in error"])
        .assert()
        .success();

    story(dir.path())
        .args(["reopen", "SH-1", "--force"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: todo"));

    story(dir.path())
        .args(["--json", "show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"superstate\": \"OPEN\""))
        .stdout(predicate::str::contains("\"deleted\": true").not())
        // Undeleting restores it to a normal open story: modifiable, and
        // counted like any other.
        .stdout(predicate::str::contains("important context"))
        .stdout(predicate::str::contains("[deleted] created in error"));

    story(dir.path())
        .args(["--json", "summary"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_open\": 1"));

    // Modifiable again, like any reopened story.
    story(dir.path())
        .args(["comment", "SH-1", "back in progress"])
        .assert()
        .success();
}

#[test]
fn reopen_ordinarily_closed_story_needs_no_force() {
    // Reopening a story closed via the normal state machine (not deleted)
    // must be completely unaffected by the guarded-undelete behavior.
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Finish me"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success();

    story(dir.path())
        .args(["reopen", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: todo"));
}

#[test]
fn doctor_fix_heals_stale_archived_snapshot_from_before_the_fix() {
    // Simulate data written by the pre-#18-fix code: the archived event log
    // correctly contains `StoryDeleted`, but the *cached* snapshot_json (as
    // the old `fold_story` would have produced it) still has
    // `superstate: "OPEN"` and no `deleted`/`deleted_reason` fields.
    // Archived snapshots are read from this cache rather than re-folded on
    // every load, so such a story would stay miscounted forever without an
    // explicit repair pass.
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Legacy deleted story"])
        .assert()
        .success();
    story(dir.path())
        .args(["delete", "SH-1", "created in error"])
        .assert()
        .success();

    // Post-fix delete already archives this correctly — deliberately
    // corrupt the cached snapshot back to what the old buggy code wrote, to
    // reproduce the pre-fix-data scenario `doctor --fix` must heal.
    {
        let connection =
            Connection::open(dir.path().join(".storyhook/archive/archive.db")).unwrap();
        let snapshot_json: String = connection
            .query_row(
                "SELECT snapshot_json FROM closed_stories WHERE id = 'SH-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let mut snapshot: serde_json::Value = serde_json::from_str(&snapshot_json).unwrap();
        let obj = snapshot.as_object_mut().unwrap();
        obj.insert("superstate".to_string(), serde_json::json!("OPEN"));
        obj.remove("deleted");
        obj.remove("deleted_reason");
        connection
            .execute(
                "UPDATE closed_stories SET snapshot_json = ?1 WHERE id = 'SH-1'",
                [serde_json::to_string(&snapshot).unwrap()],
            )
            .unwrap();
    }

    // Confirm the fixture actually reproduces the pre-fix bug before fixing.
    story(dir.path())
        .args(["--json", "summary"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_open\": 1"));

    story(dir.path())
        .args(["doctor", "--fix"])
        .assert()
        .success();

    story(dir.path())
        .args(["--json", "summary"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_open\": 0"));

    story(dir.path())
        .args(["--json", "show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"superstate\": \"CLOSED\""))
        .stdout(predicate::str::contains("\"deleted\": true"));
}
