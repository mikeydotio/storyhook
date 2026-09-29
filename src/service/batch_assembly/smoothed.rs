//! The smoothed last member of a batch (SH-834; spec B8, council decision
//! D1 on SH-834): its conflict classified again on the exact tip, and, when
//! it is insertion-only on allowlisted paths, united into one merge commit.

use super::{Assembly, AssemblyMember, LABEL, merge_message, merge_onto, record_merge};
use crate::domain::conflict_smoothing::STRATEGY;
use crate::error::AppError;
use crate::process::Cancellation;
use crate::service::batch_smoothing::{
    Classification, POINTER, SmoothedFile, admits_all, classify, policy_from_pointer,
};
use crate::service::private_objects::repository_query;
use crate::service::trial_merge::{
    BlobSource, TrialMerge, answer_oid, blob_answer, entry_answer, entry_spec, require_pinned,
};
use crate::store::{BatchResolution, ResolvedFile};
use std::path::Path;

/// The last member of a batch, merged with smoothing (SH-834; spec B8,
/// council decision D1 on SH-834).
pub struct SmoothedMerge<'a> {
    /// The repository whose object store receives the merge.
    pub repository: &'a Path,
    /// The batch branch, named in the merge commit's subject.
    pub branch: &'a str,
    /// The batch id, named in the merge commit's trailers.
    pub batch: &'a str,
    /// The base the batch branch starts from: the only commit whose
    /// `.storyhook.toml` states the allowlist.
    pub base: &'a str,
    /// The members already merged, in queue order.
    pub earlier: &'a [AssemblyMember],
    /// The member to merge last.
    pub member: &'a AssemblyMember,
}

/// How the smoothed last member merged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LastMerge {
    /// It merged cleanly after all, because a member it conflicted with
    /// left the batch: an ordinary merge commit, no resolution.
    Clean,
    /// Its conflict was united; the merge commit carries this resolution.
    Resolved(BatchResolution),
    /// Its conflict, merged again, is not one the verifier may smooth, for
    /// the reason given. No blob, tree or commit of a resolution was
    /// written, and the assembly is unchanged.
    Refused(String),
}

/// Merges `merge.member` onto `assembly`'s tip as the batch's last merge
/// commit, and appends it unless it is refused.
///
/// The conflict is classified again here, on the exact tip, with the
/// allowlist read from the base: a conflict that is not insertion-only on
/// allowlisted paths is refused before anything is written, whatever the
/// preview said. A smoothable conflict becomes one two-parent merge commit
/// whose tree is Git's conflicted tree with only the smoothed blobs
/// replaced by their union, which is checked before the commit. Its message
/// names the member, the members it conflicted with and every path.
pub fn merge_smoothed(
    merge: &SmoothedMerge<'_>,
    assembly: &mut Assembly,
    cancellation: &Cancellation,
) -> Result<LastMerge, AppError> {
    require_pinned(merge.base, LABEL)?;
    let cancelled = || cancellation.is_cancelled();
    let tip = assembly.tip.clone();
    let shape = match merge_onto(merge.repository, &tip, merge.member, &cancelled)? {
        TrialMerge::Clean { tree } => {
            let message = merge_message(merge.member, merge.branch);
            let commit = record_merge(
                merge.repository,
                &tip,
                merge.member,
                &tree,
                &message,
                &cancelled,
            )?;
            assembly.push(commit, tree);
            return Ok(LastMerge::Clean);
        }
        TrialMerge::Conflict { shape, .. } => shape,
    };
    let mut blobs = RepositoryBlobs {
        repository: merge.repository,
        cancelled: &cancelled,
    };
    let policy = match blobs
        .file(merge.base, POINTER)
        .map_err(|error| format!("reading the base's {POINTER} failed: {error}"))
        .and_then(|raw| policy_from_pointer(raw.as_deref()))
    {
        Ok(policy) => policy,
        Err(why) => return Ok(LastMerge::Refused(why)),
    };
    let files = match classify(&shape, &mut blobs) {
        Classification::UnionSmoothable(files) if admits_all(&policy, &files) => files,
        Classification::UnionSmoothable(_) => {
            return Ok(LastMerge::Refused(format!(
                "a conflicted path of {} is not on the base's [batch] smooth list",
                shape.paths().join(", ")
            )));
        }
        Classification::AgentCandidate(why) | Classification::NotSmoothable(why) => {
            return Ok(LastMerge::Refused(why));
        }
    };
    let resolved = write_resolved_tree(merge.repository, &shape.tree, &files, &cancelled)?;
    let conflicted_with = conflicted_with(merge, assembly, &files, &cancelled)?;
    let resolution = BatchResolution {
        strategy: STRATEGY.to_owned(),
        conflicted_with,
        auto_merge_tree: shape.tree.clone(),
        files: files
            .iter()
            .zip(resolved.blobs)
            .map(|(file, blob)| ResolvedFile {
                path: file.path.clone(),
                base: file.base.clone(),
                ours: file.ours.clone(),
                theirs: file.theirs.clone(),
                resolved: blob,
            })
            .collect(),
    };
    let message = resolution_message(merge, &resolution);
    let commit = record_merge(
        merge.repository,
        &tip,
        merge.member,
        &resolved.tree,
        &message,
        &cancelled,
    )?;
    assembly.push(commit, resolved.tree);
    Ok(LastMerge::Resolved(resolution))
}

/// The merge commit message of a resolved member: the ordinary subject,
/// what was resolved and how, and trailers naming the batch, the strategy,
/// the members and every path, each path C-quoted.
fn resolution_message(merge: &SmoothedMerge<'_>, resolution: &BatchResolution) -> String {
    let with = if resolution.conflicted_with.is_empty() {
        "an earlier member".to_owned()
    } else {
        resolution.conflicted_with.join(", ")
    };
    let mut message = format!(
        "{}\n\nAutomated conflict resolution ({}): {} and {with} added lines at the same \
         place in the files below, and neither changed a line of the base. This merge keeps \
         both additions, the earlier member's first. No model wrote it; the verification \
         gate certifies the merged tree.\n\nStoryhook-Batch: {}\nStoryhook-Resolution: {}\n",
        merge_message(merge.member, merge.branch),
        resolution.strategy,
        merge.member.story_id,
        merge.batch,
        resolution.strategy,
    );
    for member in &resolution.conflicted_with {
        message.push_str(&format!("Storyhook-Conflicted-With: {member}\n"));
    }
    for file in &resolution.files {
        message.push_str(&format!(
            "Storyhook-Resolved-File: {}\n",
            c_quote(&file.path)
        ));
    }
    message
}

/// `path` in double quotes with `\\` and `"` escaped: a path the deny
/// floor passed is printable ASCII without a backtick, so nothing else needs
/// escaping, and no path can end a trailer or begin another.
#[must_use]
pub fn c_quote(path: &str) -> String {
    let mut quoted = String::with_capacity(path.len() + 2);
    quoted.push('"');
    for c in path.chars() {
        if matches!(c, '"' | '\\') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

/// The resolved tree and the blob written for each file, in order.
struct ResolvedTree {
    tree: String,
    blobs: Vec<String>,
}

/// Writes each file's union as a blob in `repository`'s object store and
/// the tree that is `conflicted` with exactly those blobs replaced, through
/// a private index; then checks that the two trees differ at exactly those
/// paths.
fn write_resolved_tree(
    repository: &Path,
    conflicted: &str,
    files: &[SmoothedFile],
    cancelled: &dyn Fn() -> bool,
) -> Result<ResolvedTree, AppError> {
    let scratch = tempfile::Builder::new()
        .prefix("storyhook-batch-resolution-")
        .tempdir()
        .map_err(|error| AppError::Storage(format!("{LABEL}: a resolution directory: {error}")))?;
    let mut blobs = Vec::with_capacity(files.len());
    for (index, file) in files.iter().enumerate() {
        let content = scratch.path().join(format!("{index}.blob"));
        std::fs::write(&content, &file.resolved)
            .map_err(|error| AppError::Storage(format!("{LABEL}: writing a union: {error}")))?;
        let content = content.to_string_lossy();
        let written = run(
            repository,
            &["hash-object", "-w", "--no-filters", "--", &content],
            &[],
            cancelled,
        )?;
        blobs.push(answer_oid(&written, LABEL, "a resolved blob")?);
    }
    let index = scratch.path().join("index");
    let index = index.to_string_lossy();
    let env = [("GIT_INDEX_FILE", index.as_ref())];
    run(repository, &["read-tree", conflicted], &env, cancelled)?;
    for (file, blob) in files.iter().zip(&blobs) {
        let entry = format!("100644,{blob},{}", file.path);
        run(
            repository,
            &["update-index", "--cacheinfo", &entry],
            &env,
            cancelled,
        )?;
    }
    let tree = answer_oid(
        &run(repository, &["write-tree"], &env, cancelled)?,
        LABEL,
        "the resolved tree",
    )?;
    let changed = run(
        repository,
        &["diff-tree", "-r", "--name-only", "-z", conflicted, &tree],
        &[],
        cancelled,
    )?;
    let mut changed: Vec<String> = changed
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect();
    changed.sort_unstable();
    let mut expected: Vec<String> = files.iter().map(|file| file.path.clone()).collect();
    expected.sort_unstable();
    if changed != expected {
        return Err(AppError::Storage(format!(
            "{LABEL}: the resolved tree differs from Git's auto-merge at {changed:?}, not \
             exactly at the smoothed paths {expected:?}"
        )));
    }
    Ok(ResolvedTree { tree, blobs })
}

/// The earlier members whose merge changed a smoothed path: each prefix
/// tree against the one before it (the base's for the first member).
fn conflicted_with(
    merge: &SmoothedMerge<'_>,
    assembly: &Assembly,
    files: &[SmoothedFile],
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<String>, AppError> {
    let entry = |treeish: &str, path: &str| -> Result<Option<String>, AppError> {
        let spec = entry_spec(treeish, path)?;
        let result = repository_query(
            merge.repository,
            LABEL,
            &["rev-parse", "--verify", "--quiet", &spec],
            &[],
            &[1],
            cancelled,
        )?;
        entry_answer(&result, LABEL, &spec)
    };
    let mut before = format!("{}^{{tree}}", merge.base);
    let mut with = Vec::new();
    for (member, tree) in merge.earlier.iter().zip(&assembly.trees) {
        let mut changed = false;
        for file in files {
            if entry(&before, &file.path)? != entry(tree, &file.path)? {
                changed = true;
                break;
            }
        }
        if changed {
            with.push(member.story_id.clone());
        }
        before.clone_from(tree);
    }
    Ok(with)
}

/// Runs one Git plumbing command in `repository`; a nonzero exit is an
/// error naming it.
fn run(
    repository: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<u8>, AppError> {
    let result = repository_query(repository, LABEL, args, env, &[], cancelled)?;
    if !result.status.success() {
        return Err(AppError::Storage(format!(
            "{LABEL} git {}: {}",
            args.first().copied().unwrap_or_default(),
            String::from_utf8_lossy(&result.stderr).trim()
        )));
    }
    Ok(result.stdout)
}

/// Blobs and committed files in the repository's own object store.
struct RepositoryBlobs<'a> {
    repository: &'a Path,
    cancelled: &'a dyn Fn() -> bool,
}

impl BlobSource for RepositoryBlobs<'_> {
    fn blob(&mut self, oid: &str) -> Result<Vec<u8>, AppError> {
        require_pinned(oid, LABEL)?;
        let result = repository_query(
            self.repository,
            LABEL,
            &["cat-file", "blob", oid],
            &[],
            &[],
            self.cancelled,
        )?;
        blob_answer(&result, LABEL, oid)
    }

    fn file(&mut self, treeish: &str, path: &str) -> Result<Option<Vec<u8>>, AppError> {
        require_pinned(treeish, LABEL)?;
        let spec = entry_spec(treeish, path)?;
        let result = repository_query(
            self.repository,
            LABEL,
            &["rev-parse", "--verify", "--quiet", &spec],
            &[],
            &[1],
            self.cancelled,
        )?;
        match entry_answer(&result, LABEL, &spec)? {
            Some(oid) => self.blob(&oid).map(Some),
            None => Ok(None),
        }
    }
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
    fn a_cancelled_smoothed_merge_runs_no_git_and_appends_nothing() {
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
        let member = AssemblyMember {
            story_id: "SH-2".into(),
            branch: "worktree-SH-2".into(),
            commit: git(root.path(), &["rev-parse", "HEAD"]),
        };
        let objects = git(root.path(), &["count-objects", "-v"]);
        let mut assembly = Assembly {
            tip: base.clone(),
            tree: String::new(),
            merges: vec![base.clone()],
            trees: vec![base.clone()],
        };
        let before = assembly.clone();
        let cancelled = Cancellation::default();
        cancelled.cancel();

        let merged = merge_smoothed(
            &SmoothedMerge {
                repository: root.path(),
                branch: "storyhook/verify-batch/0123456789ab",
                batch: "0123456789ab",
                base: &base,
                earlier: &[],
                member: &member,
            },
            &mut assembly,
            &cancelled,
        );

        assert!(merged.is_err(), "{merged:?}");
        assert_eq!(assembly, before);
        assert_eq!(git(root.path(), &["count-objects", "-v"]), objects);
    }

    #[test]
    fn a_trailer_path_is_quoted_so_it_cannot_end_its_line_or_forge_another() {
        assert_eq!(c_quote("docs/spec.md"), "\"docs/spec.md\"");
        assert_eq!(c_quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }
}
