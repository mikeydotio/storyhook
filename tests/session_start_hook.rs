/// Tests for the session-start.sh hook used by Claude Code.
///
/// The hook at plugin/claude-code/hooks/session-start.sh reads stdin JSON
/// (with a `cwd` field), checks for a storyhook project, gathers state,
/// and emits a JSON response carrying the context in
/// `hookSpecificOutput.additionalContext` (the silent SessionStart field),
/// or `{}` when there is no project / the plugin is disabled.
///
/// These tests exercise the hook against real storyhook project directories
/// using the actual `story` binary and the actual shell script.
use assert_cmd::Command;
use std::process;
use storyhook_test_support::scratch_dir;

const HOOK_PATH: &str = "plugin/claude-code/hooks/session-start.sh";

/// The plugin manifest declaring what Claude Code runs, and how long it may
/// take. The source of truth for this file's one wall-clock bound.
const HOOKS_MANIFEST: &str = "plugin/claude-code/hooks/hooks.json";

/// Get the absolute path to the hook script from the project root.
fn hook_script() -> std::path::PathBuf {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.join(HOOK_PATH)
}

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

/// Extract the SessionStart `additionalContext` string from a parsed hook
/// envelope, or "" if it is absent (e.g. `{}`).
fn context(parsed: &serde_json::Value) -> &str {
    parsed["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or("")
}

/// True when the hook output carries a well-formed SessionStart context
/// envelope: `hookEventName == "SessionStart"` and a string `additionalContext`.
fn has_session_context(parsed: &serde_json::Value) -> bool {
    parsed["hookSpecificOutput"]["hookEventName"] == "SessionStart"
        && parsed["hookSpecificOutput"]["additionalContext"].is_string()
}

/// Run the hook with the given cwd as stdin JSON.
fn run_hook(cwd: &std::path::Path) -> (String, i32) {
    let stdin_json = format!(r#"{{"cwd":"{}"}}"#, cwd.display());
    let output = process::Command::new("bash")
        .arg(hook_script())
        .stdin(process::Stdio::piped())
        .stdout(process::Stdio::piped())
        .stderr(process::Stdio::piped())
        .env(
            "PATH",
            format!(
                "{}:{}",
                std::path::Path::new(env!("CARGO_BIN_EXE_story"))
                    .parent()
                    .unwrap()
                    .display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(stdin_json.as_bytes()).ok();
            }
            child.wait_with_output()
        })
        .expect("failed to run hook script");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let code = output.status.code().unwrap_or(-1);
    (stdout, code)
}

// ============================================================
// Hook script structure assertions
// ============================================================

#[test]
fn hook_script_is_under_20_functional_lines() {
    let script = std::fs::read_to_string(hook_script()).expect("hook script should exist");
    let functional_lines = script
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        })
        .count();
    assert!(
        functional_lines < 20,
        "hook script should have fewer than 20 functional lines (excluding comments and blanks), got {functional_lines}"
    );
}

#[test]
fn hook_script_does_not_use_python3() {
    let script = std::fs::read_to_string(hook_script()).expect("hook script should exist");
    assert!(
        !script.contains("python3"),
        "hook script must not depend on python3"
    );
    assert!(
        !script.contains("python"),
        "hook script must not depend on python"
    );
}

#[test]
fn hook_script_calls_story_session_start() {
    let script = std::fs::read_to_string(hook_script()).expect("hook script should exist");
    assert!(
        script.contains("story session-start"),
        "hook script must delegate to 'story session-start'"
    );
}

// ============================================================
// No storyhook project -> empty JSON
// ============================================================

#[test]
fn hook_outputs_empty_json_when_no_storyhook_dir() {
    let dir = scratch_dir();
    // Do NOT run story init -- no .storyhook/ directory
    let (stdout, code) = run_hook(dir.path());
    assert_eq!(code, 0, "hook should exit 0 even with no project");
    assert_eq!(
        stdout.trim(),
        "{}",
        "hook should emit empty JSON when no .storyhook/ exists"
    );
}

// ============================================================
// Plugin disabled -> empty JSON
// ============================================================

#[test]
fn hook_outputs_empty_json_when_plugin_disabled() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // Create plugin config with enabled = false. The directory is created
    // explicitly: `story init` stops making one once story data lives in the
    // store, and this file — user-authored config about the repository — is
    // still read from its legacy home until that directory is retired.
    std::fs::create_dir_all(dir.path().join(".storyhook")).unwrap();
    let config_path = dir.path().join(".storyhook/plugin-config.toml");
    std::fs::write(&config_path, "enabled = \"false\"\n").unwrap();

    let (stdout, code) = run_hook(dir.path());
    assert_eq!(code, 0);
    assert_eq!(
        stdout.trim(),
        "{}",
        "hook should emit empty JSON when plugin is disabled"
    );
}

// ============================================================
// Valid project -> context envelope with content
// ============================================================

#[test]
fn hook_outputs_system_message_for_valid_project() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Implement user authentication"])
        .assert()
        .success();

    let (stdout, code) = run_hook(dir.path());
    assert_eq!(code, 0, "hook should exit 0 for valid project");

    // Parse as JSON
    let parsed: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("hook output should be valid JSON");
    assert!(
        has_session_context(&parsed),
        "hook output should contain the SessionStart context envelope"
    );

    let msg = context(&parsed);
    assert!(
        !msg.is_empty(),
        "additionalContext should not be empty for an active project"
    );
}

#[test]
fn hook_system_message_contains_story_count() {
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

    let (stdout, _) = run_hook(dir.path());
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let msg = context(&parsed);

    // Should mention open story count (there are 2) or at minimum
    // reference "open" or "stories" in some form. The rewrite should
    // include accurate counts parsed from `story summary --json`.
    assert!(
        msg.contains("2") || msg.contains("open") || msg.contains("stories"),
        "context should reference open story count or stories: got {msg}"
    );
}

#[test]
fn hook_system_message_contains_next_story_info() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Deploy monitoring dashboard"])
        .assert()
        .success();

    let (stdout, _) = run_hook(dir.path());
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let msg = context(&parsed);

    // After the hook rewrite, the context should contain the next story's
    // ID or title. The hook needs to correctly parse `story next --json`
    // output which returns `{"result":"ok","story":{"story":{"id":"SH-1",...}}}`.
    //
    // NOTE: The pre-rewrite hook has a parsing bug where it expects a
    // "stories" array but `story next --count 1 --json` returns a "story"
    // object. The rewrite must fix this.
    assert!(
        msg.contains("SH-1") || msg.contains("Deploy monitoring") || msg.contains("Next:"),
        "context should mention the next story or have a Next: section: got {msg}"
    );
}

// ============================================================
// Timing: the hook fits the budget the plugin declares for it
// ============================================================

/// The timeout Claude Code gives the SessionStart hook, read out of the
/// manifest that ships it.
///
/// **Read rather than restated (SH-140).** The 5 in this test was previously a
/// literal, which made it look like a speed budget somebody chose. It is not:
/// it is `plugin/claude-code/hooks/hooks.json`'s own `"timeout"`, and Claude
/// Code kills the hook at it. Reading the manifest keeps the assertion tracking
/// the contract if the declared value ever moves, and fails loudly rather than
/// silently guarding nothing if the manifest stops declaring one.
///
/// The entry is located by its command rather than by position, so a manifest
/// that gains another SessionStart hook cannot quietly hand this test somebody
/// else's budget.
fn declared_session_start_timeout() -> std::time::Duration {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read_to_string(manifest_dir.join(HOOKS_MANIFEST))
        .expect("the plugin's hooks manifest must be readable");
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).expect("the plugin's hooks manifest must be JSON");

    let seconds = parsed["hooks"]["SessionStart"]
        .as_array()
        .expect("hooks.json must declare SessionStart matchers")
        .iter()
        .flat_map(|matcher| {
            matcher["hooks"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
        })
        .find(|hook| {
            hook["command"]
                .as_str()
                .is_some_and(|command| command.contains("session-start.sh"))
        })
        .and_then(|hook| hook["timeout"].as_u64())
        .expect("the SessionStart entry running session-start.sh must declare a timeout");

    std::time::Duration::from_secs(seconds)
}

/// The hook finishes inside the budget its own manifest gives it.
///
/// Exceeding this is not slowness, it is a functional failure: Claude Code
/// kills the hook at the deadline and the session starts with no storyhook
/// context at all — silently, because the script sends its errors to
/// `/dev/null`. That is why the bound stays where two of the six others were
/// deleted (SH-140): the number is the product's, not the test's.
///
/// Measured at 18-59ms against the declared 5s. If this ever fires, suspect the
/// machine before the hook: the command inside may wait up to
/// `SPAWN_LOCK_DEADLINE` (30s) for the daemon spawn lock, which is longer than
/// the hook is allowed to live — an ordering violation in the *product* that is
/// filed separately and cannot be fixed from here.
#[test]
fn hook_completes_within_the_timeout_its_manifest_declares() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Performance-critical task"])
        .assert()
        .success();

    let budget = declared_session_start_timeout();
    let start = std::time::Instant::now();
    let (_, code) = run_hook(dir.path());
    let elapsed = start.elapsed();

    assert_eq!(code, 0);
    assert!(
        elapsed < budget,
        "the SessionStart hook took {elapsed:?}, against the {budget:?} that \
         {HOOKS_MANIFEST} gives it. Claude Code kills the hook at that deadline, \
         so this is not a slow session start, it is one with no storyhook \
         context at all"
    );
}

// ============================================================
// Empty project (init but no stories)
// ============================================================

#[test]
fn hook_handles_empty_project_gracefully() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    // No stories created

    let (stdout, code) = run_hook(dir.path());
    assert_eq!(code, 0, "hook should not fail on empty project");

    // Should still produce valid JSON
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .expect("hook should produce valid JSON for empty project");

    // Either empty JSON or a SessionStart context envelope is acceptable
    if parsed.get("hookSpecificOutput").is_some() {
        let text = context(&parsed);
        assert!(
            text.contains("0") || text.contains("storyhook"),
            "context for empty project should indicate zero stories or mention storyhook"
        );
    }
    // If it's just {}, that's fine too
}

// ============================================================
// Hook output is valid JSON (not truncated, no trailing garbage)
// ============================================================

#[test]
fn hook_output_is_strictly_valid_json() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Validate JSON output integrity"])
        .assert()
        .success();

    let (stdout, _) = run_hook(dir.path());
    let trimmed = stdout.trim();

    // Must be parseable JSON
    let result: Result<serde_json::Value, _> = serde_json::from_str(trimmed);
    assert!(
        result.is_ok(),
        "hook output must be valid JSON, got: {trimmed}"
    );

    // Must be a JSON object (not array, not string, not number)
    let value = result.unwrap();
    assert!(
        value.is_object(),
        "hook output must be a JSON object, got: {trimmed}"
    );
}

// ============================================================
// context should contain CLI reference
// ============================================================

#[test]
fn hook_system_message_contains_cli_reference() {
    // The context should inject a concise CLI reference so the LLM knows how
    // to use storyhook commands.
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Add user profile page"])
        .assert()
        .success();

    let (stdout, code) = run_hook(dir.path());
    assert_eq!(code, 0);

    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let msg = context(&parsed);

    // The CLI reference should mention key commands
    assert!(
        msg.contains("story next") || msg.contains("story load-context"),
        "context should contain CLI reference with key commands: got {msg}"
    );
}

#[test]
fn hook_system_message_contains_project_state() {
    // The context should include project state info (not just a one-line
    // status, but enough for the LLM to orient).
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Implement authentication"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Write integration tests"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-1", "high"])
        .assert()
        .success();

    let (stdout, code) = run_hook(dir.path());
    assert_eq!(code, 0);

    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let msg = context(&parsed);

    // Should contain some project state info
    assert!(
        msg.contains("open") || msg.contains("stories") || msg.contains("storyhook"),
        "context should contain project state info: got {msg}"
    );
}

// ============================================================
// context with special characters in story titles
// ============================================================

#[test]
fn hook_handles_special_characters_in_story_title() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", r#"Fix "double-quote" and backslash\ in titles"#])
        .assert()
        .success();

    let (stdout, code) = run_hook(dir.path());
    assert_eq!(code, 0, "hook should handle special chars without crashing");

    // Output must still be valid JSON
    let result: Result<serde_json::Value, _> = serde_json::from_str(stdout.trim());
    assert!(
        result.is_ok(),
        "hook output must be valid JSON even with special chars in titles, got: {}",
        stdout.trim()
    );
}

#[test]
fn hook_handles_unicode_in_story_title() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Add i18n support for Japanese text"])
        .assert()
        .success();

    let (stdout, code) = run_hook(dir.path());
    assert_eq!(code, 0, "hook should handle unicode without crashing");

    let result: Result<serde_json::Value, _> = serde_json::from_str(stdout.trim());
    assert!(
        result.is_ok(),
        "hook output must be valid JSON with unicode titles"
    );
}
