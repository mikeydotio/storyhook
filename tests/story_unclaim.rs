//! `story unclaim <id>` — the store half of releasing a claim (SH-483).
//!
//! The mirror of `tests/story_claim.rs`, and it borrows that file's harness
//! wholesale: every `story` command goes to the daemon (the CLI's only door
//! since SH-114), and the race test needs `TestEnv::raw_story` to spawn real,
//! concurrent processes.
//!
//! **`$TMUX` is cleared on every command here too**, even though an unclaim's
//! default sentence deliberately names no tmux window. It is cleared so that
//! the *claim* each fixture performs first composes the host-only sentence
//! regardless of whether `make test` was run from inside tmux — a fixture
//! whose setup varies with the developer's terminal is a fixture that fails
//! in one of the two places.

use storyhook_test_support::{ChildGuard, Project, STORY_COMMAND_DEADLINE, TestEnv};

/// A project with a claimable state and nothing else assumed.
fn project() -> Project<'static> {
    TestEnv::shared().project().prefix("UNC").build()
}

/// `story <args> --json`, run with `$TMUX` cleared, parsed.
fn json(project: &Project<'_>, args: &[&str]) -> serde_json::Value {
    let mut command = project.story();
    command.env_remove("TMUX").env_remove("TMUX_PANE");
    let output = command
        .args(args)
        .arg("--json")
        .output()
        .expect("running story");
    assert!(
        output.status.success(),
        "`story {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("non-JSON output ({e}): {output:?}"))
}

/// The same, for a command expected to fail: returns `(exit code, envelope)`.
fn json_failure(project: &Project<'_>, args: &[&str]) -> (Option<i32>, serde_json::Value) {
    let mut command = project.story();
    command.env_remove("TMUX").env_remove("TMUX_PANE");
    let output = command
        .args(args)
        .arg("--json")
        .output()
        .expect("running story");
    assert!(
        !output.status.success(),
        "`story {}` unexpectedly succeeded",
        args.join(" ")
    );
    let value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("a --json failure must print JSON ({e}): {output:?}"));
    (output.status.code(), value)
}

/// Every comment on a story, newest last.
fn comments(project: &Project<'_>, id: &str) -> Vec<String> {
    let shown = json(project, &["show", id]);
    shown["story"]["story"]["comments"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    entry["text"]
                        .as_str()
                        .unwrap_or_else(|| panic!("a comment must carry text: {entry}"))
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The story's state, read back through an independent command.
fn state(project: &Project<'_>, id: &str) -> String {
    json(project, &["show", id])["story"]["story"]["state"]
        .as_str()
        .expect("a story has a state")
        .to_string()
}

/// Every `StoryStateChanged` this story has recorded, in order.
fn transitions(project: &Project<'_>, id: &str) -> Vec<String> {
    json(project, &["log", id])["log"]
        .as_array()
        .expect("a log has entries")
        .iter()
        .filter(|entry| entry["kind"] == "StoryStateChanged")
        .map(|entry| entry["detail"].as_str().unwrap_or_default().to_string())
        .collect()
}

// --- the ordinary release --------------------------------------------------

#[test]
fn unclaiming_returns_the_story_to_the_state_it_was_claimed_from() {
    let project = project();
    let id = project.new_story("A task");
    json(&project, &["claim", &id]);

    let released = json(&project, &["unclaim", &id]);
    assert_eq!(released["result"], "ok");
    assert_eq!(released["story"]["story"]["id"], id.as_str());
    assert_eq!(released["story"]["story"]["state"], "todo");
    // The half that is nowhere else in the envelope: where it came from.
    assert_eq!(released["unclaimed_from"], "in-progress");
    assert!(
        released["restore_fallback"].is_null(),
        "an ordinary release substitutes nothing: {released}"
    );
}

/// The whole reason this is a replay and not a constant. A story claimed out
/// of a state the project invented comes back to *that* state, not to `todo`.
#[test]
fn a_story_claimed_from_a_custom_state_comes_back_to_it() {
    let project = project();
    json(&project, &["state", "add", "triage", "--super", "OPEN"]);
    let id = project.new_story("A task");
    json(&project, &["move", &id, "triage"]);
    json(&project, &["claim", &id]);

    let released = json(&project, &["unclaim", &id]);
    assert_eq!(released["story"]["story"]["state"], "triage");
    assert!(released["restore_fallback"].is_null());
}

/// Claim, release, claim from somewhere else, release again: the second
/// release answers about the second claim, not the first.
#[test]
fn a_second_claim_is_released_to_where_the_second_claim_found_it() {
    let project = project();
    json(&project, &["state", "add", "triage", "--super", "OPEN"]);
    let id = project.new_story("A task");

    json(&project, &["claim", &id]);
    json(&project, &["unclaim", &id]);
    assert_eq!(state(&project, &id), "todo");

    json(&project, &["move", &id, "triage"]);
    json(&project, &["claim", &id]);
    let released = json(&project, &["unclaim", &id]);
    assert_eq!(released["story"]["story"]["state"], "triage");
}

/// One release, one transition — never a second `StoryStateChanged` for the
/// same move.
#[test]
fn unclaiming_writes_exactly_one_state_changed_event() {
    let project = project();
    let id = project.new_story("A task");
    json(&project, &["claim", &id]);
    let before = transitions(&project, &id).len();

    json(&project, &["unclaim", &id]);
    assert_eq!(transitions(&project, &id).len(), before + 1);
}

// --- the fallbacks ---------------------------------------------------------

/// Fallback 1: `story new --state in-progress`. There is no earlier state, so
/// `todo` is substituted — and said out loud, in the envelope and in the
/// comment.
#[test]
fn a_story_created_in_the_active_state_falls_back_to_todo_and_says_so() {
    let project = project();
    let created = json(&project, &["new", "Born claimed", "--state", "in-progress"]);
    let id = created["story"]["story"]["id"]
        .as_str()
        .expect("an id")
        .to_string();

    let released = json(&project, &["unclaim", &id]);
    assert_eq!(released["story"]["story"]["state"], "todo");
    assert_eq!(released["restore_fallback"], "no-prior-state");

    let last = comments(&project, &id).pop().expect("a default comment");
    assert!(
        last.contains("rather than the state it was claimed from"),
        "the substitution must be stated in the comment, not merely performed: {last}"
    );
    assert!(
        last.contains("no earlier state to restore"),
        "the comment must say WHY: {last}"
    );
}

/// Fallback 2: the state it was claimed from was removed from the project's
/// vocabulary while the story was held.
#[test]
fn an_origin_state_removed_mid_claim_falls_back_to_todo_and_names_it() {
    let project = project();
    json(&project, &["state", "add", "triage", "--super", "OPEN"]);
    let id = project.new_story("A task");
    json(&project, &["move", &id, "triage"]);
    json(&project, &["claim", &id]);
    json(&project, &["state", "remove", "triage"]);

    let released = json(&project, &["unclaim", &id]);
    assert_eq!(released["story"]["story"]["state"], "todo");
    assert_eq!(released["restore_fallback"], "prior-state-removed");

    let last = comments(&project, &id).pop().expect("a default comment");
    assert!(
        last.contains("triage"),
        "the comment must name the state it could not use: {last}"
    );
}

/// Fallback 3, the one with teeth: restoring the story to a state that has
/// since been reclassified CLOSED would *close* the story rather than release
/// it. The story must come back OPEN.
#[test]
fn an_origin_state_reclassified_closed_falls_back_rather_than_closing_the_story() {
    let project = project();
    json(&project, &["state", "add", "triage", "--super", "OPEN"]);
    let id = project.new_story("A task");
    json(&project, &["move", &id, "triage"]);
    json(&project, &["claim", &id]);
    json(&project, &["state", "set", "triage", "--super", "CLOSED"]);

    let released = json(&project, &["unclaim", &id]);
    assert_eq!(released["story"]["story"]["state"], "todo");
    assert_eq!(released["restore_fallback"], "prior-state-closed");
    // The whole point: the release must not have closed the story.
    assert_eq!(released["story"]["story"]["superstate"], "OPEN");
}

/// A story genuinely claimed out of `todo` lands in `todo` with **no**
/// fallback reported. The destination alone cannot tell a restoration from a
/// substitution, which is why the two are reported separately.
#[test]
fn landing_in_todo_on_purpose_reports_no_fallback() {
    let project = project();
    let id = project.new_story("A task");
    json(&project, &["claim", &id]);

    let released = json(&project, &["unclaim", &id]);
    assert_eq!(released["story"]["story"]["state"], "todo");
    assert!(
        released["restore_fallback"].is_null(),
        "restoring to todo on purpose is not a fallback: {released}"
    );
    let last = comments(&project, &id).pop().expect("a default comment");
    assert!(last.contains("the state it was claimed from"), "{last}");
    assert!(
        !last.contains("rather than"),
        "an ordinary release must not apologize for its destination: {last}"
    );
}

// --- the refusals ----------------------------------------------------------

/// The mirror of a lost claim, and the reason `expected` here is the *real*
/// active slug rather than a pseudo-state: an unclaim's precondition is
/// exactly one state, so naming it is the truth.
#[test]
fn unclaiming_a_story_nobody_claimed_is_a_conflict_naming_the_actual_state() {
    let project = project();
    let id = project.new_story("Untouched");

    let (code, envelope) = json_failure(&project, &["unclaim", &id]);
    assert_eq!(code, Some(9));
    assert_eq!(envelope["result"], "conflict");
    assert_eq!(envelope["expected"], "in-progress");
    assert_eq!(envelope["actual"], "todo");
}

/// A conflict writes *nothing* — not the state change, and not the comment.
/// This is the observable that separates one transaction from two ordered
/// comment-first: with the comment in its own earlier write, a refused
/// release would leave a comment behind saying a story had been handed back
/// that this call never released.
#[test]
fn a_conflicted_unclaim_leaves_no_comment_behind() {
    let project = project();
    let id = project.new_story("Untouched");

    json_failure(&project, &["unclaim", &id, "--comment", "handing back"]);
    assert!(
        comments(&project, &id).is_empty(),
        "a refused unclaim must write nothing at all: {:?}",
        comments(&project, &id)
    );
    assert_eq!(state(&project, &id), "todo");
}

/// Somebody else moved the story on while it was held. Reported as a
/// conflict naming where it actually is, never silently overwritten.
#[test]
fn unclaiming_a_story_someone_moved_on_is_a_conflict() {
    let project = project();
    let id = project.new_story("A task");
    json(&project, &["claim", &id]);
    json(&project, &["move", &id, "blocked"]);

    let (code, envelope) = json_failure(&project, &["unclaim", &id]);
    assert_eq!(code, Some(9));
    assert_eq!(envelope["actual"], "blocked");
}

#[test]
fn unclaiming_a_closed_story_is_refused() {
    let project = project();
    let id = project.new_story("Finished");
    json(&project, &["move", &id, "done"]);

    let (_, envelope) = json_failure(&project, &["unclaim", &id]);
    assert!(
        envelope["error"]
            .as_str()
            .unwrap_or_default()
            .contains("closed"),
        "{envelope}"
    );
}

#[test]
fn unclaiming_a_story_that_does_not_exist_is_refused() {
    let project = project();
    let (_, envelope) = json_failure(&project, &["unclaim", "UNC-999"]);
    assert_eq!(envelope["result"], "error");
}

/// Inherited from the claim side: with no state carrying the `active` role
/// and no two-OPEN-state fallback, there is nothing to release *from*. The
/// refusal names the fix.
#[test]
fn an_unclaim_without_a_resolvable_active_state_is_refused() {
    let project = project();
    let id = project.new_story("A task");
    json(&project, &["claim", &id]);
    // The default catalog's `in-progress` carries the role; cleared, three
    // OPEN states remain (todo, in-progress, verifying, blocked), not the two the
    // fallback needs.
    json(&project, &["state", "set", "in-progress", "--role", "none"]);

    let (_, envelope) = json_failure(&project, &["unclaim", &id]);
    assert!(
        envelope["error"]
            .as_str()
            .unwrap_or_default()
            .contains("--role active"),
        "the refusal must name the fix: {envelope}"
    );
}

/// A bare `story unclaim` names no story, and there is no `--next` for it to
/// fall back to. Refused, and nothing is written.
#[test]
fn a_bare_unclaim_is_refused() {
    let project = project();
    let id = project.new_story("A task");
    json(&project, &["claim", &id]);

    let (_, envelope) = json_failure(&project, &["unclaim"]);
    assert!(
        envelope["error"]
            .as_str()
            .unwrap_or_default()
            .contains("needs a story id"),
        "{envelope}"
    );
    assert_eq!(state(&project, &id), "in-progress");
}

// --- the comment -----------------------------------------------------------

/// An unclaim comments by DEFAULT, and the sentence names both ends of the
/// move — which is the pair a reader cannot recover from the story otherwise.
#[test]
fn an_unclaim_comments_by_default() {
    let project = project();
    let id = project.new_story("A task");
    json(&project, &["claim", &id]);

    json(&project, &["unclaim", &id]);
    let last = comments(&project, &id).pop().expect("a default comment");
    assert!(last.starts_with("Unclaimed from in-progress"), "{last}");
    assert!(last.contains("restored to todo"), "{last}");
    // A release is not located anywhere, unlike a claim: no host, no window.
    assert!(
        !last.contains("tmux"),
        "an unclaim's default sentence names no tmux window: {last}"
    );
}

#[test]
fn comment_replaces_the_default_text() {
    let project = project();
    let id = project.new_story("A task");
    json(&project, &["claim", &id]);

    json(&project, &["unclaim", &id, "--comment", "back to the pile"]);
    assert_eq!(
        comments(&project, &id).pop().as_deref(),
        Some("back to the pile")
    );
}

/// A caller's own sentence is written verbatim even when the destination was
/// substituted: it is their text, and splicing into it would corrupt what
/// they meant to say. The substitution still reaches them in the result.
#[test]
fn a_custom_comment_is_untouched_by_a_fallback() {
    let project = project();
    let created = json(&project, &["new", "Born claimed", "--state", "in-progress"]);
    let id = created["story"]["story"]["id"]
        .as_str()
        .expect("an id")
        .to_string();

    let released = json(&project, &["unclaim", &id, "--comment", "mine, verbatim"]);
    assert_eq!(released["restore_fallback"], "no-prior-state");
    assert_eq!(
        comments(&project, &id).pop().as_deref(),
        Some("mine, verbatim")
    );
}

#[test]
fn no_comment_suppresses_it_entirely() {
    let project = project();
    let id = project.new_story("A task");
    json(&project, &["claim", &id, "--no-comment"]);

    json(&project, &["unclaim", &id, "--no-comment"]);
    assert!(
        comments(&project, &id).is_empty(),
        "{:?}",
        comments(&project, &id)
    );
    assert_eq!(state(&project, &id), "todo");
}

#[test]
fn comment_and_no_comment_together_are_refused() {
    let project = project();
    let id = project.new_story("A task");
    json(&project, &["claim", &id]);

    let (_, envelope) = json_failure(
        &project,
        &["unclaim", &id, "--comment", "x", "--no-comment"],
    );
    assert!(
        envelope["error"]
            .as_str()
            .unwrap_or_default()
            .contains("say opposite things"),
        "{envelope}"
    );
    assert_eq!(state(&project, &id), "in-progress");
}

/// The comment and the state change are one write: both land, and both carry
/// the release's own timestamp.
///
/// This is the assertion a mutation that splits the transaction turns red —
/// a comment written in its own earlier `append_events` call carries an
/// earlier `at` than the transition it was meant to accompany, and a
/// transition written without it leaves no comment at all.
#[test]
fn the_comment_lands_in_the_same_batch_as_the_state_change() {
    let project = project();
    let id = project.new_story("A task");
    json(&project, &["claim", &id, "--no-comment"]);

    json(&project, &["unclaim", &id]);
    let log = json(&project, &["log", &id]);
    let entries = log["log"].as_array().expect("a log has entries");

    let release = entries
        .iter()
        .rev()
        .find(|entry| entry["kind"] == "StoryStateChanged")
        .expect("the release recorded a state change");
    let comment = entries
        .iter()
        .rev()
        .find(|entry| entry["kind"] == "StoryCommentAdded")
        .expect("the release recorded its comment");
    assert_eq!(
        release["at"], comment["at"],
        "one write, one instant: {release} vs {comment}"
    );
    // And in that order — the transition, then the comment about it.
    let position = |target: &serde_json::Value| {
        entries
            .iter()
            .position(|entry| entry == target)
            .expect("an entry this test just read out of the list")
    };
    assert!(
        position(release) < position(comment),
        "the transition is appended before the comment describing it"
    );
}

// --- dry run ---------------------------------------------------------------

#[test]
fn a_dry_run_writes_nothing_and_says_what_it_would_do() {
    let project = project();
    json(&project, &["state", "add", "triage", "--super", "OPEN"]);
    let id = project.new_story("A task");
    json(&project, &["move", &id, "triage"]);
    json(&project, &["claim", &id, "--no-comment"]);

    let planned = json(&project, &["unclaim", &id, "--dry-run"]);
    let message = planned["message"].as_str().expect("a plan is a message");
    assert!(
        message.contains(&format!("would unclaim {id} — in-progress -> triage")),
        "{message}"
    );
    // The sentence it WOULD write, not a description of one.
    assert!(
        message.contains("would comment on") && message.contains("Unclaimed from in-progress"),
        "{message}"
    );

    assert_eq!(state(&project, &id), "in-progress");
    assert!(comments(&project, &id).is_empty());
}

/// A dry run that reports a plan the real command would refuse is worse than
/// no dry run at all: every refusal is still made, for real.
#[test]
fn a_dry_run_still_refuses_a_story_nobody_claimed() {
    let project = project();
    let id = project.new_story("Untouched");

    let (code, envelope) = json_failure(&project, &["unclaim", &id, "--dry-run"]);
    assert_eq!(code, Some(9));
    assert_eq!(envelope["actual"], "todo");
}

/// And a dry run reports the substitution it would make, for the same reason
/// the real command does.
#[test]
fn a_dry_run_names_the_fallback_it_would_take() {
    let project = project();
    let created = json(&project, &["new", "Born claimed", "--state", "in-progress"]);
    let id = created["story"]["story"]["id"]
        .as_str()
        .expect("an id")
        .to_string();

    let planned = json(&project, &["unclaim", &id, "--dry-run"]);
    let message = planned["message"].as_str().expect("a plan is a message");
    assert!(
        message.contains("would restore to todo rather than the state it was claimed from"),
        "{message}"
    );
    assert_eq!(state(&project, &id), "in-progress");
}

#[test]
fn a_dry_run_with_no_comment_names_no_comment() {
    let project = project();
    let id = project.new_story("A task");
    json(&project, &["claim", &id]);

    let planned = json(&project, &["unclaim", &id, "--dry-run", "--no-comment"]);
    let message = planned["message"].as_str().expect("a plan is a message");
    assert!(message.contains("would not comment on"), "{message}");
}

// --- the race --------------------------------------------------------------

/// The compare-and-swap this verb rests on. N callers racing `story unclaim`
/// against one held story: exactly one wins, and every loser is told so
/// rather than silently re-releasing a story the winner already handed back
/// — which, without the CAS, would append a second transition and a second
/// comment to a story that had already left the active state.
///
/// Modeled on `tests/story_claim.rs`'s own concurrent test: every attempt is
/// spawned as a real OS process *before* any of them is waited on, which is
/// what actually races the write lock rather than inferring concurrency
/// safety from back-to-back sequential calls.
#[test]
fn concurrent_unclaimers_of_one_story_yield_exactly_one_winner() {
    let project = project();
    let id = project.new_story("The contended one");
    json(&project, &["claim", &id, "--no-comment"]);

    let env = project.env();
    const ATTEMPTS: usize = 6;
    let children: Vec<_> = (0..ATTEMPTS)
        .map(|_| {
            let mut command = env.raw_story(project.path());
            command
                .args(["unclaim", &id, "--json"])
                .env_remove("TMUX")
                .env_remove("TMUX_PANE");
            ChildGuard::spawn_with_output(&mut command)
                .expect("failed to spawn concurrent `story unclaim <id>`")
        })
        .collect();

    let mut winners = 0usize;
    let mut conflicts = 0usize;
    for mut child in children {
        let output = child.wait_with_output_within(STORY_COMMAND_DEADLINE, || {
            "a concurrent `story unclaim <id>` did not finish".to_string()
        });
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|e| panic!("non-JSON output ({e}): {output:?}"));
        match value["result"].as_str() {
            Some("ok") => winners += 1,
            Some("conflict") => {
                conflicts += 1;
                assert_eq!(output.status.code(), Some(9));
                assert_eq!(value["actual"], "todo");
            }
            other => panic!("unexpected result {other:?}: {value}"),
        }
    }
    assert_eq!(winners, 1, "exactly one release may win");
    assert_eq!(conflicts, ATTEMPTS - 1, "every loser is told, not ignored");

    // Exactly one release was recorded, so no loser wrote a redundant one.
    assert_eq!(
        comments(&project, &id).len(),
        1,
        "a losing unclaim must leave no comment: {:?}",
        comments(&project, &id)
    );
    assert_eq!(state(&project, &id), "todo");
}

/// The pair, raced against each other: a claim and an unclaim landing at once
/// must leave the story in one of the two states with a coherent log, never
/// in a half-applied one. Both orders are legitimate outcomes; what is not is
/// a release that both conflicted and wrote.
#[test]
fn a_claim_racing_an_unclaim_leaves_exactly_one_of_them_applied() {
    let project = project();
    let id = project.new_story("The contended one");
    json(&project, &["claim", &id, "--no-comment"]);

    let env = project.env();
    let mut children: Vec<_> = (0..3)
        .map(|_| {
            let mut command = env.raw_story(project.path());
            command
                .args(["unclaim", &id, "--no-comment", "--json"])
                .env_remove("TMUX")
                .env_remove("TMUX_PANE");
            ChildGuard::spawn_with_output(&mut command).expect("spawning a concurrent unclaim")
        })
        .collect();
    children.extend((0..3).map(|_| {
        let mut command = env.raw_story(project.path());
        command
            .args(["claim", &id, "--no-comment", "--json"])
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        ChildGuard::spawn_with_output(&mut command).expect("spawning a concurrent claim")
    }));

    for mut child in children {
        let output = child.wait_with_output_within(STORY_COMMAND_DEADLINE, || {
            "a claim/unclaim racer did not finish".to_string()
        });
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|e| panic!("non-JSON output ({e}): {output:?}"));
        assert!(
            matches!(value["result"].as_str(), Some("ok" | "conflict")),
            "every racer answers ok or conflict, never anything else: {value}"
        );
    }

    // Whatever order they landed in, the story sits in exactly one of the two
    // states, and its log is a coherent alternation rather than a stutter.
    let settled = state(&project, &id);
    assert!(
        settled == "todo" || settled == "in-progress",
        "the story came to rest somewhere unexpected: {settled}"
    );
    let moves = transitions(&project, &id);
    assert!(
        moves.windows(2).all(|pair| pair[0] != pair[1]),
        "a state was written twice in a row, so a CAS was lost: {moves:?}"
    );
}
