use assert_cmd::Command;
use predicates::prelude::*;
use storyhook_test_support::{TestEnv, scratch_dir};

/// Every `story` this file runs is the one THIS build produced, in the shared
/// test environment's private `HOME`, XDG directories and store — so nothing
/// here can reach the developer's own storyhook state, with or without a
/// wrapper script supplying one.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

// ============================================================
// story publish (SH-175) — makes a draft live, one-way
// ============================================================

#[test]
fn publish_makes_a_draft_live() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "A draft", "--draft"])
        .assert()
        .success();

    story(dir.path())
        .args(["publish", "SH-1"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("draft: no"));
}

#[test]
fn publish_on_an_already_live_story_is_not_an_error() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Never a draft"])
        .assert()
        .success();

    story(dir.path())
        .args(["publish", "SH-1"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("draft: no"));
}

#[test]
fn publish_a_bare_integer_id_resolves_against_the_single_registered_project() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "A draft", "--draft"])
        .assert()
        .success();

    // SH-118: a bare integer expands to the canonical id once the project is
    // unambiguous — the same grammar `story hide`/`story unhide` already get,
    // and `publish` follows it too (`invoke::story_ids::positions`).
    story(dir.path()).args(["publish", "1"]).assert().success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("draft: no"));
}

#[test]
fn publish_an_unknown_story_is_not_found() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["publish", "SH-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("SH-1"));
}

#[test]
fn publish_takes_no_flags() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "A draft", "--draft"])
        .assert()
        .success();

    // `VERB_FLAGS` has no entry for `publish` (mirrors `archive`/`unarchive`),
    // so any flag-shaped token is refused before the parser even sees it.
    story(dir.path())
        .args(["publish", "SH-1", "--force"])
        .assert()
        .failure();
}
