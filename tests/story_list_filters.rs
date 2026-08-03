// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

use assert_cmd::Command;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn list_blocked_filter() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Ready task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Blocked task"])
        .assert()
        .success();
    story(dir.path())
        .args(["block", "SH-2", "external API"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["list", "--blocked"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(!stdout.contains("SH-1"));
    assert!(stdout.contains("SH-2"));
}

#[test]
fn list_ready_filter() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Ready task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Blocked task"])
        .assert()
        .success();
    story(dir.path())
        .args(["block", "SH-2", "external API"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["list", "--ready"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SH-1"));
    assert!(!stdout.contains("SH-2"));
}

#[test]
fn list_dependency_blocked() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "First task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Second task"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-2", "blocked-by", "SH-1"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["list", "--blocked"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(!stdout.contains("SH-1"));
    assert!(stdout.contains("SH-2"));
}

#[test]
fn list_combined_filters() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "High ready"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Low ready"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "High blocked"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-1", "high"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-2", "low"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-3", "high"])
        .assert()
        .success();
    story(dir.path())
        .args(["block", "SH-3", "review"])
        .assert()
        .success();

    // Filter: ready + high priority
    let output = story(dir.path())
        .args(["list", "--ready", "--priority", "high"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SH-1"));
    assert!(!stdout.contains("SH-2"));
    assert!(!stdout.contains("SH-3"));
}

#[test]
fn list_stale_basic() {
    // --stale 0m means threshold = now, so everything with updated_at < now is stale
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Task one"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Task two"])
        .assert()
        .success();

    // With 0m stale threshold, all open stories created even a fraction of a second ago qualify
    let output = story(dir.path())
        .args(["list", "--stale", "0m"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SH-1"));
    assert!(stdout.contains("SH-2"));
}

#[test]
fn list_stale_no_matches() {
    // --stale 999d means threshold = now - 999 days; nothing is that old
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Fresh task"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["list", "--stale", "999d"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("no stories found"));
}

#[test]
fn list_stale_combined() {
    // Combine --stale with --priority
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "High prio task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Low prio task"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-1", "high"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-2", "low"])
        .assert()
        .success();

    // Both are stale (0m threshold), filter to high priority only
    let output = story(dir.path())
        .args(["list", "--stale", "0m", "--priority", "high"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SH-1"));
    assert!(!stdout.contains("SH-2"));
}
