/// Tests for the `story session-start` CLI command.
///
/// This command is designed for use by editor plugins and shell hooks.
/// It outputs raw JSON: the Claude Code SessionStart envelope
/// `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"..."}}`
/// when a project exists and the plugin is enabled, or `{}` when there is no
/// project or the plugin is disabled. `additionalContext` is injected silently
/// into the model's context (it is NOT rendered to the user, unlike the
/// previously-used `systemMessage` field).
use assert_cmd::Command;
use storyhook_test_support::{TestEnv, scratch_dir};

/// Every `story` this file runs is the one THIS build produced, in the shared
/// test environment.
///
/// `shared()`, deliberately, and NOT for the two tests in
/// `degrades_under_contention` below: those hold an exclusive flock on the
/// daemon spawn lock, and holding it from a shared environment would block every
/// other test in this binary behind `SPAWN_LOCK_DEADLINE`. They keep
/// `TestEnv::isolated()`, which is what makes holding that lock safe at all.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

/// The plugin-config file's path, with its parent directory created.
///
/// `plugin-config.toml` is *user-authored config about this repository*, so it
/// stays in the repository across the flip — its new home is the `[plugin]`
/// table of the committed pointer file, and the old one keeps being read until
/// the legacy directory is retired. These tests deliberately exercise the
/// **legacy** path, which is the one both storage models still answer; the
/// pointer-file home is covered by `tests/service_session.rs`.
///
/// The directory has to be created here because `story init` stops making one
/// the moment story data lives in the store.
fn plugin_config_path(root: &std::path::Path) -> std::path::PathBuf {
    std::fs::create_dir_all(root.join(".storyhook")).expect("creating the config directory");
    root.join(".storyhook/plugin-config.toml")
}

/// Extract the SessionStart `additionalContext` string from a parsed
/// `story session-start` envelope, or "" if it is absent (e.g. `{}`).
fn context(parsed: &serde_json::Value) -> &str {
    parsed["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or("")
}

/// True when the output carries a well-formed SessionStart context envelope:
/// `hookEventName == "SessionStart"` and a string `additionalContext`.
fn has_session_context(parsed: &serde_json::Value) -> bool {
    parsed["hookSpecificOutput"]["hookEventName"] == "SessionStart"
        && parsed["hookSpecificOutput"]["additionalContext"].is_string()
}

// ============================================================
// No .storyhook/ directory -> {}
// ============================================================

#[test]
fn session_start_no_project_outputs_empty_json() {
    let dir = scratch_dir();
    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success(), "should exit 0 with no project");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "{}",
        "should output empty JSON when no .storyhook/ exists"
    );
}

// ============================================================
// Plugin disabled -> {}
// ============================================================

#[test]
fn session_start_plugin_disabled_outputs_empty_json() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // Disable the plugin
    let config_path = plugin_config_path(dir.path());
    std::fs::write(
        &config_path,
        "[plugin]\nenabled = false\ntracking = \"normal\"\n",
    )
    .unwrap();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "{}",
        "should output empty JSON when plugin is disabled"
    );
}

#[test]
fn session_start_plugin_disabled_string_value_outputs_empty_json() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // Disable the plugin with string "false" value
    let config_path = plugin_config_path(dir.path());
    std::fs::write(&config_path, "enabled = \"false\"\n").unwrap();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "{}",
        "should output empty JSON when plugin disabled via string"
    );
}

// ============================================================
// Valid project with stories -> SessionStart context envelope
// ============================================================

#[test]
fn session_start_valid_project_outputs_system_message() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
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
        has_session_context(&parsed),
        "output should contain the SessionStart context envelope"
    );

    let msg = context(&parsed);
    assert!(!msg.is_empty(), "additionalContext should not be empty");
}

// ============================================================
// Regression: context ships via additionalContext, NOT systemMessage.
//
// The session-start context is meant to prime the model, not to be shown to
// the user. `systemMessage` is Claude Code's user-visible field and some
// versions render it as a visible block at every session start / clear. The
// context must instead ride in hookSpecificOutput.additionalContext, which is
// injected silently. This guards against regressing to the visible field.
// ============================================================

#[test]
fn session_start_uses_additional_context_not_system_message() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Prime the model silently"])
        .assert()
        .success();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();

    // Must NOT use the user-visible systemMessage field, anywhere.
    assert!(
        parsed.get("systemMessage").is_none(),
        "session-start must not emit a top-level systemMessage (it is user-visible), got: {}",
        stdout.trim()
    );
    assert!(
        parsed["hookSpecificOutput"].get("systemMessage").is_none(),
        "session-start must not nest a systemMessage inside hookSpecificOutput, got: {}",
        stdout.trim()
    );

    // Must use the silent SessionStart context envelope with the correct event.
    assert_eq!(
        parsed["hookSpecificOutput"]["hookEventName"],
        "SessionStart",
        "hookEventName must be SessionStart, got: {}",
        stdout.trim()
    );
    assert!(
        context(&parsed).contains("storyhook"),
        "additionalContext should carry the CLI reference, got: {}",
        stdout.trim()
    );
}

// ============================================================
// additionalContext contains CLI reference
// ============================================================

#[test]
fn session_start_contains_cli_reference() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
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
    let msg = context(&parsed);

    // Verify key commands from the compact reference appear
    assert!(
        msg.contains("story next"),
        "should contain 'story next' command"
    );
    assert!(
        msg.contains("story load-context"),
        "should contain 'story load-context' command"
    );
    assert!(
        msg.contains("story new"),
        "should contain 'story new' command"
    );
    assert!(
        msg.contains("story list"),
        "should contain 'story list' command"
    );
    assert!(
        msg.contains("LIFECYCLE"),
        "should contain LIFECYCLE section header"
    );
}

// ============================================================
// additionalContext contains project state
// ============================================================

#[test]
fn session_start_contains_project_state() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
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
    let msg = context(&parsed);

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
// additionalContext contains next story info
// ============================================================

#[test]
fn session_start_contains_next_story_info() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
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
    let msg = context(&parsed);

    assert!(
        msg.contains("SH-1") && msg.contains("Deploy monitoring dashboard"),
        "should contain next story ID and title, got: {msg}"
    );
    assert!(msg.contains("Next:"), "should have Next: label, got: {msg}");
}

// ============================================================
// Empty project (0 stories) -> context with CLI ref and "0 open"
// ============================================================

#[test]
fn session_start_empty_project_zero_stories() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("should be valid JSON");

    let msg = context(&parsed);

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
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
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
        has_session_context(&parsed),
        "should have the SessionStart context envelope"
    );
}

#[test]
fn session_start_unicode_in_story_title() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
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
    assert!(
        result.is_ok(),
        "output must be valid JSON with unicode titles"
    );
}

#[test]
fn session_start_newline_in_story_title() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
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
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
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
    assert!(
        value.is_object(),
        "output must be a JSON object, got: {trimmed}"
    );
}

// ============================================================
// additionalContext under 4000 characters
// ============================================================

#[test]
fn session_start_system_message_under_4000_chars() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // Create several stories to populate project state
    for i in 0..20 {
        story(dir.path())
            .args([
                "new",
                &format!(
                    "Story number {} with a reasonably long title for testing",
                    i
                ),
            ])
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
    let msg = context(&parsed);

    assert!(
        msg.len() < 4000,
        "additionalContext should be under 4000 chars, got {} chars",
        msg.len()
    );
}

// ============================================================
// Exits zero for an ordinary project
// ============================================================

/// **This test used to hold a two-second stopwatch, and it is gone (SH-140).**
///
/// The command measured 8-9ms, so the bound was 230x the true figure and could
/// not see a regression. What it *could* see was a wait that is legitimate:
/// since SH-114 the invoker is the CLI's only door, so every `story` command
/// may take the daemon spawn lock, and `SPAWN_LOCK_DEADLINE` permits 30 seconds
/// there — fifteen times this bound — before the client gives up and says so in
/// its own words. A harness that stops first replaces a named failure with an
/// anonymous one, which is the case `SPAWN_LOCK_DEADLINE`'s own doc warns
/// against: "a client that exhausts this bound and correctly reports so must
/// not race the harness meant to outlive it".
///
/// So the only honest wall-clock figure here is one *longer* than the client's,
/// and at that length it asserts nothing the client does not already assert
/// with a better message. The exit status is what this test is for.
#[test]
fn session_start_exits_zero_for_a_project_with_one_story() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Performance-critical task"])
        .assert()
        .success();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");

    assert!(output.status.success());
}

// ============================================================
// Does not wrap in JSON envelope even with --json flag
// ============================================================

#[test]
fn session_start_ignores_json_flag() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
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

    // Both should produce the same raw SessionStart envelope, not wrapped in a
    // {"result":"ok","message":"..."} CLI envelope.
    let parsed_json: serde_json::Value = serde_json::from_str(stdout_json.trim()).unwrap();
    let parsed_plain: serde_json::Value = serde_json::from_str(stdout_plain.trim()).unwrap();

    assert!(
        has_session_context(&parsed_json),
        "with --json should still emit the SessionStart context envelope"
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
    let dir = scratch_dir();
    let output = story(dir.path())
        .args(["--json", "session-start"])
        .output()
        .expect("failed to run story session-start --json");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "{}",
        "should output empty JSON even with --json flag"
    );
}

// ============================================================
// Next story with priority info
// ============================================================

#[test]
fn session_start_next_story_shows_priority() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
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
    let msg = context(&parsed);

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
fn session_start_survives_a_project_whose_stories_have_vanished() {
    // A hook that reports an error writes that error into a model's context, so
    // `session-start` has to answer with JSON no matter what it finds. The
    // corruption it has to survive changed shape with the storage: it used to
    // be a stories *directory* replaced by a file; it is now a project whose
    // story rows and events are simply gone from under it.
    let env = storyhook_test_support::TestEnv::shared();
    let project = env.project().seed_story("About to vanish").build();
    let store = project.open_store();
    let id = project.project_id(&store);
    storyhook::store::test_support::forget_story(&store, id, project.story_no(&store, "SH-1"))
        .expect("removing the story out from under the hook");

    let out = project
        .story()
        .arg("session-start")
        .output()
        .expect("running story session-start");
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str::<serde_json::Value>(stdout.trim()).unwrap_or_else(|e| {
        panic!("session-start must produce valid JSON ({e}), got: {stdout}");
    });
}

#[test]
fn session_start_survives_a_pointer_naming_a_project_that_is_not_there() {
    // The other half: a checkout that claims to belong to a project the store
    // has never held. Under `.storyhook/` this was a missing `project.toml`;
    // it is a stale pointer file now, and it is the shape a clone of a
    // repository whose store has not been migrated actually has.
    let env = storyhook_test_support::TestEnv::shared();
    let dir = storyhook_test_support::scratch_dir();
    std::fs::write(
        dir.path().join(".storyhook.toml"),
        "schema = 1\nuuid = \"00000000-0000-4000-8000-000000000000\"\nprefix = \"SH\"\n",
    )
    .expect("writing a stale pointer");

    let out = env
        .story(dir.path())
        .arg("session-start")
        .output()
        .expect("running story session-start");
    assert_eq!(
        out.status.code(),
        Some(0),
        "a hook must not fail the session"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str::<serde_json::Value>(stdout.trim()).unwrap_or_else(|e| {
        panic!("session-start must produce valid JSON ({e}), got: {stdout}");
    });
}

// ============================================================
// Plugin config parsing edge cases
// ============================================================

/// Extra whitespace around `=` in TOML config is valid and must be handled.
/// Previously this was a bug (string matching required exact spacing);
/// now we use proper TOML parsing so any valid whitespace works.
#[test]
fn session_start_plugin_config_extra_whitespace_bug_documented() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // Write config with extra whitespace around the value
    let config_path = plugin_config_path(dir.path());
    std::fs::write(
        &config_path,
        "[plugin]\nenabled  =   false\ntracking = \"normal\"\n",
    )
    .unwrap();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "{}",
        "extra whitespace around = should not prevent detecting enabled=false"
    );
}

#[test]
fn session_start_plugin_config_enabled_true_produces_system_message() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Test enabled true config"])
        .assert()
        .success();

    // Write config with enabled = true explicitly
    let config_path = plugin_config_path(dir.path());
    std::fs::write(
        &config_path,
        "[plugin]\nenabled = true\ntracking = \"normal\"\n",
    )
    .unwrap();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(
        has_session_context(&parsed),
        "enabled = true should produce a SessionStart context envelope, got: {}",
        stdout.trim()
    );
}

#[test]
fn session_start_plugin_config_malformed_still_works() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Test malformed config"])
        .assert()
        .success();

    // Write a completely malformed plugin config (not valid TOML)
    let config_path = plugin_config_path(dir.path());
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
        has_session_context(&parsed),
        "malformed plugin config should not disable the plugin, got: {}",
        stdout.trim()
    );
}

// ============================================================
// TOML parsing robustness tests
// ============================================================

#[test]
fn session_start_plugin_config_no_space_enabled_equals_false() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // Write config with no spaces around `=`
    let config_path = plugin_config_path(dir.path());
    std::fs::write(&config_path, "enabled=false\n").unwrap();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "{}",
        "enabled=false (no spaces) should disable the plugin"
    );
}

#[test]
fn session_start_plugin_config_comments_and_extra_keys() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // Write config with comments and extra keys
    let config_path = plugin_config_path(dir.path());
    std::fs::write(
        &config_path,
        "# Plugin configuration\n\
         [plugin]\n\
         enabled = false  # disable the plugin\n\
         tracking = \"normal\"\n\
         custom_key = 42\n",
    )
    .unwrap();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "{}",
        "config with comments and extra keys should still detect enabled=false"
    );
}

#[test]
fn session_start_plugin_config_nested_table() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // Write config with [plugin] nested table format
    let config_path = plugin_config_path(dir.path());
    std::fs::write(
        &config_path,
        "[plugin]\n\
         enabled = false\n\
         tracking = \"verbose\"\n",
    )
    .unwrap();

    let output = story(dir.path())
        .arg("session-start")
        .output()
        .expect("failed to run story session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        stdout.trim(),
        "{}",
        "[plugin] nested table with enabled=false should disable the plugin"
    );
}

#[test]
fn session_start_no_plugin_config_file_produces_system_message() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Test no plugin config"])
        .assert()
        .success();

    // Ensure there's no plugin-config.toml (init shouldn't create one by default)
    let config_path = plugin_config_path(dir.path());
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
        has_session_context(&parsed),
        "missing plugin config should not disable the plugin, got: {}",
        stdout.trim()
    );
}

// ============================================================
// Output structure contract: only two valid shapes
// ============================================================

#[test]
fn session_start_output_is_one_of_two_valid_shapes() {
    // The contract: output is either exactly `{}` or a SessionStart envelope
    // `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"..."}}`.
    // No top-level `systemMessage` key should ever be present.
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
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

    // The only valid top-level key is "hookSpecificOutput" (empty {} has none).
    for key in obj.keys() {
        assert_eq!(
            key, "hookSpecificOutput",
            "session-start output should only contain 'hookSpecificOutput' key, found: '{key}'"
        );
    }

    if obj.contains_key("hookSpecificOutput") {
        assert!(
            has_session_context(&parsed),
            "hookSpecificOutput must carry hookEventName=SessionStart and a string additionalContext, got: {parsed}"
        );
    }
}

// ============================================================
// stderr is clean (no warnings or debug output)
// ============================================================

#[test]
fn session_start_stderr_is_empty() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
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
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // The compact CLI reference is ~2800 bytes. We need to push the context
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

    story(dir.path()).args(["new", &title]).assert().success();

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

    // AC: SessionStart context envelope present
    let parsed = result.unwrap();
    assert!(
        has_session_context(&parsed),
        "output should contain additionalContext after truncation"
    );
}

// ============================================================
// --quiet does NOT suppress RawJson output (SH-33)
// ============================================================

#[test]
fn session_start_quiet_flag_still_outputs_json() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Test quiet flag bypass"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["--quiet", "session-start"])
        .output()
        .expect("failed to run story --quiet session-start");
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();

    // RawJson output must NOT be suppressed by --quiet
    assert!(
        !trimmed.is_empty(),
        "story --quiet session-start should still produce output (RawJson bypasses quiet)"
    );

    let parsed: serde_json::Value =
        serde_json::from_str(trimmed).expect("output should be valid JSON");

    assert!(
        has_session_context(&parsed),
        "output should contain the SessionStart context envelope even with --quiet, got: {trimmed}"
    );

    let msg = context(&parsed);
    assert!(
        !msg.is_empty(),
        "additionalContext should not be empty with --quiet"
    );
}

// ============================================================
// SH-182: session-start degrades rather than going silent when it cannot
// answer in time.
// ============================================================
//
// `story session-start` behind a held daemon spawn lock is `main.rs`'s
// `Err(_) if silent_on_failure` branch reached for real: `lifecycle::ensure`
// cannot take the lock, `--deadline` gives up before it does, and the
// failure that reaches `session::unavailable` is exactly the shape a
// contended cold start produces in production. `TestEnv` (rather than the
// plain `story()` helper the rest of this file uses) is what makes holding
// that lock from this test process safe: it is a private, isolated
// environment, so nothing here can contend with a daemon anything else on
// the machine is running.
mod degrades_under_contention {
    use fs4::FileExt;
    use storyhook::daemon::lifecycle::SPAWN_DEADLINE;
    use storyhook_test_support::TestEnv;

    /// Opens (creating if needed) and exclusively locks `env`'s daemon spawn
    /// lock. Unlocking is the caller's job.
    fn hold_spawn_lock(env: &TestEnv) -> std::fs::File {
        let environment = env.environment();
        std::fs::create_dir_all(environment.daemon_state_dir()).expect("the daemon directory");
        let held = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(environment.daemon_spawn_lock())
            .expect("opening the spawn lock");
        held.try_lock_exclusive()
            .expect("this test must be the one holding it");
        held
    }

    /// A checkout that claims a project gets a recovery line, not silence,
    /// when `--deadline` fires: the whole point of SH-182 is that this
    /// failure is now something an agent can act on.
    #[test]
    fn a_pointer_file_present_gets_a_recovery_line_not_silence() {
        let env = TestEnv::isolated();
        let project = env.project().prefix("SH").build();
        let held = hold_spawn_lock(&env);

        // `--deadline` is the CLIENT's own bound; it is what makes this
        // command give up quickly instead of waiting out the contended spawn
        // lock. The elapsed window this test measures also has to cover
        // process spawn and exit around that wait, which the deadline itself
        // says nothing about — SPAWN_DEADLINE (SH-394) is production's own
        // named allowance for "a process coming up", so the ceiling below
        // derives from both rather than hand-picking a number beside them.
        let deadline_secs = 1u64;
        let started = std::time::Instant::now();
        let output = env
            .story(project.path())
            .args(["--deadline", &deadline_secs.to_string(), "session-start"])
            .output()
            .expect("running story --deadline session-start");
        let elapsed = started.elapsed();

        let _ = FileExt::unlock(&held);
        drop(held);

        assert!(
            output.status.success(),
            "session-start must exit 0 even when it could not load project state: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let ceiling = std::time::Duration::from_secs(deadline_secs) + SPAWN_DEADLINE;
        assert!(
            elapsed < ceiling,
            "took {elapsed:?} against a --deadline of {deadline_secs}s plus a \
             {SPAWN_DEADLINE:?} process-overhead allowance"
        );

        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value =
            serde_json::from_str(stdout.trim()).expect("output must still be valid JSON");
        assert!(
            super::has_session_context(&parsed),
            "a checkout that claims a project must get the envelope, not bare {{}}: {stdout}"
        );
        let msg = super::context(&parsed);
        assert!(
            msg.contains("story load-context"),
            "the recovery line must point at a way to retry: {msg}"
        );
    }

    /// A directory with no pointer file — no reason to believe storyhook is
    /// set up here at all — keeps the original silent contract: bare `{}`,
    /// indistinguishable from "no storyhook project", exactly as it always
    /// was before SH-182.
    #[test]
    fn no_pointer_file_stays_silent() {
        let env = TestEnv::isolated();
        let dir = storyhook_test_support::scratch_dir();
        let held = hold_spawn_lock(&env);

        let output = env
            .story(dir.path())
            .args(["--deadline", "1", "session-start"])
            .output()
            .expect("running story --deadline 1 session-start");

        let _ = FileExt::unlock(&held);
        drop(held);

        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "{}",
            "a directory claiming no project must stay silent, not gain a new failure mode"
        );
    }
}
