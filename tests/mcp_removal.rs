/// Tests for MCP retirement and CLI migration guidance.
///
/// Protocol execution, old flags, and registration stay removed. The retired
/// command and its help explain CLI migration. Scaffolds stay CLI-only.
use assert_cmd::Command;
use predicates::prelude::*;
use storyhook_test_support::{TestEnv, scratch_dir};

/// Every `story` this file runs is the one THIS build produced, in the shared
/// test environment's private `HOME`, XDG directories and store — so nothing
/// here can reach the developer's own storyhook state, with or without a
/// wrapper script supplying one.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

// ============================================================
// --mcp flag removed
// ============================================================

#[test]
fn mcp_flag_no_longer_accepted() {
    // `story --mcp` should either fail with an error, be unrecognized,
    // or at minimum not start an MCP JSON-RPC server that processes requests.
    let dir = scratch_dir();

    // Send an MCP initialize request to test whether the server handles it
    let mcp_request = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
    let output = story(dir.path())
        .arg("--mcp")
        .write_stdin(mcp_request)
        .timeout(std::time::Duration::from_secs(5))
        .output()
        .expect("process should exit, not hang");

    let stdout = String::from_utf8_lossy(&output.stdout);
    // After MCP removal, the flag should either:
    // a) produce an error (non-zero exit), OR
    // b) be treated as an unknown flag and show help/error, OR
    // c) not produce a valid MCP initialize response
    let looks_like_mcp_response =
        stdout.contains("protocolVersion") && stdout.contains("serverInfo");
    assert!(
        !looks_like_mcp_response,
        "--mcp should not produce a valid MCP initialize response after removal, got: {stdout}"
    );
}

// ============================================================
// mcp-config command removed
// ============================================================

#[test]
fn mcp_config_command_returns_unknown_command_error() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["mcp-config"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown command"));
}

#[test]
fn mcp_config_with_install_flag_returns_unknown_command() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["mcp-config", "--install", "claude"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown command"));
}

#[test]
fn mcp_config_with_scope_flag_returns_unknown_command() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["mcp-config", "--scope", "project"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown command"));
}

// ============================================================
// Help system no longer references MCP
// ============================================================

#[test]
fn help_topic_mcp_config_not_found() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["help", "mcp-config"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown help topic"));
}

#[test]
fn help_topic_list_does_not_include_mcp_config() {
    let dir = scratch_dir();
    let output = story(dir.path())
        .args(["help", "mcp-config"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The error message lists available topics after "Available:";
    // "mcp-config" should not be among them (it may appear in the
    // "unknown help topic `mcp-config`" prefix, so check only the
    // portion after "Available:").
    if let Some(pos) = stdout.find("Available:") {
        let available_section = &stdout[pos..];
        // The available section is a comma-separated list; split and check
        // that none of the entries is "mcp-config".
        let topics: Vec<&str> = available_section
            .trim_start_matches("Available:")
            .split(',')
            .map(|t| t.trim())
            .collect();
        assert!(
            !topics.contains(&"mcp-config"),
            "mcp-config should not appear in the available topics list"
        );
    }
}

#[test]
fn help_output_does_not_mention_mcp_config() {
    let dir = scratch_dir();
    let output = story(dir.path()).arg("--help").assert().success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        !stdout.contains("mcp-config"),
        "--help output should not reference mcp-config"
    );
    assert!(
        !stdout.contains("--mcp"),
        "--help output should not reference --mcp flag"
    );
}

#[test]
fn json_format_help_topic_does_not_mention_mcp() {
    let dir = scratch_dir();
    let output = story(dir.path())
        .args(["help", "json-format"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        !stdout.contains("MCP"),
        "json-format help topic should not mention MCP after removal"
    );
    assert!(
        !stdout.contains("--mcp"),
        "json-format help topic should not mention --mcp flag"
    );
}

// ============================================================
// Scaffold outputs no longer reference MCP
// ============================================================

#[test]
fn scaffold_agents_md_does_not_mention_mcp() {
    let dir = scratch_dir();
    let output = story(dir.path())
        .args(["scaffold", "agents-md"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        !stdout.contains("MCP"),
        "agents-md scaffold should not mention MCP"
    );
    assert!(
        !stdout.contains("mcp-config"),
        "agents-md scaffold should not reference mcp-config command"
    );
}

#[test]
fn scaffold_cursor_rules_does_not_mention_mcp() {
    let dir = scratch_dir();
    let output = story(dir.path())
        .args(["scaffold", "cursor-rules"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        !stdout.contains("MCP"),
        "cursor-rules scaffold should not mention MCP"
    );
    assert!(
        !stdout.contains("mcp-config"),
        "cursor-rules scaffold should not reference mcp-config command"
    );
}

#[test]
fn scaffold_claude_md_does_not_mention_mcp() {
    let dir = scratch_dir();
    let output = story(dir.path())
        .args(["scaffold", "claude-md"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(
        !stdout.contains("MCP"),
        "claude-md scaffold should not mention MCP"
    );
    assert!(
        !stdout.contains("mcp-config"),
        "claude-md scaffold should not reference mcp-config command"
    );
}

// ============================================================
// Init-generated AGENTS.md does not reference MCP
// ============================================================

#[test]
fn init_generated_agents_md_does_not_mention_mcp() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "TST"])
        .assert()
        .success();
    let agents_md = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
    assert!(
        !agents_md.contains("MCP"),
        "AGENTS.md should not reference MCP"
    );
    assert!(
        !agents_md.contains("mcp-config"),
        "AGENTS.md should not reference mcp-config"
    );
}

// ============================================================
// session-start output does not reference MCP
// ============================================================

#[test]
fn session_start_system_message_does_not_mention_mcp() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    // Use a title that does NOT contain "MCP" to avoid false positives
    // from user-supplied story titles appearing in the injected context.
    story(dir.path())
        .args(["new", "Verify removed protocol is gone from session-start"])
        .assert()
        .success();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let msg = parsed["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or("");

    // Strip out the PROJECT STATE section (which includes user-supplied story titles)
    // to avoid false positives. Only check the CLI reference portion.
    let cli_ref_portion = if let Some(idx) = msg.find("PROJECT STATE") {
        &msg[..idx]
    } else {
        msg
    };

    assert!(
        !cli_ref_portion.contains("MCP"),
        "session-start CLI reference should not reference MCP"
    );
    assert!(
        !cli_ref_portion.contains("mcp-config"),
        "session-start CLI reference should not reference mcp-config"
    );
    assert!(
        !cli_ref_portion.contains("--mcp"),
        "session-start CLI reference should not reference --mcp flag"
    );
}

// ============================================================
// Source code regression guard: no MCP file reintroduction
// ============================================================

#[test]
fn no_mcp_source_files_exist() {
    // Guard against accidentally re-adding mcp.rs or mcp_install.rs
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(
        !manifest_dir.join("src/mcp.rs").exists(),
        "src/mcp.rs must not exist after MCP removal"
    );
    assert!(
        !manifest_dir.join("src/mcp_install.rs").exists(),
        "src/mcp_install.rs must not exist after MCP removal"
    );
}

/// Retired protocol callers must fail before reading input or opening a store.
#[test]
fn retired_mcp_refuses_plain_and_json_without_starting_a_daemon() {
    for json in [false, true] {
        let env = TestEnv::isolated();
        let dir = scratch_dir();
        let mut command = env.story(dir.path());
        command.arg("mcp");
        if json {
            command.arg("--json");
        }
        let output = command
            .write_stdin("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n")
            .timeout(std::time::Duration::from_secs(5))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        let diagnostic = if json {
            assert!(output.stderr.is_empty());
            let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["result"], "error");
            assert_eq!(value["exit_code"], 2);
            assert!(value.get("jsonrpc").is_none());
            value["error"].as_str().unwrap().to_owned()
        } else {
            assert!(output.stdout.is_empty());
            String::from_utf8(output.stderr).unwrap()
        };
        assert!(diagnostic.contains("retired"), "{diagnostic}");
        assert!(diagnostic.contains("--json"), "{diagnostic}");
        assert!(
            diagnostic.contains("story help json-format"),
            "{diagnostic}"
        );
        assert!(
            !env.store_path().exists(),
            "retirement must not open a store"
        );
        assert!(!env.environment().daemon_state_dir().exists());
    }
}

#[test]
fn retired_mcp_help_explains_migration() {
    let dir = scratch_dir();
    for args in [["mcp", "--help"], ["help", "mcp"]] {
        story(dir.path())
            .args(args)
            .assert()
            .success()
            .stdout(predicate::str::contains("retired"))
            .stdout(predicate::str::contains("story help json-format"));
    }
}

#[test]
fn retired_mcp_has_no_server_module_or_plugin_registration() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    assert!(!root.join("src/mcp").exists());
    let plugin: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("plugins/story/.claude-plugin/plugin.json")).unwrap(),
    )
    .unwrap();
    assert!(plugin.get("mcpServers").is_none());
}

/// Keep stdin open: a retired server must not wait for a request or EOF.
#[test]
fn retired_mcp_exits_while_protocol_input_remains_open() {
    use std::process::Stdio;
    use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE};
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let mut command = env.raw_story(dir.path());
    command.arg("mcp");
    let mut child = ChildGuard::spawn_with_output(command.stdin(Stdio::piped())).unwrap();
    let input = child.take_stdin().unwrap();
    let status = child.wait_within(STORY_COMMAND_DEADLINE, || {
        "retired MCP waited for stdin".into()
    });
    assert_eq!(status.code(), Some(2));
    drop(input);
    assert!(!env.store_path().exists());
    assert!(!env.environment().daemon_state_dir().exists());
}
