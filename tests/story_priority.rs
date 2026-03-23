use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn set_and_show_priority() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Build parser"])
        .assert()
        .success();

    story(dir.path())
        .args(["SH-1", "priority", "high"])
        .assert()
        .success()
        .stdout(predicate::str::contains("priority: high"));

    story(dir.path())
        .arg("SH-1")
        .assert()
        .success()
        .stdout(predicate::str::contains("priority: high"));
}

#[test]
fn priority_defaults_to_none() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Build parser"])
        .assert()
        .success();

    story(dir.path())
        .arg("SH-1")
        .assert()
        .success()
        .stdout(predicate::str::contains("priority: none"));
}

#[test]
fn override_priority() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Build parser"])
        .assert()
        .success();

    story(dir.path())
        .args(["SH-1", "priority", "critical"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-1", "priority", "low"])
        .assert()
        .success()
        .stdout(predicate::str::contains("priority: low"));
}

#[test]
fn invalid_priority_fails() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Build parser"])
        .assert()
        .success();

    story(dir.path())
        .args(["SH-1", "priority", "urgent"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn list_filters_by_priority() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Low task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "High task"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-1", "priority", "low"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-2", "priority", "high"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["list", "--priority", "high"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SH-2"));
    assert!(!stdout.contains("SH-1"));
}

#[test]
fn priority_in_json_output() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Build parser"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-1", "priority", "critical"])
        .assert()
        .success();

    story(dir.path())
        .args(["--json", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"priority\": \"critical\""));
}

#[test]
fn list_shows_priority_in_line() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Important task"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-1", "priority", "high"])
        .assert()
        .success();

    story(dir.path())
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("(high)"));
}
