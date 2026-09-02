//! `story list`'s default visibility filter (SH-409): closed and archived
//! stories are excluded unless a flag names them back. Permanently deleted
//! stories are absent from every surface.
//!
//! This is a deliberate reversal of the SH-175 council's "nothing-hidden-by-
//! default" contract, which `tests/story_list_drafts.rs` still pins for
//! drafts (an OPEN state, untouched here). Both losing SH-175 proposals
//! flagged the same risk in any default exclusion — no way to tell it
//! happened — so this file also pins the mitigation: `message` names what
//! was hidden and the flag that would show it, with counts that respect
//! every other filter the caller passed.

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use storyhook_test_support::{TestEnv, scratch_dir};

/// Every `story` this file runs is the one THIS build produced, in the shared
/// test environment's private `HOME`, XDG directories and store — so nothing
/// here can reach the developer's own storyhook state, with or without a
/// wrapper script supplying one.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

fn run(dir: &std::path::Path, args: &[&str]) {
    story(dir).args(args).assert().success();
}

/// `story list --json`'s `.stories[].story.id` set, order-independent.
fn list_ids(dir: &std::path::Path, args: &[&str]) -> Vec<String> {
    let mut full = vec!["list", "--json"];
    full.extend_from_slice(args);
    let output = story(dir).args(&full).assert().success();
    let stdout = output.get_output().stdout.clone();
    let json: serde_json::Value = serde_json::from_slice(&stdout).expect("valid JSON");
    json["stories"]
        .as_array()
        .expect("a stories array")
        .iter()
        .map(|view| view["story"]["id"].as_str().unwrap().to_string())
        .collect()
}

fn list_message(dir: &std::path::Path, args: &[&str]) -> Option<String> {
    let mut full = vec!["list", "--json"];
    full.extend_from_slice(args);
    let output = story(dir).args(&full).assert().success();
    let stdout = output.get_output().stdout.clone();
    let json: serde_json::Value = serde_json::from_slice(&stdout).expect("valid JSON");
    json["message"].as_str().map(str::to_string)
}

/// One of each: open, closed (not archived), archived, deleted, draft.
/// Ids are assigned in creation order, so this is also the id map:
/// SH-1 open, SH-2 closed, SH-3 archived, SH-4 deleted, SH-5 draft.
fn seed_one_of_each(dir: &std::path::Path) {
    run(dir, &["project", "new", "--prefix", "SH"]);
    run(dir, &["new", "Open story"]);
    run(dir, &["new", "Closed story"]);
    run(dir, &["new", "Archived story"]);
    run(dir, &["new", "Deleted story"]);
    run(dir, &["new", "Draft story", "--draft"]);

    run(dir, &["move", "SH-2", "done"]);
    run(dir, &["move", "SH-3", "done"]);
    run(dir, &["archive", "SH-3"]);
    run(dir, &["delete", "SH-4", "--force"]);
}

#[test]
fn bare_list_shows_only_open_stories_drafts_included() {
    let dir = scratch_dir();
    seed_one_of_each(dir.path());

    let ids = list_ids(dir.path(), &[]);
    assert_eq!(
        ids,
        ["SH-1", "SH-5"],
        "closed, archived and deleted are hidden; the draft (OPEN) still shows inline"
    );

    // Human output still carries the [draft] badge SH-175 established.
    story(dir.path())
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("[draft]"))
        .stdout(contains("[deleted]").not())
        .stdout(contains("[archived]").not());
}

#[test]
fn include_closed_shows_closed_but_not_archived_or_deleted() {
    let dir = scratch_dir();
    seed_one_of_each(dir.path());

    let ids = list_ids(dir.path(), &["--include-closed"]);
    assert_eq!(ids, ["SH-1", "SH-2", "SH-5"]);
}

#[test]
fn include_archived_implies_include_closed() {
    let dir = scratch_dir();
    seed_one_of_each(dir.path());

    let ids = list_ids(dir.path(), &["--include-archived"]);
    assert_eq!(
        ids,
        ["SH-1", "SH-2", "SH-3", "SH-5"],
        "archived implies closed — a lone --include-archived still reveals plain closed stories"
    );

    story(dir.path())
        .args(["list", "--include-archived"])
        .assert()
        .success()
        .stdout(contains("[archived]"));
}

#[test]
fn all_is_sugar_for_both_include_flags_and_never_includes_deleted() {
    let dir = scratch_dir();
    seed_one_of_each(dir.path());

    let via_all = list_ids(dir.path(), &["--all"]);
    let via_both = list_ids(dir.path(), &["--include-closed", "--include-archived"]);
    assert_eq!(via_all, via_both);
    assert_eq!(via_all, ["SH-1", "SH-2", "SH-3", "SH-5"]);
}

/// `--all` and `--include-closed --include-archived` are documented as
/// literally the same request, not merely the same result set — the parser
/// collapses `--all` into the two booleans, so this is an equality on the
/// whole envelope (including `message`), not just the id list above.
#[test]
fn all_parses_identically_to_both_include_flags() {
    let dir = scratch_dir();
    seed_one_of_each(dir.path());

    let mut via_all = story(dir.path());
    via_all.args(["list", "--all", "--json"]);
    let mut via_both = story(dir.path());
    via_both.args(["list", "--include-closed", "--include-archived", "--json"]);

    let all_stdout = via_all.assert().success().get_output().stdout.clone();
    let both_stdout = via_both.assert().success().get_output().stdout.clone();
    assert_eq!(all_stdout, both_stdout);
}

/// The story's "not includable even with a flag" clause, proven rather than
/// asserted once: sweep every combination of the three visibility flags and
/// check the deleted id never appears.
#[test]
fn deleted_never_appears_under_any_flag_combination() {
    let dir = scratch_dir();
    seed_one_of_each(dir.path());

    let flag_sets: &[&[&str]] = &[
        &[],
        &["--include-closed"],
        &["--include-archived"],
        &["--all"],
        &["--include-closed", "--include-archived"],
    ];
    for flags in flag_sets {
        let ids = list_ids(dir.path(), flags);
        assert!(
            !ids.contains(&"SH-4".to_string()),
            "SH-4 (deleted) leaked under {flags:?}: {ids:?}"
        );
    }
}

#[test]
fn state_naming_a_closed_slug_lifts_the_exclusion_and_says_why() {
    let dir = scratch_dir();
    seed_one_of_each(dir.path());

    let ids = list_ids(dir.path(), &["--state", "done"]);
    assert_eq!(
        ids,
        ["SH-2"],
        "the closed exclusion lifts for `done`, but the archived one stays hidden"
    );

    let message = list_message(dir.path(), &["--state", "done"]).unwrap();
    assert!(
        message.contains("`done` is a closed state"),
        "message: {message}"
    );
}

#[test]
fn state_lift_does_not_reveal_archived_stories() {
    let dir = scratch_dir();
    seed_one_of_each(dir.path());

    // SH-3 is archived AND in the `done` state; `--state done` alone must
    // not surface it — matching the dashboard's own Done column, which
    // shows closed cards but keeps archived ones behind a separate toggle.
    let ids = list_ids(dir.path(), &["--state", "done"]);
    assert!(!ids.contains(&"SH-3".to_string()), "{ids:?}");

    // --include-archived (or --all) still reaches it, same as any other
    // archived story.
    let ids = list_ids(dir.path(), &["--state", "done", "--include-archived"]);
    assert!(ids.contains(&"SH-3".to_string()), "{ids:?}");
}

#[test]
fn state_naming_an_open_slug_is_unaffected() {
    let dir = scratch_dir();
    seed_one_of_each(dir.path());

    // SH-5 (the draft) starts life in `todo` too, same as SH-1 — an OPEN
    // state was never excluded, so both simply match the filter as normal.
    let ids = list_ids(dir.path(), &["--state", "todo"]);
    assert_eq!(ids, ["SH-1", "SH-5"]);
    assert_eq!(
        list_message(dir.path(), &["--state", "todo"]),
        None,
        "nothing was hidden by this call, so nothing to explain"
    );
}

/// The exclusion rule reads the project's own state catalog, not a
/// hardcoded `"done"` — a custom CLOSED-superstate column lifts it too.
#[test]
fn a_custom_closed_state_lifts_the_exclusion_too() {
    let dir = scratch_dir();
    run(dir.path(), &["project", "new", "--prefix", "SH"]);
    run(
        dir.path(),
        &["state", "add", "wontfix", "--super", "CLOSED"],
    );
    run(dir.path(), &["new", "Open story"]);
    run(dir.path(), &["new", "Won't fix this one"]);
    run(dir.path(), &["move", "SH-2", "wontfix"]);

    assert_eq!(list_ids(dir.path(), &[]), ["SH-1"]);

    let ids = list_ids(dir.path(), &["--state", "wontfix"]);
    assert_eq!(ids, ["SH-2"]);
    let message = list_message(dir.path(), &["--state", "wontfix"]).unwrap();
    assert!(message.contains("`wontfix` is a closed state"), "{message}");
}

/// The hidden count in `message` is exactly honest about the caller's other
/// filters: `--label infra` reports only the hidden stories that carry that
/// label, not every hidden story in the project.
#[test]
fn the_hidden_count_respects_other_filters() {
    let dir = scratch_dir();
    run(dir.path(), &["project", "new", "--prefix", "SH"]);
    run(dir.path(), &["new", "Open, infra"]);
    run(dir.path(), &["new", "Closed, infra"]);
    run(dir.path(), &["new", "Closed, other"]);
    run(dir.path(), &["label", "SH-1", "infra"]);
    run(dir.path(), &["label", "SH-2", "infra"]);
    run(dir.path(), &["label", "SH-3", "other"]);
    run(dir.path(), &["move", "SH-2", "done"]);
    run(dir.path(), &["move", "SH-3", "done"]);

    let message = list_message(dir.path(), &["--label", "infra"]).unwrap();
    assert!(
        message.contains("1 closed") && !message.contains("2 closed"),
        "only the labelled closed story should be counted: {message}"
    );
    assert_eq!(list_ids(dir.path(), &["--label", "infra"]), ["SH-1"]);
}

/// The count still prints even when the visible result is empty — the case
/// the SH-175 council flagged as the missing mitigation for any default
/// exclusion, and the one `Response::Stories`'s own early return used to
/// drop silently before this story's fix.
#[test]
fn the_hidden_count_survives_an_empty_visible_result() {
    let dir = scratch_dir();
    run(dir.path(), &["project", "new", "--prefix", "SH"]);
    run(dir.path(), &["new", "Only story"]);
    run(dir.path(), &["move", "SH-1", "done"]);

    story(dir.path())
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("no stories found"))
        .stdout(contains("1 closed"))
        .stdout(contains("--include-closed"));

    let message = list_message(dir.path(), &[]).unwrap();
    assert!(message.contains("1 closed"), "{message}");
    assert_eq!(list_ids(dir.path(), &[]), Vec::<String>::new());
}

/// `--ready`, `--blocked` and `--stale` already restricted themselves to
/// OPEN stories before SH-409 (`is_ready`/the stale filter's own
/// `superstate == Open` retain) — the new default visibility layer sits
/// behind them and changes nothing they report.
#[test]
fn ready_blocked_and_stale_are_unaffected_by_the_new_default() {
    let dir = scratch_dir();
    run(dir.path(), &["project", "new", "--prefix", "SH"]);
    run(dir.path(), &["new", "Ready"]);
    run(dir.path(), &["new", "Blocked"]);
    run(dir.path(), &["block", "SH-2", "external dependency"]);
    run(dir.path(), &["new", "Closed"]);
    run(dir.path(), &["move", "SH-3", "done"]);

    assert_eq!(list_ids(dir.path(), &["--ready"]), ["SH-1"]);
    assert_eq!(list_ids(dir.path(), &["--blocked"]), ["SH-2"]);
    assert_eq!(
        list_message(dir.path(), &["--ready"]),
        None,
        "a closed story was never going to be ready or blocked; nothing to explain"
    );

    // Even with --include-closed, a closed story is finished, not stale or
    // ready or blocked — the stale filter's own superstate == Open retain
    // and `is_ready`/`is_claimable` are unchanged by this story.
    assert_eq!(
        list_ids(dir.path(), &["--ready", "--include-closed"]),
        ["SH-1"]
    );
    assert_eq!(
        list_ids(dir.path(), &["--blocked", "--include-closed"]),
        ["SH-2"]
    );
}
