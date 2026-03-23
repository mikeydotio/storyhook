use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn export_and_import_roundtrip() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Task one"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Task two"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-1", "priority", "high"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-1", "label", "backend"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-2", "is", "done"])
        .assert()
        .success();

    // Export
    let output = story(dir.path()).args(["export"]).assert().success();
    let export_json = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    // Import into new directory
    let dir2 = tempdir().unwrap();
    let export_file = dir2.path().join("export.json");
    std::fs::write(&export_file, &export_json).unwrap();

    story(dir2.path())
        .args(["import-project", export_file.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("imported project with 2 stories"));

    // Verify open story preserved
    story(dir2.path())
        .args(["SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Task one"))
        .stdout(predicate::str::contains("priority: high"))
        .stdout(predicate::str::contains("labels: backend"));

    // Verify archived story preserved
    story(dir2.path())
        .args(["SH-2"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Task two"))
        .stdout(predicate::str::contains("done"));

    // Verify next ID works correctly
    story(dir2.path())
        .args(["new", "Task three"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SH-3"));
}

#[test]
fn export_json_output() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "A story"])
        .assert()
        .success();

    let output = story(dir.path()).args(["export"]).assert().success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    // Export output is raw JSON (not wrapped in envelope)
    assert!(stdout.contains("\"schema\""));
    assert!(stdout.contains("\"stories\""));
}

#[test]
fn export_preserves_custom_prefix() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["init", "--prefix", "API"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Endpoint"])
        .assert()
        .success();

    let output = story(dir.path()).args(["export"]).assert().success();
    let export_json = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    let dir2 = tempdir().unwrap();
    let export_file = dir2.path().join("export.json");
    std::fs::write(&export_file, &export_json).unwrap();

    story(dir2.path())
        .args(["import-project", export_file.to_str().unwrap()])
        .assert()
        .success();

    // New stories should use the imported prefix
    story(dir2.path())
        .args(["new", "Another endpoint"])
        .assert()
        .success()
        .stdout(predicate::str::contains("API-2"));
}
