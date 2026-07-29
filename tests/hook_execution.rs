//! The managed git hooks, **executed**.
//!
//! `tests/hooks.rs` asserts that `story hooks install` writes three files and
//! that it declines to overwrite a user's own. It has never run one of them,
//! and neither had anything else: `POST_MERGE_HOOK` and `POST_COMMIT_HOOK` were
//! `&str` constants in `src/hooks.rs` with no coverage at all.
//!
//! That is the defect class SH-56 belongs to — *hook logic shipped as an
//! untested string literal*. The bug itself was one token (`%s` where `%B`
//! belonged, so a `Closes SH-12` trailer never closed anything), and it
//! plausibly never worked from the day it was written, because nothing
//! executed the script to find out. This file executes it.
//!
//! # What makes these tests real
//!
//! - `git merge` runs the installed `.git/hooks/post-merge`, which shells out
//!   to `story`, which resolves through `PATH`. So `PATH` is *prepended* with
//!   the directory holding the binary under test — without that, a hook in a
//!   test silently exercises the developer's installed build against their real
//!   store.
//! - Every `git` invocation carries [`TestEnv`]'s isolation, so the child
//!   `story` inherits the fixture's data directory rather than the machine's.
//! - The merges are `--no-ff`, which is what produces an `ORIG_HEAD` and a
//!   merge commit for the hook to look at.

use std::path::{Path, PathBuf};
use std::process::Command;

use storyhook_test_support::{TestEnv, scratch_dir, story_binary};
use tempfile::TempDir;

/// A git repository with a storyhook project and the managed hooks installed.
struct HookRepo {
    env: &'static TestEnv,
    dir: TempDir,
}

impl HookRepo {
    /// A repository on `main`, with one commit, a project, and the hooks in
    /// place.
    fn new() -> Self {
        let env = TestEnv::shared();
        let repo = Self {
            env,
            dir: scratch_dir(),
        };
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.email", "t@t"]);
        repo.git(&["config", "user.name", "t"]);
        std::fs::write(repo.path().join("f"), "a\n").expect("seeding a tracked file");
        repo.git(&["add", "f"]);
        repo.git(&["commit", "-qm", "init"]);

        repo.story(repo.path(), &["init"]);
        repo.story(repo.path(), &["hooks", "install"]);
        assert!(
            repo.path().join(".git/hooks/post-merge").is_file(),
            "fixture: the managed hooks must be installed"
        );
        repo
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Runs `git <args>` in `cwd`, with the fixture's environment and a `PATH`
    /// that finds the binary under test.
    ///
    /// The `PATH` is the load-bearing part. A hook runs `story` by name; with
    /// the ambient `PATH` that is whatever the developer has installed, pointed
    /// at whatever store that build defaults to.
    fn git_in(&self, cwd: &Path, args: &[&str]) -> std::process::Output {
        let mut command = Command::new("git");
        command.current_dir(cwd);
        self.env.apply(&mut command);
        command.env("GIT_TERMINAL_PROMPT", "0");
        command.env("PATH", path_with_binary_dir());
        let output = command.args(args).output().expect("running git");
        assert!(
            output.status.success(),
            "`git {}` in {} failed: {}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn git(&self, args: &[&str]) -> std::process::Output {
        self.git_in(self.path(), args)
    }

    /// Runs `story <args>` in `cwd`, asserting success.
    fn story(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self
            .env
            .raw_story(cwd)
            .args(args)
            .output()
            .expect("running story");
        assert!(
            output.status.success(),
            "`story {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Creates a story and returns its id.
    fn new_story(&self, title: &str) -> String {
        let out = self.story(self.path(), &["new", title, "--json"]);
        let value: serde_json::Value = serde_json::from_str(&out).expect("`story new --json`");
        value["story"]["story"]["id"]
            .as_str()
            .expect("an id")
            .to_string()
    }

    /// The story's current state.
    fn state_of(&self, id: &str) -> String {
        let out = self.story(self.path(), &["show", id, "--json"]);
        let value: serde_json::Value = serde_json::from_str(&out).expect("`story show --json`");
        value["story"]["story"]["state"]
            .as_str()
            .expect("a state")
            .to_string()
    }

    /// Commits `message` on a new branch and merges it back into `main` with
    /// `--no-ff`, which is what sets `ORIG_HEAD` and fires `post-merge`.
    fn merge_branch_with_message(&self, branch: &str, message: &str) {
        self.git(&["checkout", "-q", "-b", branch]);
        self.git(&["commit", "-q", "--allow-empty", "-m", message]);
        self.git(&["checkout", "-q", "main"]);
        self.git(&[
            "merge",
            "-q",
            "--no-ff",
            "-m",
            &format!("Merge {branch}"),
            branch,
        ]);
    }
}

/// `PATH` with the directory holding the binary under test in front.
fn path_with_binary_dir() -> std::ffi::OsString {
    let dir = story_binary()
        .parent()
        .expect("the binary has a parent directory")
        .to_path_buf();
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut entries: Vec<PathBuf> = vec![dir];
    entries.extend(std::env::split_paths(&existing));
    std::env::join_paths(entries).expect("joining PATH")
}

// ---------------------------------------------------------------------------
// SH-56: the post-merge hook and the body trailer
// ---------------------------------------------------------------------------

/// The bug, exactly as SH-56 reproduces it: a branch commit carrying a
/// `Closes SH-N` **trailer**, merged to main.
#[test]
fn a_body_trailer_closes_its_story_on_merge() {
    let repo = HookRepo::new();
    let id = repo.new_story("Closed by a trailer");
    assert_eq!(repo.state_of(&id), "todo");

    repo.merge_branch_with_message("feature", &format!("fix(x): something\n\nCloses {id}"));

    assert_eq!(
        repo.state_of(&id),
        "done",
        "a `Closes {id}` trailer in the commit BODY must close the story — the \
         hook read subjects, and this project's own commit style puts the \
         reference in the body, so following the style guide guaranteed the \
         feature did nothing"
    );
}

/// A subject-line reference still works. The fix widens the scan; it must not
/// move it.
#[test]
fn a_subject_reference_still_closes_its_story() {
    let repo = HookRepo::new();
    let id = repo.new_story("Closed from the subject");

    repo.merge_branch_with_message("feature", &format!("fix: closes {id}"));

    assert_eq!(repo.state_of(&id), "done");
}

/// One commit naming two stories closes both — SH-56's own "verify" note.
#[test]
fn a_message_naming_two_stories_closes_both() {
    let repo = HookRepo::new();
    let first = repo.new_story("First");
    let second = repo.new_story("Second");

    repo.merge_branch_with_message(
        "feature",
        &format!("feat: the work\n\nCloses {first}\nFixes {second}\n"),
    );

    assert_eq!(repo.state_of(&first), "done");
    assert_eq!(repo.state_of(&second), "done");
}

/// `Resolves` and mixed case, since the pattern claims to accept them.
#[test]
fn the_verb_may_be_resolves_and_the_case_does_not_matter() {
    let repo = HookRepo::new();
    let id = repo.new_story("Resolved");

    repo.merge_branch_with_message("feature", &format!("chore: tidy\n\nRESOLVES {id}"));

    assert_eq!(repo.state_of(&id), "done");
}

/// A bare mention is not a closing reference. Widening `%s` to `%B` exposes far
/// more text to the pattern, so what the pattern *rejects* matters more than it
/// did.
#[test]
fn a_bare_mention_in_the_body_does_not_close_anything() {
    let repo = HookRepo::new();
    let id = repo.new_story("Merely mentioned");

    repo.merge_branch_with_message(
        "feature",
        &format!("refactor: something\n\nThis is groundwork for {id}, which stays open."),
    );

    // `in-progress`, not `todo`: the *post-commit* hook links any mention and
    // moves the story out of the default open state. What must not have
    // happened is the close.
    assert_eq!(
        repo.state_of(&id),
        "in-progress",
        "only closes/fixes/resolves close a story; a body full of prose must \
         link it and leave it open"
    );
}

/// The branch guard: merging into anything but `main` or `master` does nothing.
#[test]
fn a_merge_on_a_side_branch_closes_nothing() {
    let repo = HookRepo::new();
    let id = repo.new_story("Not on main");

    // Two branches off main, merged into each other rather than into main.
    repo.git(&["checkout", "-q", "-b", "integration"]);
    repo.git(&["checkout", "-q", "-b", "feature"]);
    repo.git(&[
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        &format!("fix: work\n\nCloses {id}"),
    ]);
    repo.git(&["checkout", "-q", "integration"]);
    repo.git(&["merge", "-q", "--no-ff", "-m", "Merge feature", "feature"]);

    // Linked by the post-commit hook, as any commit naming a story is; not
    // closed, because the merge did not land on main.
    assert_eq!(
        repo.state_of(&id),
        "in-progress",
        "the hook only auto-closes on main or master — a merge into a topic \
         branch is not a release"
    );
}

// ---------------------------------------------------------------------------
// The post-commit hook, from every place a commit can be made
// ---------------------------------------------------------------------------

/// The post-commit hook runs `commit-sync`, which now scans bodies. This is
/// that path end to end: `git commit` alone links the story.
#[test]
fn the_post_commit_hook_links_a_story_named_in_the_body() {
    let repo = HookRepo::new();
    let id = repo.new_story("Linked by the hook");

    repo.git(&[
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "feat: the work",
        "-m",
        &format!("Part of {id}"),
    ]);

    let shown = repo.story(repo.path(), &["show", &id]);
    assert!(
        shown.contains("[git] "),
        "the post-commit hook must have linked the commit: {shown}"
    );
    assert_eq!(
        repo.state_of(&id),
        "in-progress",
        "and moved the story out of the default open state"
    );
}

/// A commit made from a subdirectory reaches the same project.
///
/// The root-resolution family, extended to the hook path: git runs a hook from
/// the repository root, but `story` resolves the project from *its own*
/// working directory, and nothing guarantees the two agree until it is
/// asserted.
#[test]
fn a_commit_from_a_subdirectory_links_the_same_project() {
    let repo = HookRepo::new();
    let id = repo.new_story("Linked from below");
    let deep = repo.path().join("src/inner");
    std::fs::create_dir_all(&deep).expect("creating a subdirectory");

    repo.git_in(
        &deep,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "feat: from a subdirectory",
            "-m",
            &format!("Part of {id}"),
        ],
    );

    let shown = repo.story(repo.path(), &["show", &id]);
    assert!(
        shown.contains("[git] "),
        "a commit made two directories down must link the same project: {shown}"
    );
}

/// A commit made in a **linked worktree** reaches the same project.
///
/// The headline property of the rearchitecture, asked of the hook path.
/// Before the flip each checkout carried its own `.storyhook/`, so a hook
/// firing in a worktree updated a private copy of the tracker and the main
/// checkout never saw it. One store, one project, however many checkouts.
#[test]
fn a_commit_in_a_linked_worktree_links_the_same_project() {
    let repo = HookRepo::new();
    let id = repo.new_story("Linked from a worktree");

    // The pointer file is what a second checkout inherits, so it has to be
    // committed before the worktree is cut — exactly as `worktree_truth.rs`
    // does it.
    repo.git(&["add", ".storyhook.toml"]);
    repo.git(&["commit", "-qm", "track the tracker"]);
    let worktree = repo.path().join(".claude/worktrees/a");
    repo.git(&[
        "worktree",
        "add",
        "-q",
        "--no-track",
        "-b",
        "worktree-a",
        worktree.to_str().expect("a UTF-8 fixture path"),
        "HEAD",
    ]);

    // A linked worktree's `.git` is a file, so the managed hooks live in the
    // main repository's hooks directory and fire for it too.
    repo.git_in(
        &worktree,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "feat: from a worktree",
            "-m",
            &format!("Part of {id}"),
        ],
    );

    let shown = repo.story(repo.path(), &["show", &id]);
    assert!(
        shown.contains("[git] "),
        "a commit made in a linked worktree must reach the same project as one \
         made in the main checkout — two checkouts, one tracker: {shown}"
    );
}
