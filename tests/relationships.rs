// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use tempfile::tempdir;

#[test]
fn adding_directional_relationship_creates_inverse_edge() {
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

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["relate", "SH-1", "blocks", "SH-2"])
        .assert()
        .success()
        .stdout(contains("blocks SH-2"));

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["show", "SH-2"])
        .assert()
        .success()
        .stdout(contains("blocked-by SH-1"));
}

#[test]
fn show_renders_derived_ancestor_and_descendent_relationships() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();

    for title in ["A", "B", "C"] {
        Command::cargo_bin("story")
            .unwrap()
            .current_dir(dir.path())
            .args(["new", title])
            .assert()
            .success();
    }

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["relate", "SH-1", "parent-of", "SH-2"])
        .assert()
        .success();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["relate", "SH-2", "parent-of", "SH-3"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(contains("relationships:\n- parent-of SH-2"))
        .stdout(contains("derived_relationships:\n- ancestor-of SH-3"));

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["--json", "show", "SH-3"])
        .assert()
        .success()
        .stdout(contains("\"relation\": \"descendent-of\""))
        .stdout(contains("\"other_id\": \"SH-1\""));
}

#[test]
fn archived_ancestors_still_participate_in_derived_relationships() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();

    for title in ["A", "B", "C"] {
        Command::cargo_bin("story")
            .unwrap()
            .current_dir(dir.path())
            .args(["new", title])
            .assert()
            .success();
    }

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["relate", "SH-1", "parent-of", "SH-2"])
        .assert()
        .success();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["relate", "SH-2", "parent-of", "SH-3"])
        .assert()
        .success();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["show", "SH-3"])
        .assert()
        .success()
        .stdout(contains("descendent-of SH-1"));
}

#[test]
fn parent_cycle_is_rejected() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();

    for title in ["A", "B", "C"] {
        Command::cargo_bin("story")
            .unwrap()
            .current_dir(dir.path())
            .args(["new", title])
            .assert()
            .success();
    }

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["relate", "SH-1", "parent-of", "SH-2"])
        .assert()
        .success();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["relate", "SH-2", "parent-of", "SH-3"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["relate", "SH-3", "parent-of", "SH-1"])
        .assert()
        .code(2)
        .stderr(contains("would create a cycle"));
}

#[test]
fn parent_story_shows_progress_rollup_in_json() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();

    for title in ["Parent", "Child A", "Child B"] {
        Command::cargo_bin("story")
            .unwrap()
            .current_dir(dir.path())
            .args(["new", title])
            .assert()
            .success();
    }

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["relate", "SH-1", "parent-of", "SH-2"])
        .assert()
        .success();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["relate", "SH-1", "parent-of", "SH-3"])
        .assert()
        .success();

    // Before closing any child
    let output = Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["--json", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("\"children_total\": 2"));
    assert!(stdout.contains("\"children_done\": 0"));

    // Close one child
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["move", "SH-2", "done"])
        .assert()
        .success();

    let output = Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["--json", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("\"children_done\": 1"));
    assert!(stdout.contains("\"children_total\": 2"));
}

#[test]
fn leaf_story_has_no_progress_in_json() {
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
        .args(["new", "Leaf story"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["--json", "show", "SH-1"])
        .assert()
        .success()
        .stdout(contains("children_total").not());
}
