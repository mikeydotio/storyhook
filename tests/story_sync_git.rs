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

fn init_git(dir: &std::path::Path) {
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .unwrap();
}

fn git_commit(dir: &std::path::Path, message: &str) {
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", message])
        .current_dir(dir)
        .output()
        .unwrap();
}

#[test]
fn sync_git_basic() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Fix login bug"])
        .assert()
        .success();

    git_commit(dir.path(), "Fix SH-1 bug");

    story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("linked 1 commits to 1 stories"));

    // Verify the story now has a [git] comment
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[git]"))
        .stdout(predicate::str::contains("Fix SH-1 bug"));
}

#[test]
fn sync_git_multiple_ids() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "First story"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Second story"])
        .assert()
        .success();

    git_commit(dir.path(), "Fixes SH-1 and SH-2");

    story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("linked 2 commits to 2 stories"));
}

#[test]
fn sync_git_no_matches() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Some story"])
        .assert()
        .success();

    git_commit(dir.path(), "Refactor internal code");

    story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("linked 0 commits to 0 stories"));
}

#[test]
fn sync_git_idempotent() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Idempotent story"])
        .assert()
        .success();

    git_commit(dir.path(), "Fix SH-1 issue");

    // First sync
    story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("linked 1 commits"));

    // Second sync should add nothing
    story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("linked 0 commits to 0 stories"));
}

#[test]
fn sync_git_not_a_git_repo() {
    let dir = tempdir().unwrap();
    // No git init -- just story init
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["sync-git"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("not a git repository"));
}

/// SH-279: a merge commit naming a story its own PR just closed is the shape
/// this project's own workflow produces on every merge, and it must not
/// vanish from the record. The story links; it does not move — moving is
/// still refused, exactly as before.
#[test]
fn sync_git_closed_story_linked_but_not_moved() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Closable story"])
        .assert()
        .success();

    // Close the story (archives it)
    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success();

    git_commit(dir.path(), "Ref SH-1 after close");

    story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("linked 1 commits to 1 stories"));

    story(dir.path())
        .args(["show", "SH-1", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"state\": \"done\""));
}

#[test]
fn sync_git_custom_prefix() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path())
        .args(["project", "new", "--prefix", "API"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "API endpoint"])
        .assert()
        .success();

    git_commit(dir.path(), "Fix API-1 endpoint");

    story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("linked 1 commits to 1 stories"));

    story(dir.path())
        .args(["show", "API-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[git]"))
        .stdout(predicate::str::contains("Fix API-1 endpoint"));
}

#[test]
fn sync_git_since_flag() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Recent work"])
        .assert()
        .success();

    git_commit(dir.path(), "Work on SH-1");

    // --since 0m should still pick up the very recent commit
    // (0m means 0 minutes ago, which is now)
    story(dir.path())
        .args(["sync-git", "--since", "1m"])
        .assert()
        .success()
        .stdout(predicate::str::contains("scanned"));
}

#[test]
fn sync_git_summary_message() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Story one"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Story two"])
        .assert()
        .success();

    git_commit(dir.path(), "Fix SH-1");
    git_commit(dir.path(), "Fix SH-2");
    git_commit(dir.path(), "Unrelated commit");

    story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "scanned 3 commits, linked 2 commits to 2 stories",
        ));
}

#[test]
fn sync_git_auto_transition_with_role() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // in-progress (with role=active) is now a default state

    // Create a story (starts in "todo")
    story(dir.path())
        .args(["new", "Auto-transition test"])
        .assert()
        .success();

    // Make a git commit *claiming* the story. This test is about which state
    // the transition targets, so the commit has to be one that transitions —
    // a bare mention only links (SH-124).
    git_commit(dir.path(), "Implements SH-1 feature");

    // Run sync-git
    let assert = story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success();

    let output = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        output.contains("linked 1 commits to 1 stories"),
        "unexpected output: {output}"
    );
    assert!(
        output.contains("SH-1: todo"),
        "expected transition message, got: {output}"
    );
    assert!(
        output.contains("in-progress"),
        "expected transition to in-progress, got: {output}"
    );

    // Verify the story is now in-progress
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: in-progress"));
}

#[test]
fn sync_git_no_transition_without_active_state() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // Clear the `active` role rather than removing the state that holds it:
    // `in-progress` is required of every project now (SH-125), so a project
    // with no active state is one where nothing carries the role.
    story(dir.path())
        .args(["state", "set", "in-progress", "--role", "none"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "No active state test"])
        .assert()
        .success();

    git_commit(dir.path(), "Fix SH-1 bug");

    let assert = story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success();

    let output = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        output.contains("linked 1 commits to 1 stories"),
        "unexpected output: {output}"
    );
    // Should NOT contain a transition line
    assert!(
        !output.contains("\u{2192}"),
        "should not have transition, got: {output}"
    );

    // Story stays in todo
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: todo"));
}

#[test]
fn sync_git_no_re_transition() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // in-progress (with role=active) is now a default state

    story(dir.path())
        .args(["new", "Already transitioned"])
        .assert()
        .success();

    // Manually set the story to in-progress
    story(dir.path())
        .args(["move", "SH-1", "in-progress"])
        .assert()
        .success();

    // Make a git commit referencing the story
    git_commit(dir.path(), "More work on SH-1");

    let assert = story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success();

    let output = String::from_utf8_lossy(&assert.get_output().stdout);
    // Should NOT contain a transition line since story is already in-progress
    assert!(
        !output.contains("\u{2192}"),
        "should not re-transition, got: {output}"
    );
}
