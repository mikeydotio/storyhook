//! `story init` — what a freshly initialized project has.
//!
//! This file used to assert the *shape of a directory*: `project.toml`,
//! `states.toml`, `types.toml`, `open/stories/`, `archive/archive.db`. None of
//! those are where a project lives any more, and asserting on them was always a
//! proxy for the questions that actually matter — is there a project here, does
//! it have a catalog, can it mint a story. Those questions are asked of the CLI
//! below, so this file says the same thing on both storage models and keeps
//! saying it after the legacy directory is retired.
//!
//! The agent instructions are the other half. They used to be scaffolded into
//! `.storyhook/CLAUDE.md`, inside the data directory; they are in `AGENTS.md`
//! at the repository root now — one scaffold artifact, where a fresh agent
//! looks, and not inside a directory that is about to stop existing.

// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

use assert_cmd::Command;
use tempfile::{TempDir, tempdir};

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

/// An initialized project, and the directory holding it.
fn initialized() -> TempDir {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();
    dir
}

/// The `AGENTS.md` a freshly initialized project has.
fn agents_md(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join("AGENTS.md")).expect("init must generate AGENTS.md")
}

#[test]
fn init_creates_a_usable_project() {
    let dir = initialized();

    // A project exists: the read surface answers instead of exiting 3.
    story(dir.path()).arg("summary").assert().success();
    story(dir.path()).args(["state", "list"]).assert().success();
    story(dir.path()).args(["type", "list"]).assert().success();

    // It has the default catalog…
    story(dir.path())
        .args(["state", "list"])
        .assert()
        .stdout(predicates::str::contains("todo (OPEN)"))
        .stdout(predicates::str::contains("in-progress (OPEN, active)"))
        .stdout(predicates::str::contains("done (CLOSED)"));

    // …and its counter starts at one.
    story(dir.path())
        .args(["new", "The first story"])
        .assert()
        .success()
        .stdout(predicates::str::contains("SH-1"));
}

#[test]
fn init_is_idempotent() {
    let dir = initialized();
    story(dir.path()).args(["new", "Before"]).assert().success();

    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // The second run neither reset the catalog nor rewound the counter.
    story(dir.path())
        .args(["new", "After"])
        .assert()
        .success()
        .stdout(predicates::str::contains("SH-2"));
}

#[test]
fn init_generates_agents_md_and_nothing_else_at_the_repository_root() {
    let dir = initialized();
    let entries: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name != ".storyhook")
        .collect();

    // Two artifacts, both of them things the user asked for by running `init`:
    // the instructions, and the pointer file naming which project this is.
    // Anything else appearing here is storyhook writing into a repository
    // during ordinary operation, which is the thing the rearchitecture exists
    // to stop.
    let mut sorted = entries.clone();
    sorted.sort();
    for name in &sorted {
        assert!(
            name == "AGENTS.md" || name == ".storyhook.toml",
            "init wrote an unexpected repository artifact: {name} (all: {sorted:?})"
        );
    }
    assert!(sorted.contains(&"AGENTS.md".to_string()), "{sorted:?}");
}

#[test]
fn init_agents_md_has_no_mcp_references() {
    let dir = initialized();
    let agents_md = agents_md(dir.path());
    assert!(
        !agents_md.contains("MCP"),
        "AGENTS.md must not contain 'MCP'"
    );
    assert!(
        !agents_md.contains("mcp"),
        "AGENTS.md must not contain 'mcp'"
    );
}

#[test]
fn init_agents_md_references_help() {
    let agents_md = agents_md(initialized().path());
    assert!(
        agents_md.contains("story help <command>"),
        "AGENTS.md should reference 'story help <command>'"
    );
    assert!(
        agents_md.contains("story help --compact"),
        "AGENTS.md should reference 'story help --compact'"
    );
}

#[test]
fn init_agents_md_documents_every_relationship_type() {
    let agents_md = agents_md(initialized().path());
    for relation in [
        "blocks",
        "blocked-by",
        "parent-of",
        "child-of",
        "relates-to",
        "obviates",
        "obviated-by",
        "duplicate-of",
    ] {
        assert!(
            agents_md.contains(relation),
            "AGENTS.md should document the `{relation}` relationship"
        );
    }
}

#[test]
fn init_agents_md_documents_decompose_and_graph() {
    let agents_md = agents_md(initialized().path());
    assert!(
        agents_md.contains("story decompose"),
        "AGENTS.md should document the decompose workflow"
    );
    assert!(
        agents_md.contains("story graph"),
        "AGENTS.md should document the graph command"
    );
}
