//! Tests for the `--if-state` compare-and-swap guard on `story move`.
//!
//! Conductor's storyhook-dispatch migration needs a real compare-and-swap:
//! attempt a state transition only if the story is still in the state the
//! caller read moments earlier. These tests pin that contract at the CLI
//! boundary, following the `assert_cmd`/`tempfile` pattern already used in
//! `tests/fix_cycle_5.rs`.

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
fn move_with_matching_if_state_succeeds() {
    let (dir, id) = init_and_create();

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
    let (dir, id) = init_and_create();

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
    let (dir, id) = init_and_create();
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
    let (dir, id) = init_and_create();

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
    let (dir, id) = init_and_create();

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
fn move_with_typoed_flag_name_immediately_after_state_is_refused() {
    // This test used to assert the opposite, and the premise was wrong (SH-62).
    //
    // It required `--if-stat` — `--if-state` missing its last letter — to be
    // kept as ordinary comment text while "the move proceeds unconditionally".
    // The reasoning was sound about one thing: a typo must never be *mistaken*
    // for the real flag, because silently treating it as a defeated CAS guard
    // would be the worst outcome of the three. But it drew the wrong conclusion
    // from that, and blessed the second-worst: the user asked for a guarded
    // move, the guard was silently absent, and the story moved anyway. Nothing
    // told them. That is the exact harm SH-62 was filed about — "the user
    // believes they set a field, and instead they wrote garbage" — and here the
    // field is a concurrency guard rather than a title.
    //
    // There is a third option and it is the correct one: refuse, and name the
    // token. The move does not happen, so no guard is defeated and none is
    // silently skipped.
    let (dir, id) = init_and_create();

    let out = story(dir.path())
        .args(["move", &id, "in-progress", "--if-stat", "todo"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();

    assert_eq!(out.status.code(), Some(2), "a typoed flag is refused");
    assert!(
        stderr.contains("--if-stat"),
        "the refusal names the token the user actually typed: {stderr}"
    );
    assert!(
        stderr.contains("--if-state"),
        "and lists the real flag, which is the whole repair: {stderr}"
    );

    let show = story(dir.path())
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(
        show_json["story"]["story"]["state"], "todo",
        "the story must not have moved: a guarded move whose guard did not \
         parse must not fall back to an unguarded one"
    );
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
    let (dir, id) = init_and_create();

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
fn move_if_state_against_deleted_story_reports_conflict_not_generic_error() {
    // Repro: process A does `story delete` — which, by design, forces
    // superstate CLOSED but leaves the `state` slug unchanged (see
    // fold_story's deleted handling in src/domain.rs). Process B read the
    // story before the delete and retries its now-stale --if-state whose
    // value still equals the *unchanged* state slug. A CAS guard that only
    // compares the slug would let this fall through to `ensure_open_story`,
    // which raises a generic "closed and cannot be modified" validation
    // error instead of a conflict -- exactly the failure mode this test
    // pins shut, mirroring move_if_state_against_closed_story_reports_
    // conflict_not_generic_error for the delete path specifically.
    let (dir, id) = init_and_create();

    let show = story(dir.path())
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    let original_state = show_json["story"]["story"]["state"]
        .as_str()
        .unwrap()
        .to_string();

    story(dir.path())
        .args(["delete", &id, "created in error"])
        .assert()
        .success();

    let output = story(dir.path())
        .args([
            "move",
            &id,
            "in-progress",
            "--if-state",
            &original_state,
            "--json",
        ])
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "a stale --if-state against a deleted story must not succeed"
    );
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["result"], "conflict",
        "a deleted story must still surface as a CAS conflict, not the generic \
         'closed and cannot be modified' validation error, even though the \
         state slug itself never changed: {json}"
    );
    assert_eq!(json["expected"], original_state);
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
    use std::process::Stdio;

    let (dir, id) = init_and_create();

    let show = story(dir.path())
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    let current_state = show_json["story"]["story"]["state"]
        .as_str()
        .unwrap()
        .to_string();

    let env = dir.env();
    const ATTEMPTS: usize = 20;

    // Spawn every attempt before waiting on any of them — this is what
    // actually races real processes against the project lock, rather than
    // just inferring concurrency safety from back-to-back sequential calls.
    let children: Vec<_> = (0..ATTEMPTS)
        .map(|_| {
            env.raw_story(dir.path())
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

    // The story's own history must be consistent with a single winner --
    // exactly one StoryStateChanged event landed, not zero (a lost write) and
    // not many (a double write). This is the double-write detector, and it has
    // to survive the storage change: it is asked of the events table now
    // rather than of a JSONL file, and it is the same question.
    let history = env
        .story(dir.path())
        .args(["--json", "show", &id])
        .output()
        .expect("running story show");
    let view: serde_json::Value =
        serde_json::from_slice(&history.stdout).expect("show --json must print JSON");
    assert_eq!(
        view["story"]["story"]["state"], "in-progress",
        "the winner's state must be the one it claimed: {view}"
    );

    use storyhook::store::{ReadOps as _, Store as _};
    let store = dir.open_store();
    let id_in_store = dir.project_id(&store);
    let story_no = dir.story_no(&store, &id);
    let events = store
        .read(|tx| tx.events_for(id_in_store, story_no))
        .expect("reading the story's events");
    let state_changes = events
        .iter()
        .filter(|event| event.kind == "StoryStateChanged")
        .count();
    assert_eq!(
        state_changes, 1,
        "exactly one StoryStateChanged event must be recorded, found {events:#?}"
    );
}
