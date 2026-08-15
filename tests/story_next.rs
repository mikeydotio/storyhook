// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

use storyhook_test_support::TestEnv;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn next_returns_oldest_unblocked() {
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
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-1"))
        .stdout(predicate::str::contains("First task"));
}

#[test]
fn next_respects_priority_sorting() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Low priority"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Critical priority"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-1", "low"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-2", "critical"])
        .assert()
        .success();

    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-2"))
        .stdout(predicate::str::contains("Critical priority"));
}

#[test]
fn next_skips_awaiting_stories() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Blocked task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Ready task"])
        .assert()
        .success();
    story(dir.path())
        .args(["block", "SH-1", "API spec"])
        .assert()
        .success();

    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-2"))
        .stdout(predicate::str::contains("Ready task"));
}

#[test]
fn next_skips_dependency_blocked_stories() {
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
        .args(["new", "Depends on first"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-2", "blocked-by", "SH-1"])
        .assert()
        .success();

    // SH-2 is blocked because SH-1 (which blocks it) is still open
    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-1"))
        .stdout(predicate::str::contains("First task"));
}

#[test]
fn next_unblocks_after_dependency_closed() {
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
        .args(["new", "Depends on first"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-2", "blocked-by", "SH-1"])
        .assert()
        .success();

    // Close SH-1
    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success();

    // Now SH-2 should be ready
    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-2"));
}

#[test]
fn next_count_returns_multiple() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();
    story(dir.path()).args(["new", "Task C"]).assert().success();

    let output = story(dir.path())
        .args(["next", "--count", "3"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SH-1"));
    assert!(stdout.contains("SH-2"));
    assert!(stdout.contains("SH-3"));
}

/// Regression test for SH-236: `story next --count N>1` handed back a story
/// someone had already claimed (moved to `in-progress`), because `is_ready`
/// never checked the story's state beyond the required `blocked` slug.
#[test]
fn next_skips_a_story_already_in_progress() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();
    story(dir.path())
        .args(["move", "SH-1", "in-progress"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["next", "--count", "2"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(!stdout.contains("SH-1"));
    assert!(stdout.contains("SH-2"));

    // Plain `next` (no --count) agrees: the claimed story is never the
    // single answer either.
    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-2"))
        .stdout(predicate::str::contains("Task B"));
}

#[test]
fn next_json_output() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Build parser"])
        .assert()
        .success();

    story(dir.path())
        .args(["--json", "next"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"result\": \"ok\""))
        .stdout(predicate::str::contains("\"id\": \"SH-1\""));
}

#[test]
fn next_empty_project() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("no ready stories"));
}

#[test]
fn next_all_blocked_returns_no_ready() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Blocked task"])
        .assert()
        .success();
    story(dir.path())
        .args(["block", "SH-1", "external dependency"])
        .assert()
        .success();

    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("no ready stories"));
}
// --- `story next --claim` (SH-344) ---
//
// These use `storyhook_test_support`'s `TestEnv`/`Project` rather than the
// bare `tempdir` helper above: every `story` command goes to the daemon
// (the CLI's only door since SH-114), and the race test needs
// `TestEnv::raw_story` to spawn real, concurrent processes — something the
// helper above has no equivalent of. `Project::json`/`Project::run` assert
// success internally, so the two refusal tests fall back to `Project::story`
// directly.

#[test]
fn claim_moves_the_returned_story_into_the_active_state() {
    let project = TestEnv::shared().project().prefix("CLM").build();
    let id = project.new_story("Claimable work");

    let claimed = project.json(&["next", "--claim"]);
    assert_eq!(claimed["story"]["story"]["id"], id);
    assert_eq!(claimed["story"]["story"]["state"], "in-progress");
}

#[test]
fn claim_reports_the_state_it_came_from() {
    let project = TestEnv::shared().project().prefix("CLM").build();
    project.new_story("Claimable work");

    let claimed = project.json(&["next", "--claim"]);
    assert_eq!(claimed["claimed_from"], "todo");
}

#[test]
fn claim_twice_returns_different_stories() {
    let project = TestEnv::shared().project().prefix("CLM").build();
    let first = project.new_story("First");
    let second = project.new_story("Second");

    let first_claim = project.json(&["next", "--claim"]);
    let second_claim = project.json(&["next", "--claim"]);

    assert_eq!(first_claim["story"]["story"]["id"], first);
    assert_eq!(second_claim["story"]["story"]["id"], second);
}

#[test]
fn claim_with_nothing_ready_reports_no_ready_stories() {
    let project = TestEnv::shared().project().prefix("CLM").build();

    let answer = project.json(&["next", "--claim"]);
    assert_eq!(answer["message"], "no ready stories");
    assert!(
        answer.get("story").is_none(),
        "a no-op claim must not answer with a story: {answer}"
    );
}

#[test]
fn claim_with_count_other_than_one_is_refused() {
    let project = TestEnv::shared().project().prefix("CLM").build();
    project.new_story("Claimable work");

    // Both flag orders — the parser reads flags in any order.
    project
        .run(&["next", "--claim", "--count", "2"])
        .failure()
        .code(2)
        .stderr(predicate::str::contains("--claim"));
    project
        .run(&["next", "--count", "2", "--claim"])
        .failure()
        .code(2)
        .stderr(predicate::str::contains("--claim"));
}

#[test]
fn claim_respects_the_phase_filter() {
    let project = TestEnv::shared().project().prefix("CLM").build();
    let in_phase = project.new_story("In phase 1");
    project.run(&["label", &in_phase, "phase:1"]).success();
    project.new_story("Not in any phase");

    let claimed = project.json(&["next", "--claim", "--phase", "1"]);
    assert_eq!(claimed["story"]["story"]["id"], in_phase);
}

#[test]
fn claim_without_a_resolvable_active_state_is_refused() {
    let project = TestEnv::shared().project().prefix("CLM").build();
    project.new_story("Claimable work");
    // The default catalog's `in-progress` carries the `active` role; once
    // cleared, the project still has three OPEN states (todo, in-progress,
    // blocked), not the two `domain::active_state`'s fallback needs — so
    // this reliably drives it to `None` rather than silently falling back.
    project
        .run(&["state", "set", "in-progress", "--role", "none"])
        .success();

    let output = project
        .story()
        .args(["next", "--claim", "--json"])
        .output()
        .expect("running `story next --claim --json`");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(2));
    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("`next --claim --json` must print JSON");
    assert_eq!(json["result"], "error");
    assert!(
        json["error"]
            .as_str()
            .is_some_and(|error| error.contains("--role active")),
        "the refusal should name the fix: {json}"
    );
}

#[test]
fn claim_writes_exactly_one_state_changed_event() {
    let project = TestEnv::shared().project().prefix("CLM").build();
    let id = project.new_story("Claimable work");

    project.json(&["next", "--claim"]);

    use storyhook::store::{ReadOps as _, Store as _};
    let store = project.open_store();
    let project_id = project.project_id(&store);
    let story_no = project.story_no(&store, &id);
    let events = store
        .read(|tx| tx.events_for(project_id, story_no))
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

/// The real reason `--claim` exists: N callers racing `story next --claim` at
/// once must be handed N *distinct* stories, not one winner and N-1 refusals
/// the way a compare-and-swap claim (`move --if-state`) produces. Modeled on
/// `tests/move_if_state.rs`'s own concurrent CAS test — every attempt is
/// spawned as a real OS process *before* any of them is waited on, which is
/// what actually races the write lock rather than merely inferring
/// concurrency safety from back-to-back sequential calls.
#[test]
fn claim_under_real_concurrency_yields_distinct_winners() {
    use std::process::Stdio;

    let project = TestEnv::shared().project().prefix("CLM").build();
    const READY: usize = 8;
    let seeded: std::collections::BTreeSet<String> = (0..READY)
        .map(|i| project.new_story(&format!("Task {i}")))
        .collect();

    let env = project.env();
    // Two more claimants than ready stories, so the losing tail exercises
    // "no ready stories" under the same contention rather than only the
    // winning path.
    const ATTEMPTS: usize = READY + 2;
    let children: Vec<_> = (0..ATTEMPTS)
        .map(|_| {
            env.raw_story(project.path())
                .args(["next", "--claim", "--json"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("failed to spawn concurrent `story next --claim`")
        })
        .collect();

    let mut claimed: Vec<String> = Vec::new();
    let mut empty_answers = 0usize;
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.stderr.is_empty(),
            "no concurrent attempt should print to stderr: {:?}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success(),
            "every concurrent claim attempt must succeed — a claim losing a \
             race answers \"no ready stories\", not an error: {:?}",
            String::from_utf8_lossy(&output.stdout)
        );
        let json: serde_json::Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|e| panic!("non-JSON output ({e}): {output:?}"));
        let stdout_text = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout_text.contains("SQLITE_BUSY")
                && !stdout_text.contains("locked")
                && !stdout_text.contains("timed out waiting"),
            "contention must never surface to a caller: {json}"
        );
        match json["message"].as_str() {
            Some("no ready stories") => empty_answers += 1,
            _ => {
                let id = json["story"]["story"]["id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a successful claim must name a story: {json}"))
                    .to_string();
                assert_eq!(
                    json["story"]["story"]["state"], "in-progress",
                    "a claimed story must be in the active state: {json}"
                );
                claimed.push(id);
            }
        }
    }

    let distinct: std::collections::BTreeSet<&str> = claimed.iter().map(String::as_str).collect();
    assert_eq!(
        distinct.len(),
        claimed.len(),
        "no two concurrent claimants may be handed the same story: {claimed:?}"
    );
    assert_eq!(
        claimed.len(),
        READY,
        "every ready story must be claimed exactly once: {claimed:?}"
    );
    assert_eq!(empty_answers, ATTEMPTS - READY);
    let claimed_set: std::collections::BTreeSet<String> = claimed.into_iter().collect();
    assert_eq!(
        claimed_set, seeded,
        "the claimed set must be exactly the seeded ready set"
    );
}

#[test]
fn next_skips_parent_stories() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Parent epic"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Child task"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-1", "parent-of", "SH-2"])
        .assert()
        .success();

    // Next should skip SH-1 (parent) and return SH-2 (child)
    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-2"))
        .stdout(predicate::str::contains("Child task"));
}

#[test]
fn next_returns_no_ready_when_only_parents_are_ready() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Parent epic"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Child task"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-1", "parent-of", "SH-2"])
        .assert()
        .success();
    // Block the child so only the parent would be "ready"
    story(dir.path())
        .args(["block", "SH-2", "waiting on design"])
        .assert()
        .success();

    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("no ready stories"));
}
