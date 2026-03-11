use assert_cmd::Command;
use predicates::str::contains;
use tempfile::tempdir;

#[test]
fn doctor_reports_missing_inverse_edge() {
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
        .args(["new", "A"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["new", "B"])
        .assert()
        .success();

    std::fs::write(
        dir.path().join(".storyhook/open/stories/SH-1.jsonl"),
        concat!(
            "{\"kind\":\"StoryCreated\",\"at\":\"2026-03-11T00:00:00Z\",\"title\":\"A\",\"state\":\"todo\"}\n",
            "{\"kind\":\"StoryRelationshipAdded\",\"at\":\"2026-03-11T00:00:01Z\",\"other_id\":\"SH-2\",\"relation\":\"starts-before\"}\n"
        ),
    )
    .unwrap();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("doctor")
        .assert()
        .code(5)
        .stdout(contains("missing inverse relation"));
}
