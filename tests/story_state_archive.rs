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
fn closing_a_story_takes_it_out_of_the_open_set_and_keeps_it_readable() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Finish me"])
        .assert()
        .success();

    story(dir.path())
        .args(["move", "SH-1", "done", "completed"])
        .assert()
        .success()
        .stdout(contains("closed_at:"));

    // The archive was a second storage medium; it is a column now. What it was
    // ever for is asserted instead: a closed story stops counting as open and
    // stays readable.
    story(dir.path())
        .args(["--json", "summary"])
        .assert()
        .success()
        .stdout(contains("\"total_open\": 0"));

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(contains("Finish me"))
        .stdout(contains("state: done (CLOSED)"));
}

#[test]
fn closing_story_clears_awaiting_in_archive_snapshot() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Blocked close"])
        .assert()
        .success();

    story(dir.path())
        .args(["block", "SH-1", "waiting on review"])
        .assert()
        .success();

    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success()
        .stdout(contains("closed_at:"));

    story(dir.path())
        .args(["--json", "show", "SH-1"])
        .assert()
        .success()
        .stdout(contains("\"awaiting\": null"));
}
