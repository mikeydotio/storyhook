// TODO(rearch): migrate to storyhook_test_support::scratch_dir — see clippy.toml.
#![allow(clippy::disallowed_methods)]

use assert_cmd::Command;
use std::fs;
use tempfile::TempDir;

/// Appends `body` — one or more `[hooks.*]` tables — to the checkout's
/// committed pointer file.
///
/// This is where event-hook configuration lives after the flip: a table in
/// `.storyhook.toml` rather than a file inside a directory that is about to
/// stop existing.
///
/// **Appended, not written over.** It used to replace the whole file with a
/// fabricated identity, which worked only because the store also kept an index
/// of directories to fall back on; with that index gone (SH-119) a pointer
/// naming a project the store does not have makes the checkout unresolvable,
/// which is the correct refusal and useless as a hook fixture. Appending is
/// also what a user does: `story project new` writes the identity, and the
/// `[hooks]` tables are hand-added underneath it.
fn write_hooks(dir: &std::path::Path, body: &str) {
    let path = dir.join(".storyhook.toml");
    let identity = fs::read_to_string(&path)
        .expect("`story project new` must have written the pointer file this appends to");
    fs::write(&path, format!("{identity}\n{body}")).unwrap();
}

/// Writes the *legacy* `.storyhook/hooks.toml`, creating its directory.
///
/// Only the two tests whose subject is the legacy fallback use this. The
/// directory has to be created explicitly because `story init` no longer makes
/// one once the store is the default.
fn write_legacy_hooks(dir: &std::path::Path, body: &str) {
    fs::create_dir_all(dir.join(".storyhook")).unwrap();
    fs::write(dir.join(".storyhook/hooks.toml"), body).unwrap();
}

fn init_project(dir: &std::path::Path) {
    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir)
        .args(["project", "new", "--prefix", "SH"])
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
    write_hooks(
        dir,
        &format!(
            "[hooks.on_create]\ncommand = \"cat > {}\"\n",
            output_file.display()
        ),
    );

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

    write_hooks(dir, "[hooks.on_create]\ncommand = \"exit 1\"\n");

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
    write_hooks(
        dir,
        &format!(
            "[hooks.on_create]\ncommand = \"cat > {}\"\n",
            output_file.display()
        ),
    );

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

    write_hooks(
        dir,
        "[hooks.on_create]\ncommand = \"echo created\"\n\
         [hooks.on_close]\ncommand = \"echo closed\"\n",
    );

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

/// The `[hooks]` table in the committed pointer file is read in place of
/// `.storyhook/hooks.toml`.
///
/// Where event-hook configuration lives is the other half of the question the
/// flip answers: it is *user-authored config about this repository*, not story
/// data, so it stays in the repository — it just stops living inside a
/// directory that is about to stop existing.
#[test]
fn hooks_can_be_configured_in_the_pointer_file() {
    let dir = TempDir::new().unwrap();
    let dir = dir.path();
    init_project(dir);

    let output_file = dir.join("hook_output.json");
    write_hooks(
        dir,
        &format!(
            "[hooks.on_create]\ncommand = \"cat > {}\"\n",
            output_file.display()
        ),
    );

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir)
        .args(["new", "Configured in the pointer"])
        .assert()
        .success();

    assert!(
        output_file.exists(),
        "a hook declared in .storyhook.toml's [hooks] table must fire"
    );
    let payload: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&output_file).unwrap()).unwrap();
    assert_eq!(payload["story_title"], "Configured in the pointer");
}

#[test]
fn the_pointers_hooks_table_wins_over_the_legacy_file() {
    let dir = TempDir::new().unwrap();
    let dir = dir.path();
    init_project(dir);

    let legacy = dir.join("legacy.json");
    let pointed = dir.join("pointed.json");
    write_legacy_hooks(
        dir,
        &format!("[on_create]\ncommand = \"cat > {}\"\n", legacy.display()),
    );
    write_hooks(
        dir,
        &format!(
            "[hooks.on_create]\ncommand = \"cat > {}\"\n",
            pointed.display()
        ),
    );

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir)
        .args(["new", "Two homes, one answer"])
        .assert()
        .success();

    assert!(pointed.exists(), "the pointer's table must be the one read");
    assert!(
        !legacy.exists(),
        "a repository that has moved its hooks must not fire the old ones as well"
    );
}

#[test]
fn a_pointer_with_no_hooks_table_leaves_the_legacy_file_in_charge() {
    let dir = TempDir::new().unwrap();
    let dir = dir.path();
    init_project(dir);

    let legacy = dir.join("legacy.json");
    write_legacy_hooks(
        dir,
        &format!("[on_create]\ncommand = \"cat > {}\"\n", legacy.display()),
    );
    // No `[hooks]` table: the pointer file `story project new` wrote carries
    // identity and nothing else, which is exactly the premise here.

    Command::cargo_bin("story")
        .unwrap()
        .current_dir(dir)
        .args(["new", "Still on the old config"])
        .assert()
        .success();

    assert!(
        legacy.exists(),
        "the two storage models coexist until the daemon wave"
    );
}
