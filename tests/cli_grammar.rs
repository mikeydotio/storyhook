use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

fn init_and_create(dir: &std::path::Path) {
    story(dir).arg("init").assert().success();
    story(dir).args(["new", "Test story"]).assert().success();
}

#[test]
fn show_displays_story() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Test story"));
}

#[test]
fn comment_adds_comment() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    story(dir.path())
        .args(["comment", "SH-1", "my note"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("my note"));
}

#[test]
fn assign_sets_assignee() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    story(dir.path())
        .args(["member", "add", "Test User <test@test.com>"])
        .assert()
        .success();

    story(dir.path())
        .args(["assign", "SH-1", "test-user"])
        .assert()
        .success();
}

#[test]
fn move_changes_state() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    story(dir.path())
        .args(["move", "SH-1", "in-progress"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: in-progress"));
}

#[test]
fn move_to_done_archives() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success()
        .stdout(predicate::str::contains("closed_at:"));

    // Archived stories still appear in list with [done] state
    story(dir.path())
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("[done]"));

    // JSONL file is moved out of open/stories/
    assert!(
        !dir.path()
            .join(".storyhook/open/stories/SH-1.jsonl")
            .exists()
    );
}

#[test]
fn block_sets_awaiting() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    story(dir.path())
        .args(["block", "SH-1", "waiting for API"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("waiting for API"));
}

#[test]
fn unblock_clears_awaiting() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    story(dir.path())
        .args(["block", "SH-1", "waiting for API"])
        .assert()
        .success();

    story(dir.path())
        .args(["unblock", "SH-1"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("awaiting:").not());
}

#[test]
fn prioritize_sets_priority() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    story(dir.path())
        .args(["prioritize", "SH-1", "high"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("priority: high"));
}

#[test]
fn label_adds_labels() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    story(dir.path())
        .args(["label", "SH-1", "bug,frontend"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("bug"))
        .stdout(predicate::str::contains("frontend"));
}

#[test]
fn unlabel_removes_labels() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    story(dir.path())
        .args(["label", "SH-1", "bug,frontend"])
        .assert()
        .success();

    story(dir.path())
        .args(["unlabel", "SH-1", "bug"])
        .assert()
        .success();
}

#[test]
fn reopen_reopens() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success();

    story(dir.path())
        .args(["reopen", "SH-1"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Test story"));
}

#[test]
fn delete_soft_deletes() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    story(dir.path())
        .args(["delete", "SH-1", "created in error"])
        .assert()
        .success();
}

#[test]
fn relate_adds_relationship() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());
    story(dir.path())
        .args(["new", "Second story"])
        .assert()
        .success();

    story(dir.path())
        .args(["relate", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("blocks"))
        .stdout(predicate::str::contains("SH-2"));
}

#[test]
fn unrelate_removes_relationship() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());
    story(dir.path())
        .args(["new", "Second story"])
        .assert()
        .success();

    story(dir.path())
        .args(["relate", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();

    story(dir.path())
        .args(["unrelate", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();
}

#[test]
fn link_is_alias_for_relate() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());
    story(dir.path())
        .args(["new", "Second story"])
        .assert()
        .success();

    story(dir.path())
        .args(["link", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();
}

#[test]
fn unlink_is_alias_for_unrelate() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());
    story(dir.path())
        .args(["new", "Second story"])
        .assert()
        .success();

    story(dir.path())
        .args(["link", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();

    story(dir.path())
        .args(["unlink", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();
}

#[test]
fn set_batch_updates() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    story(dir.path())
        .args(["set", "SH-1", "--title", "New title", "--priority", "high"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("New title"))
        .stdout(predicate::str::contains("priority: high"));
}

#[test]
fn set_json_flag_conflicts_with_global_json() {
    // The `story set <id> --json '...'` flag is documented but currently
    // conflicts with the global `--json` output flag: split_global_flags
    // strips all `--json` tokens before parse_set sees them.
    // This test documents the current behavior: the command fails with a
    // usage error because the JSON body becomes a bare positional arg.
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    story(dir.path())
        .args([
            "set",
            "SH-1",
            "--json",
            r#"{"title":"JSON title","priority":"critical"}"#,
        ])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn set_blocked_via_set() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    story(dir.path())
        .args(["set", "SH-1", "--blocked", "waiting for design"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("waiting for design"));
}

#[test]
fn set_unblocked_via_set() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    story(dir.path())
        .args(["block", "SH-1", "waiting for design"])
        .assert()
        .success();

    story(dir.path())
        .args(["set", "SH-1", "--unblocked"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("awaiting:").not());
}

#[test]
fn old_syntax_errors() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    // "story SH-1 is done" should fail — SH-1 is not a valid command
    story(dir.path())
        .args(["SH-1", "is", "done"])
        .assert()
        .failure();

    // "story SH-1 assign mikey" should fail
    story(dir.path())
        .args(["SH-1", "assign", "mikey"])
        .assert()
        .failure();

    // "story SH-1 reopen" should fail
    story(dir.path())
        .args(["SH-1", "reopen"])
        .assert()
        .failure();
}

#[test]
fn new_with_state() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["new", "My story", "--state", "in-progress"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: in-progress"));
}
