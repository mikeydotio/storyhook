use assert_cmd::Command;
use predicates::str::contains;
use tempfile::tempdir;

#[test]
fn closed_state_moves_story_to_archive_db() {
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
        .args(["new", "Finish me"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["SH-1", "is", "done", "completed"])
        .assert()
        .success()
        .stdout(contains("closed_at:"));

    assert!(dir.path().join(".storyhook/archive/archive.db").exists());
    assert!(
        !dir.path()
            .join(".storyhook/open/stories/SH-1.jsonl")
            .exists()
    );

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("SH-1")
        .assert()
        .success()
        .stdout(contains("Finish me"))
        .stdout(contains("state: done (CLOSED)"));
}
