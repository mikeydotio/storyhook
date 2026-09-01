//! `story member add` — the project's roster.
//!
//! This file used to read `.storyhook/members.jsonl` back and assert on the
//! appended event's fields. The roster is a table in the store now, so the
//! question is asked of the CLI instead: a member exists exactly when a story
//! can be assigned to them and the assignment reports the id storyhook minted.
//! That is a stronger claim than "a line was appended to a file" — it is the
//! reason the roster exists — and it is true on both storage models.

use assert_cmd::Command;
use storyhook_test_support::{TestEnv, scratch_dir};

/// Every `story` this file runs is the one THIS build produced, in the shared
/// test environment's private `HOME`, XDG directories and store — so nothing
/// here can reach the developer's own storyhook state, with or without a
/// wrapper script supplying one.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

/// Runs `story <args> --json` and parses stdout.
fn json(dir: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let out = story(dir)
        .args(args)
        .arg("--json")
        .output()
        .expect("running story");
    assert!(
        out.status.success(),
        "`story {} --json` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("story --json must print JSON")
}

#[test]
fn a_member_added_by_name_and_email_can_be_assigned_work() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["member", "add", "mikey <mw@mikey.io>"])
        .assert()
        .success()
        // The id is derived from the name, and scripts read it off this line.
        .stdout(predicates::str::contains("added member mikey"));

    let id = json(dir.path(), &["new", "Something to assign"])["story"]["story"]["id"]
        .as_str()
        .expect("a minted id")
        .to_string();
    let assigned = json(dir.path(), &["assign", &id, "mikey"]);
    assert_eq!(
        assigned["story"]["story"]["assignee"], "mikey",
        "a member that was added must be assignable by the id `member add` reported"
    );
}

#[test]
fn a_member_added_by_github_handle_gets_the_handle_as_its_id() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["member", "add", "-g", "grace-hopper"])
        .assert()
        .success()
        .stdout(predicates::str::contains("added member grace-hopper"));

    let id = json(dir.path(), &["new", "Something else"])["story"]["story"]["id"]
        .as_str()
        .expect("a minted id")
        .to_string();
    assert_eq!(
        json(dir.path(), &["assign", &id, "grace-hopper"])["story"]["story"]["assignee"],
        "grace-hopper"
    );
}

#[test]
fn assigning_to_a_member_that_was_never_added_fails() {
    // The negative half: without it the two tests above would pass against an
    // `assign` that accepted any string at all.
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    let id = json(dir.path(), &["new", "Unassignable"])["story"]["story"]["id"]
        .as_str()
        .expect("a minted id")
        .to_string();

    story(dir.path())
        .args(["assign", &id, "nobody"])
        .assert()
        .failure()
        .code(3);
}
