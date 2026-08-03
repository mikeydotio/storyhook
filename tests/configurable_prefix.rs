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

#[test]
fn custom_prefix_generates_correct_ids() {
    let dir = tempdir().unwrap();
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
    let dir = tempdir().unwrap();
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
    let dir = tempdir().unwrap();
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
    let dir = tempdir().unwrap();
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
