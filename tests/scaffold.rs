use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn scaffold_agents_md_contains_workflow_commands() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["scaffold", "agents-md"])
        .assert()
        .success()
        .stdout(predicate::str::contains("story next"))
        .stdout(predicate::str::contains("story load-context"));
}

#[test]
fn scaffold_agents_md_uses_project_prefix() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["init", "--prefix", "API"])
        .assert()
        .success();
    story(dir.path())
        .args(["scaffold", "agents-md"])
        .assert()
        .success()
        .stdout(predicate::str::contains("API-<n>"));
}

#[test]
fn scaffold_cursor_rules_contains_storyhook() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["scaffold", "cursor-rules"])
        .assert()
        .success()
        .stdout(predicate::str::contains("storyhook"));
}

#[test]
fn scaffold_claude_md_is_short_pointer() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["scaffold", "claude-md"])
        .assert()
        .success()
        .stdout(predicate::str::contains(".storyhook/CLAUDE.md"))
        .stdout(predicate::str::contains("story load-context"));
}

#[test]
fn init_creates_storyhook_claude_md_with_prefix() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["init", "--prefix", "WEB"])
        .assert()
        .success();
    let claude_md = std::fs::read_to_string(dir.path().join(".storyhook/CLAUDE.md")).unwrap();
    assert!(claude_md.contains("WEB-<n>"));
    assert!(claude_md.contains("story load-context"));
    assert!(claude_md.contains("Planning mode"));
}

#[test]
fn scaffold_invalid_kind_returns_error() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["scaffold", "invalid-kind"])
        .assert()
        .code(2);
}
