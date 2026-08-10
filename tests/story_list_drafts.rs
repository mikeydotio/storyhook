// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

/// SH-175's council verdict: `story list` never default-excludes anything,
/// including drafts — they show inline with a `[draft]` badge, and `--drafts`
/// only ever narrows to drafts-only. This is a deliberate divergence from the
/// web dashboard's board, which does exclude them by default (a separate
/// server-side path — `tests/web_test.rs` pins that half).
#[test]
fn list_with_no_flags_shows_drafts_inline_with_a_badge() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Live"]).assert().success();
    story(dir.path())
        .args(["new", "A sketch", "--draft"])
        .assert()
        .success();

    story(dir.path())
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("SH-1"))
        .stdout(contains("SH-2"))
        .stdout(contains("[draft]"));
}

#[test]
fn list_drafts_narrows_to_drafts_only() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Live"]).assert().success();
    story(dir.path())
        .args(["new", "A sketch", "--draft"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["list", "--drafts"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("SH-2"), "the draft should be listed: {text}");
    assert!(
        !text.contains("SH-1"),
        "the live story must not appear under --drafts: {text}"
    );
}

#[test]
fn list_drafts_with_no_drafts_reports_none_found() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path()).args(["new", "Live"]).assert().success();

    story(dir.path())
        .args(["list", "--drafts"])
        .assert()
        .success()
        .stdout(contains("no stories found"));
}

/// A published draft loses its badge and starts counting for `--ready`
/// (SH-175's council verdict scoped readiness independently of `list`'s
/// visibility, but the CLI end-to-end shape is worth pinning together).
#[test]
fn a_published_draft_no_longer_carries_the_draft_badge_and_becomes_ready() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "A sketch", "--draft"])
        .assert()
        .success();

    story(dir.path())
        .args(["list", "--ready"])
        .assert()
        .success()
        .stdout(contains("no stories found"));

    story(dir.path())
        .args(["publish", "SH-1"])
        .assert()
        .success();

    story(dir.path())
        .args(["list"])
        .assert()
        .success()
        .stdout(contains("[draft]").not());

    story(dir.path())
        .args(["list", "--ready"])
        .assert()
        .success()
        .stdout(contains("SH-1"));
}
