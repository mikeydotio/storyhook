use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
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
            "{\"kind\":\"StoryRelationshipAdded\",\"at\":\"2026-03-11T00:00:01Z\",\"other_id\":\"SH-2\",\"relation\":\"blocks\"}\n"
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

#[test]
fn doctor_reports_parent_cycle_and_show_suppresses_virtual_relationships() {
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
            "{\"kind\":\"StoryRelationshipAdded\",\"at\":\"2026-03-11T00:00:01Z\",\"other_id\":\"SH-2\",\"relation\":\"parent-of\"}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.path().join(".storyhook/open/stories/SH-2.jsonl"),
        concat!(
            "{\"kind\":\"StoryCreated\",\"at\":\"2026-03-11T00:00:00Z\",\"title\":\"B\",\"state\":\"todo\"}\n",
            "{\"kind\":\"StoryRelationshipAdded\",\"at\":\"2026-03-11T00:00:01Z\",\"other_id\":\"SH-1\",\"relation\":\"child-of\"}\n",
            "{\"kind\":\"StoryRelationshipAdded\",\"at\":\"2026-03-11T00:00:04Z\",\"other_id\":\"SH-1\",\"relation\":\"parent-of\"}\n"
        ),
    )
    .unwrap();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("doctor")
        .assert()
        .code(5)
        .stdout(contains("parent/child cycle detected"));

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["--json", "show", "SH-1"])
        .assert()
        .success()
        .stdout(contains("\"parent-of\""))
        .stdout(contains("\"ancestor-of\"").not())
        .stdout(contains("\"descendent-of\"").not());
}
