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
fn graph_overview() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();
    story(dir.path())
        .args(["relate", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();

    story(dir.path())
        .args(["graph"])
        .assert()
        .success()
        .stdout(predicate::str::contains("open stories: 2"))
        .stdout(predicate::str::contains("dependency edges:"));
}

#[test]
fn graph_critical_path() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();
    story(dir.path()).args(["new", "First"]).assert().success();
    story(dir.path()).args(["new", "Second"]).assert().success();
    story(dir.path()).args(["new", "Third"]).assert().success();
    story(dir.path())
        .args(["relate", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-2", "blocks", "SH-3"])
        .assert()
        .success();

    story(dir.path())
        .args(["graph", "--critical-path"])
        .assert()
        .success()
        .stdout(predicate::str::contains("3 stories"))
        .stdout(predicate::str::contains("SH-1 -> SH-2 -> SH-3"));
}

#[test]
fn graph_blocked_by() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Blocker"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Blocked"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Also blocked"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-2", "blocks", "SH-3"])
        .assert()
        .success();

    story(dir.path())
        .args(["graph", "--blocked-by", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("blocked by SH-1"))
        .stdout(predicate::str::contains("SH-2"))
        .stdout(predicate::str::contains("SH-3"));
}

#[test]
fn graph_parallel_groups() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();
    // Group 1: SH-1 -> SH-2
    story(dir.path())
        .args(["new", "Group1 A"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Group1 B"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();
    // Group 2: SH-3 (independent)
    story(dir.path())
        .args(["new", "Independent"])
        .assert()
        .success();

    story(dir.path())
        .args(["graph", "--parallel-groups"])
        .assert()
        .success()
        .stdout(predicate::str::contains("parallel groups: 2"));
}

#[test]
fn graph_json_output() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Task"]).assert().success();

    story(dir.path())
        .args(["--json", "graph"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"result\": \"ok\""))
        .stdout(predicate::str::contains("\"total_open\": 1"));
}

#[test]
fn graph_no_dependencies() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Solo task"])
        .assert()
        .success();

    story(dir.path())
        .args(["graph", "--critical-path"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1 stories"));
}

#[test]
fn graph_blocked_by_nonexistent() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();

    story(dir.path())
        .args(["graph", "--blocked-by", "SH-999"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}
