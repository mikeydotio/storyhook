use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn reopen_archived_story() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Fix bug"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-1", "is", "done"])
        .assert()
        .success();

    // Reopen
    story(dir.path())
        .args(["SH-1", "reopen"])
        .assert()
        .success()
        .stdout(predicate::str::contains("state: todo"));

    // Verify it's modifiable again
    story(dir.path())
        .args(["SH-1", "Added more context"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Added more context"));
}

#[test]
fn reopen_already_open_story_fails() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Open story"])
        .assert()
        .success();

    story(dir.path())
        .args(["SH-1", "reopen"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("already open"));
}

#[test]
fn reopen_nonexistent_story_fails() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["SH-999", "reopen"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("not found"));
}

#[test]
fn reopen_preserves_comments() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Task with comments"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-1", "Important note"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-1", "is", "done"])
        .assert()
        .success();

    story(dir.path())
        .args(["SH-1", "reopen"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Important note"));
}

#[test]
fn reopen_json_output() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Reopen me"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-1", "is", "done"])
        .assert()
        .success();

    story(dir.path())
        .args(["--json", "SH-1", "reopen"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"result\": \"ok\""))
        .stdout(predicate::str::contains("\"state\": \"todo\""));
}
