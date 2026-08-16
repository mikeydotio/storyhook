//! Tests for `story log <id>` — the write-provenance audit trail (SH-246).
//!
//! The council's verdict is recorded on SH-246; these tests pin the
//! parts of it that are behaviour rather than prose.
//!
//! # What this trail is, and what it is not
//!
//! It answers "what wrote this, and when" from the store, in one lookup. It is
//! a **diagnostic aid, not a tamper-proof audit log** — the declared half is
//! self-attested by a caller who could have declared anything, and on a
//! single-user loopback daemon anyone able to set `$STORYHOOK_ACTOR` can
//! already write to the store directly. The tests below pin that the two halves
//! stay *distinguishable*, which is the property that survives that admission:
//!
//! * `command` is derived by the daemon from the arm it dispatched. A caller
//!   cannot lie about it, because it is never asked.
//! * `actor` is whatever the caller put in `$STORYHOOK_ACTOR`. It renders in
//!   parentheses, and parentheses mean self-attested throughout this output.
//!
//! # The motivating incident
//!
//! SH-239 was claimed and released 106 seconds later, and answering "by what?"
//! took a raw sqlite3 dump, two Rust source files and a 1000-line shell script.
//! `dispatch_claim_and_rollback_are_distinguishable` is that incident reduced to
//! the lookup it should always have been.

use assert_cmd::Command;
use storyhook::domain::StoryEvent;
use storyhook::store::test_support::inject_events;
use storyhook_test_support::{Project, TestEnv};

/// Every `story` invocation in this file runs in the shared test environment's
/// private HOME/XDG directories, so nothing here can reach the developer's own
/// storyhook state.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

/// A `TST`-prefixed project holding one story, and that story's id.
fn init_and_create() -> (Project<'static>, String) {
    let project = TestEnv::shared().project().prefix("TST").build();
    let id = project.new_story("Test story");
    (project, id)
}

/// `story log <id> --json`, parsed.
fn log_json(dir: &std::path::Path, id: &str) -> serde_json::Value {
    let out = story(dir).args(["log", id, "--json"]).output().unwrap();
    assert!(
        out.status.success(),
        "story log --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "story log --json emitted invalid JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

/// The entries array out of `story log --json`.
fn entries(dir: &std::path::Path, id: &str) -> Vec<serde_json::Value> {
    log_json(dir, id)["log"]
        .as_array()
        .expect("`log` is an array")
        .clone()
}

#[test]
fn log_lists_every_event_oldest_first() {
    let (dir, id) = init_and_create();
    story(dir.path())
        .args(["move", &id, "in-progress"])
        .assert()
        .success();

    let entries = entries(dir.path(), &id);
    assert!(
        entries.len() >= 2,
        "creation and the move should both appear: {entries:#?}"
    );

    let kinds: Vec<&str> = entries
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds.first(),
        Some(&"StoryCreated"),
        "oldest first: {kinds:?}"
    );
    assert!(
        kinds.contains(&"StoryStateChanged"),
        "the move must appear: {kinds:?}"
    );

    // Timestamps are non-decreasing, which is what "oldest first" means to a
    // reader scanning for when something happened.
    let ats: Vec<&str> = entries.iter().map(|e| e["at"].as_str().unwrap()).collect();
    let mut sorted = ats.clone();
    sorted.sort_unstable();
    assert_eq!(ats, sorted, "entries must be ordered oldest first");
}

#[test]
fn log_covers_every_event_kind_not_only_state_changes() {
    // The council chose all-kinds over transitions-only: it costs nothing (one
    // row shape) and answers strictly more incident questions.
    let (dir, id) = init_and_create();
    story(dir.path())
        .args(["comment", &id, "a comment"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", &id, "high"])
        .assert()
        .success();

    let kinds: Vec<String> = entries(dir.path(), &id)
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect();
    assert!(
        kinds.iter().any(|k| k == "StoryCommentAdded"),
        "a comment is part of the trail: {kinds:?}"
    );
    assert!(
        kinds.iter().any(|k| k == "StoryPrioritySet"),
        "a priority change is part of the trail: {kinds:?}"
    );
}

#[test]
fn every_new_event_records_the_command_that_wrote_it() {
    // `command` is daemon-derived: it names the arm that was dispatched, so a
    // caller cannot misstate it.
    let (dir, id) = init_and_create();
    story(dir.path())
        .args(["move", &id, "in-progress"])
        .assert()
        .success();

    let entries = entries(dir.path(), &id);
    let moved = entries
        .iter()
        .rev()
        .find(|e| e["kind"] == "StoryStateChanged")
        .expect("the move is in the log");
    assert_eq!(
        moved["command"], "set-state",
        "the state change was written by the arm `story move` dispatches — the daemon never \
         sees the verb the user typed, so recording that would make the attested half \
         caller-supplied: {moved:#?}"
    );

    let created = &entries[0];
    assert_eq!(
        created["command"], "new",
        "the creation was written by `story new`: {created:#?}"
    );
}

#[test]
fn a_declared_actor_is_recorded_alongside_the_command() {
    let (dir, id) = init_and_create();
    story(dir.path())
        .env("STORYHOOK_ACTOR", "story.sh:dispatch")
        .args(["move", &id, "in-progress"])
        .assert()
        .success();

    let entries = entries(dir.path(), &id);
    let moved = entries
        .iter()
        .rev()
        .find(|e| e["kind"] == "StoryStateChanged")
        .expect("the move is in the log");
    assert_eq!(moved["actor"], "story.sh:dispatch");
    assert_eq!(
        moved["command"], "set-state",
        "a declared actor never replaces the derived command — both are kept, \
         so a reader can tell the attested half from the self-attested half"
    );
}

#[test]
fn an_undeclared_actor_is_null_rather_than_guessed() {
    // No `$STORYHOOK_ACTOR` means the caller declared nothing. That is not the
    // same as the command being unknown, and it must not be filled in from it.
    let (dir, id) = init_and_create();
    story(dir.path())
        .args(["move", &id, "in-progress"])
        .assert()
        .success();

    let moved = entries(dir.path(), &id)
        .into_iter()
        .rev()
        .find(|e| e["kind"] == "StoryStateChanged")
        .expect("the move is in the log");
    assert!(
        moved["actor"].is_null(),
        "an undeclared actor is null, not a copy of the command: {moved:#?}"
    );
    assert_eq!(moved["command"], "set-state");
}

#[test]
fn dispatch_claim_and_rollback_are_distinguishable() {
    // The motivating incident, reduced to a lookup. Both writes are `story
    // move`; only the declared actor tells them apart, which is exactly why a
    // verb-only record was rejected.
    let (dir, id) = init_and_create();
    story(dir.path())
        .env("STORYHOOK_ACTOR", "story.sh:dispatch")
        .args(["move", &id, "in-progress"])
        .assert()
        .success();
    story(dir.path())
        .env("STORYHOOK_ACTOR", "story.sh:dispatch-rollback")
        .args(["move", &id, "todo"])
        .assert()
        .success();

    let moves: Vec<serde_json::Value> = entries(dir.path(), &id)
        .into_iter()
        .filter(|e| e["kind"] == "StoryStateChanged")
        .collect();
    assert_eq!(moves.len(), 2, "two moves: {moves:#?}");
    assert_eq!(moves[0]["actor"], "story.sh:dispatch");
    assert_eq!(moves[1]["actor"], "story.sh:dispatch-rollback");
    assert_eq!(moves[0]["command"], "set-state");
    assert_eq!(moves[1]["command"], "set-state");
}

#[test]
fn human_output_renders_a_declared_actor_in_parentheses() {
    // Parentheses are the convention for "self-attested" in this output; a bare
    // word is the daemon-derived command.
    let (dir, id) = init_and_create();
    story(dir.path())
        .env("STORYHOOK_ACTOR", "story.sh:dispatch-rollback")
        .args(["move", &id, "in-progress"])
        .assert()
        .success();

    let out = story(dir.path()).args(["log", &id]).output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("(story.sh:dispatch-rollback)"),
        "a declared actor renders parenthesized: {text}"
    );
    assert!(
        text.contains("set-state"),
        "the derived command renders too: {text}"
    );
}

#[test]
fn an_unrecorded_actor_is_not_rendered_as_the_unset_dash() {
    // A pre-cutover event's provenance was never captured. `-` in this codebase
    // means "unset, could be set later" (an assignee), and reusing it here would
    // make an old event read as "nobody did this".
    //
    // The events table is genuinely append-only — `events_reject_update`
    // refuses an UPDATE outright — so a pre-cutover row cannot be faked by
    // blanking one. It is *appended*, through the same injector `story migrate`
    // and `import-project` write with: those replay a history rather than
    // perform it, so they record `Provenance::unrecorded`, which is exactly the
    // state every row written before SH-246 is in.
    let project = TestEnv::shared().project().prefix("UNR").build();
    let id = project.new_story("Story with a pre-cutover event");
    let store = project.open_store();
    let project_id = project.project_id(&store);

    inject_events(
        &store,
        project_id,
        project.story_no(&store, &id),
        &[StoryEvent::StoryStateChanged {
            at: "2026-03-11T00:00:01Z".to_string(),
            state: "in-progress".to_string(),
        }],
    )
    .expect("injecting an event with no provenance");

    let out = story(project.path()).args(["log", &id]).output().unwrap();
    assert!(
        out.status.success(),
        "story log failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("(unrecorded)"),
        "provenance that was never captured says so: {text}"
    );
    // The distinction this test exists for: not the `-` this codebase uses for
    // a field that is merely unset, which would read as "nobody did this".
    assert!(
        !text.lines().any(|line| line.contains("  -  ")),
        "an unrecorded actor must not borrow the unset-field dash: {text}"
    );

    let injected = entries(project.path(), &id)
        .into_iter()
        .find(|entry| entry["at"] == "2026-03-11T00:00:01Z")
        .expect("the injected event is in the log");
    assert!(injected["command"].is_null());
    assert!(injected["actor"].is_null());
}

#[test]
fn an_actor_carrying_control_characters_is_refused_not_sanitized() {
    // This string is rendered into a terminal and stored in an audit trail. A
    // trail that silently alters what it was told is worse than one that
    // declines to record, so the refusal is loud and names the variable.
    let (dir, id) = init_and_create();
    let out = story(dir.path())
        .env("STORYHOOK_ACTOR", "evil\u{1b}[2Kactor")
        .args(["move", &id, "in-progress"])
        .output()
        .unwrap();

    assert!(
        !out.status.success(),
        "an escape sequence in an audit label must refuse"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("STORYHOOK_ACTOR"),
        "the refusal names the variable so it can be fixed: {stderr}"
    );

    // And it refused *before* writing: the story never moved.
    let show = story(dir.path())
        .args(["show", &id, "--json"])
        .output()
        .unwrap();
    let show_json: serde_json::Value = serde_json::from_slice(&show.stdout).unwrap();
    assert_eq!(
        show_json["story"]["story"]["state"], "todo",
        "a refused actor must not have written anything"
    );
}

#[test]
fn an_actor_carrying_a_newline_is_refused() {
    // A newline would let one entry forge additional lines in the rendered
    // trail — the same class as the escape sequence above.
    let (dir, id) = init_and_create();
    let out = story(dir.path())
        .env("STORYHOOK_ACTOR", "line-one\nline-two")
        .args(["move", &id, "in-progress"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "a newline in an actor must refuse");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("STORYHOOK_ACTOR"),
        "the refusal names the variable"
    );
}

#[test]
fn an_over_long_actor_is_refused() {
    let (dir, id) = init_and_create();
    let out = story(dir.path())
        .env("STORYHOOK_ACTOR", "x".repeat(4096))
        .args(["move", &id, "in-progress"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "an unbounded actor must refuse");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("STORYHOOK_ACTOR"),
        "the refusal names the variable"
    );
}

#[test]
fn an_empty_actor_is_treated_as_undeclared_rather_than_refused() {
    // `STORYHOOK_ACTOR=` is what an unset-but-exported variable looks like in a
    // shell script, and refusing it would break callers for no diagnostic gain.
    let (dir, id) = init_and_create();
    story(dir.path())
        .env("STORYHOOK_ACTOR", "")
        .args(["move", &id, "in-progress"])
        .assert()
        .success();

    let moved = entries(dir.path(), &id)
        .into_iter()
        .rev()
        .find(|e| e["kind"] == "StoryStateChanged")
        .expect("the move is in the log");
    assert!(
        moved["actor"].is_null(),
        "an empty declaration is no declaration"
    );
}

#[test]
fn log_refuses_an_unknown_story() {
    let (dir, _id) = init_and_create();
    let out = story(dir.path()).args(["log", "TST-999"]).output().unwrap();
    assert!(!out.status.success(), "an unknown story is an error");
}

#[test]
fn history_is_not_a_cli_verb() {
    // `Invocation::History` is the TUI's undo primitive and is deliberately not
    // CLI-reachable (src/cli.rs). `story log` was named to sit beside it rather
    // than collide with it; this fails if anyone later wires `history` up as a
    // synonym and reintroduces two "history" concepts in one codebase.
    let (dir, id) = init_and_create();
    let out = story(dir.path()).args(["history", &id]).output().unwrap();
    assert!(
        !out.status.success(),
        "`story history` must not exist as a verb: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}
