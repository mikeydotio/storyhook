use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn summary_shows_counts() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();
    story(dir.path()).args(["new", "Task C"]).assert().success();
    story(dir.path())
        .args(["SH-3", "is", "done"])
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
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();
    story(dir.path())
        .args(["SH-1", "priority", "high"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-2", "priority", "critical"])
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
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Ready A"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Blocked B"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-2", "awaits", "waiting on API"])
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

#[test]
fn summary_json_output() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
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
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .arg("summary")
        .assert()
        .success()
        .stdout(predicate::str::contains("stories: 0 (0 open, 0 closed)"));
}
