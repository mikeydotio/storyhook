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
        .stderr(predicate::str::contains(
            "priority must be one of: critical, high, medium, low",
        ));

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
    //
    // The negative assertion is scoped to the `labels:` line rather than the
    // whole rendering, because searching an entire story for a bare label name
    // is a proxy that any unrelated field can break: SH-359 added
    // `priority: none (not assessed)`, and "a-sse-ssed" contains "sse". The
    // test was reporting a label-removal failure that had not happened. Assert
    // what this test actually means — that `sse` is not among the labels.
    story(dir.path())
        .args(["unlabel", "SH-1", "sse"])
        .assert()
        .success()
        .stdout(predicate::str::contains("labels: web"))
        .stdout(predicate::function(|out: &str| {
            let labels = out
                .lines()
                .find(|line| line.starts_with("labels:"))
                .expect("`story show` always renders a labels line");
            !labels
                .trim_start_matches("labels:")
                .split(',')
                .any(|label| label.trim() == "sse")
        }));
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
// Required creation metadata (SH-449)
// ============================================================

#[test]
fn new_without_metadata_uses_low_and_the_first_configured_type() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Defaulted work"])
        .assert()
        .success()
        .stdout(predicate::str::contains("warning:").not());

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("priority: low"))
        .stdout(predicate::str::contains("type: normal"));
}

#[test]
fn new_rejects_explicit_none_without_allocating_a_story() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Invalid", "--priority", "none"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "priority must be one of: critical, high, medium, low",
        ));

    story(dir.path())
        .args(["new", "First valid"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-1"));
}

#[test]
fn new_accepts_each_assignable_priority() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    for (index, level) in ["critical", "high", "medium", "low"].iter().enumerate() {
        story(dir.path())
            .args(["new", &format!("Story {index}"), "--priority", level])
            .assert()
            .success();
    }
}

#[test]
fn quiet_prints_nothing_and_still_creates_with_required_defaults() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Quiet default", "--quiet"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("priority: low"))
        .stdout(predicate::str::contains("type: normal"));
}
