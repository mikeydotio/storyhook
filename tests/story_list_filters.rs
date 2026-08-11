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

/// Regression test for SH-236, `story list --ready`'s side of the same bug
/// `next_skips_a_story_already_in_progress` (tests/story_next.rs) pins for
/// `story next`: a story already claimed (moved to `in-progress`) is neither
/// ready to pick up nor blocked — it's excluded from both filters.
#[test]
fn list_ready_filter_excludes_in_progress() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Todo task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Claimed task"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-2", "in-progress"])
        .assert()
        .success();

    let ready = story(dir.path())
        .args(["list", "--ready"])
        .assert()
        .success();
    let ready_stdout = String::from_utf8(ready.get_output().stdout.clone()).unwrap();
    assert!(ready_stdout.contains("SH-1"));
    assert!(!ready_stdout.contains("SH-2"));

    let blocked = story(dir.path())
        .args(["list", "--blocked"])
        .assert()
        .success();
    let blocked_stdout = String::from_utf8(blocked.get_output().stdout.clone()).unwrap();
    assert!(!blocked_stdout.contains("SH-2"));
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
