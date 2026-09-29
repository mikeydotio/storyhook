//! Assembles a verification batch's branch (SH-831; spec B4).
//!
//! The batch branch starts at the base the trial merges used; each member's
//! head is merged in queue order as a two-parent merge commit (`--no-ff`), so
//! no history is rewritten and every member's own commits stay reachable. The
//! merges are Git plumbing in the repository's own object store, so they can
//! be pushed; no ref, index, HEAD or working tree changes. The commits carry
//! the repository's configured identity, the one the repository's push hook
//! accepts, and are never signed: a signer must not prompt inside the daemon.

use super::private_objects::repository_query;
use super::trial_merge::{MERGE_TREE, TrialMerge, answer_oid, merge_answer, require_pinned};
use crate::error::AppError;
use crate::process::Cancellation;
use std::path::Path;

/// Names batch assembly in every Git error it reports.
const LABEL: &str = "batch assembly";

/// `merge-tree --write-tree` exits 1 for a conflicted merge: an answer.
const MERGE_ANSWERS: &[i32] = &[1];

/// One member to merge, in queue order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssemblyMember {
    /// The story's public id, named in its merge commit's message.
    pub story_id: String,
    /// The story's branch, named in its merge commit's message.
    pub branch: String,
    /// The exact commit to merge.
    pub commit: String,
}

/// The assembled batch branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assembly {
    /// The last merge commit: what the batch branch points at.
    pub tip: String,
    /// The tip's tree.
    pub tree: String,
    /// The merge commit made for each member, in member order: the tip of
    /// each prefix of the batch.
    pub merges: Vec<String>,
    /// Each merge commit's tree, in member order: the tree a gate of that
    /// prefix judges (SH-833).
    pub trees: Vec<String>,
}

/// Merges `members` onto `base` in order in `repository`, for the branch
/// `branch`, and answers the merge commits.
///
/// A conflict is an error naming the member and its paths: the members were
/// chosen because their trial merges onto the same base were clean, so a
/// conflict here means the inputs changed. Stops as soon as `cancellation`
/// fires.
pub fn assemble(
    repository: &Path,
    branch: &str,
    base: &str,
    members: &[AssemblyMember],
    cancellation: &Cancellation,
) -> Result<Assembly, AppError> {
    require_pinned(base, LABEL)?;
    let cancelled = || cancellation.is_cancelled();
    let mut assembly = Assembly {
        tip: base.to_owned(),
        tree: String::new(),
        merges: Vec::with_capacity(members.len()),
        trees: Vec::with_capacity(members.len()),
    };
    for member in members {
        let tree = match merge_onto(repository, &assembly.tip, member, &cancelled)? {
            TrialMerge::Clean { tree } => tree,
            TrialMerge::Conflict { paths, .. } => {
                return Err(AppError::Storage(format!(
                    "{LABEL}: {} ({}) conflicts with the batch so far in {}",
                    member.story_id,
                    member.commit,
                    paths.join(", ")
                )));
            }
        };
        let message = merge_message(member, branch);
        let commit = record_merge(
            repository,
            &assembly.tip,
            member,
            &tree,
            &message,
            &cancelled,
        )?;
        assembly.push(commit, tree);
    }
    if assembly.merges.is_empty() {
        return Err(AppError::Validation(format!(
            "{LABEL} of {branch} has no members to merge"
        )));
    }
    Ok(assembly)
}

impl Assembly {
    /// Appends the merge commit `commit`, whose tree is `tree`, as the new
    /// tip.
    fn push(&mut self, commit: String, tree: String) {
        self.merges.push(commit.clone());
        self.trees.push(tree.clone());
        self.tip = commit;
        self.tree = tree;
    }
}

/// Merges `member` onto the commit `tip` in `repository`'s object store.
fn merge_onto(
    repository: &Path,
    tip: &str,
    member: &AssemblyMember,
    cancelled: &dyn Fn() -> bool,
) -> Result<TrialMerge, AppError> {
    require_pinned(&member.commit, LABEL)?;
    let mut args = MERGE_TREE.to_vec();
    args.extend([tip, member.commit.as_str()]);
    let merged = repository_query(repository, LABEL, &args, &[], MERGE_ANSWERS, cancelled)?;
    merge_answer(&merged, LABEL, tip, &member.commit)
}

/// The subject of `member`'s merge commit on the batch branch `branch`.
fn merge_message(member: &AssemblyMember, branch: &str) -> String {
    format!(
        "Merge {} ({}) into {branch}",
        member.story_id, member.branch
    )
}

/// Records the merge of `member` onto `tip`, whose tree is `tree`, as a
/// two-parent commit with the repository's configured identity, never
/// signed, and answers it.
fn record_merge(
    repository: &Path,
    tip: &str,
    member: &AssemblyMember,
    tree: &str,
    message: &str,
    cancelled: &dyn Fn() -> bool,
) -> Result<String, AppError> {
    let committed = repository_query(
        repository,
        LABEL,
        &[
            "commit-tree",
            "--no-gpg-sign",
            "-p",
            tip,
            "-p",
            &member.commit,
            "-m",
            message,
            tree,
        ],
        &[],
        &[],
        cancelled,
    )?;
    if !committed.status.success() {
        return Err(AppError::Storage(format!(
            "{LABEL} could not record the merge of {} ({}): {}",
            member.story_id,
            member.commit,
            String::from_utf8_lossy(&committed.stderr).trim()
        )));
    }
    answer_oid(&committed.stdout, LABEL, "the merge commit")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) -> String {
        let output = crate::env::git_env::command(root)
            .args(args)
            .output()
            .expect("fixture: running git");
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    #[test]
    fn a_cancelled_assembly_runs_no_git_and_writes_no_object() {
        let root = storyhook_test_support::scratch_dir();
        git(root.path(), &["init", "-q"]);
        git(root.path(), &["config", "user.name", "t"]);
        git(root.path(), &["config", "user.email", "t@t"]);
        git(
            root.path(),
            &["commit", "-q", "--allow-empty", "-m", "base"],
        );
        let base = git(root.path(), &["rev-parse", "HEAD"]);
        git(
            root.path(),
            &["commit", "-q", "--allow-empty", "-m", "member"],
        );
        let member = git(root.path(), &["rev-parse", "HEAD"]);
        let objects = git(root.path(), &["count-objects", "-v"]);
        let cancelled = Cancellation::default();
        cancelled.cancel();

        let result = assemble(
            root.path(),
            "storyhook/verify-batch/0123456789ab",
            &base,
            &[AssemblyMember {
                story_id: "SH-1".into(),
                branch: "worktree-SH-1".into(),
                commit: member,
            }],
            &cancelled,
        );

        assert!(result.is_err(), "{result:?}");
        assert_eq!(git(root.path(), &["count-objects", "-v"]), objects);
    }
}
