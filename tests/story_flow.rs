use assert_cmd::Command;
use predicates::str::contains;
use tempfile::tempdir;

#[test]
fn story_new_assigns_monotonic_ids_and_show_works() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
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
        .arg("SH-1")
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
        .arg("init")
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
        .args(["SH-1", "assign", "mikey"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["SH-1", "First pass done"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("SH-1")
        .assert()
        .success()
        .stdout(contains("assignee: mikey"))
        .stdout(contains("First pass done"));
}
