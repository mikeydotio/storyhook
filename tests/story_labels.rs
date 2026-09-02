use assert_cmd::Command;
use predicates::prelude::*;
use storyhook_test_support::{TestEnv, scratch_dir};

/// Every `story` this file runs is the one THIS build produced, in the shared
/// test environment's private `HOME`, XDG directories and store — so nothing
/// here can reach the developer's own storyhook state, with or without a
/// wrapper script supplying one.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

#[test]
fn add_labels() {
    let dir = scratch_dir();
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
    let dir = scratch_dir();
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
    let dir = scratch_dir();
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
    let dir = scratch_dir();
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
    let dir = scratch_dir();
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

/// Repeated `--label` and a comma-bearing `--label` value are the same sink
/// (SH-164): `--label "a, b" --label c` files the same three labels as
/// `--labels "a,b,c"` or three repeated `--label` flags.
#[test]
fn new_combines_repeated_and_comma_bearing_label_flags() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Three labels", "--label", "a, b", "--label", "c"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("labels: a, b, c"));
}

/// `story label`/`story unlabel`/`story set --labels` all round-trip a label
/// added by any of the other two — one label set, addressed three ways.
#[test]
fn label_unlabel_and_set_labels_round_trip() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Round trip"])
        .assert()
        .success();

    story(dir.path())
        .args(["label", "SH-1", "bug"])
        .assert()
        .success();
    story(dir.path())
        .args(["set", "SH-1", "--labels", "backend"])
        .assert()
        .success();
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("labels: backend, bug"));

    story(dir.path())
        .args(["unlabel", "SH-1", "bug"])
        .assert()
        .success()
        .stdout(predicate::str::contains("labels: backend"))
        .stdout(predicate::str::contains("bug").not());
}

#[test]
fn labels_show_in_list() {
    let dir = scratch_dir();
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
