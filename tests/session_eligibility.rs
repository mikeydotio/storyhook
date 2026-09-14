//! SH-687: CLI eligibility is authoritative, structured, and read-only.

use serde_json::Value;
use storyhook_test_support::{Project, TestEnv};

fn run(p: &Project<'_>, args: &[&str]) -> String {
    String::from_utf8(
        p.story()
            .args(args)
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap()
}

fn eligibility(p: &Project<'_>, id: &str) -> Value {
    let response: Value =
        serde_json::from_str(&run(p, &["session-eligibility", id, "--json"])).unwrap();
    assert_eq!(response["result"], "ok");
    let result = response["session_eligibility"].clone();
    assert_eq!(result["schema_version"], 1);
    result
}

#[test]
fn lifecycle_and_renamed_active_role_are_read_without_writes() {
    let env = TestEnv::isolated();
    let p = env.project().prefix("SH").build();
    run(&p, &["new", "target"]);
    assert_eq!(eligibility(&p, "1")["reason"], "inactive");
    run(&p, &["state", "set", "in-progress", "--role", "none"]);
    run(
        &p,
        &[
            "state", "add", "working", "--super", "OPEN", "--role", "active",
        ],
    );
    run(&p, &["move", "SH-1", "working"]);
    let before = run(&p, &["export"]);
    let answer = eligibility(&p, "1");
    assert_eq!(answer["story_id"], "SH-1");
    assert_eq!(answer["eligible"], true);
    assert_eq!(answer["reason"], "eligible");
    assert_eq!(before, run(&p, &["export"]));
    run(&p, &["move", "SH-1", "verifying"]);
    assert_eq!(eligibility(&p, "1")["eligible"], false);
    run(&p, &["move", "SH-1", "done"]);
    assert_eq!(eligibility(&p, "1")["reason"], "closed");
}

#[test]
fn dependencies_obviation_and_explicit_awaiting_use_domain_readiness() {
    let env = TestEnv::isolated();
    let p = env.project().prefix("SH").build();
    run(&p, &["new", "target"]);
    run(&p, &["new", "dependency"]);
    run(&p, &["move", "SH-1", "in-progress"]);
    run(&p, &["relate", "SH-1", "blocked-by", "SH-2"]);
    assert_eq!(eligibility(&p, "1")["reason"], "blocked");
    run(&p, &["move", "SH-2", "done"]);
    assert_eq!(eligibility(&p, "1")["eligible"], true);
    run(&p, &["relate", "SH-1", "obviated-by", "SH-2"]);
    assert_eq!(eligibility(&p, "1")["eligible"], false);
    run(&p, &["unrelate", "SH-1", "obviated-by", "SH-2"]);
    run(&p, &["block", "SH-1", "human review"]);
    assert_eq!(eligibility(&p, "1")["reason"], "awaiting");
}

#[test]
fn missing_foreign_and_invalid_requests_fail_loudly() {
    let env = TestEnv::isolated();
    let p = env.project().prefix("SH").build();
    let other = env.project().prefix("OTHER").build();
    run(&other, &["new", "foreign target"]);
    for args in [
        vec!["session-eligibility"],
        vec!["session-eligibility", "SH-999"],
        vec!["session-eligibility", "OTHER-1"],
        vec!["session-eligibility", "1", "extra"],
        vec!["session-eligibility", "1", "--unknown"],
    ] {
        p.story().args(args).assert().failure();
    }
}

#[test]
fn missing_active_role_is_diagnostic_and_drafts_remain_ineligible() {
    let env = TestEnv::isolated();
    let p = env.project().prefix("SH").build();
    run(&p, &["new", "target"]);
    run(&p, &["move", "SH-1", "in-progress"]);
    run(&p, &["state", "set", "in-progress", "--role", "none"]);
    p.story()
        .args(["session-eligibility", "1", "--json"])
        .assert()
        .failure()
        .stdout(predicates::str::contains(
            "no unambiguous active state role",
        ));
    run(&p, &["state", "set", "in-progress", "--role", "active"]);
    run(&p, &["new", "draft", "--draft"]);
    run(&p, &["move", "SH-2", "in-progress"]);
    assert_eq!(eligibility(&p, "2")["reason"], "blocked");
    run(&p, &["publish", "SH-2"]);
    assert_eq!(eligibility(&p, "2")["eligible"], true);
}
