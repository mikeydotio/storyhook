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

// ============================================================
// story new --description / --priority / --label(s) / --assignee
// ============================================================

#[test]
fn new_with_description_sets_description() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "Login crash", "--description", "Users can't log in"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("description: Users can't log in"));
}

#[test]
fn new_without_description_omits_description_line() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "No description here"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("description:").not());
}

#[test]
fn new_with_priority_sets_priority() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "Urgent bug", "--priority", "critical"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("priority: critical"));
}

#[test]
fn new_with_invalid_priority_is_rejected_and_creates_no_story() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "Bad priority", "--priority", "urgent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid priority"));

    story(dir.path()).args(["show", "SH-1"]).assert().failure();
}

#[test]
fn new_with_repeated_label_flags_accumulates() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "Multi-label", "--label", "bug", "--label", "web"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("labels: bug, web"));
}

#[test]
fn new_with_labels_csv_splits_and_trims() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "CSV labels", "--labels", "bug, web, cli"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("labels: bug, cli, web"));
}

#[test]
fn new_with_assignee_sets_assignee() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["member", "add", "mikey <mw@mikey.io>"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Assigned story", "--assignee", "mikey"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("assignee: mikey"));
}

#[test]
fn new_with_unknown_assignee_is_rejected_and_creates_no_story() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "Bad assignee", "--assignee", "nobody"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("member `nobody` not found"));

    story(dir.path()).args(["show", "SH-1"]).assert().failure();
}

#[test]
fn new_with_all_fields_writes_single_enriched_story() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["member", "add", "mikey <mw@mikey.io>"])
        .assert()
        .success();

    story(dir.path())
        .args([
            "new",
            "Full story",
            "--type",
            "bug",
            "--description",
            "Everything at once",
            "--priority",
            "high",
            "--assignee",
            "mikey",
            "--label",
            "web",
            "--label",
            "urgent",
        ])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type: bug"))
        .stdout(predicate::str::contains("description: Everything at once"))
        .stdout(predicate::str::contains("priority: high"))
        .stdout(predicate::str::contains("assignee: mikey"))
        .stdout(predicate::str::contains("labels: urgent, web"));
}

// ============================================================
// story set --description
// ============================================================

#[test]
fn set_description_updates_existing_story() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Needs a description"])
        .assert()
        .success();

    story(dir.path())
        .args(["set", "SH-1", "--description", "Added after creation"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "description: Added after creation",
        ));
}

#[test]
fn set_description_last_write_wins() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Story", "--description", "First draft"])
        .assert()
        .success();

    story(dir.path())
        .args(["set", "SH-1", "--description", "Final draft"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("description: Final draft"))
        .stdout(predicate::str::contains("description: First draft").not());
}

// ============================================================
// story import maps description to a first-class field, not a comment
// ============================================================

#[test]
fn import_maps_description_to_description_field_not_comment() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    let json = r#"[
        {"title": "Imported story", "description": "Came from import"}
    ]"#;

    story(dir.path())
        .args(["import"])
        .write_stdin(json)
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("description: Came from import"))
        .stdout(predicate::str::contains("comments:").not());
}
