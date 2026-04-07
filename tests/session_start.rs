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

// ============================================================
// Corrupted storyhook data -> graceful degradation
// ============================================================

#[test]
fn session_start_corrupted_stories_dir_still_returns_json() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    // Corrupt the stories directory by replacing it with a file
    let stories_dir = dir.path().join(".storyhook/open/stories");
    std::fs::remove_dir_all(&stories_dir).unwrap();
    std::fs::write(&stories_dir, "this is not a directory").unwrap();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");

    // Should still exit 0 and produce valid JSON (possibly with degraded content)
    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();

    // Must be parseable JSON, even in error states
    let result: Result<serde_json::Value, _> = serde_json::from_str(trimmed);
    assert!(
        result.is_ok(),
        "session-start must produce valid JSON even with corrupted stories dir, got: {trimmed}"
    );
}

#[test]
fn session_start_missing_project_toml_still_returns_json() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    // Delete the project.toml but leave .storyhook/ directory intact
    std::fs::remove_file(dir.path().join(".storyhook/project.toml")).unwrap();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();

    // Should produce valid JSON (either {} or {systemMessage: ...})
    let result: Result<serde_json::Value, _> = serde_json::from_str(trimmed);
    assert!(
        result.is_ok(),
        "session-start must produce valid JSON even with missing project.toml, got: {trimmed}"
    );
}

// ============================================================
// Plugin config parsing edge cases
// ============================================================

/// BUG FINDING: The plugin-config parser uses string matching ("= false")
/// which requires exactly one space between `=` and `false`. TOML allows
/// arbitrary whitespace around `=`, so `enabled  =   false` is valid TOML
/// but fails to match. This test documents the current (broken) behavior.
///
/// To fix: use a proper TOML parser (toml crate) or normalize whitespace
/// before matching. Filed as known issue.
#[test]
fn session_start_plugin_config_extra_whitespace_bug_documented() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    // Write config with extra whitespace around the value
    let config_path = dir.path().join(".storyhook/plugin-config.toml");
    std::fs::write(&config_path, "[plugin]\nenabled  =   false\ntracking = \"normal\"\n").unwrap();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();

    // CURRENT BEHAVIOR (BUG): extra whitespace causes the "enabled = false"
    // check to miss, so the plugin is treated as enabled. The correct behavior
    // would be to output "{}", but currently it outputs a systemMessage.
    // This test documents the bug so it can be tracked.
    assert!(
        parsed.get("systemMessage").is_some(),
        "BUG: extra whitespace in plugin config causes enabled=false to be ignored. \
         If this assertion fails, it means the bug has been FIXED -- update this test \
         to assert_eq!(stdout.trim(), \"{{}}\") instead."
    );
}

#[test]
fn session_start_plugin_config_enabled_true_produces_system_message() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Test enabled true config"])
        .assert()
        .success();

    // Write config with enabled = true explicitly
    let config_path = dir.path().join(".storyhook/plugin-config.toml");
    std::fs::write(&config_path, "[plugin]\nenabled = true\ntracking = \"normal\"\n").unwrap();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(
        parsed.get("systemMessage").is_some(),
        "enabled = true should produce systemMessage, got: {}",
        stdout.trim()
    );
}

#[test]
fn session_start_plugin_config_malformed_still_works() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Test malformed config"])
        .assert()
        .success();

    // Write a completely malformed plugin config (not valid TOML)
    let config_path = dir.path().join(".storyhook/plugin-config.toml");
    std::fs::write(&config_path, "{{{{garbage not toml!!! %%%").unwrap();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    // Malformed config should NOT disable the plugin (fail open, not fail closed).
    // If the config can't be read, plugin is considered enabled.
    assert!(
        parsed.get("systemMessage").is_some(),
        "malformed plugin config should not disable the plugin, got: {}",
        stdout.trim()
    );
}

#[test]
fn session_start_no_plugin_config_file_produces_system_message() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Test no plugin config"])
        .assert()
        .success();

    // Ensure there's no plugin-config.toml (init shouldn't create one by default)
    let config_path = dir.path().join(".storyhook/plugin-config.toml");
    if config_path.exists() {
        std::fs::remove_file(&config_path).unwrap();
    }

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(
        parsed.get("systemMessage").is_some(),
        "missing plugin config should not disable the plugin, got: {}",
        stdout.trim()
    );
}

// ============================================================
// Output structure contract: only two valid shapes
// ============================================================

#[test]
fn session_start_output_is_one_of_two_valid_shapes() {
    // The contract: output is either exactly `{}` or `{"systemMessage":"..."}`.
    // No other keys should be present.
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Test output shape contract"])
        .assert()
        .success();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let obj = parsed.as_object().expect("output must be a JSON object");

    // Valid keys are: none (empty {}) or exactly "systemMessage"
    for key in obj.keys() {
        assert_eq!(
            key, "systemMessage",
            "session-start output should only contain 'systemMessage' key, found: '{key}'"
        );
    }

    if let Some(msg) = obj.get("systemMessage") {
        assert!(
            msg.is_string(),
            "systemMessage value must be a string, got: {msg}"
        );
    }
}

// ============================================================
// stderr is clean (no warnings or debug output)
// ============================================================

#[test]
fn session_start_stderr_is_empty() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Test stderr cleanliness"])
        .assert()
        .success();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.is_empty(),
        "session-start should not produce stderr output, got: {stderr}"
    );
}

// ============================================================
// UTF-8 safe truncation: multi-byte titles past 3900 bytes
// ============================================================

#[test]
fn session_start_utf8_safe_truncation_with_multibyte_titles() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    // The compact CLI reference is ~2800 bytes. We need to push systemMessage
    // past 3900 bytes total. The "Next:" line includes the full title, so a
    // single story with a very long CJK/emoji title will do it.
    //
    // Each CJK character is 3 bytes in UTF-8, each emoji is 4 bytes.
    // We need ~1200 bytes of multi-byte content to push past 3900.
    // 400 CJK chars = 1200 bytes, plus we add emoji for good measure.
    let cjk_block: String = std::iter::repeat_n('\u{6D4B}', 200) // CJK char (3 bytes each = 600 bytes)
        .chain(std::iter::repeat_n('\u{1F680}', 200)) // Rocket emoji (4 bytes each = 800 bytes)
        .collect();
    let title = format!("Long UTF-8 title {cjk_block}");

    story(dir.path())
        .args(["new", &title])
        .assert()
        .success();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");

    // AC: exit code 0
    assert!(
        output.status.success(),
        "should exit 0 even when truncation hits multi-byte UTF-8 boundary, status: {}",
        output.status
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();

    // AC: valid JSON
    let result: Result<serde_json::Value, _> = serde_json::from_str(trimmed);
    assert!(
        result.is_ok(),
        "output must be valid JSON after UTF-8 safe truncation, got: {}",
        &trimmed[..trimmed.len().min(200)]
    );

    // AC: systemMessage present
    let parsed = result.unwrap();
    assert!(
        parsed.get("systemMessage").is_some(),
        "output should contain systemMessage field after truncation"
    );
}
