//! Fix cycle 5 regression tests for SH-34 through SH-39.
//!
//! These stories are all text/config changes. The tests verify the
//! *before state* (detecting each bug) and will pass once the fix is applied.
//! Each test exercises real CLI output or real file content — no mocks.

use assert_cmd::Command;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

// ============================================================
// SH-34: HELP_TEXT missing --compact and --all flags
// The usage line should show the optional args and flags:
//   story help [<command>] [--compact] [--all]
// ============================================================

#[test]
fn sh34_help_text_shows_compact_flag_in_usage() {
    let output = Command::cargo_bin("story")
        .unwrap()
        .arg("--help")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();

    // Find the line that starts with "story help"
    let help_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("story help"))
        .expect("--help output should contain a 'story help' usage line");

    assert!(
        help_line.contains("--compact"),
        "help usage line should document --compact flag, got: {help_line}"
    );
}

#[test]
fn sh34_help_text_shows_all_flag_in_usage() {
    let output = Command::cargo_bin("story")
        .unwrap()
        .arg("--help")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();

    let help_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("story help"))
        .expect("--help output should contain a 'story help' usage line");

    assert!(
        help_line.contains("--all"),
        "help usage line should document --all flag, got: {help_line}"
    );
}

#[test]
fn sh34_help_text_shows_command_as_optional() {
    let output = Command::cargo_bin("story")
        .unwrap()
        .arg("--help")
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();

    let help_line = stdout
        .lines()
        .find(|l| l.trim_start().starts_with("story help"))
        .expect("--help output should contain a 'story help' usage line");

    // The <command> arg should be shown as optional with square brackets
    assert!(
        help_line.contains("[<command>]") || help_line.contains("[command]"),
        "help usage should show command as optional (square brackets), got: {help_line}"
    );
}

// ============================================================
// SH-35: Ghost command --tree in scaffold init template
// The generated CLAUDE.md should not reference `story graph --tree`
// because --tree is not a valid flag on the graph command.
// ============================================================

#[test]
fn sh35_init_claude_md_does_not_reference_graph_tree() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["init", "--prefix", "TST"])
        .assert()
        .success();

    let claude_md = std::fs::read_to_string(dir.path().join(".storyhook/CLAUDE.md")).unwrap();
    assert!(
        !claude_md.contains("--tree"),
        ".storyhook/CLAUDE.md should not reference nonexistent --tree flag, \
         but found it in the generated template"
    );
}

#[test]
fn sh35_scaffold_claude_md_does_not_reference_graph_tree() {
    let dir = tempdir().unwrap();
    let output = story(dir.path())
        .args(["scaffold", "claude-md"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();

    assert!(
        !stdout.contains("--tree"),
        "scaffold claude-md should not reference nonexistent --tree flag"
    );
}

#[test]
fn sh35_init_claude_md_graph_section_only_has_valid_flags() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["init", "--prefix", "TST"])
        .assert()
        .success();

    let claude_md = std::fs::read_to_string(dir.path().join(".storyhook/CLAUDE.md")).unwrap();

    // Find lines containing "story graph" and verify they only use known flags
    let known_graph_flags = ["--critical-path", "--blocked-by", "--parallel-groups"];
    for line in claude_md.lines() {
        if line.contains("story graph") && line.contains("--") {
            let has_known_flag = known_graph_flags.iter().any(|f| line.contains(f));
            assert!(
                has_known_flag,
                "graph command line contains unknown flag: {line}"
            );
        }
    }
}

// ============================================================
// SH-36: VERSION file vs Cargo.toml version drift
// VERSION says v0.12.0, Cargo.toml should match (without v prefix).
// .semver/config.yaml should list Cargo.toml as a tracked file.
// ============================================================

#[test]
fn sh36_cargo_toml_version_matches_version_file() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let version_content =
        std::fs::read_to_string(manifest_dir.join("VERSION")).expect("VERSION file should exist");
    let version_str = version_content.trim().trim_start_matches('v');

    let cargo_toml =
        std::fs::read_to_string(manifest_dir.join("Cargo.toml")).expect("Cargo.toml should exist");
    let cargo_parsed: toml::Value =
        toml::from_str(&cargo_toml).expect("Cargo.toml should be valid TOML");
    let cargo_version = cargo_parsed["package"]["version"]
        .as_str()
        .expect("Cargo.toml should have package.version");

    assert_eq!(
        cargo_version, version_str,
        "Cargo.toml version ({cargo_version}) must match VERSION file ({version_str})"
    );
}

#[test]
fn sh36_pre_bump_hook_syncs_cargo_toml() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let hook_path = manifest_dir.join(".semver/hooks/pre-bump/sync-cargo-toml.sh");

    assert!(
        hook_path.exists(),
        ".semver/hooks/pre-bump/sync-cargo-toml.sh should exist"
    );
    // The sync must be a *pre-bump* hook: the plugin's release commit is created
    // with no pathspec, so only a hook that edits and stages Cargo.toml/Cargo.lock
    // *before* the commit gets them in it. A post-bump hook leaves them dirty.
    assert!(
        !manifest_dir
            .join(".semver/hooks/post-bump/sync-cargo-toml.sh")
            .exists(),
        "sync hook must be pre-bump, not post-bump (post-bump leaves the tree dirty)"
    );

    let hook = std::fs::read_to_string(&hook_path).expect("should be able to read hook script");

    assert!(
        hook.contains("NEW_VERSION#v"),
        "hook should strip v prefix from $NEW_VERSION"
    );
    assert!(hook.contains("Cargo.toml"), "hook should update Cargo.toml");
    assert!(
        hook.contains("Cargo.lock"),
        "hook should also sync Cargo.lock so the release commit stays clean"
    );
    assert!(
        hook.contains("git add"),
        "hook must stage its changes so they land in the pathspec-less release commit"
    );
}

// ============================================================
// SH-38: No CHANGELOG entry for MCP removal
// The CHANGELOG mentions MCP tools being *added* in older versions
// (v0.6.0, v0.9.0, v0.10.0), but never documents their *removal*.
// There should be a "Removed" section explicitly calling out the
// MCP removal and pointing users to session hooks / plugin install.
// ============================================================

#[test]
fn sh38_changelog_has_mcp_removal_entry() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let changelog = std::fs::read_to_string(manifest_dir.join("CHANGELOG.md"))
        .expect("CHANGELOG.md should exist");

    // The existing changelog mentions MCP only in "Added" and "Changed"
    // sections of older versions (v0.6.0 through v0.10.0). There is no
    // entry documenting that MCP was *removed*. We need a line that
    // mentions MCP in a removal context, not just an addition context.
    //
    // Strategy: search for any single line (or pair of adjacent lines)
    // that contains both "MCP" (or "--mcp" or "mcp-config") AND removal
    // language ("removed", "removal", "dropped", "retired", "eliminated").
    let removal_words = ["removed", "removal", "dropped", "retired", "eliminated"];
    let mcp_words = ["MCP", "--mcp", "mcp-config", "mcp server", "mcp tool"];

    let has_mcp_removal_line = changelog.lines().any(|line| {
        let lower = line.to_lowercase();
        let has_removal = removal_words.iter().any(|w| lower.contains(w));
        let has_mcp = mcp_words.iter().any(|w| lower.contains(&w.to_lowercase()));
        has_removal && has_mcp
    });

    assert!(
        has_mcp_removal_line,
        "CHANGELOG.md should contain a line documenting MCP removal \
         (e.g., 'Removed MCP server and all MCP tools'). \
         Currently MCP is only mentioned in Added/Changed sections."
    );
}

#[test]
fn sh38_changelog_mcp_removal_mentions_migration_path() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let changelog = std::fs::read_to_string(manifest_dir.join("CHANGELOG.md"))
        .expect("CHANGELOG.md should exist");

    // The removal entry should tell users what replaced MCP.
    // We look for "story plugin install" appearing in the same version
    // block as MCP removal language, or on a nearby line.
    // Simplest check: somewhere in the changelog, a "Removed" section
    // or a removal line should mention the migration path.
    let has_migration_note = changelog.contains("story plugin install claude-code")
        || (changelog.contains("### Removed") && changelog.contains("session hook"));

    assert!(
        has_migration_note,
        "CHANGELOG.md MCP removal entry should mention the migration path \
         (story plugin install claude-code, session hooks). \
         Currently no such migration guidance exists in the changelog."
    );
}

// ============================================================
// SH-39: Stale skill invocation in plugin install message
// The install message should mention `story load-context`
// as an alternative to the /storyhook:context skill invocation.
//
// NOTE: `plugin install` requires bundled plugin files that only
// exist in a full build/repo checkout, not alongside the test binary.
// We test the source string directly to verify the fix.
// ============================================================

#[test]
fn sh39_plugin_source_contains_load_context_hint() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let plugin_rs = std::fs::read_to_string(manifest_dir.join("src/plugin.rs"))
        .expect("src/plugin.rs should exist");

    // The install function builds a message string. After the fix,
    // it should contain "story load-context" somewhere in the message.
    assert!(
        plugin_rs.contains("story load-context"),
        "src/plugin.rs install message should mention 'story load-context' \
         as a direct alternative to /storyhook:context"
    );
}

#[test]
fn sh39_plugin_source_contains_or_run_directly_phrasing() {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let plugin_rs = std::fs::read_to_string(manifest_dir.join("src/plugin.rs"))
        .expect("src/plugin.rs should exist");

    // The fix adds "(or run story load-context directly)" to the install message.
    assert!(
        plugin_rs.contains("or run story load-context directly")
            || plugin_rs.contains("run story load-context directly"),
        "src/plugin.rs should include the '(or run story load-context directly)' hint"
    );
}
