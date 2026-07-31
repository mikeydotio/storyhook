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
fn search_by_title() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
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
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
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
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
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
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
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
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
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
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
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
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
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
