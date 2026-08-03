//! `story state list|add|set|remove|reorder` — the CLI half of per-repo
//! status configuration (SH-41).

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

fn init(dir: &std::path::Path) {
    story(dir)
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
}

/// The catalog as `story state list` renders it.
///
/// Asked of the CLI rather than of `.storyhook/states.toml`, because the file
/// is not where the catalog lives after the flip — and because the rendering is
/// the part a user can actually observe. Every assertion below was originally
/// written against the TOML; each one now names the same fact in the form the
/// command reports it.
fn state_listing(dir: &std::path::Path) -> String {
    let out = story(dir)
        .args(["state", "list"])
        .output()
        .expect("running `story state list`");
    assert!(
        out.status.success(),
        "`story state list` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ============================================================
// story state list
// ============================================================

#[test]
fn state_list_shows_defaults_with_superstates() {
    let dir = tempdir().unwrap();
    init(dir.path());

    story(dir.path())
        .args(["state", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("todo (OPEN)"))
        .stdout(predicate::str::contains("in-progress (OPEN, active)"))
        .stdout(predicate::str::contains("done (CLOSED)"));
}

#[test]
fn state_list_shows_descriptions_and_counts() {
    let dir = tempdir().unwrap();
    init(dir.path());
    story(dir.path())
        .args([
            "state",
            "add",
            "review",
            "--super",
            "OPEN",
            "--description",
            "Waiting on a reviewer",
        ])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "A story"])
        .assert()
        .success();

    story(dir.path())
        .args(["state", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("todo (OPEN) — 1 open"))
        .stdout(predicate::str::contains(
            "review (OPEN) — Waiting on a reviewer",
        ));
}

#[test]
fn state_list_reflects_order() {
    let dir = tempdir().unwrap();
    init(dir.path());
    story(dir.path())
        .args(["state", "reorder", "done,todo,blocked,in-progress"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["state", "list"])
        .output()
        .unwrap()
        .stdout;
    let text = String::from_utf8(output).unwrap();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(lines[0].starts_with("done"), "got: {lines:?}");
    assert!(lines[1].starts_with("todo"), "got: {lines:?}");
}

// ============================================================
// story state add
// ============================================================

#[test]
fn state_add_stores_description_and_role() {
    let dir = tempdir().unwrap();
    init(dir.path());

    story(dir.path())
        .args([
            "state",
            "add",
            "review",
            "--super",
            "OPEN",
            "--description",
            "Waiting on a reviewer",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("added state review (OPEN)"));

    assert!(state_listing(dir.path()).contains("Waiting on a reviewer"));
}

#[test]
fn state_add_accepts_equals_form_flags() {
    let dir = tempdir().unwrap();
    init(dir.path());

    story(dir.path())
        .args([
            "state",
            "add",
            "review",
            "--super=OPEN",
            "--description=Hmm",
        ])
        .assert()
        .success();

    assert!(state_listing(dir.path()).contains("Hmm"));
}

#[test]
fn state_add_rejects_an_invalid_slug() {
    let dir = tempdir().unwrap();
    init(dir.path());

    story(dir.path())
        .args(["state", "add", "In Review", "--super", "OPEN"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid state slug"));
}

#[test]
fn state_add_rejects_an_invalid_superstate() {
    let dir = tempdir().unwrap();
    init(dir.path());

    story(dir.path())
        .args(["state", "add", "review", "--super", "MAYBE"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("OPEN or CLOSED"));
}

#[test]
fn state_add_requires_a_superstate() {
    let dir = tempdir().unwrap();
    init(dir.path());

    story(dir.path())
        .args(["state", "add", "review"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("usage: story state add"));
}

// ============================================================
// story state set
// ============================================================

#[test]
fn state_set_updates_description_then_clears_it() {
    let dir = tempdir().unwrap();
    init(dir.path());

    story(dir.path())
        .args(["state", "set", "todo", "--description", "Not started yet"])
        .assert()
        .success()
        .stdout(predicate::str::contains("updated state todo (OPEN)"));
    assert!(state_listing(dir.path()).contains("Not started yet"));

    story(dir.path())
        .args(["state", "set", "todo", "--no-description"])
        .assert()
        .success();
    assert!(!state_listing(dir.path()).contains("Not started yet"));
}

#[test]
fn state_set_moves_and_clears_the_active_role() {
    let dir = tempdir().unwrap();
    init(dir.path());

    story(dir.path())
        .args(["state", "set", "in-progress", "--role", "none"])
        .assert()
        .success();
    assert!(
        !state_listing(dir.path()).contains("active"),
        "clearing the role must leave no status carrying it"
    );

    story(dir.path())
        .args(["state", "set", "todo", "--role", "active"])
        .assert()
        .success();
    assert!(state_listing(dir.path()).contains("todo (OPEN, active)"));
}

#[test]
fn state_set_rejects_a_second_active_role() {
    let dir = tempdir().unwrap();
    init(dir.path());

    story(dir.path())
        .args(["state", "set", "todo", "--role", "active"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("only one state may have role"));
}

#[test]
fn state_set_rejects_contradictory_description_flags() {
    let dir = tempdir().unwrap();
    init(dir.path());

    story(dir.path())
        .args([
            "state",
            "set",
            "todo",
            "--description",
            "x",
            "--no-description",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("contradict"));
}

#[test]
fn state_set_rejects_an_empty_change_set() {
    let dir = tempdir().unwrap();
    init(dir.path());

    story(dir.path())
        .args(["state", "set", "todo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nothing to change"));
}

#[test]
fn state_set_superstate_requires_a_destination_when_occupied() {
    let dir = tempdir().unwrap();
    init(dir.path());
    story(dir.path())
        .args(["state", "add", "in-review", "--super", "OPEN"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "A story"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-1", "in-review"])
        .assert()
        .success();

    story(dir.path())
        .args(["state", "set", "in-review", "--super", "CLOSED"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("1 open story"))
        .stderr(predicate::str::contains("in-progress"));

    // Refused edits change nothing.
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .stdout(predicate::str::contains("state: in-review (OPEN)"));
}

#[test]
fn state_set_superstate_migrates_with_a_destination() {
    let dir = tempdir().unwrap();
    init(dir.path());
    story(dir.path())
        .args(["state", "add", "in-review", "--super", "OPEN"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "A story"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-1", "in-review"])
        .assert()
        .success();

    story(dir.path())
        .args([
            "state",
            "set",
            "in-review",
            "--super",
            "CLOSED",
            "--move-stories-to",
            "in-progress",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("moved 1 story to in-progress"));

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .stdout(predicate::str::contains("state: in-progress"));
}

// ============================================================
// story state remove
// ============================================================

#[test]
fn state_remove_drops_an_empty_state() {
    let dir = tempdir().unwrap();
    init(dir.path());
    // A state beyond the required floor: the four a project must have cannot
    // be removed at all (SH-125), so removal itself needs a fifth to remove.
    story(dir.path())
        .args(["state", "add", "in-review", "--super", "OPEN"])
        .assert()
        .success();

    story(dir.path())
        .args(["state", "remove", "in-review"])
        .assert()
        .success()
        .stdout(predicate::str::contains("removed state in-review"));
    assert!(!state_listing(dir.path()).contains("in-review"));
}

#[test]
fn state_remove_requires_a_destination_when_occupied() {
    let dir = tempdir().unwrap();
    init(dir.path());
    story(dir.path())
        .args(["state", "add", "in-review", "--super", "OPEN"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "A story"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-1", "in-review"])
        .assert()
        .success();

    story(dir.path())
        .args(["state", "remove", "in-review"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("1 open story"));
}

#[test]
fn state_remove_migrates_with_a_destination() {
    let dir = tempdir().unwrap();
    init(dir.path());
    story(dir.path())
        .args(["state", "add", "in-review", "--super", "OPEN"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "A story"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-1", "in-review"])
        .assert()
        .success();

    story(dir.path())
        .args([
            "state",
            "remove",
            "in-review",
            "--move-stories-to",
            "in-progress",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("moved 1 story to in-progress"));

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .stdout(predicate::str::contains("state: in-progress"));
}

#[test]
fn state_remove_refuses_a_state_with_archived_history() {
    let dir = tempdir().unwrap();
    init(dir.path());
    // `cancelled` rather than `done`, so the removal is blocked by the
    // archived story alone and not by the "keep one CLOSED state" rule.
    story(dir.path())
        .args(["state", "add", "cancelled", "--super", "CLOSED"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "A story"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-1", "cancelled"])
        .assert()
        .success();

    story(dir.path())
        .args(["state", "remove", "cancelled", "--move-stories-to", "todo"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("1 archived story"));
}

/// Removing a required state fails on the structural rule first — the project
/// would be left without a state every project must have — whatever its
/// stories happen to be doing.
#[test]
fn state_remove_reports_the_structural_rule_before_story_counts() {
    let dir = tempdir().unwrap();
    init(dir.path());
    story(dir.path())
        .args(["new", "A story"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success();

    story(dir.path())
        .args(["state", "remove", "done"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("every project needs"))
        .stderr(predicate::str::contains("`done`"));
}

/// Each of the four is refused, and each refusal names the way out. `done` is
/// also the only CLOSED state a default project has, so this subsumes the
/// older "at least one CLOSED state" refusal on this path (SH-125).
#[test]
fn state_remove_refuses_every_required_state() {
    let dir = tempdir().unwrap();
    init(dir.path());

    for slug in ["todo", "in-progress", "blocked", "done"] {
        story(dir.path())
            .args(["state", "remove", slug])
            .assert()
            .failure()
            .stderr(predicate::str::contains("every project needs"))
            .stderr(predicate::str::contains("story doctor --fix"));
    }

    let listing = state_listing(dir.path());
    for slug in ["todo", "in-progress", "blocked", "done"] {
        assert!(listing.contains(slug), "`{slug}` was removed: {listing}");
    }
}

// ============================================================
// story state reorder
// ============================================================

#[test]
fn state_reorder_accepts_csv_and_separate_arguments() {
    let dir = tempdir().unwrap();
    init(dir.path());

    story(dir.path())
        .args(["state", "reorder", "done,todo,blocked,in-progress"])
        .assert()
        .success()
        .stdout(predicate::str::contains("done, todo, blocked, in-progress"));

    story(dir.path())
        .args(["state", "reorder", "todo", "in-progress", "blocked", "done"])
        .assert()
        .success()
        .stdout(predicate::str::contains("todo, in-progress, blocked, done"));
}

/// The first OPEN state is where `story new` puts a story, so reordering is
/// not cosmetic.
#[test]
fn state_reorder_changes_where_new_stories_land() {
    let dir = tempdir().unwrap();
    init(dir.path());
    story(dir.path())
        .args(["state", "reorder", "in-progress,todo,blocked,done"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "A story"])
        .assert()
        .success();
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .stdout(predicate::str::contains("state: in-progress"));
}

#[test]
fn state_reorder_rejects_a_partial_order() {
    let dir = tempdir().unwrap();
    init(dir.path());

    story(dir.path())
        .args(["state", "reorder", "todo,done"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("must list every state"));
}

#[test]
fn state_reorder_rejects_unknown_slugs() {
    let dir = tempdir().unwrap();
    init(dir.path());

    story(dir.path())
        .args(["state", "reorder", "todo,in-progress,blocked,done,nope"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("`nope` not found"));
}

#[test]
fn state_reorder_requires_an_order() {
    let dir = tempdir().unwrap();
    init(dir.path());

    story(dir.path())
        .args(["state", "reorder"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("usage: story state reorder"));
}

// ============================================================
// Help and usage
// ============================================================

#[test]
fn state_without_a_subcommand_shows_usage() {
    let dir = tempdir().unwrap();
    init(dir.path());

    story(dir.path())
        .arg("state")
        .assert()
        .failure()
        .stderr(predicate::str::contains("usage: story state list"));
}

#[test]
fn state_help_topic_documents_every_verb() {
    let dir = tempdir().unwrap();
    init(dir.path());

    story(dir.path())
        .args(["help", "state"])
        .assert()
        .success()
        .stdout(predicate::str::contains("story state list"))
        .stdout(predicate::str::contains("story state set"))
        .stdout(predicate::str::contains("story state reorder"))
        .stdout(predicate::str::contains("--move-stories-to"));
}
