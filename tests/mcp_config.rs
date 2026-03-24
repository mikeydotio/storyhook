use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn mcp_config_outputs_storyhook_entry() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["mcp-config"])
        .assert()
        .success()
        .stdout(predicate::str::contains("storyhook"))
        .stdout(predicate::str::contains("--mcp"));
}

#[test]
fn mcp_config_scope_project_outputs_valid_json() {
    let dir = tempdir().unwrap();
    let output = story(dir.path())
        .args(["mcp-config", "--scope", "project"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(parsed.get("mcpServers").is_some());
    assert!(parsed["mcpServers"].get("storyhook").is_some());
}

#[test]
fn mcp_config_works_without_init() {
    let dir = tempdir().unwrap();
    // Do NOT run story init — should still work
    story(dir.path())
        .args(["mcp-config"])
        .assert()
        .success()
        .stdout(predicate::str::contains("storyhook"));
}

#[test]
fn mcp_config_install_claude() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .env("HOME", dir.path().to_str().unwrap())
        .args(["mcp-config", "--install", "claude"])
        .assert()
        .success()
        .stdout(predicate::str::contains("registered"));

    // Verify the file was created
    let claude_json = dir.path().join(".claude.json");
    assert!(claude_json.exists());
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
    assert!(content["mcpServers"]["storyhook"].is_object());
}

#[test]
fn mcp_config_uninstall_claude() {
    let dir = tempdir().unwrap();
    // Install first
    story(dir.path())
        .env("HOME", dir.path().to_str().unwrap())
        .args(["mcp-config", "--install", "claude"])
        .assert()
        .success();
    // Uninstall
    story(dir.path())
        .env("HOME", dir.path().to_str().unwrap())
        .args(["mcp-config", "--uninstall", "claude"])
        .assert()
        .success()
        .stdout(predicate::str::contains("deregistered"));

    // Verify storyhook entry removed
    let claude_json = dir.path().join(".claude.json");
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
    assert!(
        content.get("mcpServers").is_none()
            || !content["mcpServers"]
                .as_object()
                .unwrap()
                .contains_key("storyhook")
    );
}

#[test]
fn mcp_config_uninstall_preserves_other_entries() {
    let dir = tempdir().unwrap();
    // Create a config with another MCP server
    let claude_json = dir.path().join(".claude.json");
    std::fs::write(
        &claude_json,
        r#"{"mcpServers":{"other-tool":{"command":"other"}}}"#,
    )
    .unwrap();
    // Install storyhook
    story(dir.path())
        .env("HOME", dir.path().to_str().unwrap())
        .args(["mcp-config", "--install", "claude"])
        .assert()
        .success();
    // Uninstall storyhook
    story(dir.path())
        .env("HOME", dir.path().to_str().unwrap())
        .args(["mcp-config", "--uninstall", "claude"])
        .assert()
        .success();
    // Verify other-tool survived
    let content: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
    assert!(content["mcpServers"]["other-tool"].is_object());
}

#[test]
fn mcp_config_install_unknown_provider() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["mcp-config", "--install", "foobar"])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("unknown provider"));
}
