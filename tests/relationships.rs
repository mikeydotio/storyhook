use assert_cmd::Command;
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
        .args(["SH-1", "blocks", "SH-2"])
        .assert()
        .success()
        .stdout(contains("blocks SH-2"));

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("SH-2")
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
        .args(["SH-1", "parent-of", "SH-2"])
        .assert()
        .success();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["SH-2", "parent-of", "SH-3"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("SH-1")
        .assert()
        .success()
        .stdout(contains("relationships:\n- parent-of SH-2"))
        .stdout(contains("derived_relationships:\n- ancestor-of SH-3"));

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["SH-3", "--json"])
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
        .args(["SH-1", "parent-of", "SH-2"])
        .assert()
        .success();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["SH-2", "parent-of", "SH-3"])
        .assert()
        .success();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["SH-1", "is", "done"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("SH-3")
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
        .args(["SH-1", "parent-of", "SH-2"])
        .assert()
        .success();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["SH-2", "parent-of", "SH-3"])
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["SH-3", "parent-of", "SH-1"])
        .assert()
        .code(2)
        .stdout(contains("would create a cycle"));
}
