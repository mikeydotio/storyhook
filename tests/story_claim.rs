//! `story claim (<id> | --next)` — the one atomic claim verb (SH-476).
//!
//! These use `storyhook_test_support`'s `TestEnv`/`Project` rather than a bare
//! `tempdir`: every `story` command goes to the daemon (the CLI's only door
//! since SH-114), and the race test needs `TestEnv::raw_story` to spawn real,
//! concurrent processes.
//!
//! **Every command here removes `$TMUX` from the child's environment.** The
//! default comment names the caller's tmux window when there is one, and
//! `make test` is routinely run from inside tmux — a test that asserted the
//! host-only sentence while inheriting a live `$TMUX` would pass on a
//! developer's laptop and fail in a detached run, or the reverse. The sentence
//! itself is proved on both branches as a unit test of
//! `storyhook::claim_comment::default_comment`, which needs no tmux at all.

use predicates::prelude::*;
use storyhook_test_support::{Project, TestEnv, scratch_dir_named};

/// A project with a claimable state and nothing else assumed.
fn project() -> Project<'static> {
    TestEnv::shared().project().prefix("CLM").build()
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

// --- the two forms, and the refusal of neither/both ------------------------

#[test]
fn claiming_by_id_moves_that_story_into_the_active_state() {
    let project = project();
    let first = project.new_story("First");
    let second = project.new_story("Second");

    // The *second* story, so a pass cannot be explained by `--next`'s pick.
    let claimed = json(&project, &["claim", &second]);
    assert_eq!(claimed["story"]["story"]["id"], second);
    assert_eq!(claimed["story"]["story"]["state"], "in-progress");
    assert_eq!(claimed["claimed_from"], "todo");
    assert_eq!(
        json(&project, &["show", &first])["story"]["story"]["state"],
        "todo",
        "claiming one story must not touch another"
    );
}

#[test]
fn claiming_next_takes_what_story_next_would_answer() {
    let project = project();
    let first = project.new_story("First");
    project.new_story("Second");

    let claimed = json(&project, &["claim", "--next"]);
    assert_eq!(claimed["story"]["story"]["id"], first);
    assert_eq!(claimed["story"]["story"]["state"], "in-progress");
    assert_eq!(claimed["claimed_from"], "todo");
}

/// The whole reason the verb refuses rather than defaulting: a mutating call
/// whose id argument came out empty must not claim whatever sorts first.
#[test]
fn a_bare_claim_is_refused_and_writes_nothing() {
    let project = project();
    let id = project.new_story("Not to be claimed");

    project
        .story()
        .args(["claim"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("--next"));

    assert_eq!(
        json(&project, &["show", &id])["story"]["story"]["state"],
        "todo",
        "a refused claim must write nothing"
    );
}

#[test]
fn an_id_and_next_together_are_refused_in_either_order() {
    let project = project();
    let id = project.new_story("Claimable");

    for args in [
        vec!["claim", id.as_str(), "--next"],
        vec!["claim", "--next", id.as_str()],
    ] {
        project
            .story()
            .args(&args)
            .assert()
            .failure()
            .code(2)
            .stderr(predicate::str::contains("two different requests"));
    }

    assert_eq!(
        json(&project, &["show", &id])["story"]["story"]["state"],
        "todo"
    );
}

/// `--phase` narrows what `--next` picks; beside an explicit id it has no
/// meaning, and the parser has nowhere to put it. Refused, never ignored.
#[test]
fn phase_beside_an_explicit_id_is_refused() {
    let project = project();
    let id = project.new_story("Claimable");

    project
        .story()
        .args(["claim", &id, "--phase", "1"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("--phase"));
}

#[test]
fn claiming_next_respects_the_phase_filter() {
    let project = project();
    let in_phase = project.new_story("In phase 1");
    project.run(&["label", &in_phase, "phase:1"]).success();
    project.new_story("Not in any phase");

    let claimed = json(&project, &["claim", "--next", "--phase", "1"]);
    assert_eq!(claimed["story"]["story"]["id"], in_phase);
}

/// A claim TAKES its answer, so the next caller must be given a different one.
/// Inherited from the claiming mode SH-344 bolted onto `story next`, which is
/// the guarantee that mode existed for.
///
/// The sequential twin of `concurrent_claimants_are_handed_distinct_stories`
/// below, kept beside it deliberately: this one is deterministic and cheap,
/// and it fails on a claim that answers without writing at all, which the
/// racing version could in principle explain away as contention.
#[test]
fn claiming_next_twice_hands_out_two_different_stories() {
    let project = project();
    let first = project.new_story("First");
    let second = project.new_story("Second");

    assert_eq!(
        json(&project, &["claim", "--next"])["story"]["story"]["id"],
        first
    );
    assert_eq!(
        json(&project, &["claim", "--next"])["story"]["story"]["id"],
        second
    );
}

/// One claim, one transition — never a second `StoryStateChanged` for the
/// same move. The `--next` twin of the assertion
/// `concurrent_claimants_of_one_id_yield_exactly_one_winner` makes for an id.
#[test]
fn claiming_next_writes_exactly_one_state_changed_event() {
    use storyhook::store::{ReadOps as _, Store as _};

    let project = project();
    let id = project.new_story("Claimable work");

    json(&project, &["claim", "--next"]);

    let store = project.open_store();
    let project_id = project.project_id(&store);
    let story_no = project.story_no(&store, &id);
    let events = store
        .read(|tx| tx.events_for(project_id, story_no))
        .expect("reading the story's events");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "StoryStateChanged")
            .count(),
        1,
        "exactly one StoryStateChanged event must be recorded: {events:#?}"
    );
}

#[test]
fn claiming_next_with_nothing_ready_reports_no_ready_stories() {
    let project = project();

    let answer = json(&project, &["claim", "--next"]);
    assert_eq!(answer["message"], "no ready stories");
    assert!(
        answer.get("story").is_none(),
        "a no-op claim must not answer with a story: {answer}"
    );
}

// --- the conflict ----------------------------------------------------------

/// The one race a single write transaction cannot remove: somebody else got
/// there first. Reported the way `story move --if-state` reports a lost
/// compare-and-swap, so a caller reads both the same way.
#[test]
fn claiming_an_already_claimed_story_is_a_conflict_naming_the_actual_state() {
    let project = project();
    let id = project.new_story("Contended");
    json(&project, &["claim", &id]);

    let (code, envelope) = json_failure(&project, &["claim", &id]);
    assert_eq!(code, Some(9), "a conflict is exit 9: {envelope}");
    assert_eq!(envelope["result"], "conflict");
    assert_eq!(envelope["actual"], "in-progress");
    assert_eq!(
        envelope["expected"], "unclaimed",
        "a claim's precondition is not one slug, so `expected` carries the \
         pseudo-state rather than inventing a real one: {envelope}"
    );
}

/// A conflict writes *nothing* — not the state change, and not the comment.
///
/// This is the observable that separates one transaction from two ordered
/// comment-first: with the comment in its own earlier write, a refused claim
/// would leave a comment behind saying work had started on a story this call
/// never took.
#[test]
fn a_conflicted_claim_leaves_no_comment_behind() {
    let project = project();
    let id = project.new_story("Contended");
    json(&project, &["claim", &id, "--no-comment"]);
    let before = comments(&project, &id);

    json_failure(&project, &["claim", &id, "--comment", "second claimant"]);

    assert_eq!(
        comments(&project, &id),
        before,
        "a refused claim must not have written its comment"
    );
}

#[test]
fn claiming_a_closed_story_is_refused() {
    let project = project();
    let id = project.new_story("Finished");
    project.run(&["move", &id, "done"]).success();

    let (code, envelope) = json_failure(&project, &["claim", &id]);
    assert_eq!(code, Some(2), "{envelope}");
    assert_eq!(envelope["result"], "error");
}

#[test]
fn claiming_a_story_that_does_not_exist_is_refused() {
    let project = project();
    let (code, envelope) = json_failure(&project, &["claim", "CLM-999"]);
    assert_eq!(code, Some(3), "{envelope}");
}

/// Inherited from SH-344's claiming mode: with no state carrying the
/// `active` role and no two-OPEN-state fallback, a claim has nowhere to go.
/// The refusal names the fix.
#[test]
fn a_claim_without_a_resolvable_active_state_is_refused() {
    let project = project();
    let id = project.new_story("Claimable");
    // The default catalog's `in-progress` carries the role; cleared, three
    // OPEN states remain (todo, in-progress, blocked), not the two the
    // fallback needs.
    project
        .run(&["state", "set", "in-progress", "--role", "none"])
        .success();

    for args in [vec!["claim", id.as_str()], vec!["claim", "--next"]] {
        let (code, envelope) = json_failure(&project, &args);
        assert_eq!(code, Some(2), "{envelope}");
        assert!(
            envelope["error"]
                .as_str()
                .is_some_and(|error| error.contains("--role active")),
            "the refusal should name the fix: {envelope}"
        );
    }
}

// --- the comment -----------------------------------------------------------

/// A claim comments by DEFAULT — the flag is not opt-in (user determination,
/// 2026-08-25). Outside tmux the sentence degrades to the host alone.
#[test]
fn a_claim_comments_by_default() {
    let project = project();
    let id = project.new_story("Claimable");

    json(&project, &["claim", &id]);

    let posted = comments(&project, &id);
    assert_eq!(posted.len(), 1, "exactly one comment: {posted:?}");
    assert!(
        posted[0].starts_with("Starting work on this story on "),
        "outside tmux the default names the host and no window: {posted:?}"
    );
    assert!(
        !posted[0].contains("tmux"),
        "the tmux clause must be absent, not empty: {posted:?}"
    );
}

#[test]
fn comment_replaces_the_default_text() {
    let project = project();
    let id = project.new_story("Claimable");

    json(
        &project,
        &["claim", &id, "--comment", "picked up by the loop"],
    );

    assert_eq!(comments(&project, &id), vec!["picked up by the loop"]);
}

#[test]
fn no_comment_suppresses_it_entirely() {
    let project = project();
    let id = project.new_story("Claimable");

    json(&project, &["claim", &id, "--no-comment"]);

    assert!(
        comments(&project, &id).is_empty(),
        "--no-comment must post nothing"
    );
}

#[test]
fn comment_and_no_comment_together_are_refused() {
    let project = project();
    let id = project.new_story("Claimable");

    project
        .story()
        .args(["claim", &id, "--comment", "x", "--no-comment"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("opposite"));
}

/// `--comment` takes its value as the next token, always. An optional-value
/// flag would read `story claim SH-1 --comment --json` as a comment saying
/// `--json` — SH-357's shape, a word landing where nobody meant it to.
#[test]
fn comment_without_a_value_is_refused() {
    let project = project();
    let id = project.new_story("Claimable");

    project
        .story()
        .args(["claim", &id, "--comment"])
        .assert()
        .failure()
        .code(2);
}

/// The comment and the state change are one write: both land, and both carry
/// the claim's own timestamp.
#[test]
fn the_comment_lands_in_the_same_batch_as_the_state_change() {
    use storyhook::store::{ReadOps as _, Store as _};

    let project = project();
    let id = project.new_story("Claimable");
    json(&project, &["claim", &id, "--comment", "one write"]);

    let store = project.open_store();
    let project_id = project.project_id(&store);
    let story_no = project.story_no(&store, &id);
    let events = store
        .read(|tx| tx.events_for(project_id, story_no))
        .expect("reading the story's events");

    let changed = events
        .iter()
        .position(|event| event.kind == "StoryStateChanged")
        .expect("the claim wrote a state change");
    let commented = events
        .iter()
        .position(|event| event.kind == "StoryCommentAdded")
        .expect("the claim wrote its comment");
    assert_eq!(
        commented,
        changed + 1,
        "the comment is appended in the same batch, immediately after the \
         transition: {events:#?}"
    );
    assert_eq!(
        events[changed].at, events[commented].at,
        "one `now` for the whole batch: {events:#?}"
    );
}

/// The tmux branch of the default sentence, end to end through the real
/// binary, against a **fake** `tmux` on `PATH`.
///
/// A fake rather than the real thing, and rather than a `test.skip` when tmux
/// is absent: a test that quietly does not run on a machine without tmux is
/// the SH-306 shape — a check whose silence reads as a pass. The fake also
/// records its own argv, which is the half a unit test of
/// `default_comment` structurally cannot reach: that the pane from
/// `$TMUX_PANE` is actually passed through with `-t`, so the answer describes
/// *this* pane's window rather than whichever window the attached client
/// happens to be looking at.
#[test]
fn inside_tmux_the_default_names_the_window_this_pane_is_in() {
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    let project = project();
    let id = project.new_story("Claimable");

    let fake_dir = scratch_dir_named("fake-tmux");
    let argv_log = fake_dir.path().join("argv");
    let fake = fake_dir.path().join("tmux");
    let mut script = std::fs::File::create(&fake).expect("creating the fake tmux");
    write!(
        script,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" > {}\nprintf 'work:7\\n'\n",
        argv_log.display()
    )
    .expect("writing the fake tmux");
    drop(script);
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))
        .expect("making the fake tmux executable");

    // Prepended to the harness's own PATH, not the parent process's: that is
    // what puts the `story` binary under test where the daemon's own
    // `$PATH`-identity check (SH-404) expects it.
    let path = format!(
        "{}:{}",
        fake_dir.path().display(),
        project.env().path_with_binary().to_string_lossy()
    );
    project
        .story()
        .env("PATH", path)
        .env("TMUX", "/tmp/tmux-501/default,1,0")
        .env("TMUX_PANE", "%9")
        .args(["claim", &id])
        .assert()
        .success();

    assert_eq!(
        comments(&project, &id),
        vec![format!(
            "Starting work on this story in {} tmux window work:7",
            hostname()
        )]
    );

    let argv = std::fs::read_to_string(&argv_log).expect("the fake tmux ran");
    assert!(
        argv.contains("-t %9"),
        "the probe must ask about THIS pane, not the attached client's: {argv}"
    );
    assert!(
        argv.contains("#{session_name}:#{window_index}"),
        "the probe must ask for session:window: {argv}"
    );
}

/// This machine's hostname, read the way `claim_comment` reads it.
fn hostname() -> String {
    String::from_utf8(
        std::process::Command::new("hostname")
            .output()
            .expect("running hostname")
            .stdout,
    )
    .expect("a UTF-8 hostname")
    .trim()
    .to_string()
}

// --- dry run ---------------------------------------------------------------

#[test]
fn a_dry_run_claim_writes_nothing_and_says_what_it_would_do() {
    let project = project();
    let id = project.new_story("Claimable");

    let answer = json(&project, &["claim", &id, "--dry-run"]);
    let message = answer["message"]
        .as_str()
        .unwrap_or_else(|| panic!("a dry run answers with a message: {answer}"));
    assert!(
        message.contains(&format!("would claim {id}")) && message.contains("todo -> in-progress"),
        "the plan names the transition: {message}"
    );
    assert!(
        message.contains("would comment on"),
        "the plan names the comment it would post: {message}"
    );

    assert_eq!(
        json(&project, &["show", &id])["story"]["story"]["state"],
        "todo",
        "a dry run writes nothing"
    );
    assert!(comments(&project, &id).is_empty(), "nor its comment");
}

#[test]
fn a_dry_run_with_no_comment_names_no_comment() {
    let project = project();
    let id = project.new_story("Claimable");

    let answer = json(&project, &["claim", &id, "--dry-run", "--no-comment"]);
    let message = answer["message"].as_str().expect("a message");
    assert!(!message.contains("would comment"), "{message}");
}

/// A dry run that reports a plan the real command would refuse is worse than
/// no dry run at all: every refusal is still made, for real.
#[test]
fn a_dry_run_still_refuses_an_already_claimed_story() {
    let project = project();
    let id = project.new_story("Contended");
    json(&project, &["claim", &id]);

    let (code, envelope) = json_failure(&project, &["claim", &id, "--dry-run"]);
    assert_eq!(code, Some(9), "{envelope}");
    assert_eq!(envelope["result"], "conflict");
}

#[test]
fn a_dry_run_of_next_with_nothing_ready_reports_no_ready_stories() {
    let project = project();
    let answer = json(&project, &["claim", "--next", "--dry-run"]);
    assert_eq!(answer["message"], "no ready stories");
}

// --- the race --------------------------------------------------------------

/// The reason the verb exists. N callers racing `story claim --next` at once
/// must be handed N *distinct* stories, not one winner and N-1 refusals.
/// Modeled on `tests/move_if_state.rs`'s own concurrent CAS test: every
/// attempt is spawned as a real OS process *before* any of them is waited on,
/// which is what actually races the write lock rather than inferring
/// concurrency safety from back-to-back sequential calls.
///
/// A mutation that reverts the claim to two transactions — select, then move
/// — turns this red: two claimants select the same top story and the second
/// either overwrites the first's claim or is refused.
#[test]
fn concurrent_claimants_are_handed_distinct_stories() {
    use std::process::Stdio;

    let project = project();
    const READY: usize = 8;
    let seeded: std::collections::BTreeSet<String> = (0..READY)
        .map(|i| project.new_story(&format!("Task {i}")))
        .collect();

    let env = project.env();
    // Two more claimants than ready stories, so the losing tail exercises
    // "no ready stories" under the same contention.
    const ATTEMPTS: usize = READY + 2;
    let children: Vec<_> = (0..ATTEMPTS)
        .map(|_| {
            env.raw_story(project.path())
                .args(["claim", "--next", "--json"])
                .env_remove("TMUX")
                .env_remove("TMUX_PANE")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("failed to spawn concurrent `story claim --next`")
        })
        .collect();

    let mut claimed: Vec<String> = Vec::new();
    let mut empty_answers = 0usize;
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "every concurrent claim must succeed — a claim losing a race \
             answers \"no ready stories\", not an error: {:?}",
            String::from_utf8_lossy(&output.stdout)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|e| panic!("non-JSON output ({e}): {output:?}"));
        match value["message"].as_str() {
            Some("no ready stories") => empty_answers += 1,
            _ => {
                let id = value["story"]["story"]["id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a successful claim must name a story: {value}"))
                    .to_string();
                assert_eq!(value["story"]["story"]["state"], "in-progress");
                claimed.push(id);
            }
        }
    }

    let distinct: std::collections::BTreeSet<&str> = claimed.iter().map(String::as_str).collect();
    assert_eq!(
        distinct.len(),
        claimed.len(),
        "two claimants were handed the same story: {claimed:?}"
    );
    assert_eq!(
        distinct.len(),
        READY,
        "every ready story should have found exactly one claimant: {claimed:?}"
    );
    assert_eq!(empty_answers, ATTEMPTS - READY);
    assert!(
        seeded.iter().all(|id| distinct.contains(id.as_str())),
        "the claimed set must be exactly the seeded one"
    );
}

/// The same race against one *named* story: exactly one claimant wins and
/// every loser is told so, rather than silently overwriting the winner's
/// claim.
#[test]
fn concurrent_claimants_of_one_id_yield_exactly_one_winner() {
    use std::process::Stdio;

    let project = project();
    let id = project.new_story("The contended one");

    let env = project.env();
    const ATTEMPTS: usize = 6;
    let children: Vec<_> = (0..ATTEMPTS)
        .map(|_| {
            env.raw_story(project.path())
                .args(["claim", &id, "--json"])
                .env_remove("TMUX")
                .env_remove("TMUX_PANE")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("failed to spawn concurrent `story claim <id>`")
        })
        .collect();

    let mut winners = 0usize;
    let mut conflicts = 0usize;
    for child in children {
        let output = child.wait_with_output().unwrap();
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|e| panic!("non-JSON output ({e}): {output:?}"));
        match value["result"].as_str() {
            Some("ok") => winners += 1,
            Some("conflict") => {
                conflicts += 1;
                assert_eq!(output.status.code(), Some(9));
                assert_eq!(value["actual"], "in-progress");
            }
            other => panic!("unexpected result {other:?}: {value}"),
        }
    }
    assert_eq!(winners, 1, "exactly one claimant may win");
    assert_eq!(conflicts, ATTEMPTS - 1, "every loser is told, not ignored");

    // Exactly one transition was recorded, so no loser wrote a redundant one.
    use storyhook::store::{ReadOps as _, Store as _};
    let store = project.open_store();
    let project_id = project.project_id(&store);
    let story_no = project.story_no(&store, &id);
    let events = store
        .read(|tx| tx.events_for(project_id, story_no))
        .expect("reading the story's events");
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "StoryStateChanged")
            .count(),
        1,
        "a lost claim must write no transition at all: {events:#?}"
    );
}
