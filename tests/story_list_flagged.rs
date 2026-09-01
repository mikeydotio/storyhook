use assert_cmd::Command;
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
fn list_flagged_filters_to_attention_items() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path()).args(["new", "A"]).assert().success();

    story(dir.path()).args(["new", "B"]).assert().success();

    story(dir.path())
        .args(["relate", "SH-1", "obviates", "SH-2"])
        .assert()
        .success();

    story(dir.path())
        .args(["list", "--flagged"])
        .assert()
        .success()
        .stdout(contains("SH-2"));
}
