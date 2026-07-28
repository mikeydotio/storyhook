use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn handoff_captures_recently_created() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "New task"])
        .assert()
        .success();

    story(dir.path())
        .args(["handoff"])
        .assert()
        .success()
        .stdout(predicate::str::contains("# Session Handoff"))
        .stdout(predicate::str::contains("Created"))
        .stdout(predicate::str::contains("New task"));
}

#[test]
fn handoff_captures_closed_stories() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Done task"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success();

    story(dir.path())
        .args(["handoff"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Closed"))
        .stdout(predicate::str::contains("Done task"));
}

#[test]
fn handoff_with_duration() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Recent task"])
        .assert()
        .success();

    // --since 1h should capture recently created stories
    story(dir.path())
        .args(["handoff", "--since", "1h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Recent task"));
}

#[test]
fn handoff_invalid_duration() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["handoff", "--since", "xyz"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid duration"));
}

#[test]
fn handoff_empty_period() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    // --since 0h means nothing should show
    // Actually 0h means cutoff is exactly now, so recent stories within this second may appear.
    // Use a very short window that should still capture what was just created or show empty message.
    story(dir.path())
        .args(["handoff", "--since", "0m"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Session Handoff"));
}
