//! SH-499 — a story is an epic because it is TYPED one, not because it has
//! children.
//!
//! User determination, 2026-08-27: "'is an epic because it has children' was
//! never an intended truth in the design. All epics are essentially folders;
//! they exist to group other stories as their children. But not all stories
//! with children are epics."
//!
//! The codebase said the opposite, and said it on purpose —
//! `apply_computed_epic_states` documented the conflation as intent, and eight
//! behavioural gates across Rust, `story.sh` and the dashboard asked
//! `has_children`. So giving any ordinary story a child — a bug that spawned a
//! follow-up, a chore with one sub-task — silently stopped it being work: its
//! state was taken over and computed, it could not be moved, `story next` would
//! not offer it, dispatch refused it, and the board hid it by default. Nobody
//! asked for a folder; it acquired one edge.
//!
//! These tests are written from the *normal* parent's side, because that is the
//! side that was wrong. The epic's own behaviour is asserted alongside each one,
//! so a fix that simply deleted epic semantics fails here rather than passing.

use assert_cmd::Command;
use storyhook_test_support::{TestEnv, scratch_dir};

/// Every `story` this file runs is the one THIS build produced, in the shared
/// test environment's private `HOME`, XDG directories and store — so nothing
/// here can reach the developer's own storyhook state, with or without a
/// wrapper script supplying one.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

fn show(dir: &std::path::Path, id: &str) -> serde_json::Value {
    let out = story(dir).args(["show", id, "--json"]).output().unwrap();
    assert!(out.status.success(), "story show {id} failed");
    serde_json::from_slice(&out.stdout).expect("show --json is a document")
}

/// A project holding one `normal` parent (`SH-1`) with one child (`SH-2`), and
/// one `epic` parent (`SH-3`) with one child (`SH-4`).
///
/// Both shapes in one fixture on purpose: every assertion below can then state
/// the contrast in the same breath, and a change that flattens the two into one
/// behaviour fails whichever way it flattened them.
fn both_kinds_of_parent() -> tempfile::TempDir {
    let dir = scratch_dir();
    let p = dir.path();
    story(p)
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(p)
        .args(["new", "a normal story that happens to have a child"])
        .assert()
        .success();
    story(p).args(["new", "its child"]).assert().success();
    story(p)
        .args(["relate", "SH-1", "parent-of", "SH-2"])
        .assert()
        .success();
    story(p)
        .args(["new", "a real epic", "--type", "epic"])
        .assert()
        .success();
    story(p)
        .args(["new", "the epic's child"])
        .assert()
        .success();
    story(p)
        .args(["relate", "SH-3", "parent-of", "SH-4"])
        .assert()
        .success();
    dir
}

#[test]
fn a_normal_parent_keeps_its_own_state_while_an_epic_has_one_computed() {
    let dir = both_kinds_of_parent();
    let p = dir.path();

    // Move each parent's child. The epic must follow its child; the normal
    // story must not, because its state is its own.
    story(p)
        .args(["move", "SH-2", "in-progress"])
        .assert()
        .success();
    story(p)
        .args(["move", "SH-4", "in-progress"])
        .assert()
        .success();

    let normal = show(p, "SH-1");
    assert_eq!(
        normal["story"]["story"]["state"], "todo",
        "a normal parent's state is its own: {normal:#}"
    );
    // Absent OR false. The field is omitted from the wire when false, so
    // asserting `== false` would fail against a story that is correctly not
    // computed -- and asserting `!= true` is the claim anyway.
    assert_ne!(
        normal["story"]["story"]["state_computed"], true,
        "and it is not marked computed: {normal:#}"
    );

    let epic = show(p, "SH-3");
    assert_eq!(
        epic["story"]["story"]["state"], "in-progress",
        "an epic still follows its children: {epic:#}"
    );
    assert_eq!(epic["story"]["story"]["state_computed"], true);
}

#[test]
fn a_normal_parent_can_be_moved_directly_and_an_epic_cannot() {
    let dir = both_kinds_of_parent();
    let p = dir.path();

    story(p)
        .args(["move", "SH-1", "in-progress"])
        .assert()
        .success();
    assert_eq!(show(p, "SH-1")["story"]["story"]["state"], "in-progress");

    story(p)
        .args(["move", "SH-3", "in-progress"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("is an epic"));
}

#[test]
fn story_next_offers_a_normal_parent_and_never_an_epic() {
    // The sharpest consequence for an agent: work that exists is handed out.
    // A normal parent was excluded from the ready queue purely for having one
    // edge, so nothing would ever pick it up.
    let dir = both_kinds_of_parent();
    let p = dir.path();

    let out = story(p)
        .args(["list", "--ready", "--json"])
        .output()
        .unwrap();
    let ready: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let ids: Vec<String> = ready["stories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["story"]["id"].as_str().unwrap().to_string())
        .collect();

    assert!(
        ids.contains(&"SH-1".to_string()),
        "a normal parent is ready work: {ids:?}"
    );
    assert!(
        !ids.contains(&"SH-3".to_string()),
        "an epic is never offered as work: {ids:?}"
    );
}

#[test]
fn the_refusal_no_longer_claims_children_are_what_make_an_epic() {
    // The message is part of the defect: it taught the wrong rule to everyone
    // who hit it. Asserted so a fix that changes only the predicate, leaving
    // the sentence to keep explaining the old one, is caught.
    let dir = both_kinds_of_parent();
    let p = dir.path();

    let out = story(p)
        .args(["move", "SH-3", "in-progress"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("because it has children"),
        "the refusal must not name children as the cause: {stderr}"
    );
}

#[test]
fn an_epic_with_no_children_at_all_is_still_an_epic() {
    // A folder is a folder when it is empty. Guards against a fix that decides
    // epic-ness needs BOTH the type and some children, which would be the same
    // conflation wearing a different hat.
    let dir = scratch_dir();
    let p = dir.path();
    story(p)
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(p)
        .args(["new", "an empty folder", "--type", "epic"])
        .assert()
        .success();

    story(p)
        .args(["move", "SH-1", "in-progress"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("is an epic"));
}

#[test]
fn a_normal_parent_still_reports_progress() {
    // Progress is a FACT about children, not a role, so it survives the split.
    // This is what stops the fix being implemented as "only epics have
    // children" -- which would be tidy and wrong.
    let dir = both_kinds_of_parent();
    let p = dir.path();

    let normal = show(p, "SH-1");
    assert_eq!(
        normal["story"]["progress"]["children_total"], 1,
        "a normal parent still counts its children: {normal:#}"
    );
}

#[test]
fn public_relationship_guidance_says_epic_identity_is_typed_not_structural() {
    const INVARIANT: &str = "Only a story whose type is epic is an epic.";
    const OBSOLETE: &str = "A story with children is a structural epic";

    let readme =
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md"))
            .expect("README.md is readable");
    let relate = storyhook::help_topics::get_help_topic("relate")
        .expect("the shipped relate help topic exists");

    for (surface, text) in [
        ("README.md", readme.as_str()),
        ("story help relate", relate),
    ] {
        assert!(
            text.contains(INVARIANT),
            "{surface} must teach the type-based epic invariant: {INVARIANT}"
        );
        assert!(
            !text.contains(OBSOLETE),
            "{surface} still teaches the pre-SH-499 structural-epic rule"
        );
        assert!(
            text.contains("ordinary story")
                && text.contains("children")
                && text.contains("actionable"),
            "{surface} must make clear that an ordinary parent retains its own work"
        );
    }
}
