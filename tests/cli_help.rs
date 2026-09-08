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
fn help_shows_binary_name_story() {
    let mut command = story(TestEnv::shared().home());
    command.arg("--help");
    command.assert().success().stdout(contains("story"));
}

#[test]
fn help_shows_the_complete_daemon_command_family() {
    let mut command = story(TestEnv::shared().home());
    command.arg("--help");
    command
        .assert()
        .success()
        .stdout(contains("story daemon start [--port <PORT>]"))
        .stdout(contains("story daemon restart"))
        .stdout(contains("story daemon stop [--force]"))
        .stdout(contains("story daemon status"))
        .stdout(contains("story daemon install [--this-binary]"))
        .stdout(contains("story daemon uninstall"))
        .stdout(contains("story daemon token"));
}

#[test]
fn unknown_command_returns_clear_error() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["frobnicate"])
        .assert()
        .code(2)
        .stderr(contains("unknown command `frobnicate`"));
}

#[test]
fn unknown_command_with_hyphen_not_story_id() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["mcp-config-old"])
        .assert()
        .code(2)
        .stderr(contains("unknown command"));
}
