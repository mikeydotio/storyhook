use assert_cmd::Command;
use tempfile::tempdir;

#[test]
fn init_creates_storyhook_layout() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();

    assert!(dir.path().join(".storyhook/project.toml").exists());
    assert!(dir.path().join(".storyhook/states.toml").exists());
    assert!(dir.path().join(".storyhook/types.toml").exists());
    assert!(dir.path().join(".storyhook/open/stories").exists());
    assert!(dir.path().join(".storyhook/archive/archive.db").exists());
}

#[test]
fn init_storyhook_claude_md_no_mcp_references() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();

    let claude_md = std::fs::read_to_string(dir.path().join(".storyhook/CLAUDE.md")).unwrap();
    assert!(!claude_md.contains("MCP"), ".storyhook/CLAUDE.md must not contain 'MCP'");
    assert!(!claude_md.contains("mcp"), ".storyhook/CLAUDE.md must not contain 'mcp'");
}

#[test]
fn init_storyhook_claude_md_references_help() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();

    let claude_md = std::fs::read_to_string(dir.path().join(".storyhook/CLAUDE.md")).unwrap();
    assert!(claude_md.contains("story help <command>"), ".storyhook/CLAUDE.md should reference 'story help <command>'");
    assert!(claude_md.contains("story help --compact"), ".storyhook/CLAUDE.md should reference 'story help --compact'");
}

#[test]
fn init_storyhook_claude_md_has_relationship_types() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();

    let claude_md = std::fs::read_to_string(dir.path().join(".storyhook/CLAUDE.md")).unwrap();
    assert!(claude_md.contains("blocks"), ".storyhook/CLAUDE.md should document 'blocks' relationship");
    assert!(claude_md.contains("parent-of"), ".storyhook/CLAUDE.md should document 'parent-of' relationship");
    assert!(claude_md.contains("relates-to"), ".storyhook/CLAUDE.md should document 'relates-to' relationship");
    assert!(claude_md.contains("obviates"), ".storyhook/CLAUDE.md should document 'obviates' relationship");
    assert!(claude_md.contains("duplicate-of"), ".storyhook/CLAUDE.md should document 'duplicate-of' relationship");
}

#[test]
fn init_storyhook_claude_md_has_decompose_and_graph() {
    let dir = tempdir().unwrap();
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir.path())
        .arg("init")
        .assert()
        .success();

    let claude_md = std::fs::read_to_string(dir.path().join(".storyhook/CLAUDE.md")).unwrap();
    assert!(claude_md.contains("story decompose"), ".storyhook/CLAUDE.md should document decompose workflow");
    assert!(claude_md.contains("story graph"), ".storyhook/CLAUDE.md should document graph command");
}
