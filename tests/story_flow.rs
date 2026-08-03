// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use tempfile::tempdir;

#[test]
fn story_new_assigns_monotonic_ids_and_show_works() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["new", "First"])
        .assert()
        .success()
        .stdout(contains("SH-1"));

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["new", "Second"])
        .assert()
        .success()
        .stdout(contains("SH-2"));

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(contains("First"));
}

#[test]
fn comment_and_assign_append_events() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["member", "add", "mikey <mw@mikey.io>"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["new", "Routing"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["assign", "SH-1", "mikey"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["comment", "SH-1", "First pass done"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(contains("assignee: mikey"))
        .stdout(contains("First pass done"));
}

#[test]
fn awaiting_can_be_set_and_cleared() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["new", "Blocked work"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["block", "SH-1", "blocked on API"])
        .assert()
        .success()
        .stdout(contains("awaiting: blocked on API"));

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["--json", "show", "SH-1"])
        .assert()
        .success()
        .stdout(contains("\"awaiting\": \"blocked on API\""));

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["unblock", "SH-1"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicates::str::contains("awaiting:").not());
}
