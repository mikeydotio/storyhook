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
    story(dir.path()).arg("init").assert().success();

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
    story(dir.path()).arg("init").assert().success();

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
    story(dir.path()).arg("init").assert().success();

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
    story(dir.path()).arg("init").assert().success();

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
    story(dir.path()).arg("init").assert().success();

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
    story(dir.path()).arg("init").assert().success();

    let json = r#"[{"title": "Task one"}]"#;

    story(dir.path())
        .args(["--json", "import"])
        .write_stdin(json)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"result\": \"ok\""))
        .stdout(predicate::str::contains("\"id\": \"SH-1\""));
}
