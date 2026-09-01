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

#[test]
fn custom_prefix_generates_correct_ids() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "API"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "First endpoint"])
        .assert()
        .success()
        .stdout(predicate::str::contains("API-1"));
    story(dir.path())
        .args(["new", "Second endpoint"])
        .assert()
        .success()
        .stdout(predicate::str::contains("API-2"));
}

#[test]
fn custom_prefix_stories_are_showable() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "WEB"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Build homepage"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "WEB-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("WEB-1 Build homepage"));
}

#[test]
fn custom_prefix_stories_support_relationships() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "FE"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Parent task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Child task"])
        .assert()
        .success();

    story(dir.path())
        .args(["relate", "FE-1", "parent-of", "FE-2"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "FE-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("parent-of FE-2"));
}

#[test]
fn default_prefix_is_sh() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Default prefix"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-1"));
}
