//! Staged membership must survive identity generation without changing Git state.

use super::{TreeRepo, assert_ok, checkout, stdout};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use storyhook_test_support::scratch_dir;

fn files_under(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

#[test]
fn staged_additions_use_current_bytes_and_exclude_untracked_files() {
    let repo = TreeRepo::new();
    let original = stdout(&repo.tree());
    let name = "-new\tfile\nwith spaces";
    repo.write(name, "staged content\n");
    assert_ok(&repo.git(&["add", "--", name]), "staging a new path");
    let staged = repo.tree();
    assert_ok(&staged, "identity with a staged addition");
    assert_ne!(stdout(&staged), original, "the new path must be present");

    repo.write(name, "new current content\n");
    repo.write("f", "unstaged tracked edit\n");
    repo.write("untracked", "must be excluded\n");
    let before = files_under(&repo.path().join(".git"));
    let current = repo.tree();
    assert_ok(&current, "identity after current tracked bytes change");
    assert_ne!(stdout(&current), stdout(&staged));
    assert_eq!(
        files_under(&repo.path().join(".git")),
        before,
        "identity must not rewrite the source index or insert canonical objects"
    );

    assert_ok(
        &repo.git(&["add", "-u"]),
        "staging exactly the tracked bytes",
    );
    let expected = repo.git(&["write-tree"]);
    assert_ok(&expected, "materializing the independent expected tree");
    assert_eq!(stdout(&current), stdout(&expected));
}

#[test]
fn staged_deletions_stay_deleted_when_the_file_remains_on_disk() {
    let repo = TreeRepo::new();
    assert_ok(&repo.git(&["rm", "--cached", "f"]), "staging deletion");
    let expected = repo.git(&["write-tree"]);
    assert_ok(&expected, "materializing the empty staged tree");
    let before = files_under(&repo.path().join(".git"));

    let deleted = repo.tree();
    assert_ok(&deleted, "identity after a cached deletion");
    assert_eq!(stdout(&deleted), stdout(&expected));
    repo.write("f", "the retained file is now untracked\n");
    let edited = repo.tree();
    assert_ok(
        &edited,
        "identity after editing the untracked retained file",
    );
    assert_eq!(stdout(&edited), stdout(&expected));
    assert_eq!(files_under(&repo.path().join(".git")), before);
}

fn alternate_git(repo: &TreeRepo, index: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(repo.path())
        .env("GIT_INDEX_FILE", index)
        .output()
        .expect("running Git against the fixture's alternate index")
}

#[test]
fn relative_alternate_split_index_preserves_membership_and_source_state() {
    let repo = TreeRepo::new();
    assert_ok(
        &repo.git(&["config", "core.splitIndex", "true"]),
        "enabling split indexes",
    );
    let nested = repo.path().join("nested directory");
    std::fs::create_dir(&nested).unwrap();
    let index = nested.join("alternate index");
    assert_ok(
        &alternate_git(&repo, &index, &["read-tree", "HEAD"]),
        "initializing the alternate index",
    );
    repo.write("new", "staged\n");
    assert_ok(
        &alternate_git(&repo, &index, &["add", "new"]),
        "staging only in the alternate index",
    );
    assert_ok(
        &alternate_git(&repo, &index, &["update-index", "--split-index"]),
        "splitting the alternate index",
    );
    let shared = alternate_git(&repo, &index, &["rev-parse", "--shared-index-path"]);
    assert_ok(&shared, "locating the alternate shared index");
    assert!(
        !stdout(&shared).is_empty(),
        "fixture must actually be split"
    );
    repo.write("new", "current worktree content\n");
    let before = files_under(&repo.path().join(".git"));
    let index_before = std::fs::read(&index).unwrap();
    let objects = scratch_dir();

    let actual = Command::new("bash")
        .arg(checkout().join("scripts/tracked-tree.sh"))
        .arg(objects.path())
        .current_dir(&nested)
        .env("GIT_INDEX_FILE", "nested directory/alternate index")
        .output()
        .expect("identifying content through a relative alternate split index");

    assert_ok(&actual, "identity from an alternate split index");
    assert_eq!(files_under(&repo.path().join(".git")), before);
    assert_eq!(std::fs::read(&index).unwrap(), index_before);
    assert_ok(
        &alternate_git(&repo, &index, &["add", "-u"]),
        "staging current tracked bytes in the alternate index",
    );
    let expected = alternate_git(&repo, &index, &["write-tree"]);
    assert_ok(&expected, "materializing the alternate expected tree");
    assert_eq!(stdout(&actual), stdout(&expected));
    let canonical = repo.git(&["write-tree"]);
    assert_ok(&canonical, "reading the canonical index's tree");
    assert_ne!(stdout(&actual), stdout(&canonical));
}

#[test]
fn a_corrupt_source_index_cannot_produce_an_identity() {
    let repo = TreeRepo::new();
    std::fs::write(repo.path().join(".git/index"), "corrupt index\n").unwrap();

    let actual = repo.tree();

    assert!(!actual.status.success(), "a corrupt index must fail closed");
    assert!(
        actual.stdout.is_empty(),
        "failure must not print a tree oid"
    );
}
