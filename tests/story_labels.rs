// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn add_labels() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Build parser"])
        .assert()
        .success();

    story(dir.path())
        .args(["label", "SH-1", "bug,backend"])
        .assert()
        .success()
        .stdout(predicate::str::contains("labels: backend, bug"));
}

#[test]
fn remove_label() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Build parser"])
        .assert()
        .success();
    story(dir.path())
        .args(["label", "SH-1", "bug,backend,frontend"])
        .assert()
        .success();

    story(dir.path())
        .args(["unlabel", "SH-1", "frontend"])
        .assert()
        .success()
        .stdout(predicate::str::contains("labels: backend, bug"))
        .stdout(predicate::str::contains("frontend").not());
}

#[test]
fn labels_deduplicated() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Build parser"])
        .assert()
        .success();
    story(dir.path())
        .args(["label", "SH-1", "bug,bug,bug"])
        .assert()
        .success()
        .stdout(predicate::str::contains("labels: bug"));
}

#[test]
fn list_filters_by_label() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Bug fix"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Feature work"])
        .assert()
        .success();
    story(dir.path())
        .args(["label", "SH-1", "bug"])
        .assert()
        .success();
    story(dir.path())
        .args(["label", "SH-2", "feature"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["list", "--label", "bug"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SH-1"));
    assert!(!stdout.contains("SH-2"));
}

#[test]
fn labels_in_json_output() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Build parser"])
        .assert()
        .success();
    story(dir.path())
        .args(["label", "SH-1", "api,backend"])
        .assert()
        .success();

    story(dir.path())
        .args(["--json", "show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"labels\""))
        .stdout(predicate::str::contains("\"api\""))
        .stdout(predicate::str::contains("\"backend\""));
}

#[test]
fn labels_show_in_list() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Build parser"])
        .assert()
        .success();
    story(dir.path())
        .args(["label", "SH-1", "bug"])
        .assert()
        .success();

    story(dir.path())
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("[bug]"));
}
