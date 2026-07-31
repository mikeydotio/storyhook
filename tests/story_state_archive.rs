// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

use assert_cmd::Command;
use predicates::str::contains;
use tempfile::tempdir;

#[test]
fn closing_a_story_takes_it_out_of_the_open_set_and_keeps_it_readable() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["project", "init"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["new", "Finish me"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["move", "SH-1", "done", "completed"])
        .assert()
        .success()
        .stdout(contains("closed_at:"));

    // The archive was a second storage medium; it is a column now. What it was
    // ever for is asserted instead: a closed story stops counting as open and
    // stays readable.
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["--json", "summary"])
        .assert()
        .success()
        .stdout(contains("\"total_open\": 0"));

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(contains("Finish me"))
        .stdout(contains("state: done (CLOSED)"));
}

#[test]
fn closing_story_clears_awaiting_in_archive_snapshot() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["project", "init"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["new", "Blocked close"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["block", "SH-1", "waiting on review"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success()
        .stdout(contains("closed_at:"));

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["--json", "show", "SH-1"])
        .assert()
        .success()
        .stdout(contains("\"awaiting\": null"));
}
