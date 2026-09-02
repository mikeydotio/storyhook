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

#[test]
fn story_new_assigns_monotonic_ids_and_show_works() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "First"])
        .assert()
        .success()
        .stdout(contains("SH-1"));

    story(dir.path())
        .args(["new", "Second"])
        .assert()
        .success()
        .stdout(contains("SH-2"));

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(contains("First"));
}

#[test]
fn comment_and_assign_append_events() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["member", "add", "mikey <mw@mikey.io>"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Routing"])
        .assert()
        .success();

    story(dir.path())
        .args(["assign", "SH-1", "mikey"])
        .assert()
        .success();

    story(dir.path())
        .args(["comment", "SH-1", "First pass done"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(contains("assignee: mikey"))
        .stdout(contains("First pass done"));
}

/// SH-261's own repro, end to end over `/api/v1/invoke`: verification that
/// arrives after a story closes lands on the story it verifies.
#[test]
fn a_closed_story_takes_a_comment_from_the_command_line() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Ship it"])
        .assert()
        .success();

    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success();

    story(dir.path())
        .args([
            "comment",
            "SH-1",
            "verified in CI: run 31657323566, four green targets",
        ])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(contains("verified in CI: run 31657323566"))
        // Still closed: the comment records something about the story, it does
        // not reopen it.
        .stdout(contains("(CLOSED)"));
}

/// A permanently deleted story cannot receive new observations because its id
/// no longer resolves.
#[test]
fn a_deleted_story_is_not_found_when_commented_on() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Never mind"])
        .assert()
        .success();

    story(dir.path())
        .args(["delete", "SH-1", "--force"])
        .assert()
        .success();

    story(dir.path())
        .args(["comment", "SH-1", "one more thing"])
        .assert()
        .failure()
        .stderr(contains("story `SH-1` not found"));
}

/// A comment naming another story surfaces on *that* story's `referenced_by`
/// block, in the `[comment]` shape beside `[git]` and `[pr]` (SH-220) — and
/// never as a comment on it.
#[test]
fn a_comment_naming_another_story_shows_up_under_its_referenced_by() {
    let dir = scratch_dir();
    for args in [
        vec!["project", "new", "--prefix", "SH"],
        vec!["new", "Mentioned"],
        vec!["new", "Mentioner"],
        vec!["comment", "SH-2", "superseded by SH-1"],
    ] {
        story(dir.path()).args(&args).assert().success();
    }

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(contains("referenced_by:"))
        .stdout(contains("[comment] SH-2: superseded by SH-1"))
        .stdout(contains("comments:").not());

    story(dir.path())
        .args(["show", "SH-2"])
        .assert()
        .success()
        .stdout(contains("superseded by SH-1"))
        .stdout(contains("[comment]").not());
}

#[test]
fn awaiting_can_be_set_and_cleared() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["new", "Blocked work"])
        .assert()
        .success();

    story(dir.path())
        .args(["block", "SH-1", "blocked on API"])
        .assert()
        .success()
        .stdout(contains("awaiting: blocked on API"));

    story(dir.path())
        .args(["--json", "show", "SH-1"])
        .assert()
        .success()
        .stdout(contains("\"awaiting\": \"blocked on API\""));

    story(dir.path())
        .args(["unblock", "SH-1"])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicates::str::contains("awaiting:").not());
}
