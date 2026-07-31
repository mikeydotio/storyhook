// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn decompose_single_heading() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();

    let md = "# Build the API\n";
    let file_path = dir.path().join("spec.md");
    std::fs::write(&file_path, md).unwrap();

    let output = story(dir.path())
        .args(["decompose", file_path.to_str().unwrap()])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("Build the API"));
}

#[test]
fn decompose_nested_headings() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();

    let md = "# Parent Epic\n## Child Feature\n";
    let file_path = dir.path().join("spec.md");
    std::fs::write(&file_path, md).unwrap();

    story(dir.path())
        .args(["decompose", file_path.to_str().unwrap()])
        .assert()
        .success();

    // Verify parent story
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Parent Epic"));

    // Verify child story with relationship
    story(dir.path())
        .args(["show", "SH-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Child Feature"))
        .stdout(predicate::str::contains("child-of SH-1"));
}

#[test]
fn decompose_three_levels() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();

    let md = "# Epic\n## Feature\n### Task\n";
    let file_path = dir.path().join("spec.md");
    std::fs::write(&file_path, md).unwrap();

    story(dir.path())
        .args(["decompose", file_path.to_str().unwrap()])
        .assert()
        .success();

    // Epic has no parent
    let output = story(dir.path()).args(["show", "SH-1"]).assert().success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("Epic"));
    assert!(stdout.contains("parent-of SH-2"));

    // Feature is child of Epic
    story(dir.path())
        .args(["show", "SH-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("child-of SH-1"));

    // Task is child of Feature
    story(dir.path())
        .args(["show", "SH-3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("child-of SH-2"));
}

#[test]
fn decompose_checkboxes() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();

    let md = "# Epic\n- [ ] First task\n- [ ] Second task\n";
    let file_path = dir.path().join("spec.md");
    std::fs::write(&file_path, md).unwrap();

    story(dir.path())
        .args(["decompose", file_path.to_str().unwrap()])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("First task"));

    story(dir.path())
        .args(["show", "SH-3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Second task"));
}

#[test]
fn decompose_checked_items_skipped() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();

    let md = "# Epic\n- [x] Done task\n- [ ] Open task\n";
    let file_path = dir.path().join("spec.md");
    std::fs::write(&file_path, md).unwrap();

    story(dir.path())
        .args(["decompose", file_path.to_str().unwrap()])
        .assert()
        .success();

    // Only 2 stories should be created (Epic + Open task), not Done task
    let output = story(dir.path()).args(["list"]).assert().success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("Epic"));
    assert!(stdout.contains("Open task"));
    assert!(!stdout.contains("Done task"));
}

#[test]
fn decompose_empty_file() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();

    let file_path = dir.path().join("empty.md");
    std::fs::write(&file_path, "").unwrap();

    story(dir.path())
        .args(["decompose", file_path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("no stories to import"));
}

#[test]
fn decompose_no_headings() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();

    let md = "Just some plain text\nAnother line\n";
    let file_path = dir.path().join("plain.md");
    std::fs::write(&file_path, md).unwrap();

    story(dir.path())
        .args(["decompose", file_path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("no stories to import"));
}

#[test]
fn decompose_dry_run() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();

    let md = "# Epic\n## Feature\n";
    let file_path = dir.path().join("spec.md");
    std::fs::write(&file_path, md).unwrap();

    // --dry-run should output JSON without creating stories
    let output = story(dir.path())
        .args(["decompose", file_path.to_str().unwrap(), "--dry-run"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    // Should be valid JSON
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(parsed.is_array());
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["title"], "Epic");
    assert_eq!(arr[1]["title"], "Feature");

    // No stories should actually exist
    let output = story(dir.path()).args(["list"]).assert().success();
    let list_stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(list_stdout.contains("no stories found"));
}

#[test]
fn decompose_roundtrip() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();

    let md = "# Parent\n## Child A\n## Child B\n### Grandchild\n";
    let file_path = dir.path().join("spec.md");
    std::fs::write(&file_path, md).unwrap();

    story(dir.path())
        .args(["decompose", file_path.to_str().unwrap()])
        .assert()
        .success();

    // Verify all stories exist
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Parent"));
    story(dir.path())
        .args(["show", "SH-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Child A"))
        .stdout(predicate::str::contains("child-of SH-1"));
    story(dir.path())
        .args(["show", "SH-3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Child B"))
        .stdout(predicate::str::contains("child-of SH-1"));
    story(dir.path())
        .args(["show", "SH-4"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Grandchild"))
        .stdout(predicate::str::contains("child-of SH-3"));
}

#[test]
fn decompose_stdin() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();

    let md = "# Stdin Epic\n## Stdin Feature\n";

    story(dir.path())
        .args(["decompose", "--stdin"])
        .write_stdin(md)
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Stdin Epic"));
    story(dir.path())
        .args(["show", "SH-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Stdin Feature"))
        .stdout(predicate::str::contains("child-of SH-1"));
}

#[test]
fn decompose_dry_run_works_without_project() {
    let dir = tempdir().unwrap();
    // No story init — directory has no .storyhook/

    let md = "# Epic\n## Feature\n";

    story(dir.path())
        .args(["decompose", "--stdin", "--dry-run"])
        .write_stdin(md)
        .assert()
        .success()
        .stdout(predicate::str::contains("Epic"))
        .stdout(predicate::str::contains("Feature"));
}

#[test]
fn decompose_yaml_file() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();

    let yaml = r#"stories:
  - title: Auth Service
    priority: high
    children:
      - title: Login endpoint
      - title: Signup endpoint
"#;
    let file_path = dir.path().join("spec.yaml");
    std::fs::write(&file_path, yaml).unwrap();

    story(dir.path())
        .args(["decompose", file_path.to_str().unwrap()])
        .assert()
        .success();

    // Verify parent
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Auth Service"))
        .stdout(predicate::str::contains("high"));

    // Verify children
    story(dir.path())
        .args(["show", "SH-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Login endpoint"))
        .stdout(predicate::str::contains("child-of SH-1"));

    story(dir.path())
        .args(["show", "SH-3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Signup endpoint"))
        .stdout(predicate::str::contains("child-of SH-1"));
}

#[test]
fn decompose_yaml_dry_run() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();

    let yaml = r#"stories:
  - title: Epic One
    children:
      - title: Task A
"#;
    let file_path = dir.path().join("spec.yml");
    std::fs::write(&file_path, yaml).unwrap();

    let output = story(dir.path())
        .args(["decompose", file_path.to_str().unwrap(), "--dry-run"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    let parsed: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert!(parsed.is_array());
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["title"], "Epic One");
    assert_eq!(arr[1]["title"], "Task A");

    // No stories should actually exist
    let output = story(dir.path()).args(["list"]).assert().success();
    let list_stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(list_stdout.contains("no stories found"));
}

#[test]
fn decompose_markdown_with_priority() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();

    let md = "# [HIGH] Secure the API\n";
    let file_path = dir.path().join("spec.md");
    std::fs::write(&file_path, md).unwrap();

    story(dir.path())
        .args(["decompose", file_path.to_str().unwrap()])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Secure the API"))
        .stdout(predicate::str::contains("high"));
}

#[test]
fn decompose_markdown_with_labels() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "init"])
        .assert()
        .success();

    let md = "# Build API #backend #security\n";
    let file_path = dir.path().join("spec.md");
    std::fs::write(&file_path, md).unwrap();

    story(dir.path())
        .args(["decompose", file_path.to_str().unwrap()])
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Build API"))
        .stdout(predicate::str::contains("backend"))
        .stdout(predicate::str::contains("security"));
}
