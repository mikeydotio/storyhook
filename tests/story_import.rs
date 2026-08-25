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
fn import_creates_stories_from_json() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    let json = r#"[
        {"title": "Build API"},
        {"title": "Write docs"},
        {"title": "Deploy service"}
    ]"#;

    let output = story(dir.path())
        .args(["import"])
        .write_stdin(json)
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SH-1"));
    assert!(stdout.contains("SH-2"));
    assert!(stdout.contains("SH-3"));

    // Verify they exist
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Build API"));
}

#[test]
fn import_with_priority_and_labels() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    let json = r#"[
        {"title": "Critical bug", "priority": "critical", "labels": ["bug", "backend"]}
    ]"#;

    story(dir.path())
        .args(["import"])
        .write_stdin(json)
        .assert()
        .success();

    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("priority: critical"))
        .stdout(predicate::str::contains("labels: backend, bug"));
}

#[test]
fn import_with_cross_references() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    let json = r#"[
        {"title": "Parent task"},
        {"title": "Child task", "relationships": [{"relation": "child-of", "ref_index": 0}]},
        {"title": "Blocked by child", "relationships": [{"relation": "blocked-by", "ref_index": 1}]}
    ]"#;

    story(dir.path())
        .args(["import"])
        .write_stdin(json)
        .assert()
        .success();

    // Verify parent-child relationship
    story(dir.path())
        .args(["show", "SH-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("child-of SH-1"));

    // Verify blocked-by relationship
    story(dir.path())
        .args(["show", "SH-3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("blocked-by SH-2"));

    // Verify inverse on parent
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("parent-of SH-2"));
}

#[test]
fn import_from_file() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    let json = r#"[{"title": "From file"}]"#;
    let file_path = dir.path().join("stories.json");
    std::fs::write(&file_path, json).unwrap();

    story(dir.path())
        .args(["import", file_path.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("From file"));
}

#[test]
fn import_empty_array() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    story(dir.path())
        .args(["import"])
        .write_stdin("[]")
        .assert()
        .success()
        .stdout(predicate::str::contains("no stories to import"));
}

#[test]
fn import_json_output() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    let json = r#"[{"title": "Task one"}]"#;

    story(dir.path())
        .args(["--json", "import"])
        .write_stdin(json)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"result\": \"ok\""))
        .stdout(predicate::str::contains("\"id\": \"SH-1\""));
}

#[test]
fn import_with_story_type() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    let json = r#"[
        {"title": "Login crash", "story_type": "bug"},
        {"title": "New dashboard", "story_type": "normal"},
        {"title": "Plain task"}
    ]"#;

    story(dir.path())
        .args(["import"])
        .write_stdin(json)
        .assert()
        .success();

    // Verify story_type is set for typed stories
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type: bug"));

    story(dir.path())
        .args(["show", "SH-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type: normal"));

    // Verify an omitted type uses the project's first configured type.
    story(dir.path())
        .args(["show", "SH-3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type: normal"));
}

#[test]
fn import_rejects_invalid_story_type() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    let json = r#"[
        {"title": "Valid story", "story_type": "bug"},
        {"title": "Bad story", "story_type": "foo"},
        {"title": "Also bad", "story_type": "bar"}
    ]"#;

    story(dir.path())
        .args(["import"])
        .write_stdin(json)
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown types: bar, foo"))
        .stderr(predicate::str::contains("Available types:"));
}

#[test]
fn import_atomicity_on_type_error() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    let json = r#"[
        {"title": "Good story", "story_type": "bug"},
        {"title": "Bad story", "story_type": "nonexistent"}
    ]"#;

    // Import should fail
    story(dir.path())
        .args(["import"])
        .write_stdin(json)
        .assert()
        .failure();

    // No stories should have been created (all-or-nothing)
    story(dir.path())
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-1").not());
}

#[test]
fn import_accepts_valid_story_type() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    // SH-157: `task` was a fifth default type; it no longer is, so the four
    // remaining defaults are the whole set worth exercising here.
    let json = r#"[
        {"title": "A bug", "story_type": "bug"},
        {"title": "A story", "story_type": "normal"},
        {"title": "A chore", "story_type": "chore"},
        {"title": "An epic", "story_type": "epic"}
    ]"#;

    story(dir.path())
        .args(["import"])
        .write_stdin(json)
        .assert()
        .success();

    // Verify all four were created with correct types
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type: bug"));
    story(dir.path())
        .args(["show", "SH-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type: normal"));
    story(dir.path())
        .args(["show", "SH-3"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type: chore"));
    story(dir.path())
        .args(["show", "SH-4"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type: epic"));
}

// ============================================================
// Required import metadata (SH-449)
// ============================================================

#[test]
fn import_with_omitted_metadata_defaults_every_entry() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    let json = r#"[{"title": "One"}, {"title": "Two"}]"#;

    story(dir.path())
        .args(["import"])
        .write_stdin(json)
        .assert()
        .success()
        .stdout(predicate::str::contains("warning:").not());

    for id in ["SH-1", "SH-2"] {
        story(dir.path())
            .args(["show", id])
            .assert()
            .success()
            .stdout(predicate::str::contains("priority: low"))
            .stdout(predicate::str::contains("type: normal"));
    }
}

#[test]
fn import_with_explicit_priority_none_rejects_the_whole_batch() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    let json = r#"[{"title": "Would be valid"}, {"title": "Invalid", "priority": "none"}]"#;

    story(dir.path())
        .args(["import"])
        .write_stdin(json)
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "priority must be one of: critical, high, medium, low",
        ));

    story(dir.path())
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Would be valid").not());
}

#[test]
fn import_with_an_unparseable_priority_rejects_without_a_partial_write() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    let json = r#"[{"title": "Typo'd priority", "priority": "urgent"}]"#;

    story(dir.path())
        .args(["import"])
        .write_stdin(json)
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "priority must be one of: critical, high, medium, low",
        ));

    story(dir.path())
        .args(["list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Typo'd priority").not());
}

#[test]
fn import_json_returns_defaulted_stories_without_warnings() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["project", "new", "--prefix", "SH"])
        .assert()
        .success();

    let json = r#"[{"title": "Defaulted"}, {"title": "Explicit", "priority": "high", "story_type": "bug"}]"#;

    let output = story(dir.path())
        .args(["import", "--json"])
        .write_stdin(json)
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    let envelope: serde_json::Value =
        serde_json::from_str(&stdout).expect("`story import --json` must emit one JSON object");

    assert!(envelope.get("warnings").is_none(), "{envelope}");

    let stories = envelope["stories"].as_array().expect("a `stories` array");
    let defaulted = stories
        .iter()
        .find(|s| s["story"]["id"] == "SH-1")
        .expect("SH-1 in the stories array");
    assert_eq!(defaulted["story"]["priority"], "low");
    assert_eq!(defaulted["story"]["story_type"], "normal");
    assert!(defaulted.get("warnings").is_none());
}
