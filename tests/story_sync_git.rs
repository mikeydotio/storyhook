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

fn git_commit(dir: &std::path::Path, message: &str) {
    std::process::Command::new("git")
        .args(["commit", "--allow-empty", "-m", message])
        .current_dir(dir)
        .output()
        .unwrap();
}

#[test]
fn sync_git_basic() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Fix login bug"])
        .assert()
        .success();

    git_commit(dir.path(), "Fix SH-1 bug");

    story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("added 1 comments to 1 stories"));

    // Verify the story now has a [git] comment
    story(dir.path())
        .arg("SH-1")
        .assert()
        .success()
        .stdout(predicate::str::contains("[git]"))
        .stdout(predicate::str::contains("Fix SH-1 bug"));
}

#[test]
fn sync_git_multiple_ids() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "First story"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Second story"])
        .assert()
        .success();

    git_commit(dir.path(), "Fixes SH-1 and SH-2");

    story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("added 2 comments to 2 stories"));
}

#[test]
fn sync_git_no_matches() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Some story"])
        .assert()
        .success();

    git_commit(dir.path(), "Refactor internal code");

    story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("added 0 comments to 0 stories"));
}

#[test]
fn sync_git_idempotent() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Idempotent story"])
        .assert()
        .success();

    git_commit(dir.path(), "Fix SH-1 issue");

    // First sync
    story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("added 1 comments"));

    // Second sync should add nothing
    story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("added 0 comments to 0 stories"));
}

#[test]
fn sync_git_not_a_git_repo() {
    let dir = tempdir().unwrap();
    // No git init -- just story init
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["sync-git"])
        .assert()
        .failure()
        .code(2)
        .stdout(predicate::str::contains("not a git repository"));
}

#[test]
fn sync_git_closed_story_ignored() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Closable story"])
        .assert()
        .success();

    // Close the story (archives it)
    story(dir.path())
        .args(["SH-1", "is", "done"])
        .assert()
        .success();

    git_commit(dir.path(), "Ref SH-1 after close");

    story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("added 0 comments to 0 stories"));
}

#[test]
fn sync_git_custom_prefix() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path())
        .args(["init", "--prefix", "API"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "API endpoint"])
        .assert()
        .success();

    git_commit(dir.path(), "Fix API-1 endpoint");

    story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("added 1 comments to 1 stories"));

    story(dir.path())
        .arg("API-1")
        .assert()
        .success()
        .stdout(predicate::str::contains("[git]"))
        .stdout(predicate::str::contains("Fix API-1 endpoint"));
}

#[test]
fn sync_git_since_flag() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Recent work"])
        .assert()
        .success();

    git_commit(dir.path(), "Work on SH-1");

    // --since 0m should still pick up the very recent commit
    // (0m means 0 minutes ago, which is now)
    story(dir.path())
        .args(["sync-git", "--since", "1m"])
        .assert()
        .success()
        .stdout(predicate::str::contains("scanned"));
}

#[test]
fn sync_git_summary_message() {
    let dir = tempdir().unwrap();
    init_git(dir.path());
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Story one"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Story two"])
        .assert()
        .success();

    git_commit(dir.path(), "Fix SH-1");
    git_commit(dir.path(), "Fix SH-2");
    git_commit(dir.path(), "Unrelated commit");

    story(dir.path())
        .args(["sync-git", "--since", "1h"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "scanned 3 commits, added 2 comments to 2 stories",
        ));
}
