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
fn search_by_title() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Build authentication"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Fix database"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["search", "authentication"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SH-1"));
    assert!(!stdout.contains("SH-2"));
}

#[test]
fn search_by_comment() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Task one"])
        .assert()
        .success();
    story(dir.path())
        .args(["comment", "SH-1", "Found the root cause in the parser"])
        .assert()
        .success();

    story(dir.path())
        .args(["search", "parser"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-1"));
}

#[test]
fn search_by_label() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Some task"])
        .assert()
        .success();
    story(dir.path())
        .args(["label", "SH-1", "backend"])
        .assert()
        .success();

    story(dir.path())
        .args(["search", "backend"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-1"));
}

#[test]
fn search_includes_archived_stories() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Completed auth work"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success();

    story(dir.path())
        .args(["search", "auth"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-1"));
}

#[test]
fn search_case_insensitive() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Build Authentication"])
        .assert()
        .success();

    story(dir.path())
        .args(["search", "authentication"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-1"));
}

#[test]
fn search_no_results() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Build parser"])
        .assert()
        .success();

    story(dir.path())
        .args(["search", "nonexistent"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no stories found"));
}

#[test]
fn search_json_output() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Build parser"])
        .assert()
        .success();

    story(dir.path())
        .args(["--json", "search", "parser"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"result\": \"ok\""))
        .stdout(predicate::str::contains("\"id\": \"SH-1\""));
}
