use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn help_shows_binary_name_story() {
    let mut command = Command::cargo_bin("story").unwrap();
    command.arg("--help");
    command.assert().success().stdout(contains("story"));
}
