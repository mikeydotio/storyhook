/// Tests for the new `story help --compact` and `story help --all` flags.
///
/// These flags add LLM-optimized output modes to the help system:
/// - `--compact`: minimal CLI reference, under ~80 lines
/// - `--all`: all topics concatenated as a single document
use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

// ============================================================
// story help --compact
// ============================================================

#[test]
fn help_compact_produces_output_with_key_commands() {
    let dir = tempdir().unwrap();
    let output = story(dir.path())
        .args(["help", "--compact"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    // Must contain the core workflow commands
    assert!(stdout.contains("new"), "compact help should mention 'new' command");
    assert!(stdout.contains("list"), "compact help should mention 'list' command");
    assert!(stdout.contains("next"), "compact help should mention 'next' command");
    assert!(stdout.contains("move"), "compact help should mention 'move' command");
    assert!(stdout.contains("show"), "compact help should mention 'show' command");
    assert!(
        stdout.contains("load-context"),
        "compact help should mention 'load-context' command"
    );
}

#[test]
fn help_compact_is_concise() {
    let dir = tempdir().unwrap();
    let output = story(dir.path())
        .args(["help", "--compact"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let line_count = stdout.lines().count();

    assert!(
        line_count <= 100,
        "compact help should be at most 100 lines, got {line_count}"
    );
    assert!(
        line_count >= 40,
        "compact help should have at least 40 lines of useful content, got {line_count}"
    );
}

#[test]
fn help_compact_does_not_include_full_topic_explanations() {
    let dir = tempdir().unwrap();
    let output = story(dir.path())
        .args(["help", "--compact"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    // Full topics include "When to use:" sections; compact should not
    assert!(
        !stdout.contains("When to use:"),
        "compact help should not include verbose 'When to use' sections"
    );
}

// ============================================================
// story help --all
// ============================================================

#[test]
fn help_all_produces_all_topics() {
    let dir = tempdir().unwrap();
    let output = story(dir.path())
        .args(["help", "--all"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    // Should contain content from multiple distinct topics
    assert!(stdout.contains("story init"), "should include init topic");
    assert!(stdout.contains("story new"), "should include new topic");
    assert!(stdout.contains("story list"), "should include list topic");
    assert!(stdout.contains("story next"), "should include next topic");
    assert!(stdout.contains("story show"), "should include show topic");
    assert!(stdout.contains("story move"), "should include move topic");
    assert!(
        stdout.contains("story decompose"),
        "should include decompose topic"
    );
    assert!(stdout.contains("story graph"), "should include graph topic");
    assert!(
        stdout.contains("story doctor"),
        "should include doctor topic"
    );
    assert!(
        stdout.contains("story scaffold"),
        "should include scaffold topic"
    );
}

#[test]
fn help_all_is_much_longer_than_compact() {
    let dir = tempdir().unwrap();
    let compact_output = story(dir.path())
        .args(["help", "--compact"])
        .assert()
        .success();
    let compact = String::from_utf8(compact_output.get_output().stdout.clone()).unwrap();

    let all_output = story(dir.path())
        .args(["help", "--all"])
        .assert()
        .success();
    let all = String::from_utf8(all_output.get_output().stdout.clone()).unwrap();

    let compact_lines = compact.lines().count();
    let all_lines = all.lines().count();

    assert!(
        all_lines > compact_lines * 3,
        "--all ({all_lines} lines) should be much longer than --compact ({compact_lines} lines)"
    );
}

#[test]
fn help_all_does_not_mention_mcp_after_removal() {
    let dir = tempdir().unwrap();
    let output = story(dir.path())
        .args(["help", "--all"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    assert!(
        !stdout.contains("mcp-config"),
        "--all help should not include removed mcp-config topic"
    );
}

// ============================================================
// Existing help behavior preserved
// ============================================================

#[test]
fn help_with_topic_still_works() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["help", "init"])
        .assert()
        .success()
        .stdout(predicate::str::contains("story init"));
}

#[test]
fn help_with_unknown_topic_still_fails() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["help", "nonexistent-command"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("unknown help topic"));
}

#[test]
fn help_no_args_lists_topics() {
    let dir = tempdir().unwrap();
    // `story help` with no args should show the main help text
    story(dir.path())
        .args(["help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("story"));
}

// ============================================================
// --compact with --json flag
// ============================================================

#[test]
fn help_compact_with_json_flag_produces_json() {
    let dir = tempdir().unwrap();
    let output = story(dir.path())
        .args(["--json", "help", "--compact"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    // JSON output should be valid JSON
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("--compact --json should produce valid JSON");
    assert_eq!(
        parsed["result"], "ok",
        "JSON response should have result: ok"
    );
    // The message field should contain the compact help content
    let message = parsed["message"].as_str().unwrap_or("");
    assert!(
        message.contains("new"),
        "JSON compact help message should contain key commands"
    );
}

// ============================================================
// --all with --json flag
// ============================================================

#[test]
fn help_all_with_json_flag_produces_json() {
    let dir = tempdir().unwrap();
    let output = story(dir.path())
        .args(["--json", "help", "--all"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    // JSON output should be valid JSON
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("--all --json should produce valid JSON");
    assert_eq!(
        parsed["result"], "ok",
        "JSON response should have result: ok"
    );
    // The message field should contain the full help content
    let message = parsed["message"].as_str().unwrap_or("");
    assert!(
        message.contains("story init"),
        "JSON --all help message should contain init topic"
    );
    assert!(
        message.contains("story decompose"),
        "JSON --all help message should contain decompose topic"
    );
}

// ============================================================
// Compact reference size contract (integration-level)
// ============================================================

#[test]
fn help_compact_output_under_3000_chars() {
    let dir = tempdir().unwrap();
    let output = story(dir.path())
        .args(["help", "--compact"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    assert!(
        stdout.len() < 3000,
        "compact help output must stay under 3000 chars for LLM context windows, got {} chars",
        stdout.len()
    );
}

#[test]
fn help_compact_does_not_reference_mcp() {
    let dir = tempdir().unwrap();
    let output = story(dir.path())
        .args(["help", "--compact"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    assert!(
        !stdout.contains("MCP"),
        "compact help must not reference MCP"
    );
    assert!(
        !stdout.contains("mcp"),
        "compact help must not reference mcp"
    );
}

// ============================================================
// Edge cases
// ============================================================

#[test]
fn help_compact_and_all_are_mutually_exclusive_or_last_wins() {
    // If both --compact and --all are given, the implementation should
    // either reject it or pick one deterministically (no crash).
    let dir = tempdir().unwrap();
    let result = story(dir.path())
        .args(["help", "--compact", "--all"])
        .output()
        .unwrap();
    // Should not crash (exit code 0 or a usage error, not a panic)
    assert!(
        result.status.code().unwrap_or(1) <= 2,
        "providing both --compact and --all should not crash"
    );
}

#[test]
fn help_compact_with_topic_arg_prefers_topic_or_errors() {
    // `story help --compact init` — should the topic or --compact win?
    // Either behavior is fine; this test just ensures no crash/panic.
    let dir = tempdir().unwrap();
    let result = story(dir.path())
        .args(["help", "--compact", "init"])
        .output()
        .unwrap();
    assert!(
        result.status.code().unwrap_or(1) <= 2,
        "help --compact with a topic argument should not crash"
    );
}
