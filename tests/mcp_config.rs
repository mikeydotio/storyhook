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
