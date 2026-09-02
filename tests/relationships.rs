use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use storyhook_test_support::{TestEnv, scratch_dir};

/// Every `story` this file runs is the one THIS build produced, in the shared
/// test environment's private `HOME`, XDG directories and store — so nothing
/// here can reach the developer's own storyhook state, with or without a
/// wrapper script supplying one.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

fn story_json(dir: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let output = story(dir).args(args).arg("--json").output().unwrap();
    assert!(
        output.status.success(),
        "story {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("story --json emits one document")
}

#[test]
fn adding_directional_relationship_creates_inverse_edge() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path()).args(["new", "A"]).assert().success();

    story(dir.path()).args(["new", "B"]).assert().success();

    story(dir.path())
        .args(["relate", "SH-1", "blocks", "SH-2"])
        .assert()
        .success()
        .stdout(contains("blocks SH-2"));

    story(dir.path())
        .args(["show", "SH-2"])
        .assert()
        .success()
        .stdout(contains("blocked-by SH-1"));
}

#[test]
fn closing_a_blocker_removes_both_cli_relationships_permanently() {
    let dir = scratch_dir();
    let path = dir.path();
    story(path)
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    for title in ["Blocker", "Dependent"] {
        story(path).args(["new", title]).assert().success();
    }
    story(path)
        .args(["relate", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();
    story(path)
        .args(["move", "SH-1", "done"])
        .assert()
        .success();

    for id in ["SH-1", "SH-2"] {
        let shown = story_json(path, &["show", id]);
        assert_eq!(
            shown["story"]["story"]["relationships"],
            serde_json::json!([]),
            "closing the blocker must retract the relationship from {id}"
        );
    }

    story(path).args(["reopen", "SH-1"]).assert().success();
    let ready = story_json(path, &["next", "--count", "2"]);
    let ready_ids = ready["stories"]
        .as_array()
        .expect("next --count emits stories")
        .iter()
        .map(|view| view["story"]["id"].as_str().expect("ready story id"))
        .collect::<Vec<_>>();
    assert_eq!(ready_ids, ["SH-1", "SH-2"]);
}

#[test]
fn show_renders_derived_ancestor_and_descendent_relationships() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    for title in ["A", "B", "C"] {
        story(dir.path()).args(["new", title]).assert().success();
    }

    story(dir.path())
        .args(["relate", "SH-1", "parent-of", "SH-2"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-2", "parent-of", "SH-3"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(contains("relationships:\n- parent-of SH-2"))
        .stdout(contains("derived_relationships:\n- ancestor-of SH-3"));

    story(dir.path())
        .args(["--json", "show", "SH-3"])
        .assert()
        .success()
        .stdout(contains("\"relation\": \"descendent-of\""))
        .stdout(contains("\"other_id\": \"SH-1\""));
}

#[test]
fn archived_ancestors_still_participate_in_derived_relationships() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    for title in ["A", "B", "C"] {
        story(dir.path()).args(["new", title]).assert().success();
    }

    story(dir.path())
        .args(["relate", "SH-1", "parent-of", "SH-2"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-2", "parent-of", "SH-3"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-3", "done"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-3"])
        .assert()
        .success()
        .stdout(contains("descendent-of SH-1"));
}

#[test]
fn parent_cycle_is_rejected() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    for title in ["A", "B", "C"] {
        story(dir.path()).args(["new", title]).assert().success();
    }

    story(dir.path())
        .args(["relate", "SH-1", "parent-of", "SH-2"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-2", "parent-of", "SH-3"])
        .assert()
        .success();

    story(dir.path())
        .args(["relate", "SH-3", "parent-of", "SH-1"])
        .assert()
        .code(2)
        .stderr(contains("would create a cycle"));
}

#[test]
fn parent_story_shows_progress_rollup_in_json() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    for title in ["Parent", "Child A", "Child B"] {
        story(dir.path()).args(["new", title]).assert().success();
    }

    story(dir.path())
        .args(["relate", "SH-1", "parent-of", "SH-2"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-1", "parent-of", "SH-3"])
        .assert()
        .success();

    // Before closing any child
    let output = story(dir.path())
        .args(["--json", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("\"children_total\": 2"));
    assert!(stdout.contains("\"children_done\": 0"));

    // Close one child
    story(dir.path())
        .args(["move", "SH-2", "done"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["--json", "list"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("\"children_done\": 1"));
    assert!(stdout.contains("\"children_total\": 2"));
}

#[test]
fn leaf_story_has_no_progress_in_json() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Leaf story"])
        .assert()
        .success();

    story(dir.path())
        .args(["--json", "show", "SH-1"])
        .assert()
        .success()
        .stdout(contains("children_total").not());
}
