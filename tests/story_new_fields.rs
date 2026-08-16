// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

// ============================================================
// story new --description / --priority / --label(s) / --assignee
// ============================================================

#[test]
fn new_with_description_sets_description() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Login crash", "--description", "Users can't log in"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("description: Users can't log in"));
}

#[test]
fn new_without_description_omits_description_line() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "No description here"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("description:").not());
}

#[test]
fn new_with_priority_sets_priority() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Urgent bug", "--priority", "critical"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("priority: critical"));
}

#[test]
fn new_with_invalid_priority_is_rejected_and_creates_no_story() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Bad priority", "--priority", "urgent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid priority"));

    story(dir.path()).args(["show", "SH-1"]).assert().failure();
}

#[test]
fn new_with_repeated_label_flags_accumulates() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Multi-label", "--label", "bug", "--label", "web"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("labels: bug, web"));
}

#[test]
fn new_with_labels_csv_splits_and_trims() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "CSV labels", "--labels", "bug, web, cli"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("labels: bug, cli, web"));
}

/// SH-164's repro: SH-145 was filed with `--label "daemon,tailnet"` — a
/// single `--label` value that itself carries a comma, the one shape
/// `--label` used to pass through verbatim while every other label-bearing
/// surface split on it. A single `--label "web,sse"` must file the same two
/// labels a `--labels "web,sse"` or two repeated `--label` flags would.
#[test]
fn new_with_a_comma_inside_a_single_label_flag_still_splits() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "SH-145 repro", "--label", "web,sse"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("labels: sse, web"));

    // The point of splitting on the way in: the resulting labels are
    // addressable again, which `web,sse` as one label never was.
    story(dir.path())
        .args(["unlabel", "SH-1", "sse"])
        .assert()
        .success()
        .stdout(predicate::str::contains("labels: web"))
        .stdout(predicate::str::contains("sse").not());
}

#[test]
fn new_with_assignee_sets_assignee() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["member", "add", "mikey <mw@mikey.io>"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Assigned story", "--assignee", "mikey"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("assignee: mikey"));
}

#[test]
fn new_with_unknown_assignee_is_rejected_and_creates_no_story() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Bad assignee", "--assignee", "nobody"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("member `nobody` not found"));

    story(dir.path()).args(["show", "SH-1"]).assert().failure();
}

#[test]
fn new_with_all_fields_writes_single_enriched_story() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["member", "add", "mikey <mw@mikey.io>"])
        .assert()
        .success();

    story(dir.path())
        .args([
            "new",
            "Full story",
            "--type",
            "bug",
            "--description",
            "Everything at once",
            "--priority",
            "high",
            "--assignee",
            "mikey",
            "--label",
            "web",
            "--label",
            "urgent",
        ])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type: bug"))
        .stdout(predicate::str::contains("description: Everything at once"))
        .stdout(predicate::str::contains("priority: high"))
        .stdout(predicate::str::contains("assignee: mikey"))
        .stdout(predicate::str::contains("labels: urgent, web"));
}

// ============================================================
// story set --description
// ============================================================

#[test]
fn set_description_updates_existing_story() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Needs a description"])
        .assert()
        .success();

    story(dir.path())
        .args(["set", "SH-1", "--description", "Added after creation"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "description: Added after creation",
        ));
}

#[test]
fn set_description_last_write_wins() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Story", "--description", "First draft"])
        .assert()
        .success();

    story(dir.path())
        .args(["set", "SH-1", "--description", "Final draft"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("description: Final draft"))
        .stdout(predicate::str::contains("description: First draft").not());
}

// ============================================================
// story import maps description to a first-class field, not a comment
// ============================================================

#[test]
fn import_maps_description_to_description_field_not_comment() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    let json = r#"[
        {"title": "Imported story", "description": "Came from import"}
    ]"#;

    story(dir.path())
        .args(["import"])
        .write_stdin(json)
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("description: Came from import"))
        .stdout(predicate::str::contains("comments:").not());
}

// ============================================================
// story new --draft (SH-175)
// ============================================================

#[test]
fn new_without_draft_is_live() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Live story"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("draft: no"));
}

#[test]
fn new_with_draft_flag_creates_a_draft() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "A sketch", "--draft"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("draft: yes"));
}

#[test]
fn a_draft_claims_a_story_id_like_any_other_story() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "A draft", "--draft"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-1"));

    story(dir.path())
        .args(["new", "The next story"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-2"));
}

// ============================================================
// story new with no --priority at all (SH-354)
// ============================================================
//
// `Priority::None` is the enum's `#[default]`, so a story filed with no
// `--priority` lands there — and `none` is not "unset". The rubric shipped in
// `story help priority-rubric` defines it as *deliberately parked*: work whose
// owner has decided the loop must not pick it up. An omitted flag therefore
// stores a claim nobody made, silently, at the bottom of `ready_order`. SH-283
// is what that costs — a live, silent, cross-story overwrite of the system of
// record, filed at `none` and sorted dead last of twenty-six.
//
// The repair is a warning, not a refusal and not a prompt: `story new` is the
// most-used command in the tool, `dispatch` runs inside the daemon where there
// is no terminal to ask at, and agents create stories non-interactively — a
// prompt would degrade to a hang or a silent default exactly where the defect
// happens. Decided unanimously by council; see
// `.council/sh354-priority-rubric-reach-and-none-default/DECISION.md`.
//
// The load-bearing distinction is *flag absent* versus *`--priority none`
// given*. Both end as the same enum variant, so the check has to happen where
// the `Option` still exists. The paired tests below are what stop that from
// regressing into either failure: nagging every deliberate parking, or going
// quiet on the omission this story exists to catch.

/// The phrase the warning is recognised by, in both renderings.
const PRIORITY_WARNING_MARKER: &str = "priority not set";

#[test]
fn new_without_a_priority_warns_and_still_exits_zero() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Unassessed work"])
        .assert()
        // Exit zero: the story was created. The warning is about what was not
        // said, never about what failed.
        .success()
        .stdout(predicate::str::contains(PRIORITY_WARNING_MARKER))
        // It must name the consequence, not merely scold.
        .stdout(predicate::str::contains("story next"))
        // …and where the criteria are…
        .stdout(predicate::str::contains("story help priority-rubric"))
        // …and which story it is talking about, so the remedy it offers is one
        // the reader can run rather than one they have to adapt. A batch caller
        // reading several of these needs the id to tell them apart.
        .stdout(predicate::str::contains("story prioritize SH-1 <level>"));

    // The story really is there, at the level the warning named.
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("priority: none"));
}

#[test]
fn new_with_an_explicit_priority_none_stays_silent() {
    // The load-bearing case. `--priority none` is a decision — the rubric's own
    // "deliberately parked" — and warning about it would train every reader to
    // ignore the warning that matters. This passing while the test above fails
    // (or vice versa) is the whole point of the pair: it proves the check reads
    // the flag, not the stored value, which is the one thing that cannot be
    // told apart after `Option::unwrap_or_default`.
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Parked on purpose", "--priority", "none"])
        .assert()
        .success()
        .stdout(predicate::str::contains(PRIORITY_WARNING_MARKER).not());
}

#[test]
fn new_with_any_stated_priority_stays_silent() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    for (index, level) in ["critical", "high", "medium", "low"].iter().enumerate() {
        story(dir.path())
            .args(["new", &format!("Story {index}"), "--priority", level])
            .assert()
            .success()
            .stdout(predicate::str::contains(PRIORITY_WARNING_MARKER).not());
    }
}

#[test]
fn the_warning_is_data_in_the_json_envelope_and_a_line_in_the_human_rendering() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // A scripted caller reads it as a field, never by regexing prose.
    let json = story(dir.path())
        .args(["new", "Unassessed work", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let envelope: serde_json::Value =
        serde_json::from_slice(&json).expect("`story new --json` must emit one JSON object");
    let warnings = envelope["warnings"]
        .as_array()
        .expect("the envelope must carry a `warnings` array when a warning was raised");
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .is_some_and(|w| w.contains(PRIORITY_WARNING_MARKER))),
        "the priority warning must be data in the envelope, not prose in `message`: \
         {envelope}"
    );

    // And a human sees it. `StoryView::warnings` was serialized into the JSON
    // envelope but never rendered by `render_story`, which printed
    // `flagged_reasons` and nothing else — so before SH-354 a warning parked
    // there reached machines and nobody else. That asymmetry is SH-354's own
    // failure mode one layer up, and this assertion is what keeps it closed.
    story(dir.path())
        .args(["new", "Unassessed work, again"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "warning: {PRIORITY_WARNING_MARKER}"
        )));
}

#[test]
fn quiet_prints_nothing_and_still_creates() {
    // `--quiet` suppresses every rendering in this tool, and the warning does
    // not get an exemption: a caller that asked for silence and got a line
    // would be a worse surprise than the one this story is fixing. The record
    // is unaffected either way, and `story show` still tells the truth.
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Quietly unassessed", "--quiet"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("priority: none"));
}

#[test]
fn the_warning_does_not_depend_on_the_type_slug() {
    // The story asked whether `--type bug` specifically should be treated
    // differently. It must not be: `bug` is only a *default* type slug, a
    // project may rename or drop it, and a check keyed on the word would go
    // quiet for exactly the projects that renamed it — a guard that stops
    // guarding without saying so. The trigger is the missing flag, and nothing
    // else.
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    for slug in ["bug", "chore", "epic"] {
        story(dir.path())
            .args(["new", &format!("A {slug}"), "--type", slug])
            .assert()
            .success()
            .stdout(predicate::str::contains(PRIORITY_WARNING_MARKER));
    }
}
