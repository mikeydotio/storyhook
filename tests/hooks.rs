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

/// **Nothing under `src/` builds a hooks directory by hand** (SH-313, SH-314).
///
/// The defect both stories share is one line: `<root>/.git/hooks`, assumed
/// rather than asked. That assumption is wrong twice over — `core.hooksPath`
/// replaces the directory wholesale, and a linked worktree's `.git` is a file —
/// and in both cases it fails *silently*, writing executables git will never
/// run and reporting success.
///
/// Derived over `git ls-files` rather than a hand-maintained list, in the style
/// of `tests/store_isolation.rs` and `tests/dead_public_surface.rs`. SH-136
/// records three separate drifts of a hand-maintained count in this repository
/// before it stopped being trusted, and SH-198 records ten dead `pub` items
/// accumulating for the same reason: a list someone has to remember to update
/// is the thing that goes stale.
///
/// The rule is deliberately about *hooks* paths, not about `.git` generally.
/// `service::project::is_repository_top_level` joins `.git` on purpose and
/// wants a directory specifically — that is how it tells a main checkout from a
/// linked worktree — so a blanket ban would have to carve an exception for it,
/// and an exception is the seed of the next stale list.
#[test]
fn no_source_file_builds_a_hooks_directory_by_hand() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--", "src/*.rs"])
        .output()
        .expect("listing tracked sources");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing"
    );
    let files: Vec<&str> = std::str::from_utf8(&listed.stdout)
        .expect("utf-8 paths")
        .split('\0')
        .filter(|p| !p.is_empty())
        .collect();
    assert!(
        files.len() > 20,
        "the scan found only {} source files — it is broken, not the tree",
        files.len()
    );

    for file in files {
        let text = std::fs::read_to_string(root.join(file))
            .unwrap_or_else(|e| panic!("reading {file}: {e}"));
        // Line comments only: this is about what the code does, and every
        // mention of the old path in this tree is prose explaining why it was
        // wrong.
        let code: String = text
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            !code.contains(".git/hooks"),
            "{file} names `.git/hooks` in code. That is not where git looks when \
             `core.hooksPath` is set, and it does not exist in a linked worktree. \
             Ask git: `rev-parse --git-path hooks` and `--git-common-dir`."
        );
        assert!(
            !(code.contains(r#"join(".git")"#) && code.contains(r#"join("hooks")"#)),
            "{file} builds a hooks directory from a hand-joined `.git`. Ask git \
             instead — `hooks::HookDirs` already does, and two answers to one \
             question is how SH-313 and SH-314 both happened."
        );
    }
}
