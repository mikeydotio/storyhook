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
fn set_and_show_priority() {
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
        .args(["prioritize", "SH-1", "high"])
        .assert()
        .success()
        .stdout(predicate::str::contains("priority: high"));

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("priority: high"));
}

#[test]
fn priority_defaults_to_low() {
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
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("priority: low"));
}

#[test]
fn override_priority() {
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
        .args(["prioritize", "SH-1", "critical"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-1", "low"])
        .assert()
        .success()
        .stdout(predicate::str::contains("priority: low"));
}

#[test]
fn invalid_priority_fails() {
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
        .args(["prioritize", "SH-1", "urgent"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn list_filters_by_priority() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Low task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "High task"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-1", "low"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-2", "high"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["list", "--priority", "high"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SH-2"));
    assert!(!stdout.contains("SH-1"));
}

#[test]
fn priority_in_json_output() {
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
        .args(["prioritize", "SH-1", "critical"])
        .assert()
        .success();

    story(dir.path())
        .args(["--json", "show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"priority\": \"critical\""));
}

#[test]
fn list_shows_priority_in_line() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Important task"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-1", "high"])
        .assert()
        .success();

    story(dir.path())
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("(high)"));
}

/// A project where low SH-1 blocks critical SH-2, beside an unrelated high
/// SH-3: SH-1's blocker floor is critical (SH-788).
fn floored_project() -> tempfile::TempDir {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    for (title, level) in [
        ("Low blocker", "low"),
        ("Critical dependent", "critical"),
        ("Unrelated", "high"),
    ] {
        story(dir.path())
            .args(["new", title, "--priority", level])
            .assert()
            .success();
    }
    story(dir.path())
        .args(["relate", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();
    dir
}

fn stdout(dir: &std::path::Path, args: &[&str]) -> String {
    let output = story(dir).args(args).output().unwrap();
    assert!(output.status.success(), "{args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn json(dir: &std::path::Path, args: &[&str]) -> serde_json::Value {
    serde_json::from_str(&stdout(dir, args)).unwrap()
}

/// SH-788: wherever a priority prints for a person, a blocker shows its own
/// level and then, in parentheses, the level it sorts at.
#[test]
fn a_blocker_prints_its_blocker_floor_beside_its_own_level() {
    let dir = floored_project();
    let dir = dir.path();

    let list = stdout(dir, &["list"]);
    assert!(list.contains("SH-1 [todo] (low (critical))"), "{list}");
    assert!(list.contains("SH-2 [todo] (critical)"), "{list}");
    assert!(list.contains("SH-3 [todo] (high)"), "{list}");

    let show = stdout(dir, &["show", "SH-1"]);
    assert!(show.contains("priority: low (critical)\n"), "{show}");
    let dependent = stdout(dir, &["show", "SH-2"]);
    assert!(dependent.contains("priority: critical\n"), "{dependent}");

    let next = stdout(dir, &["next"]);
    assert!(
        next.starts_with("SH-1 "),
        "the floored blocker goes first: {next}"
    );
    assert!(next.contains("priority: low (critical)\n"), "{next}");

    let search = stdout(dir, &["search", "blocker"]);
    assert!(search.contains("SH-1 [todo] (low (critical))"), "{search}");

    let summary = stdout(dir, &["summary"]);
    assert!(
        summary.contains("  SH-1 [todo] (low (critical)) Low blocker\n"),
        "{summary}"
    );

    let context = stdout(dir, &["load-context"]);
    assert!(
        context.contains("- SH-1 Low blocker (low (critical)); complexity:"),
        "{context}"
    );
}

#[test]
fn json_carries_the_blocker_floor_only_while_it_raises_the_story() {
    let dir = floored_project();
    let dir = dir.path();

    let floored = json(dir, &["show", "SH-1", "--json"]);
    assert_eq!(floored["story"]["story"]["priority"], "low");
    assert_eq!(floored["story"]["blocker_floor"], "critical");
    let unrelated = json(dir, &["show", "SH-3", "--json"]);
    assert!(
        unrelated["story"].get("blocker_floor").is_none(),
        "absent, not null, when nothing raises the story: {unrelated}"
    );
    let context = json(dir, &["load-context", "--json"]);
    let row = context["ready_stories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "SH-1")
        .expect("SH-1 is ready");
    assert_eq!(row["blocker_floor"], "critical", "{context}");

    // The floor lasts only as long as the blockage: dropping the dependent
    // (a blocked story cannot finish) ends it.
    story(dir)
        .args(["move", "SH-2", "dropped"])
        .assert()
        .success();
    let released = json(dir, &["show", "SH-1", "--json"]);
    assert!(
        released["story"].get("blocker_floor").is_none(),
        "{released}"
    );
    assert!(stdout(dir, &["list"]).contains("SH-1 [todo] (low)"));
}

/// The floor is a scheduling fact, not a severity claim: filters and counts
/// read the stored level.
#[test]
fn filters_and_counts_read_the_stored_level() {
    let dir = floored_project();
    let dir = dir.path();

    let critical = stdout(dir, &["list", "--priority", "critical"]);
    assert!(critical.contains("SH-2"), "{critical}");
    assert!(!critical.contains("SH-1"), "{critical}");
    let low = stdout(dir, &["list", "--priority", "low"]);
    assert!(low.contains("SH-1 [todo] (low (critical))"), "{low}");

    let summary = json(dir, &["summary", "--json"]);
    let mut by_priority: Vec<(String, u64)> = summary["summary"]["by_priority"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| {
            (
                pair[0].as_str().unwrap().to_string(),
                pair[1].as_u64().unwrap(),
            )
        })
        .collect();
    by_priority.sort();
    assert_eq!(
        by_priority,
        [
            ("critical".to_string(), 1),
            ("high".to_string(), 1),
            ("low".to_string(), 1)
        ]
    );
}
