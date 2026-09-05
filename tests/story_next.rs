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
fn next_returns_oldest_unblocked() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "First task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Second task"])
        .assert()
        .success();

    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-1"))
        .stdout(predicate::str::contains("First task"));
}

#[test]
fn next_respects_priority_sorting() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Low priority"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Critical priority"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-1", "low"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-2", "critical"])
        .assert()
        .success();

    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-2"))
        .stdout(predicate::str::contains("Critical priority"));
}

#[test]
fn next_skips_awaiting_stories() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Blocked task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Ready task"])
        .assert()
        .success();
    story(dir.path())
        .args(["block", "SH-1", "API spec"])
        .assert()
        .success();

    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-2"))
        .stdout(predicate::str::contains("Ready task"));
}

#[test]
fn next_skips_dependency_blocked_stories() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "First task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Depends on first"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-2", "blocked-by", "SH-1"])
        .assert()
        .success();

    // SH-2 is blocked because SH-1 (which blocks it) is still open
    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-1"))
        .stdout(predicate::str::contains("First task"));
}

#[test]
fn next_unblocks_after_dependency_closed() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "First task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Depends on first"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-2", "blocked-by", "SH-1"])
        .assert()
        .success();

    // Close SH-1
    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success();

    // Now SH-2 should be ready
    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-2"));
}

#[test]
fn next_count_orders_a_blocked_story_after_its_blocker() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Blocker"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Dependent"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["next", "--count", "2"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let blocker = stdout.find("SH-1").expect("blocker is listed");
    let dependent = stdout.find("SH-2").expect("dependent is listed");
    assert!(
        blocker < dependent,
        "the blocker must precede its dependent"
    );
}

#[test]
fn next_count_returns_multiple() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();
    story(dir.path()).args(["new", "Task C"]).assert().success();

    let output = story(dir.path())
        .args(["next", "--count", "3"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SH-1"));
    assert!(stdout.contains("SH-2"));
    assert!(stdout.contains("SH-3"));
}

/// Regression test for SH-236: `story next --count N>1` handed back a story
/// someone had already claimed (moved to `in-progress`), because `is_ready`
/// never checked the story's state beyond the required `blocked` slug.
#[test]
fn next_skips_a_story_already_in_progress() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Task A"]).assert().success();
    story(dir.path()).args(["new", "Task B"]).assert().success();
    story(dir.path())
        .args(["move", "SH-1", "in-progress"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["next", "--count", "2"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(!stdout.contains("SH-1"));
    assert!(stdout.contains("SH-2"));

    // Plain `next` (no --count) agrees: the claimed story is never the
    // single answer either.
    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-2"))
        .stdout(predicate::str::contains("Task B"));
}

#[test]
fn next_json_output() {
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
        .args(["--json", "next"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"result\": \"ok\""))
        .stdout(predicate::str::contains("\"id\": \"SH-1\""));
}

#[test]
fn next_empty_project() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("no ready stories"));
}

#[test]
fn next_all_blocked_returns_no_ready() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Blocked task"])
        .assert()
        .success();
    story(dir.path())
        .args(["block", "SH-1", "external dependency"])
        .assert()
        .success();

    story(dir.path())
        .arg("next")
        .assert()
        .success()
        .stdout(predicate::str::contains("no ready stories"));
}

#[test]
fn next_epic_scope_includes_grandchildren_only_and_preserves_ready_order() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Root epic", "--type", "epic"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Nested epic", "--type", "epic"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Outside critical"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Scoped low"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Scoped critical"])
        .assert()
        .success();
    story(dir.path())
        .args(["relate", "SH-1", "parent-of", "SH-2"])
        .assert()
        .success();
    for child in ["SH-4", "SH-5"] {
        story(dir.path())
            .args(["relate", "SH-2", "parent-of", child])
            .assert()
            .success();
    }
    story(dir.path())
        .args(["prioritize", "SH-3", "critical"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-4", "low"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-5", "critical"])
        .assert()
        .success();
    for id in ["SH-3", "SH-4", "SH-5"] {
        story(dir.path())
            .args(["label", id, "phase:1"])
            .assert()
            .success();
    }

    let output = story(dir.path())
        .args([
            "next",
            "--epic",
            "SH-1",
            "--exclude-label",
            "unknown",
            "--phase",
            "1",
            "--count",
            "3",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        !stdout.contains("SH-3"),
        "outside story leaked in: {stdout}"
    );
    let critical = stdout.find("SH-5").expect("scoped critical grandchild");
    let low = stdout.find("SH-4").expect("scoped low grandchild");
    assert!(
        critical < low,
        "ready ordering changed inside scope: {stdout}"
    );
}

#[test]
fn next_exclude_label_matches_list_label_case_and_csv_semantics() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Mixed case"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Excluded"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Unlabelled"])
        .assert()
        .success();
    story(dir.path())
        .args(["label", "SH-1", "No-Auto"])
        .assert()
        .success();
    story(dir.path())
        .args(["label", "SH-2", "no-auto"])
        .assert()
        .success();

    let output = story(dir.path())
        .args([
            "next",
            "--count",
            "3",
            "--exclude-label",
            " unknown, no-auto ",
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        !stdout.contains("SH-1"),
        "mixed-case writes and selectors have one lowercase identity: {stdout}"
    );
    assert!(
        !stdout.contains("SH-2"),
        "named label was not excluded: {stdout}"
    );
    assert!(
        stdout.contains("SH-3"),
        "unknown label excluded work: {stdout}"
    );
}

#[test]
fn next_epic_scope_refuses_a_non_epic_by_name() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Ordinary story"])
        .assert()
        .success();

    story(dir.path())
        .args(["next", "--epic", "SH-1"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("story `SH-1` is not an epic"));
}
