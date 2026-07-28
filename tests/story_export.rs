use assert_cmd::Command;
use predicates::prelude::*;
use storyhook_test_support::{TestEnv, scratch_dir};

/// Every `story` invocation in this file runs in the shared test
/// environment's private HOME/XDG directories, so nothing here can reach the
/// developer's own storyhook state.
fn story(dir: &std::path::Path) -> Command {
    TestEnv::shared().story(dir)
}

#[test]
fn export_and_import_roundtrip() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args(["new", "Task one"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Task two"])
        .assert()
        .success();
    story(dir.path())
        .args(["prioritize", "SH-1", "high"])
        .assert()
        .success();
    story(dir.path())
        .args(["label", "SH-1", "backend"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-2", "done"])
        .assert()
        .success();

    // Export
    let output = story(dir.path()).args(["export"]).assert().success();
    let export_json = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    // Import into new directory
    let dir2 = scratch_dir();
    let export_file = dir2.path().join("export.json");
    std::fs::write(&export_file, &export_json).unwrap();

    story(dir2.path())
        .args(["import-project", export_file.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("imported project with 2 stories"));

    // Verify open story preserved
    story(dir2.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Task one"))
        .stdout(predicate::str::contains("priority: high"))
        .stdout(predicate::str::contains("labels: backend"));

    // Verify archived story preserved
    story(dir2.path())
        .args(["show", "SH-2"])
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
fn export_import_roundtrips_description() {
    let dir = TestEnv::shared().project().build();
    story(dir.path())
        .args([
            "new",
            "Described story",
            "--description",
            "What this story is about",
        ])
        .assert()
        .success();

    let output = story(dir.path()).args(["export"]).assert().success();
    let export_json = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    let dir2 = scratch_dir();
    let export_file = dir2.path().join("export.json");
    std::fs::write(&export_file, &export_json).unwrap();

    story(dir2.path())
        .args(["import-project", export_file.to_str().unwrap()])
        .assert()
        .success();

    story(dir2.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "description: What this story is about",
        ));
}

#[test]
fn export_json_output() {
    let dir = TestEnv::shared().project().build();
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
    let dir = TestEnv::shared().project().prefix("API").build();
    story(dir.path())
        .args(["new", "Endpoint"])
        .assert()
        .success();

    let output = story(dir.path()).args(["export"]).assert().success();
    let export_json = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    let dir2 = scratch_dir();
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

#[test]
fn export_and_import_roundtrip_with_types() {
    let dir = TestEnv::shared().project().build();

    // Define a custom type (not one of the defaults)
    story(dir.path())
        .args(["type", "add", "hotfix"])
        .assert()
        .success();

    // Create a story with that type
    story(dir.path())
        .args(["new", "Fix crash on login", "--type", "hotfix"])
        .assert()
        .success();

    // Verify the type is set on the story
    story(dir.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("type: hotfix"));

    // Export
    let output = story(dir.path()).args(["export"]).assert().success();
    let export_json = String::from_utf8(output.get_output().stdout.clone()).unwrap();

    // Verify the export JSON contains the types field
    assert!(export_json.contains("\"types\""));
    assert!(export_json.contains("\"hotfix\""));

    // Import into a new directory
    let dir2 = scratch_dir();
    let export_file = dir2.path().join("export.json");
    std::fs::write(&export_file, &export_json).unwrap();

    story(dir2.path())
        .args(["import-project", export_file.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("imported project with 1 stories"));

    // Verify the story has the correct type in the imported project
    story(dir2.path())
        .args(["show", "SH-1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Fix crash on login"))
        .stdout(predicate::str::contains("type: hotfix"));

    // Verify types.toml was restored with the custom type:
    // doctor should not flag "hotfix" as unknown
    story(dir2.path()).args(["doctor"]).assert().success();
}
