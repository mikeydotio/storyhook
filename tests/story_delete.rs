//! Regression tests for #18: `story delete` left `state`/`superstate`
//! unchanged, so a deleted story kept counting as OPEN — it still showed up
//! in open counts, `story next`, `--ready` filters, and kept blocking any
//! story that was `blocked-by` it.
//!
//! The fix forces a deleted story's folded `superstate` to CLOSED (while
//! preserving its `state` slug as a truthful record), surfaces `deleted` /
//! `deleted_reason` on the snapshot, and makes `story reopen` a guarded
//! undelete for soft-deleted stories.

// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn delete_excludes_story_from_open_counts() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();
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
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();
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
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();
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
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();
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
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();
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
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();
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
fn delete_closes_the_story_and_leaves_it_readable() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Test story"])
        .assert()
        .success();

    story(dir.path())
        .args(["delete", "SH-1", "created in error"])
        .assert()
        .success();

    // The open/archive *split* is gone: one row with an `archived` flag
    // replaces two storage media that could, and did, disagree (SH-20). What
    // the split was a proxy for survives and is asserted directly — a deleted
    // story stops counting as open, keeps the state it was deleted in, and
    // stays readable.
    story(dir.path())
        .args(["--json", "summary"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_open\": 0"));
    story(dir.path())
        .args(["--json", "show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"state\": \"todo\""))
        .stdout(predicate::str::contains("\"superstate\": \"CLOSED\""))
        .stdout(predicate::str::contains("\"deleted\": true"));
}

#[test]
fn reopen_deleted_story_without_force_fails_and_stays_closed() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();
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
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();
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
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();
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
fn doctor_fix_heals_a_read_model_row_that_disagrees_with_its_events() {
    // The defect this reproduces was specific to the legacy archive: a deleted
    // story's *cached* snapshot said `superstate: OPEN` while its event log
    // said `StoryDeleted`, because archived snapshots were read from the cache
    // rather than re-folded. The shape survives the flip — a read-model row
    // that disagrees with its own history — and so does the repair, which is
    // now a rebuild-and-diff rather than a special case.
    //
    // The row is fabricated rather than reached: the store writes the read
    // model inside the transaction that appends the events it was folded from,
    // so no public path can produce this. That is exactly why `doctor --fix`
    // has to be able to heal it — a database written by an older storyhook can
    // hold it, and a migrated tree can too.
    let env = storyhook_test_support::TestEnv::shared();
    let project = env.project().seed_story("Legacy deleted story").build();
    project
        .run(&["delete", "SH-1", "created in error"])
        .success();

    let store = project.open_store();
    let id = project.project_id(&store);
    storyhook::store::test_support::corrupt_snapshot(
        &store,
        id,
        project.story_no(&store, "SH-1"),
        |snapshot| {
            let object = snapshot.as_object_mut().expect("a snapshot object");
            object.insert("superstate".to_string(), serde_json::json!("OPEN"));
            object.remove("deleted");
            object.remove("deleted_reason");
        },
    )
    .expect("corrupting the read model");

    // Confirm the fixture reproduces the miscount before healing it.
    project
        .run(&["--json", "summary"])
        .success()
        .stdout(predicate::str::contains("\"total_open\": 1"));

    project.run(&["doctor", "--fix"]).success();

    project
        .run(&["--json", "summary"])
        .success()
        .stdout(predicate::str::contains("\"total_open\": 0"));
    project
        .run(&["--json", "show", "SH-1"])
        .success()
        .stdout(predicate::str::contains("\"superstate\": \"CLOSED\""))
        .stdout(predicate::str::contains("\"deleted\": true"));
}
