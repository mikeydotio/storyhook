use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

fn init_git(dir: &std::path::Path) {
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
}

#[test]
fn hooks_install_creates_hook_files() {
    let dir = tempdir().unwrap();
    init_git(dir.path());

    story(dir.path())
        .args(["hooks", "install"])
        .assert()
        .success()
        .stdout(predicate::str::contains("post-commit — installed"))
        .stdout(predicate::str::contains("post-merge — installed"))
        .stdout(predicate::str::contains("prepare-commit-msg — installed"));

    assert!(dir.path().join(".git/hooks/post-commit").exists());
    assert!(dir.path().join(".git/hooks/post-merge").exists());
    assert!(dir.path().join(".git/hooks/prepare-commit-msg").exists());
}

#[test]
fn hooks_install_skips_existing_user_hooks() {
    let dir = tempdir().unwrap();
    init_git(dir.path());

    // Create a custom user hook without the storyhook marker
    let hooks_dir = dir.path().join(".git/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    let custom_content = "#!/bin/sh\necho 'my custom hook'\n";
    std::fs::write(hooks_dir.join("post-commit"), custom_content).unwrap();

    story(dir.path())
        .args(["hooks", "install"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "post-commit — skipped (existing user hook)",
        ));

    // Verify the custom hook was NOT overwritten
    let content = std::fs::read_to_string(hooks_dir.join("post-commit")).unwrap();
    assert_eq!(content, custom_content);
}

#[test]
fn hooks_install_overwrites_storyhook_hooks() {
    let dir = tempdir().unwrap();
    init_git(dir.path());

    // Create a hook with the storyhook marker (old version)
    let hooks_dir = dir.path().join(".git/hooks");
    std::fs::create_dir_all(&hooks_dir).unwrap();
    let old_content = "#!/bin/sh\n# storyhook managed hook -- do not edit this line\necho 'old'\n";
    std::fs::write(hooks_dir.join("post-commit"), old_content).unwrap();

    story(dir.path())
        .args(["hooks", "install"])
        .assert()
        .success()
        .stdout(predicate::str::contains("post-commit — installed"));

    // Verify the content was updated (not the old content)
    let content = std::fs::read_to_string(hooks_dir.join("post-commit")).unwrap();
    assert_ne!(content, old_content);
    assert!(content.contains("storyhook managed hook"));
}

#[test]
fn hooks_uninstall_removes_storyhook_hooks() {
    let dir = tempdir().unwrap();
    init_git(dir.path());

    // Install first
    story(dir.path())
        .args(["hooks", "install"])
        .assert()
        .success();

    assert!(dir.path().join(".git/hooks/post-commit").exists());

    // Uninstall
    story(dir.path())
        .args(["hooks", "uninstall"])
        .assert()
        .success()
        .stdout(predicate::str::contains("post-commit — removed"))
        .stdout(predicate::str::contains("post-merge — removed"))
        .stdout(predicate::str::contains("prepare-commit-msg — removed"));

    assert!(!dir.path().join(".git/hooks/post-commit").exists());
    assert!(!dir.path().join(".git/hooks/post-merge").exists());
    assert!(!dir.path().join(".git/hooks/prepare-commit-msg").exists());
}

#[test]
fn hooks_uninstall_preserves_user_hooks() {
    let dir = tempdir().unwrap();
    init_git(dir.path());

    // Install storyhook hooks
    story(dir.path())
        .args(["hooks", "install"])
        .assert()
        .success();

    // Replace post-commit with a user hook (no marker)
    let hooks_dir = dir.path().join(".git/hooks");
    let user_content = "#!/bin/sh\necho 'user hook'\n";
    std::fs::write(hooks_dir.join("post-commit"), user_content).unwrap();

    // Uninstall
    story(dir.path())
        .args(["hooks", "uninstall"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "post-commit — skipped (not a storyhook hook)",
        ));

    // Verify user hook survived
    let content = std::fs::read_to_string(hooks_dir.join("post-commit")).unwrap();
    assert_eq!(content, user_content);
}

#[test]
fn hooks_install_fails_outside_git_repo() {
    let dir = tempdir().unwrap();
    // Do NOT init git

    story(dir.path())
        .args(["hooks", "install"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("not a git repository"));
}

#[test]
fn hooks_uninstall_idempotent() {
    let dir = tempdir().unwrap();
    init_git(dir.path());

    // Uninstall when no hooks exist should succeed
    story(dir.path())
        .args(["hooks", "uninstall"])
        .assert()
        .success()
        .stdout(predicate::str::contains("post-commit — not present"))
        .stdout(predicate::str::contains("post-merge — not present"))
        .stdout(predicate::str::contains("prepare-commit-msg — not present"));
}
