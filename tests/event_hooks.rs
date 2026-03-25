use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

fn init_project(dir: &std::path::Path) {
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir)
        .args(["init"])
        .assert()
        .success();
}

#[test]
fn hook_fires_on_create() {
    let dir = TempDir::new().unwrap();
    let dir = dir.path();
    // Init git repo
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .unwrap();

    init_project(dir);

    let output_file = dir.join("hook_output.json");
    let hooks_toml = format!(
        "[on_create]\ncommand = \"cat > {}\"\n",
        output_file.display()
    );
    fs::write(dir.join(".storyhook/hooks.toml"), hooks_toml).unwrap();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir)
        .args(["new", "Test story"])
        .assert()
        .success();

    assert!(output_file.exists(), "hook output file should exist");
    let content = fs::read_to_string(&output_file).unwrap();
    let payload: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(payload["event_type"], "create");
    assert_eq!(payload["story_title"], "Test story");
}

#[test]
fn hook_failure_does_not_prevent_operation() {
    let dir = TempDir::new().unwrap();
    let dir = dir.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .unwrap();

    init_project(dir);

    let hooks_toml = "[on_create]\ncommand = \"exit 1\"\n";
    fs::write(dir.join(".storyhook/hooks.toml"), hooks_toml).unwrap();

    // Operation should still succeed even though hook fails
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir)
        .args(["new", "Test story"])
        .assert()
        .success();
}

#[test]
fn no_hooks_flag_suppresses_hooks() {
    let dir = TempDir::new().unwrap();
    let dir = dir.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .unwrap();

    init_project(dir);

    let output_file = dir.join("hook_output.json");
    let hooks_toml = format!(
        "[on_create]\ncommand = \"cat > {}\"\n",
        output_file.display()
    );
    fs::write(dir.join(".storyhook/hooks.toml"), hooks_toml).unwrap();

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir)
        .args(["--no-hooks", "new", "Test story"])
        .assert()
        .success();

    assert!(
        !output_file.exists(),
        "hook should not have fired with --no-hooks"
    );
}

#[test]
fn hooks_list_shows_configured() {
    let dir = TempDir::new().unwrap();
    let dir = dir.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();

    init_project(dir);

    let hooks_toml =
        "[on_create]\ncommand = \"echo created\"\n[on_close]\ncommand = \"echo closed\"\n";
    fs::write(dir.join(".storyhook/hooks.toml"), hooks_toml).unwrap();

    let output = Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir)
        .args(["hooks", "list"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("on_create"));
    assert!(stdout.contains("on_close"));
}

#[test]
fn hooks_list_no_config() {
    let dir = TempDir::new().unwrap();
    let dir = dir.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();

    init_project(dir);

    let output = Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir)
        .args(["hooks", "list"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no hooks configured"));
}

#[test]
fn no_hooks_toml_operations_work() {
    let dir = TempDir::new().unwrap();
    let dir = dir.path();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(dir)
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(dir)
        .output()
        .unwrap();

    init_project(dir);

    // No hooks.toml — operations should work fine
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir)
        .args(["new", "Test story"])
        .assert()
        .success();
}
