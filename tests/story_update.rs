//! Integration tests for `story update` and `story --version`.
//!
//! These are intentionally network-free: every case is either an argument-parse
//! error (rejected before any HTTP call) or a help/version query. The live
//! download/replace path is exercised manually, not in CI.

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use storyhook_test_support::{TestEnv, scratch_dir};

/// Every `story` this file runs is the one THIS build produced, in the shared
/// test environment's private `HOME`, XDG directories and store — so nothing
/// here can reach the developer's own storyhook state, with or without a
/// wrapper script supplying one.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

#[test]
fn update_rejects_unknown_flag() {
    // The message moved with SH-62 — the flag gate answers ahead of
    // `parse_update` and names the token instead of printing a usage line —
    // but the contract this test exists for is unchanged: exit 2, and the
    // rejection is about the flag.
    let dir = scratch_dir();
    story(dir.path())
        .args(["update", "--bogus"])
        .assert()
        .code(2)
        .stderr(contains("unknown flag `--bogus`"));
}

#[test]
fn update_rejects_stray_positional() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["update", "foo"])
        .assert()
        .code(2)
        .stderr(contains("usage: story update"));
}

#[test]
fn update_check_and_force_are_mutually_exclusive() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["update", "--check", "--force"])
        .assert()
        .code(2)
        .stderr(contains("mutually exclusive"));
}

#[test]
fn update_is_a_recognized_command() {
    // A bad flag must be answered as a bad *flag for this verb*, NOT as the
    // top-level "unknown command" error — proving the dispatch arm is wired.
    // Since SH-62 the flag gate answers first, and it names the verb, so the
    // proof is stronger than it was: the message could not say `story update`
    // unless `update` had been recognized.
    let dir = scratch_dir();
    story(dir.path())
        .args(["update", "--bogus"])
        .assert()
        .code(2)
        .stderr(contains("for `story update`").and(contains("unknown command").not()));
}

#[test]
fn help_update_topic_exists() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["help", "update"])
        .assert()
        .success()
        .stdout(contains("story update"));
}

#[test]
fn top_level_help_lists_update() {
    story(TestEnv::shared().home())
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("story update"));
}

#[test]
fn version_flag_prints_version() {
    let expected = format!("story {}", env!("CARGO_PKG_VERSION"));
    story(TestEnv::shared().home())
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(expected.clone()));
    story(TestEnv::shared().home())
        .arg("-V")
        .assert()
        .success()
        .stdout(contains(expected));
}
