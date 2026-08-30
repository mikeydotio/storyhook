#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

use storyhook_test_support::scratch_dir;

fn git(cwd: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("running git")
}

fn git_with_index(cwd: &Path, index: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .env("GIT_INDEX_FILE", index)
        .current_dir(cwd)
        .output()
        .expect("running git with an isolated index")
}

fn git_with_objects(cwd: &Path, primary: &Path, alternate: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .args(args)
        .env("GIT_OBJECT_DIRECTORY", primary)
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", alternate)
        .current_dir(cwd)
        .output()
        .expect("running git with private primary objects")
}

fn assert_ok(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} should succeed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn trimmed_stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct ImmutableObjects {
    path: PathBuf,
    armed: bool,
}

impl ImmutableObjects {
    fn freeze(path: PathBuf) -> Self {
        let out = Command::new("chflags")
            .args(["-R", "uchg"])
            .arg(&path)
            .output()
            .expect("running chflags to freeze the common object database");
        assert_ok(&out, "freezing the common object database");
        Self { path, armed: true }
    }

    fn restore(&mut self) {
        let out = Command::new("chflags")
            .args(["-R", "nouchg"])
            .arg(&self.path)
            .output()
            .expect("running chflags to restore the common object database");
        if out.status.success() {
            self.armed = false;
        }
        assert_ok(&out, "restoring the common object database");
    }
}

impl Drop for ImmutableObjects {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let out = Command::new("chflags")
            .args(["-R", "nouchg"])
            .arg(&self.path)
            .output();
        if !matches!(out, Ok(ref out) if out.status.success()) {
            eprintln!(
                "failed to restore mutable flags on {}: {out:?}",
                self.path.display()
            );
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct ObjectEntry {
    relative_path: PathBuf,
    is_dir: bool,
    len: u64,
    modified: SystemTime,
}

fn object_database_state(root: &Path) -> Vec<ObjectEntry> {
    fn visit(root: &Path, path: &Path, state: &mut Vec<ObjectEntry>) {
        let metadata = std::fs::symlink_metadata(path).expect("reading object database metadata");
        state.push(ObjectEntry {
            relative_path: path
                .strip_prefix(root)
                .expect("object path belongs to the object database")
                .to_path_buf(),
            is_dir: metadata.is_dir(),
            len: metadata.len(),
            modified: metadata
                .modified()
                .expect("reading object modification time"),
        });

        if metadata.is_dir() {
            let mut children = std::fs::read_dir(path)
                .expect("reading the object database")
                .map(|entry| entry.expect("reading an object database entry").path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                visit(root, &child, state);
            }
        }
    }

    let mut state = Vec::new();
    visit(root, root, &mut state);
    state
}

#[test]
fn a_linked_worktree_prints_its_tracked_tree_without_writing_shared_objects() {
    let fixture = scratch_dir();
    let primary = fixture.path().join("primary");
    let linked = fixture.path().join("linked");
    std::fs::create_dir(&primary).expect("creating the primary checkout");

    assert_ok(&git(&primary, &["init", "-q", "-b", "main"]), "git init");
    std::fs::write(primary.join("tracked"), "tracked content\n")
        .expect("writing the tracked fixture file");
    assert_ok(&git(&primary, &["add", "tracked"]), "tracking the file");
    assert_ok(
        &git(
            &primary,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "init",
            ],
        ),
        "creating the initial commit",
    );

    let add_worktree = Command::new("git")
        .args(["worktree", "add", "-q", "-b", "linked"])
        .arg(&linked)
        .current_dir(&primary)
        .output()
        .expect("running git worktree add");
    assert_ok(&add_worktree, "creating the linked worktree");
    assert_eq!(
        trimmed_stdout(&git(&linked, &["ls-files"])),
        "tracked",
        "the linked fixture must contain tracked content"
    );
    assert_ok(
        &git(&linked, &["diff", "--quiet"]),
        "proving the linked worktree has no tracked diff",
    );
    assert_ok(
        &git(&linked, &["diff", "--cached", "--quiet"]),
        "proving the linked index has no tracked diff",
    );

    let expected_out = git(&linked, &["rev-parse", "HEAD^{tree}"]);
    assert_ok(&expected_out, "reading the committed tracked-tree identity");
    let expected = trimmed_stdout(&expected_out);
    let common_objects = primary.join(".git").join("objects");

    let mut immutable = ImmutableObjects::freeze(common_objects.clone());
    let objects_before = object_database_state(&common_objects);

    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("tracked-tree.sh");
    let out = Command::new("bash")
        .arg(script)
        .current_dir(&linked)
        .output()
        .expect("running scripts/tracked-tree.sh");
    let actual = trimmed_stdout(&out);
    let objects_after = object_database_state(&common_objects);

    let probe_index = fixture.path().join("probe-index");
    let read_tree = git_with_index(&linked, &probe_index, &["read-tree", "HEAD"]);
    let add_tracked = git_with_index(&linked, &probe_index, &["add", "-u", "--", ":/"]);
    let write_tree = git_with_index(&linked, &probe_index, &["write-tree"]);

    immutable.restore();

    assert_ok(&read_tree, "the isolated read-tree permission control");
    assert_ok(&write_tree, "the isolated write-tree permission control");
    let add_stderr = String::from_utf8_lossy(&add_tracked.stderr);
    assert_eq!(
        add_tracked.status.code(),
        Some(128),
        "the fixture must isolate the object-write failure to git add; \
         stderr: {add_stderr}"
    );
    assert!(
        add_stderr.contains("failed to insert into database")
            && add_stderr.contains("Operation not permitted"),
        "git add must fail at the real common-object write boundary, not at \
         pathspec resolution; stderr: {add_stderr}"
    );
    assert_eq!(
        objects_before, objects_after,
        "tracked-tree.sh must not mutate the shared common object database"
    );
    assert!(
        out.status.success() && actual == expected,
        "linked worktree must yield tracked-tree identity {expected:?} without \
         writing shared objects; got {actual:?} with status {}\n\
         git add permission diagnostic: {add_stderr}",
        out.status
    );
}

#[test]
fn caller_owned_objects_keep_a_dirty_tracked_tree_resolvable_without_mutating_git_state() {
    let fixture = scratch_dir();
    let repo = fixture.path().join("repo");
    let private_objects = fixture.path().join("private-objects");
    std::fs::create_dir(&repo).expect("creating the repository");
    std::fs::create_dir(&private_objects).expect("creating caller-owned objects");

    assert_ok(&git(&repo, &["init", "-q", "-b", "main"]), "git init");
    std::fs::write(repo.join("modified"), "original modified file\n")
        .expect("writing the file that will be modified");
    std::fs::write(repo.join("deleted"), "original deleted file\n")
        .expect("writing the file that will be deleted");
    assert_ok(
        &git(&repo, &["add", "modified", "deleted"]),
        "tracking fixture files",
    );
    assert_ok(
        &git(
            &repo,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "init",
            ],
        ),
        "creating the initial commit",
    );

    std::fs::write(repo.join("modified"), "new dirty content\n").expect("modifying a tracked file");
    std::fs::remove_file(repo.join("deleted")).expect("deleting a tracked file");
    std::fs::write(repo.join("untracked"), "must not enter the identity\n")
        .expect("writing an untracked file");

    let common_objects = repo.join(".git").join("objects");
    let index_before =
        std::fs::read(repo.join(".git").join("index")).expect("snapshotting the real index");
    let head_before = trimmed_stdout(&git(&repo, &["rev-parse", "HEAD"]));
    let head_tree = trimmed_stdout(&git(&repo, &["rev-parse", "HEAD^{tree}"]));
    let objects_before = object_database_state(&common_objects);
    let mut immutable = ImmutableObjects::freeze(common_objects.clone());

    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("scripts")
        .join("tracked-tree.sh");
    let out = Command::new("bash")
        .arg(script)
        .arg(&private_objects)
        .current_dir(&repo)
        .output()
        .expect("running tracked-tree.sh with caller-owned objects");
    let dirty_tree = trimmed_stdout(&out);
    assert_ok(&out, "caller-owned dirty tracked-tree generation");

    let resolve = git_with_objects(
        &repo,
        &private_objects,
        &common_objects,
        &["cat-file", "-e", &format!("{dirty_tree}^{{tree}}")],
    );
    let changed = git_with_objects(
        &repo,
        &private_objects,
        &common_objects,
        &["diff", "--name-only", &head_tree, &dirty_tree],
    );
    let objects_after = object_database_state(&common_objects);

    immutable.restore();

    assert_ne!(
        dirty_tree, head_tree,
        "modified and deleted tracked files must change the identity"
    );
    assert_ok(
        &resolve,
        "resolving the dirty tree after the producer exits",
    );
    assert_ok(&changed, "diffing the caller-owned dirty tree");
    assert_eq!(
        trimmed_stdout(&changed),
        "deleted\nmodified",
        "the dirty tree must include modified/deleted tracked paths and exclude untracked paths"
    );
    assert_eq!(
        objects_before, objects_after,
        "caller-owned identity generation and resolution must not mutate source objects"
    );
    assert_eq!(
        std::fs::read(repo.join(".git").join("index")).expect("re-reading the real index"),
        index_before,
        "identity generation must not mutate the real index"
    );
    assert_eq!(
        trimmed_stdout(&git(&repo, &["rev-parse", "HEAD"])),
        head_before,
        "identity generation must not move refs"
    );
    assert!(
        std::fs::read_dir(&private_objects)
            .expect("reading caller-owned objects")
            .next()
            .is_some(),
        "the producer must leave generated objects for its caller to resolve"
    );
}
