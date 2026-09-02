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

#[test]
fn scaffold_agents_md_contains_workflow_commands() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["scaffold", "agents-md"])
        .assert()
        .success()
        .stdout(predicate::str::contains("story next"))
        .stdout(predicate::str::contains("story load-context"));
}

#[test]
fn scaffold_agents_md_uses_project_prefix() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "API"])
        .assert()
        .success();
    story(dir.path())
        .args(["scaffold", "agents-md"])
        .assert()
        .success()
        .stdout(predicate::str::contains("API-<n>"));
}

#[test]
fn scaffold_agents_md_references_help_compact() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["scaffold", "agents-md"])
        .assert()
        .success()
        .stdout(predicate::str::contains("story help --compact"));
}

#[test]
fn scaffold_agents_owns_epic_and_reserved_label_guidance_while_claude_points_to_it() {
    let dir = scratch_dir();
    let agents = story(dir.path())
        .args(["scaffold", "agents-md"])
        .output()
        .expect("scaffolding AGENTS.md");
    assert!(agents.status.success());
    let agents = String::from_utf8(agents.stdout).expect("AGENTS.md scaffold is UTF-8");
    let agents_words = agents.split_whitespace().collect::<Vec<_>>().join(" ");
    for required in [
        "Typed epics are folders",
        "carry no executable steps of their own",
        "`no-auto`",
        "`human-only`",
    ] {
        assert!(
            agents_words.contains(required),
            "the authoritative AGENTS.md scaffold must contain {required:?}"
        );
    }

    let claude = story(dir.path())
        .args(["scaffold", "claude-md"])
        .output()
        .expect("scaffolding CLAUDE.md");
    assert!(claude.status.success());
    let claude = String::from_utf8(claude.stdout).expect("CLAUDE.md scaffold is UTF-8");
    assert!(claude.contains("AGENTS.md"));
    for duplicated_policy in ["Typed epics are folders", "`no-auto`", "`human-only`"] {
        assert!(
            !claude.contains(duplicated_policy),
            "CLAUDE.md must inherit {duplicated_policy:?} through its AGENTS.md pointer, not \
             maintain a second copy"
        );
    }
}

#[test]
fn scaffold_agents_md_no_mcp_references() {
    let dir = scratch_dir();
    let output = story(dir.path())
        .args(["scaffold", "agents-md"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.contains("MCP"),
        "agents-md scaffold must not contain 'MCP'"
    );
    assert!(
        !stdout.contains("mcp"),
        "agents-md scaffold must not contain 'mcp'"
    );
}

#[test]
fn scaffold_cursor_rules_contains_storyhook() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["scaffold", "cursor-rules"])
        .assert()
        .success()
        .stdout(predicate::str::contains("storyhook"));
}

#[test]
fn scaffold_cursor_rules_references_help_command() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["scaffold", "cursor-rules"])
        .assert()
        .success()
        .stdout(predicate::str::contains("story help <command>"));
}

#[test]
fn scaffold_cursor_rules_no_mcp_references() {
    let dir = scratch_dir();
    let output = story(dir.path())
        .args(["scaffold", "cursor-rules"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.contains("MCP"),
        "cursor-rules scaffold must not contain 'MCP'"
    );
    assert!(
        !stdout.contains("mcp"),
        "cursor-rules scaffold must not contain 'mcp'"
    );
}

#[test]
fn scaffold_claude_md_is_short_pointer() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["scaffold", "claude-md"])
        .assert()
        .success()
        .stdout(predicate::str::contains("AGENTS.md"))
        .stdout(predicate::str::contains("story load-context"));
}

#[test]
fn scaffold_claude_md_no_mcp_references() {
    let dir = scratch_dir();
    let output = story(dir.path())
        .args(["scaffold", "claude-md"])
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        !stdout.contains("MCP"),
        "claude-md scaffold must not contain 'MCP'"
    );
    assert!(
        !stdout.contains("mcp"),
        "claude-md scaffold must not contain 'mcp'"
    );
}

#[test]
fn init_generates_agents_md_carrying_this_projects_prefix() {
    // The full agent instructions used to be scaffolded into
    // `.storyhook/CLAUDE.md` beside the story data. They are in `AGENTS.md`
    // now — one scaffold artifact at the repository root, which is where a
    // fresh agent looks and which survives the directory being retired.
    let dir = scratch_dir();
    story(dir.path())
        .args(["project", "new", "--prefix", "WEB"])
        .assert()
        .success();
    let agents_md = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
    assert!(agents_md.contains("WEB-<n>"));
    assert!(agents_md.contains("story load-context"));
    assert!(agents_md.contains("## Planning"));
}

#[test]
fn scaffold_invalid_kind_returns_error() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["scaffold", "invalid-kind"])
        .assert()
        .code(2);
}
