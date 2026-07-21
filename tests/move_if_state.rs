//! Tests for the `--if-state` compare-and-swap guard on `story move`.
//!
//! Conductor's storyhook-dispatch migration needs a real compare-and-swap:
//! attempt a state transition only if the story is still in the state the
//! caller read moments earlier. These tests pin that contract at the CLI
//! boundary, following the `assert_cmd`/`tempfile` pattern already used in
//! `tests/fix_cycle_5.rs`.

use assert_cmd::Command;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

fn init_and_create(dir: &std::path::Path) -> String {
    story(dir)
        .args(["init", "--prefix", "TST"])
        .assert()
        .success();
    let output = story(dir)
        .args(["new", "Test story", "--json"])
        .output()
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    json["story"]["story"]["id"].as_str().unwrap().to_string()
}

#[test]
fn move_with_matching_if_state_succeeds() {
    let dir = tempdir().unwrap();
    let id = init_and_create(dir.path());

    // Whatever state `new` seeds stories into — read it back rather than
    // hardcoding, so this test doesn't assume storyhook's default state name.
    let show = story(dir.path())
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    let current_state = show_json["story"]["story"]["state"].as_str().unwrap();

    story(dir.path())
        .args(["move", &id, "in-progress", "--if-state", current_state])
        .assert()
        .success();
}

#[test]
fn move_with_stale_if_state_conflicts_not_errors() {
    let dir = tempdir().unwrap();
    let id = init_and_create(dir.path());

    // First mover wins.
    story(dir.path())
        .args(["move", &id, "in-progress"])
        .assert()
        .success();

    // Second mover reads a stale state and must be told "conflict", not
    // succeed silently and not surface as an undifferentiated error.
    let output = story(dir.path())
        .args(["move", &id, "in-progress", "--if-state", "todo", "--json"])
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "a stale --if-state must not succeed"
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["result"], "conflict",
        "conflict must be machine-distinguishable from a generic error, got: {json}"
    );
    assert_eq!(json["expected"], "todo");
}

#[test]
fn move_without_if_state_is_unconditional_as_before() {
    // Backward compatibility: existing callers (humans, the web dashboard,
    // /storyhook:work) that never pass --if-state keep today's behavior.
    let dir = tempdir().unwrap();
    let id = init_and_create(dir.path());
    story(dir.path())
        .args(["move", &id, "in-progress"])
        .assert()
        .success();
}
