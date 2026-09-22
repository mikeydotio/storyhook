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

/// A data argument must not become part of the command name in a diagnostic.
#[test]
fn unknown_flag_names_the_command_and_a_working_help_topic() {
    let dir = scratch_dir();
    for (args, path, topic) in [
        (vec!["new", "private title", "--typo"], "new", "new"),
        (vec!["show", "SH-1", "--typo"], "show", "show"),
        (vec!["engine", "start", "--typo"], "engine start", "engine"),
    ] {
        for json in [false, true] {
            let mut command = story(dir.path());
            command.args(&args);
            if json {
                command.arg("--json");
            }
            let output = command.output().unwrap();
            assert_eq!(output.status.code(), Some(2));
            let message = if json {
                assert!(output.stderr.is_empty());
                let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
                assert_eq!(value["result"], "error");
                value["error"].as_str().unwrap().to_owned()
            } else {
                assert!(output.stdout.is_empty());
                String::from_utf8(output.stderr).unwrap()
            };
            assert!(
                message.contains(&format!("for `story {path}`.")),
                "{message}"
            );
            assert!(
                message.contains(&format!("story help {topic}")),
                "{message}"
            );
            assert!(!message.contains("private title"), "{message}");
            assert!(storyhook::help_topics::get_help_topic(topic).is_some());
        }
    }
}
