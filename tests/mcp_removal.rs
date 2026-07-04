/// Tests verifying that MCP functionality has been completely removed.
///
/// These tests assert the negative — that MCP-related commands, flags,
/// help topics, and scaffold output references are all gone. They exist
/// to catch regressions if someone accidentally re-introduces MCP paths.
use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

// ============================================================
// --mcp flag removed
// ============================================================

#[test]
fn mcp_flag_no_longer_accepted() {
    // `story --mcp` should either fail with an error, be unrecognized,
    // or at minimum not start an MCP JSON-RPC server that processes requests.
    let dir = tempdir().unwrap();

    // Send an MCP initialize request to test whether the server handles it
    let mcp_request = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
    let output = Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
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
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["mcp-config"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("unknown command"));
}

#[test]
fn mcp_config_with_install_flag_returns_unknown_command() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["mcp-config", "--install", "claude"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("unknown command"));
}

#[test]
fn mcp_config_with_scope_flag_returns_unknown_command() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["mcp-config", "--scope", "project"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("unknown command"));
}

// ============================================================
// Help system no longer references MCP
// ============================================================

#[test]
fn help_topic_mcp_config_not_found() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["help", "mcp-config"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("unknown help topic"));
}

#[test]
fn help_topic_list_does_not_include_mcp_config() {
    let dir = tempdir().unwrap();
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
    let dir = tempdir().unwrap();
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
    let dir = tempdir().unwrap();
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
    let dir = tempdir().unwrap();
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
    let dir = tempdir().unwrap();
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
    let dir = tempdir().unwrap();
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
// Init-generated CLAUDE.md does not reference MCP
// ============================================================

#[test]
fn init_generated_claude_md_does_not_mention_mcp() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["init", "--prefix", "TST"])
        .assert()
        .success();
    let claude_md = std::fs::read_to_string(dir.path().join(".storyhook/CLAUDE.md")).unwrap();
    assert!(
        !claude_md.contains("MCP"),
        ".storyhook/CLAUDE.md should not reference MCP"
    );
    assert!(
        !claude_md.contains("mcp-config"),
        ".storyhook/CLAUDE.md should not reference mcp-config"
    );
}

// ============================================================
// session-start output does not reference MCP
// ============================================================

#[test]
fn session_start_system_message_does_not_mention_mcp() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
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
