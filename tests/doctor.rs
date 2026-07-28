// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

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
        .stderr(contains("missing inverse relation"));
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
        .stderr(contains("parent/child cycle detected"));

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

#[test]
fn doctor_flags_unknown_story_type() {
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

    // Manually write a story with an unknown type
    std::fs::write(
        dir.path().join(".storyhook/open/stories/SH-1.jsonl"),
        concat!(
            "{\"kind\":\"StoryCreated\",\"at\":\"2026-03-11T00:00:00Z\",\"title\":\"A\",\"state\":\"todo\"}\n",
            "{\"kind\":\"StoryTypeSet\",\"at\":\"2026-03-11T00:00:01Z\",\"story_type\":\"nonexistent-type\"}\n"
        ),
    )
    .unwrap();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("doctor")
        .assert()
        .code(5)
        .stderr(contains("unknown type `nonexistent-type`"));
}

#[test]
fn doctor_does_not_flag_known_story_type() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();

    // Add a type
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["type", "add", "feature"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["new", "A"])
        .assert()
        .success();

    // Set the story type to the known type
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["set", "SH-1", "--type", "feature"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("doctor")
        .assert()
        .success();
}
