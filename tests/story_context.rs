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
fn context_generates_markdown() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Build API"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Write docs"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-1", "high"])
        .assert()
        .success();

    story(dir.path())
        .args(["context"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Project Status"))
        .stdout(predicate::str::contains("2 open"))
        .stdout(predicate::str::contains("Ready to Work"))
        .stdout(predicate::str::contains("SH-1"));
}

#[test]
fn context_shows_blocked_stories() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Blocked task"])
        .assert()
        .success();
    story(dir.path())
        .args(["block", "SH-1", "external API"])
        .assert()
        .success();

    story(dir.path())
        .args(["context"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Blocked"))
        .stdout(predicate::str::contains("external API"));
}

#[test]
fn context_json_format() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "A task"]).assert().success();

    story(dir.path())
        .args(["context", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"open\": 1"))
        .stdout(predicate::str::contains("\"ready_count\": 1"));
}

#[test]
fn context_empty_project() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["context"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 open"));
}

#[test]
fn context_shows_type_distribution() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Fix login crash", "--type", "bug"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Add dashboard", "--type", "normal"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Untyped task"])
        .assert()
        .success();

    story(dir.path())
        .args(["context"])
        .assert()
        .success()
        .stdout(predicate::str::contains("## Type Distribution"))
        .stdout(predicate::str::contains("- bug: 1"))
        .stdout(predicate::str::contains("- normal: 1"))
        .stdout(predicate::str::contains("- Default: 1"));
}

#[test]
fn context_json_includes_type_distribution() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Fix login crash", "--type", "bug"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Untyped task"])
        .assert()
        .success();

    story(dir.path())
        .args(["context", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"by_type\""))
        .stdout(predicate::str::contains("\"bug\": 1"))
        .stdout(predicate::str::contains("\"Default\": 1"));
}
