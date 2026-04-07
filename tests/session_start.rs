/// Tests for the `story session-start` CLI command.
///
/// This command is designed for use by editor plugins and shell hooks.
/// It outputs raw JSON: `{"systemMessage":"..."}` when a project exists
/// and plugin is enabled, or `{}` when no project or plugin is disabled.
use assert_cmd::Command;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

// ============================================================
// No .storyhook/ directory -> {}
// ============================================================

#[test]
fn session_start_no_project_outputs_empty_json() {
    let dir = tempdir().unwrap();
    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success(), "should exit 0 with no project");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "{}", "should output empty JSON when no .storyhook/ exists");
}

// ============================================================
// Plugin disabled -> {}
// ============================================================

#[test]
fn session_start_plugin_disabled_outputs_empty_json() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    // Disable the plugin
    let config_path = dir.path().join(".storyhook/plugin-config.toml");
    std::fs::write(&config_path, "[plugin]\nenabled = false\ntracking = \"normal\"\n").unwrap();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "{}", "should output empty JSON when plugin is disabled");
}

#[test]
fn session_start_plugin_disabled_string_value_outputs_empty_json() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    // Disable the plugin with string "false" value
    let config_path = dir.path().join(".storyhook/plugin-config.toml");
    std::fs::write(&config_path, "enabled = \"false\"\n").unwrap();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "{}", "should output empty JSON when plugin disabled via string");
}

// ============================================================
// Valid project with stories -> systemMessage JSON
// ============================================================

#[test]
fn session_start_valid_project_outputs_system_message() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Implement user authentication"])
        .assert()
        .success();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("output should be valid JSON");

    assert!(
        parsed.get("systemMessage").is_some(),
        "output should contain systemMessage field"
    );

    let msg = parsed["systemMessage"].as_str().unwrap_or("");
    assert!(!msg.is_empty(), "systemMessage should not be empty");
}

// ============================================================
// systemMessage contains CLI reference
// ============================================================

#[test]
fn session_start_contains_cli_reference() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Add user profile page"])
        .assert()
        .success();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let msg = parsed["systemMessage"].as_str().unwrap_or("");

    // Verify key commands from the compact reference appear
    assert!(msg.contains("story next"), "should contain 'story next' command");
    assert!(msg.contains("story load-context"), "should contain 'story load-context' command");
    assert!(msg.contains("story new"), "should contain 'story new' command");
    assert!(msg.contains("story list"), "should contain 'story list' command");
    assert!(msg.contains("LIFECYCLE"), "should contain LIFECYCLE section header");
}

// ============================================================
// systemMessage contains project state
// ============================================================

#[test]
fn session_start_contains_project_state() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Build login page"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Add password reset flow"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-1", "high"])
        .assert()
        .success();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let msg = parsed["systemMessage"].as_str().unwrap_or("");

    // Should mention 2 open stories
    assert!(
        msg.contains("2 open stories"),
        "should reference 2 open stories, got: {msg}"
    );
    // Should mention ready count
    assert!(
        msg.contains("ready"),
        "should mention ready count, got: {msg}"
    );
}

// ============================================================
// systemMessage contains next story info
// ============================================================

#[test]
fn session_start_contains_next_story_info() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Deploy monitoring dashboard"])
        .assert()
        .success();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let msg = parsed["systemMessage"].as_str().unwrap_or("");

    assert!(
        msg.contains("SH-1") && msg.contains("Deploy monitoring dashboard"),
        "should contain next story ID and title, got: {msg}"
    );
    assert!(
        msg.contains("Next:"),
        "should have Next: label, got: {msg}"
    );
}

// ============================================================
// Empty project (0 stories) -> systemMessage with CLI ref and "0 open"
// ============================================================

#[test]
fn session_start_empty_project_zero_stories() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("should be valid JSON");

    let msg = parsed["systemMessage"].as_str().unwrap_or("");

    // Should still contain CLI reference
    assert!(
        msg.contains("story next"),
        "empty project should still include CLI reference"
    );
    // Should indicate 0 stories
    assert!(
        msg.contains("0 open stories"),
        "should indicate 0 open stories for empty project, got: {msg}"
    );
}

// ============================================================
// Special characters in story titles -> valid JSON
// ============================================================

#[test]
fn session_start_special_characters_in_title() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", r#"Fix "double-quote" and backslash\ in titles"#])
        .assert()
        .success();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(
        output.status.success(),
        "should handle special chars without crashing"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let result: Result<serde_json::Value, _> = serde_json::from_str(stdout.trim());
    assert!(
        result.is_ok(),
        "output must be valid JSON even with special chars in titles, got: {}",
        stdout.trim()
    );

    let parsed = result.unwrap();
    assert!(parsed.is_object(), "output must be a JSON object");
    assert!(
        parsed.get("systemMessage").is_some(),
        "should have systemMessage field"
    );
}

#[test]
fn session_start_unicode_in_story_title() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Add i18n support for Japanese text"])
        .assert()
        .success();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let result: Result<serde_json::Value, _> = serde_json::from_str(stdout.trim());
    assert!(result.is_ok(), "output must be valid JSON with unicode titles");
}

#[test]
fn session_start_newline_in_story_title() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    // Create a story with a title containing special JSON chars
    story(dir.path())
        .args(["new", "Fix tab\there and newline\nhere"])
        .assert()
        .success();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let result: Result<serde_json::Value, _> = serde_json::from_str(stdout.trim());
    assert!(
        result.is_ok(),
        "output must be valid JSON with control chars in titles, got: {}",
        stdout.trim()
    );
}

// ============================================================
// Output is strictly valid JSON
// ============================================================

#[test]
fn session_start_output_is_valid_json_object() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Validate JSON output integrity"])
        .assert()
        .success();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();

    let result: Result<serde_json::Value, _> = serde_json::from_str(trimmed);
    assert!(result.is_ok(), "output must be valid JSON, got: {trimmed}");

    let value = result.unwrap();
    assert!(value.is_object(), "output must be a JSON object, got: {trimmed}");
}

// ============================================================
// systemMessage under 4000 characters
// ============================================================

#[test]
fn session_start_system_message_under_4000_chars() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    // Create several stories to populate project state
    for i in 0..20 {
        story(dir.path())
            .args(["new", &format!("Story number {} with a reasonably long title for testing", i)])
            .assert()
            .success();
    }

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let msg = parsed["systemMessage"].as_str().unwrap_or("");

    assert!(
        msg.len() < 4000,
        "systemMessage should be under 4000 chars, got {} chars",
        msg.len()
    );
}

// ============================================================
// Performance: completes within 2 seconds
// ============================================================

#[test]
fn session_start_completes_within_two_seconds() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Performance-critical task"])
        .assert()
        .success();

    let start = std::time::Instant::now();
    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    let elapsed = start.elapsed();

    assert!(output.status.success());
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "session-start should complete within 2 seconds, took {:?}",
        elapsed
    );
}

// ============================================================
// Does not wrap in JSON envelope even with --json flag
// ============================================================

#[test]
fn session_start_ignores_json_flag() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Test JSON flag behavior"])
        .assert()
        .success();

    // With --json flag
    let output_json = story(dir.path())
        .args(["--json", "session-start"])
        .output()
        .expect("failed to run story session-start --json");
    assert!(output_json.status.success());

    // Without --json flag
    let output_plain = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output_plain.status.success());

    let stdout_json = String::from_utf8_lossy(&output_json.stdout);
    let stdout_plain = String::from_utf8_lossy(&output_plain.stdout);

    // Both should produce the same raw JSON output (systemMessage),
    // not wrapped in {"result":"ok","message":"..."} envelope
    let parsed_json: serde_json::Value = serde_json::from_str(stdout_json.trim()).unwrap();
    let parsed_plain: serde_json::Value = serde_json::from_str(stdout_plain.trim()).unwrap();

    assert!(
        parsed_json.get("systemMessage").is_some(),
        "with --json should still have systemMessage, not envelope"
    );
    assert!(
        parsed_json.get("result").is_none(),
        "with --json should NOT have result envelope"
    );
    assert_eq!(
        parsed_json, parsed_plain,
        "output should be identical with and without --json"
    );
}

// ============================================================
// No project: also works with --json flag
// ============================================================

#[test]
fn session_start_no_project_with_json_flag() {
    let dir = tempdir().unwrap();
    let output = story(dir.path())
        .args(["--json", "session-start"])
        .output()
        .expect("failed to run story session-start --json");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout.trim(), "{}", "should output empty JSON even with --json flag");
}

// ============================================================
// Next story with priority info
// ============================================================

#[test]
fn session_start_next_story_shows_priority() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Low priority task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Critical bug fix"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-2", "critical"])
        .assert()
        .success();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let msg = parsed["systemMessage"].as_str().unwrap_or("");

    // The next story should be SH-2 (critical priority sorts first)
    assert!(
        msg.contains("SH-2") && msg.contains("Critical bug fix"),
        "next story should be the critical one (SH-2), got: {msg}"
    );
    assert!(
        msg.contains("critical"),
        "should show priority for next story, got: {msg}"
    );
}
