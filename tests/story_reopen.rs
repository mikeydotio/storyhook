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
fn reopen_archived_story() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Fix bug"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success();

    // Reopen
    story(dir.path())
        .args(["reopen", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: todo"));

    // Verify it's modifiable again
    story(dir.path())
        .args(["comment", "SH-1", "Added more context"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Added more context"));
}

#[test]
fn reopen_already_open_story_fails() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Open story"])
        .assert()
        .success();

    story(dir.path())
        .args(["reopen", "SH-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already open"));
}

#[test]
fn reopen_nonexistent_story_fails() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["reopen", "SH-999"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn reopen_preserves_comments() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Task with comments"])
        .assert()
        .success();
    story(dir.path())
        .args(["comment", "SH-1", "Important note"])
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
        .stdout(predicate::str::contains("Important note"));
}

#[test]
fn reopen_json_output() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Reopen me"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success();

    story(dir.path())
        .args(["--json", "reopen", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"result\": \"ok\""))
        .stdout(predicate::str::contains("\"state\": \"todo\""));
}
