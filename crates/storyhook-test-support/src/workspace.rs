//! A merged (or unmerged) story workspace on disk: a bare origin, a clone
//! registered as the project checkout, and one linked worktree on a story
//! branch — the topology `story cleanup` and the verifier's reap operate on.
//!
//! Deliberately returns only paths and strings, never a storyhook type: the
//! lib's own unit tests reach this crate through a dev-dependency cycle, where
//! a `StoryCleanupLease` built here would be a *different* type from the one
//! `src/service/cleanup.rs` names. The caller assembles the lease.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use crate::scratch::scratch_dir_named;

/// One story workspace: origin, checkout and a linked worktree.
pub struct StoryWorkspace {
    /// Root holding every path below; removed on drop.
    pub root: TempDir,
    /// The bare origin repository.
    pub origin: PathBuf,
    /// The clone that stands in for the project's registered checkout.
    pub checkout: PathBuf,
    /// The linked worktree, canonical path.
    pub worktree: PathBuf,
    /// The story branch the worktree is checked out on.
    pub branch: String,
    /// The story id the worktree is named for.
    pub story_id: String,
}

impl StoryWorkspace {
    /// Builds the workspace for `story_id`, with the story branch merged into
    /// `dev` — the origin's default branch — when `merged` is true. The
    /// worktree carries a 4096-byte ignored `target/artifact`, so a byte count
    /// has something to measure.
    #[must_use]
    pub fn new(story_id: &str, merged: bool) -> Self {
        let root = scratch_dir_named("story-workspace-");
        let origin = root.path().join("origin.git");
        let checkout = root.path().join("repo");
        let worktree = root.path().join(story_id);
        let branch = format!("worktree-{story_id}");
        git(root.path(), &["init", "--bare", &origin.to_string_lossy()]);
        git(
            root.path(),
            &[
                "clone",
                &origin.to_string_lossy(),
                &checkout.to_string_lossy(),
            ],
        );
        git(&checkout, &["config", "user.name", "Workspace Fixture"]);
        git(
            &checkout,
            &["config", "user.email", "workspace@example.test"],
        );
        fs::write(checkout.join("README"), "base").unwrap();
        fs::write(checkout.join(".gitignore"), "target/\n").unwrap();
        git(&checkout, &["add", "README", ".gitignore"]);
        git(&checkout, &["commit", "-m", "base"]);
        git(&checkout, &["branch", "-M", "dev"]);
        git(&checkout, &["push", "-u", "origin", "dev"]);
        git(&origin, &["symbolic-ref", "HEAD", "refs/heads/dev"]);
        git(&checkout, &["remote", "set-head", "origin", "dev"]);
        git(
            &checkout,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                &worktree.to_string_lossy(),
                "dev",
            ],
        );
        fs::write(worktree.join("work"), "story work").unwrap();
        fs::create_dir(worktree.join("target")).unwrap();
        fs::write(worktree.join("target/artifact"), vec![0_u8; 4096]).unwrap();
        git(&worktree, &["add", "work"]);
        git(&worktree, &["commit", "-m", "story work"]);
        git(&worktree, &["push", "-u", "origin", &branch]);
        if merged {
            git(
                &checkout,
                &["merge", "--no-ff", "-m", "merge story", &branch],
            );
            git(&checkout, &["push", "origin", "dev"]);
        }
        Self {
            origin,
            checkout: checkout.canonicalize().unwrap(),
            worktree: worktree.canonicalize().unwrap(),
            branch,
            story_id: story_id.into(),
            root,
        }
    }

    /// Whether `origin` still holds the story branch.
    #[must_use]
    pub fn origin_has_branch(&self) -> bool {
        let output = git_command(&self.checkout)
            .args(["ls-remote", "--heads", "origin", &self.branch])
            .output()
            .expect("running git ls-remote");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        !output.stdout.is_empty()
    }

    /// Whether the checkout still holds the local story branch.
    #[must_use]
    pub fn local_branch_exists(&self) -> bool {
        git_command(&self.checkout)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{}", self.branch),
            ])
            .status()
            .expect("running git show-ref")
            .success()
    }

    /// The worktree's private git directory, where dispatch writes its
    /// cleanup-lease marker.
    #[must_use]
    pub fn worktree_git_dir(&self) -> PathBuf {
        let output = git_command(&self.worktree)
            .args(["rev-parse", "--absolute-git-dir"])
            .output()
            .expect("running git rev-parse");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        PathBuf::from(String::from_utf8_lossy(&output.stdout).trim())
    }
}

/// The production-sanitized `git`, so no inherited `GIT_DIR` or credential
/// prompt can reach a fixture (the `project.rs` rule).
fn git_command(cwd: &Path) -> Command {
    let mut command = storyhook::env::git_env::command(cwd);
    command.env("GIT_TERMINAL_PROMPT", "0");
    command
}

fn git(cwd: &Path, args: &[&str]) {
    let output = git_command(cwd)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("spawning git {}: {error}", args.join(" ")));
    assert!(
        output.status.success(),
        "git {} in {}: {}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}
