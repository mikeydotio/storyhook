//! Real CLI coverage of review reads and the human-review lifecycle.

use serde_json::Value;
use storyhook_test_support::{Project, TestEnv};

fn run(p: &Project<'_>, args: &[&str]) -> String {
    let out = p
        .story()
        .args(args)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    String::from_utf8(out).unwrap()
}

fn json(p: &Project<'_>, args: &[&str]) -> Value {
    serde_json::from_str(&run(p, args)).unwrap()
}

#[test]
fn cli_context_scopes_candidates_to_the_project_and_preserves_existing_output() {
    let env = TestEnv::isolated();
    let p = env.project().prefix("SH").build();
    let foreign = env.project().prefix("OTHER").build();
    run(&p, &["new", "target"]);
    run(&p, &["new", "candidate"]);
    run(&p, &["move", "SH-2", "in-progress"]);
    run(&foreign, &["new", "foreign candidate"]);
    run(&foreign, &["move", "OTHER-1", "in-progress"]);
    let before = run(&p, &["load-context", "--format", "json"]);
    let r = json(
        &p,
        &[
            "load-context",
            "--story",
            "SH-1",
            "--format",
            "json",
            "--json",
        ],
    );
    assert_eq!(
        r["obviation_review"]["candidates"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        r["obviation_review"]["candidates"][0]["story"]["id"],
        "SH-2"
    );
    assert_eq!(
        r,
        json(&p, &["context", "--story", "SH-1", "--format", "json"])
    );
    assert_eq!(
        r,
        json(&p, &["load-context", "--story", "1", "--format", "json"])
    );
    let markdown = run(&p, &["load-context", "--story", "SH-1"]);
    assert!(markdown.contains("Obviation review"));
    assert!(markdown.contains("SH-2"));
    assert_eq!(before, run(&p, &["load-context", "--format", "json"]));
    assert!(
        serde_json::from_str::<Value>(&before)
            .unwrap()
            .get("obviation_review")
            .is_none()
    );
}

#[test]
fn cli_rejects_missing_or_unknown_story_arguments() {
    let env = TestEnv::isolated();
    let p = env.project().prefix("SH").build();
    for args in [
        vec!["load-context", "--story"],
        vec!["load-context", "--story", "--json"],
        vec!["load-context", "--story", "SH-999"],
        vec!["load-context", "--story", "OTHER-1"],
        vec!["load-context", "--story", "SH-1", "--story", "SH-2"],
    ] {
        p.story().args(&args).assert().failure();
    }
}

#[test]
fn suspected_obviation_stays_open_and_blocked_until_human_resolution() {
    let env = TestEnv::isolated();
    let p = env.project().prefix("SH").build();
    for title in ["target", "replacement", "other dependency"] {
        run(&p, &["new", title]);
    }
    run(&p, &["move", "SH-1", "in-progress"]);
    run(&p, &["move", "SH-2", "in-progress"]);
    run(
        &p,
        &[
            "comment",
            "SH-1",
            "Possible obviation: replacement covers requirements; original state in-progress",
        ],
    );
    run(&p, &["relate", "SH-1", "obviated-by", "SH-2"]);
    run(
        &p,
        &[
            "move",
            "SH-1",
            "blocked",
            "--if-state",
            "in-progress",
            "--reason",
            "Human review of possible obviation",
        ],
    );
    let target = json(&p, &["show", "SH-1", "--json"]);
    assert_eq!(target["story"]["story"]["superstate"], "OPEN");
    assert_eq!(target["story"]["story"]["state"], "blocked");
    assert_eq!(
        target["story"]["story"]["awaiting"],
        "Human review of possible obviation"
    );
    let replacement = json(&p, &["show", "SH-2", "--json"]);
    assert!(
        replacement["story"]["story"]["relationships"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["relation"] == "obviates" && r["other_id"] == "SH-1")
    );
    run(&p, &["move", "SH-2", "done"]);
    let ready = json(&p, &["list", "--ready", "--json"]);
    assert!(
        ready["stories"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["story"]["id"] != "SH-1")
    );
    // Human rejection removes only the reviewed edge; unrelated blockers survive.
    run(&p, &["relate", "SH-1", "blocked-by", "SH-3"]);
    run(&p, &["unrelate", "SH-1", "obviated-by", "SH-2"]);
    run(&p, &["unblock", "SH-1"]);
    let target = json(&p, &["show", "SH-1", "--json"]);
    assert!(
        target["story"]["story"]["relationships"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["relation"] == "blocked-by" && r["other_id"] == "SH-3")
    );
    // A human may instead accept the finding and abandon the target, never complete it.
    run(&p, &["relate", "SH-1", "obviated-by", "SH-2"]);
    run(&p, &["close", "SH-1", "Human confirmed obviation"]);
    let target = json(&p, &["show", "SH-1", "--json"]);
    assert_eq!(target["story"]["story"]["state"], "dropped");
}
