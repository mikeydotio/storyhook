use assert_cmd::Command;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn list_blocked_filter() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Ready task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Blocked task"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-2", "awaits", "external API"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["list", "--blocked"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(!stdout.contains("SH-1"));
    assert!(stdout.contains("SH-2"));
}

#[test]
fn list_ready_filter() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Ready task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Blocked task"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-2", "awaits", "external API"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["list", "--ready"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SH-1"));
    assert!(!stdout.contains("SH-2"));
}

#[test]
fn list_dependency_blocked() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "First task"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Second task"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-2", "follows", "SH-1"])
        .assert()
        .success();

    let output = story(dir.path())
        .args(["list", "--blocked"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(!stdout.contains("SH-1"));
    assert!(stdout.contains("SH-2"));
}

#[test]
fn list_combined_filters() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "High ready"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Low ready"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "High blocked"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-1", "priority", "high"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-2", "priority", "low"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-3", "priority", "high"])
        .assert()
        .success();
    story(dir.path())
        .args(["SH-3", "awaits", "review"])
        .assert()
        .success();

    // Filter: ready + high priority
    let output = story(dir.path())
        .args(["list", "--ready", "--priority", "high"])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).unwrap();
    assert!(stdout.contains("SH-1"));
    assert!(!stdout.contains("SH-2"));
    assert!(!stdout.contains("SH-3"));
}
