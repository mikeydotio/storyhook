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
fn summary_shows_counts() {
    let dir = tempdir().unwrap();
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
    let dir = tempdir().unwrap();
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
    let dir = tempdir().unwrap();
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
    let dir = tempdir().unwrap();
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
    let dir = tempdir().unwrap();
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
    let dir = tempdir().unwrap();
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
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // Create stories: 2 with no type (Default), 1 bug, 2 normal
    // "bug" and "normal" are default types from init
    story(dir.path())
        .args(["new", "Untyped A"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Untyped B"])
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
        .stdout(predicate::str::contains("Default: 2"))
        .stdout(predicate::str::contains("bug: 1"))
        .stdout(predicate::str::contains("normal: 2"));
}
