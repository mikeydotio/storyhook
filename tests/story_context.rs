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
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Build API"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Write docs"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-1", "priority", "high"])
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
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Blocked task"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-1", "awaits", "external API"])
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
    story(dir.path()).arg("init").assert().success();
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
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["context"])
        .assert()
        .success()
        .stdout(predicate::str::contains("0 open"));
}
