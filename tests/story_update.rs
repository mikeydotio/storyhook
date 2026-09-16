//! Integration tests for `story update` and `story --version`.
//!
//! Real CLI and replacement flows with controlled gh release endpoints.

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

/// The help must say what the update does to the plugins, because the
/// person deciding whether to run it is the one whose host session the
/// reinstall affects (SH-667).
#[test]
fn help_update_names_the_plugin_reinstall_and_its_retry() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["help", "update"])
        .assert()
        .success()
        .stdout(
            contains("reinstalls the plugin")
                .and(contains("story plugin reinstall"))
                .and(contains("story doctor install")),
        );
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

#[test]
fn explicit_release_source_routes_without_a_project_checkout() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch_dir();
    let gh = dir.path().join("gh");
    std::fs::write(&gh, "#!/bin/sh\nprintf '%s\\n' \"$GH_HOST|$GH_REPO|$*\" > \"$(dirname \"$0\")/calls\"\nprintf '{\"tagName\":\"v999.0.0\"}'\n").unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    story(dir.path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                dir.path().display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env("GH_HOST", "wrong.example")
        .env("GH_REPO", "wrong.example/foreign/repo")
        .args([
            "update",
            "--check",
            "--source",
            "github.pie.apple.com/acme/storyhook",
        ])
        .assert()
        .success()
        .stdout(contains("update available"));
    let calls = std::fs::read_to_string(dir.path().join("calls")).unwrap();
    assert_eq!(
        calls.trim(),
        "github.pie.apple.com|github.pie.apple.com/acme/storyhook|release view --json tagName --repo github.pie.apple.com/acme/storyhook"
    );
}

#[test]
fn legacy_installation_never_guesses_a_release_source() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["update", "--check"])
        .assert()
        .failure()
        .stderr(contains("--source HOST/OWNER/REPO"));
}

#[test]
fn update_downloads_smoke_tests_and_publishes_matching_source_metadata() {
    use sha2::{Digest, Sha256};
    use std::{fs, os::unix::fs::PermissionsExt};
    for valid in [false, true] {
        let dir = scratch_dir();
        let installed = dir.path().join("installed-story");
        fs::copy(env!("CARGO_BIN_EXE_story"), &installed).unwrap();
        let stage = dir.path().join("asset");
        fs::create_dir(&stage).unwrap();
        let replacement = b"#!/bin/sh\n[ \"$1\" = --help ]\n";
        let replacement: &[u8] = if valid {
            replacement
        } else {
            b"#!/bin/sh\nexit 1\n"
        };
        let original = format!("{:x}", Sha256::digest(fs::read(&installed).unwrap()));
        fs::write(stage.join("story"), replacement).unwrap();
        let archive = dir.path().join("fixture.tar.gz");
        assert!(
            std::process::Command::new("tar")
                .args(["czf"])
                .arg(&archive)
                .arg("-C")
                .arg(&stage)
                .arg("story")
                .status()
                .unwrap()
                .success()
        );
        let gh = dir.path().join("gh");
        fs::write(&gh, "#!/bin/sh\ncase \"$1 $2\" in\n'release view') printf '{\"tagName\":\"v999.0.0\"}' ;;\n'release download') while [ \"$1\" != --output ]; do shift; done; cp \"$(dirname \"$0\")/fixture.tar.gz\" \"$2\" ;;\n*) exit 91;;\nesac\n").unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        let mut cmd = std::process::Command::new(&installed);
        TestEnv::shared().apply(&mut cmd);
        cmd.current_dir(dir.path())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    dir.path().display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .args(["update", "--source", "github.pie.apple.com/acme/storyhook"]);
        let output = cmd.output().unwrap();
        if !valid {
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("failed to execute"));
            assert_eq!(
                format!("{:x}", Sha256::digest(fs::read(&installed).unwrap())),
                original
            );
            assert!(!dir.path().join("installed-story.source.json").exists());
            continue;
        }
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read(&installed).unwrap(), replacement);
        let metadata: serde_json::Value = serde_json::from_slice(
            &fs::read(dir.path().join("installed-story.source.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(metadata["version"], 1);
        assert_eq!(metadata["source"], "github.pie.apple.com/acme/storyhook");
        assert_eq!(
            metadata["sha256"],
            format!("{:x}", Sha256::digest(replacement))
        );
    }
}

#[test]
fn invalid_release_sources_fail_before_any_gh_process() {
    let dir = scratch_dir();
    for source in [
        "owner/repo",
        "https://host/owner/repo",
        "host/owner/repo/extra",
        "host/../repo",
        "user:secret@host/owner/repo",
        "host/owner/repo?token=secret",
    ] {
        story(dir.path())
            .args(["update", "--check", "--source", source])
            .assert()
            .failure()
            .stderr(contains("secret").not());
    }
}
