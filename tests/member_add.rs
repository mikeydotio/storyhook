// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

use assert_cmd::Command;
use tempfile::tempdir;

#[test]
fn member_add_writes_jsonl_event() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .args(["member", "add", "mikey <mw@mikey.io>"])
        .assert()
        .success();

    let data = std::fs::read_to_string(dir.path().join(".storyhook/members.jsonl")).unwrap();
    assert!(data.contains("mw@mikey.io"));
    assert!(data.contains("\"id\":\"mikey\""));
}
