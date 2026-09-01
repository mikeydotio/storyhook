use assert_cmd::Command;
use predicates::prelude::*;
use storyhook_test_support::{TestEnv, scratch_dir};

/// Every `story` this file runs is the one THIS build produced, in the shared
/// test environment's private `HOME`, XDG directories and store — so nothing
/// here can reach the developer's own storyhook state, with or without a
/// wrapper script supplying one.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

#[test]
fn summary_shows_counts() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();
    story(dir.path()).args(["new", "Task C"]).assert().success();
    story(dir.path())
        .args(["move", "SH-3", "done"])
        .assert()
        .success();

    story(dir.path())
        .arg("summary")
        .assert()
        .success()
        .stdout(predicate::str::contains("stories: 3 (2 open, 1 closed)"))
        .stdout(predicate::str::contains("ready: 2"));
}

#[test]
fn summary_shows_priority_breakdown() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();
    story(dir.path())
        .args(["prioritize", "SH-1", "high"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-2", "critical"])
        .assert()
        .success();

    story(dir.path())
        .arg("summary")
        .assert()
        .success()
        .stdout(predicate::str::contains("high: 1"))
        .stdout(predicate::str::contains("critical: 1"));
}

#[test]
fn summary_shows_ready_stories() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Ready A"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Blocked B"])
        .assert()
        .success();
    story(dir.path())
        .args(["block", "SH-2", "waiting on API"])
        .assert()
        .success();

    story(dir.path())
        .arg("summary")
        .assert()
        .success()
        .stdout(predicate::str::contains("ready: 1"))
        .stdout(predicate::str::contains("blocked: 1"))
        .stdout(predicate::str::contains("Ready A"));
}

/// Regression test for SH-236: an in-progress story is already claimed, so
/// it must not inflate `summary`'s ready count (nor its blocked count — it
/// isn't blocked either, just already spoken for).
#[test]
fn summary_excludes_in_progress_from_ready_and_blocked() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Ready A"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Claimed B"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-2", "in-progress"])
        .assert()
        .success();

    story(dir.path())
        .arg("summary")
        .assert()
        .success()
        .stdout(predicate::str::contains("ready: 1"))
        .stdout(predicate::str::contains("blocked: 0"))
        .stdout(predicate::str::contains("Ready A"))
        .stdout(predicate::str::contains("Claimed B").not());
}

#[test]
fn summary_json_output() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Task A"]).assert().success();

    story(dir.path())
        .args(["--json", "summary"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"total_open\": 1"))
        .stdout(predicate::str::contains("\"ready_count\": 1"));
}

#[test]
fn summary_empty_project() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .arg("summary")
        .assert()
        .success()
        .stdout(predicate::str::contains("stories: 0 (0 open, 0 closed)"));
}

#[test]
fn summary_shows_type_breakdown() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // Create stories: 2 using the default type, 1 explicit bug, 2 explicit normal.
    // "bug" and "normal" are stock types from init; normal is configured first.
    story(dir.path())
        .args(["new", "Defaulted A"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Defaulted B"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Bug fix", "--type", "bug"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Story X", "--type", "normal"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Story Y", "--type", "normal"])
        .assert()
        .success();

    // Verify summary shows type breakdown
    story(dir.path())
        .arg("summary")
        .assert()
        .success()
        .stdout(predicate::str::contains("by type:"))
        .stdout(predicate::str::contains("bug: 1"))
        .stdout(predicate::str::contains("normal: 4"))
        .stdout(predicate::str::contains("Default:").not());
}
