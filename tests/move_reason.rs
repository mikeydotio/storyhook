//! Tests for `story move <id> <state> --reason <text>` (SH-205) — the
//! opt-in `awaiting` field the council decided on (verdict on SH-205). Mirrors
//! `move_if_state.rs`'s pattern; the two flags share the same leading,
//! order-independent recognition window immediately after `<state>`.

use assert_cmd::Command;
use storyhook_test_support::{Project, TestEnv};

/// Every `story` invocation in this file runs in the shared test
/// environment's private HOME/XDG directories, so nothing here can reach the
/// developer's own storyhook state.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

/// A `TST`-prefixed project holding one story, and that story's id.
fn init_and_create() -> (Project<'static>, String) {
    let project = TestEnv::shared().project().prefix("TST").build();
    let id = project.new_story("Test story");
    (project, id)
}

#[test]
fn move_with_reason_sets_state_and_awaiting_atomically() {
    let (dir, id) = init_and_create();

    story(dir.path())
        .args(["move", &id, "blocked", "--reason", "waiting on SH-9"])
        .assert()
        .success();

    let show = story(dir.path())
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(show_json["story"]["story"]["state"], "blocked");
    assert_eq!(
        show_json["story"]["story"]["awaiting"], "waiting on SH-9",
        "{show_json}"
    );
}

#[test]
fn move_without_reason_is_unconditional_as_before() {
    // Backward compatibility: existing callers (humans, scripts, agents, CI)
    // that never pass --reason keep today's behavior — awaiting stays null.
    let (dir, id) = init_and_create();

    story(dir.path())
        .args(["move", &id, "blocked"])
        .assert()
        .success();

    let show = story(dir.path())
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(show_json["story"]["story"]["state"], "blocked");
    assert!(
        show_json["story"]["story"]["awaiting"].is_null(),
        "{show_json}"
    );
}

#[test]
fn move_with_reason_and_if_state_works_in_either_order() {
    let (dir, id) = init_and_create();

    story(dir.path())
        .args([
            "move",
            &id,
            "blocked",
            "--if-state",
            "todo",
            "--reason",
            "order one",
        ])
        .assert()
        .success();
    let show = story(dir.path())
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(show_json["story"]["story"]["awaiting"], "order one");

    // Unblock, then repeat with the flags swapped.
    story(dir.path()).args(["unblock", &id]).assert().success();
    story(dir.path())
        .args(["move", &id, "todo"])
        .assert()
        .success();

    story(dir.path())
        .args([
            "move",
            &id,
            "blocked",
            "--reason",
            "order two",
            "--if-state",
            "todo",
        ])
        .assert()
        .success();
    let show = story(dir.path())
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(show_json["story"]["story"]["awaiting"], "order two");
}

#[test]
fn move_reason_requires_a_value() {
    let (dir, id) = init_and_create();

    let out = story(dir.path())
        .args(["move", &id, "blocked", "--reason"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--reason requires a value"), "{stderr}");
}

#[test]
fn move_reason_combined_with_a_closed_state_is_refused() {
    let (dir, id) = init_and_create();

    let out = story(dir.path())
        .args(["move", &id, "done", "--reason", "why though"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot be combined with a move to a closed state"),
        "{stderr}"
    );

    let show = story(dir.path())
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(
        show_json["story"]["story"]["state"], "todo",
        "a refused move must not have happened: {show_json}"
    );
}

#[test]
fn move_comment_containing_reason_token_is_not_spliced_or_dropped() {
    // Mirrors move_if_state.rs's identical regression test for --if-state:
    // `--reason` is recognized only in the leading flag run immediately
    // after <state>; anywhere else it is inert comment text.
    let (dir, id) = init_and_create();

    story(dir.path())
        .args([
            "move",
            &id,
            "in-progress",
            "retry",
            "--reason",
            "later",
            "please",
        ])
        .assert()
        .success();

    let show = story(dir.path())
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(show_json["story"]["story"]["state"], "in-progress");
    assert!(
        show_json["story"]["story"]["awaiting"].is_null(),
        "--reason past the leading flag run must not be recognized: {show_json}"
    );
    let comments = show_json["story"]["story"]["comments"].as_array().unwrap();
    assert_eq!(
        comments.last().unwrap()["text"],
        "retry --reason later please",
        "an --reason-shaped substring inside free-text comment tokens must be preserved \
         verbatim: {show_json}"
    );
}

#[test]
fn move_with_typoed_reason_flag_is_refused() {
    let (dir, id) = init_and_create();

    let out = story(dir.path())
        .args(["move", &id, "blocked", "--raisin", "todo"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    assert_eq!(out.status.code(), Some(2), "a typoed flag is refused");
    assert!(stderr.contains("--raisin"), "{stderr}");
    assert!(stderr.contains("--reason"), "{stderr}");

    let show = story(dir.path())
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(
        show_json["story"]["story"]["state"], "todo",
        "the story must not have moved: a refused flag must not silently degrade to \
         an unguarded move"
    );
}
