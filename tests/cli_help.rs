use assert_cmd::Command;
use predicates::str::contains;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn help_shows_binary_name_story() {
    let mut command = Command::cargo_bin("story").unwrap();
    command.arg("--help");
    command.assert().success().stdout(contains("story"));
}

#[test]
fn unknown_command_returns_clear_error() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["frobnicate"])
        .assert()
        .code(2)
        .stdout(contains("unknown command `frobnicate`"));
}

#[test]
fn unknown_command_with_hyphen_not_story_id() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["mcp-config-old"])
        .assert()
        .code(2)
        .stdout(contains("unknown command"));
}
