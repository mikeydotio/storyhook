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
        .args(["SH-1", "starts-before", "SH-2"])
        .assert()
        .success()
        .stdout(contains("starts-before SH-2"));

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("SH-2")
        .assert()
        .success()
        .stdout(contains("starts-after SH-1"));
}
