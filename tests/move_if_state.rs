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

#[test]
fn move_comment_starting_with_double_dash_is_preserved_verbatim() {
    // Regression: comments have always been unrestricted free text (see
    // move_without_if_state_is_unconditional_as_before and the pre-existing
    // `story comment` command, which stores any string verbatim including
    // one that starts with `--`). `--if-state` is only ever recognized in
    // the one position immediately after <state>, so a comment argument
    // that happens to begin with `--` must never be mistaken for an
    // unrecognized flag and rejected.
    let dir = tempdir().unwrap();
    let id = init_and_create(dir.path());

    story(dir.path())
        .args(["move", &id, "done", "-- deployed to prod"])
        .assert()
        .success();

    let show = story(dir.path())
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    let comments = show_json["story"]["story"]["comments"].as_array().unwrap();
    assert_eq!(
        comments.last().unwrap()["text"],
        "-- deployed to prod",
        "a comment beginning with `--` must be stored verbatim, not rejected as an unrecognized flag: {show_json}"
    );
}

#[test]
fn move_comment_containing_if_state_token_is_not_spliced_or_dropped() {
    // Regression: an unquoted, multi-word comment that happens to contain
    // the literal token `--if-state` anywhere past the first trailing
    // position must never be torn out of the comment and treated as a real
    // CAS guard — that would silently corrupt the stored comment (dropping
    // the flag-and-value pair) and activate an unintended precondition
    // check, both with zero diagnostic signal. `--if-state` is recognized
    // only when it is the very first trailing token (immediately after
    // <state>); anywhere else it is inert comment text, exactly like the
    // pre-existing unconditional `join_tokens(&args[3..])` behavior.
    let dir = tempdir().unwrap();
    let id = init_and_create(dir.path());

    story(dir.path())
        .args([
            "move",
            &id,
            "in-progress",
            "retry",
            "--if-state",
            "todo",
            "later",
        ])
        .assert()
        .success();

    let show = story(dir.path())
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(show_json["story"]["story"]["state"], "in-progress");
    let comments = show_json["story"]["story"]["comments"].as_array().unwrap();
    assert_eq!(
        comments.last().unwrap()["text"],
        "retry --if-state todo later",
        "an --if-state-shaped substring inside free-text comment tokens must be preserved verbatim, not spliced out: {show_json}"
    );
}

#[test]
fn move_with_typoed_flag_name_immediately_after_state_is_comment_not_error() {
    // A flag name is recognized only via an exact string match in the one
    // position immediately after <state> — a typo like `--if-stat`
    // (missing the trailing `e`) in that exact position is therefore never
    // mistaken for an attempt at the real flag. It is preserved as ordinary
    // comment text, and the move proceeds unconditionally (exactly as it
    // would have before `--if-state` existed), rather than being rejected
    // or silently treated as a defeated CAS guard.
    let dir = tempdir().unwrap();
    let id = init_and_create(dir.path());

    story(dir.path())
        .args(["move", &id, "in-progress", "--if-stat", "todo"])
        .assert()
        .success();

    let show = story(dir.path())
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(show_json["story"]["story"]["state"], "in-progress");
    let comments = show_json["story"]["story"]["comments"].as_array().unwrap();
    assert_eq!(comments.last().unwrap()["text"], "--if-stat todo");
}

#[test]
fn move_if_state_against_closed_story_reports_conflict_not_generic_error() {
    // Repro: process A does an unconditional `move ... done` (closing and
    // archiving the story). Process B — which read the story before it
    // closed — retries with its now-stale --if-state and must be told
    // "conflict", not `ensure_open_story`'s generic "closed and cannot be
    // modified" validation error, since a caller branching on
    // result == "conflict" must not misclassify a benign lost race as a
    // fatal, unexpected error.
    let dir = tempdir().unwrap();
    let id = init_and_create(dir.path());

    story(dir.path())
        .args(["move", &id, "done"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["move", &id, "in-progress", "--if-state", "todo", "--json"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["result"], "conflict",
        "a closed story must still surface as a CAS conflict when if_state doesn't match, got: {json}"
    );
    assert_eq!(json["expected"], "todo");
    assert_eq!(json["actual"], "done");
}

#[test]
fn move_if_state_under_real_concurrency_yields_exactly_one_winner() {
    // The entire reason --if-state exists is to make a real compare-and-swap
    // safe under concurrent access (conductor's claim protocol). A
    // sequential CLI invocation proves nothing about the check-then-write
    // guard under contention — this spawns real, concurrent OS processes
    // racing the same story through the same advisory project lock
    // (src/lock.rs), so a future reordering of the if_state comparison
    // relative to `lock::with_project_lock` (a TOCTOU regression) would
    // fail this test even though every sequential test above stays green.
    use std::process::{Command, Stdio};

    let dir = tempdir().unwrap();
    let id = init_and_create(dir.path());

    let show = story(dir.path())
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    let current_state = show_json["story"]["story"]["state"]
        .as_str()
        .unwrap()
        .to_string();

    let binary = assert_cmd::cargo::cargo_bin("story");
    const ATTEMPTS: usize = 20;

    // Spawn every attempt before waiting on any of them — this is what
    // actually races real processes against the project lock, rather than
    // just inferring concurrency safety from back-to-back sequential calls.
    let children: Vec<_> = (0..ATTEMPTS)
        .map(|_| {
            Command::new(&binary)
                .current_dir(dir.path())
                .args([
                    "move",
                    &id,
                    "in-progress",
                    "--if-state",
                    &current_state,
                    "--json",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("failed to spawn concurrent `story move`")
        })
        .collect();

    let mut successes = 0usize;
    let mut conflicts = 0usize;
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.stderr.is_empty(),
            "no concurrent attempt should print to stderr: {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|e| panic!("non-JSON output ({e}): {output:?}"));
        if output.status.success() {
            assert_eq!(json["result"], "ok");
            successes += 1;
        } else {
            assert_eq!(
                json["result"], "conflict",
                "a losing concurrent attempt must fail as a conflict, not an undifferentiated error, got: {json}"
            );
            assert_eq!(json["expected"], current_state);
            conflicts += 1;
        }
    }

    assert_eq!(
        successes, 1,
        "exactly one of {ATTEMPTS} concurrent claimants must win the CAS race"
    );
    assert_eq!(conflicts, ATTEMPTS - 1);

    // The story's own event log must be consistent with a single winner --
    // exactly one StoryStateChanged event landed, not zero (lost write) and
    // not many (double write / corruption).
    let events_path = dir
        .path()
        .join(".storyhook")
        .join("open")
        .join("stories")
        .join(format!("{id}.jsonl"));
    let events_raw = std::fs::read_to_string(&events_path).unwrap();
    let state_change_count = events_raw
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|event| event["kind"] == "StoryStateChanged")
        .count();
    assert_eq!(
        state_change_count, 1,
        "exactly one StoryStateChanged event must be recorded, found the raw log: {events_raw}"
    );
}
