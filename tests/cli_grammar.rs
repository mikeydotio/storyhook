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
        .success()
        .stdout(predicate::str::contains("deleted SH-1: created in error"));

    // Soft delete: archived (JSONL removed from open/stories/), not erased —
    // still readable, but as a CLOSED, marked-deleted story. See
    // tests/story_delete.rs for full coverage of #18 (deletion must flip
    // superstate to CLOSED so it stops counting as open/ready/a blocker).
    assert!(
        !dir.path()
            .join(".storyhook/open/stories/SH-1.jsonl")
            .exists()
    );
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Test story"))
        .stdout(predicate::str::contains("state: todo (CLOSED, deleted)"))
        .stdout(predicate::str::contains("deleted_reason: created in error"));
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
fn set_json_patch_applies_fields() {
    // `story set <id> --json '...'` applies a JSON patch to the story.
    // split_global_flags detects that --json is followed by a value arg
    // and keeps it for the subcommand parser instead of consuming it as
    // the global JSON-output flag.
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
        .success()
        .stdout(predicate::str::contains("title -> JSON title"))
        .stdout(predicate::str::contains("priority -> critical"));
}

// Regression: #39 — the `--json` patch's "assignee" key wrote the raw input
// as `member_id` with no membership check, unlike the `--assignee` flag
// (parsed by the same `set` command) right next to it, which already
// validated via `storage::find_member`. See also `story_new_fields.rs`'s
// `new_with_unknown_assignee_is_rejected_and_creates_no_story`.

#[test]
fn set_json_patch_unknown_assignee_is_rejected_and_does_not_set() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    story(dir.path())
        .args(["set", "SH-1", "--json", r#"{"assignee":"nobody"}"#])
        .assert()
        .failure()
        .stderr(predicate::str::contains("member `nobody` not found"));

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("assignee: -"));
}

#[test]
fn set_json_patch_assignee_by_github_handle_normalizes_to_member_id() {
    let dir = tempdir().unwrap();
    init_and_create(dir.path());

    // `id` is a lowercased slug of the handle, so this member's id
    // ("mikeyward") differs in case from its github handle ("MikeyWard").
    story(dir.path())
        .args(["member", "add", "-g", "MikeyWard"])
        .assert()
        .success();

    story(dir.path())
        .args(["set", "SH-1", "--json", r#"{"assignee":"MikeyWard"}"#])
        .assert()
        .success()
        .stdout(predicate::str::contains("assignee -> MikeyWard"));

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("assignee: mikeyward"));
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

// -----------------------------------------------------------------------
// Phase tests
// -----------------------------------------------------------------------

#[test]
fn phase_list_empty() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "A task"]).assert().success();

    story(dir.path())
        .args(["phase", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no phases found"));
}

#[test]
fn phase_add_and_list() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();
    story(dir.path()).args(["new", "Task C"]).assert().success();

    // Assign stories to phases
    story(dir.path())
        .args(["phase", "add", "SH-1", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("assigned SH-1 to phase 1"));
    story(dir.path())
        .args(["phase", "add", "SH-2", "1"])
        .assert()
        .success();
    story(dir.path())
        .args(["phase", "add", "SH-3", "2"])
        .assert()
        .success();

    // List phases
    let output = story(dir.path()).args(["phase", "list"]).assert().success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("Phase 1"), "should show Phase 1");
    assert!(stdout.contains("Phase 2"), "should show Phase 2");
}

#[test]
fn phase_show() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();

    story(dir.path())
        .args(["phase", "add", "SH-1", "1"])
        .assert()
        .success();
    story(dir.path())
        .args(["phase", "add", "SH-2", "2"])
        .assert()
        .success();

    // Show phase 1 should only have Task A
    story(dir.path())
        .args(["phase", "show", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Task A"))
        .stdout(predicate::str::contains("Task B").not());
}

#[test]
fn phase_create() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["phase", "create", "1", "Foundation"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Phase 1: Foundation"))
        .stdout(predicate::str::contains("phase:1"));
}

#[test]
fn phase_remove() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Task A"]).assert().success();

    story(dir.path())
        .args(["phase", "add", "SH-1", "1"])
        .assert()
        .success();

    story(dir.path())
        .args(["phase", "remove", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed phase assignment"));

    // Verify no phase label remains
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("phase:").not());
}

#[test]
fn list_with_phase_filter() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();

    story(dir.path())
        .args(["phase", "add", "SH-1", "1"])
        .assert()
        .success();

    // List with --phase 1 should only show Task A
    story(dir.path())
        .args(["list", "--phase", "1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Task A"))
        .stdout(predicate::str::contains("Task B").not());
}

#[test]
fn next_with_phase_filter() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();

    story(dir.path())
        .args(["phase", "add", "SH-2", "2"])
        .assert()
        .success();

    // Next with --phase 2 should only return Task B
    story(dir.path())
        .args(["next", "--phase", "2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Task B"));
}

#[test]
fn decompose_sets_phase_labels() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    let spec = "### Wave 1\n- [ ] Task A\n- [ ] Task B\n### Wave 2\n- [ ] Task C\n";
    let spec_path = dir.path().join("spec.md");
    std::fs::write(&spec_path, spec).unwrap();

    story(dir.path())
        .args(["decompose", "spec.md"])
        .assert()
        .success();

    // Task A and B should have phase:1 label
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("phase:1"));

    story(dir.path())
        .args(["show", "SH-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("phase:1"));

    // Task C should have phase:2 label
    story(dir.path())
        .args(["show", "SH-3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("phase:2"));
}

#[test]
fn load_context_shows_phases() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();

    story(dir.path())
        .args(["phase", "add", "SH-1", "1"])
        .assert()
        .success();
    story(dir.path())
        .args(["phase", "add", "SH-2", "2"])
        .assert()
        .success();

    story(dir.path())
        .args(["load-context"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Phase Progress"))
        .stdout(predicate::str::contains("Phase 1"))
        .stdout(predicate::str::contains("Phase 2"));
}

#[test]
fn old_context_alias_works() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Task A"]).assert().success();

    // "context" should still work as an alias for "load-context"
    story(dir.path())
        .args(["context"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Project Status"));
}

#[test]
fn load_context_basic() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Task A"]).assert().success();

    story(dir.path())
        .args(["load-context"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Project Status"))
        .stdout(predicate::str::contains("1 open"));
}
