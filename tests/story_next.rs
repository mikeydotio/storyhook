use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn next_returns_oldest_unblocked() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "First task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Second task"])
        .assert()
        .success();

    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-1"))
        .stdout(predicate::str::contains("First task"));
}

#[test]
fn next_respects_priority_sorting() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Low priority"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Critical priority"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-1", "priority", "low"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-2", "priority", "critical"])
        .assert()
        .success();

    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-2"))
        .stdout(predicate::str::contains("Critical priority"));
}

#[test]
fn next_skips_awaiting_stories() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Blocked task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Ready task"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-1", "awaits", "API spec"])
        .assert()
        .success();

    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-2"))
        .stdout(predicate::str::contains("Ready task"));
}

#[test]
fn next_skips_dependency_blocked_stories() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "First task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Depends on first"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-2", "follows", "SH-1"])
        .assert()
        .success();

    // SH-2 is blocked because SH-1 (which it follows) is still open
    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-1"))
        .stdout(predicate::str::contains("First task"));
}

#[test]
fn next_unblocks_after_dependency_closed() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "First task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Depends on first"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-2", "follows", "SH-1"])
        .assert()
        .success();

    // Close SH-1
    story(dir.path())
        .args(["SH-1", "is", "done"])
        .assert()
        .success();

    // Now SH-2 should be ready
    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-2"));
}

#[test]
fn next_count_returns_multiple() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();
    story(dir.path()).args(["new", "Task C"]).assert().success();

    let output = story(dir.path())
        .args(["next", "--count", "3"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SH-1"));
    assert!(stdout.contains("SH-2"));
    assert!(stdout.contains("SH-3"));
}

#[test]
fn next_json_output() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Build parser"])
        .assert()
        .success();

    story(dir.path())
        .args(["--json", "next"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"result\": \"ok\""))
        .stdout(predicate::str::contains("\"id\": \"SH-1\""));
}

#[test]
fn next_empty_project() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("no ready stories"));
}

#[test]
fn next_all_blocked_returns_no_ready() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Blocked task"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-1", "awaits", "external dependency"])
        .assert()
        .success();

    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("no ready stories"));
}
