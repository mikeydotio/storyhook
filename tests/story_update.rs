//! Integration tests for `story update` and `story --version`.
//!
//! These are intentionally network-free: every case is either an argument-parse
//! error (rejected before any HTTP call) or a help/version query. The live
//! download/replace path is exercised manually, not in CI.

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn update_rejects_unknown_flag() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["update", "--bogus"])
        .assert()
        .code(2)
        .stdout(contains("usage: story update"));
}

#[test]
fn update_rejects_stray_positional() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["update", "foo"])
        .assert()
        .code(2)
        .stdout(contains("usage: story update"));
}

#[test]
fn update_check_and_force_are_mutually_exclusive() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["update", "--check", "--force"])
        .assert()
        .code(2)
        .stdout(contains("mutually exclusive"));
}

#[test]
fn update_is_a_recognized_command() {
    // A bad flag must yield the update-specific usage, NOT the top-level
    // "unknown command" error — proving the dispatch arm is wired.
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["update", "--bogus"])
        .assert()
        .code(2)
        .stdout(contains("usage: story update").and(contains("unknown command").not()));
}

#[test]
fn help_update_topic_exists() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["help", "update"])
        .assert()
        .success()
        .stdout(contains("story update"));
}

#[test]
fn top_level_help_lists_update() {
    Command::cargo_bin("story")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("story update"));
}

#[test]
fn version_flag_prints_version() {
    let expected = format!("story {}", env!("CARGO_PKG_VERSION"));
    Command::cargo_bin("story")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(expected.clone()));
    Command::cargo_bin("story")
        .unwrap()
        .arg("-V")
        .assert()
        .success()
        .stdout(contains(expected));
}
