//! Trial merges for verification batching (SH-830; spec B1 and B4).
//!
//! A trial merge answers whether a story's head merges cleanly onto a base
//! plus the batch members already accepted. Each accepted member is recorded
//! as a merge commit whose parents are the batch so far and the member, which
//! is how B4 assembles a batch branch (`--no-ff` merges in queue order), so
//! the next story is tried against exactly what the batch would hold. The
//! production merger works in private object storage
//! ([`super::private_objects`]): no checkout, index, ref or repository object
//! changes.

use super::private_objects::PrivateObjects;
use super::project_fault::is_pinned_oid;
use crate::error::AppError;
use crate::process::Cancellation;
use std::path::Path;
use std::time::Instant;

/// Names trial merges in every Git error they report.
const LABEL: &str = "trial merge";

/// Exit codes that are answers rather than failures: `rev-parse --verify
/// --quiet` exits 1 for a name that resolves to nothing, and `merge-tree
/// --write-tree` exits 1 for a conflicted merge.
const ANSWERS: &[i32] = &[1];

/// The environment of every trial merge command. A partial clone must never
/// reach the network for a missing object; a missing object fails the trial.
const QUERY_ENV: [(&str, &str); 1] = [("GIT_NO_LAZY_FETCH", "1")];

/// Identity and dates of every trial merge commit. Fixed, so the same inputs
/// always give the same commit, and independent of the Git configuration the
/// daemon's cleared environment may lack.
const COMMIT_ENV: [(&str, &str); 7] = [
    ("GIT_NO_LAZY_FETCH", "1"),
    ("GIT_AUTHOR_NAME", "storyhook"),
    ("GIT_AUTHOR_EMAIL", "storyhook@localhost"),
    ("GIT_AUTHOR_DATE", "1970-01-01T00:00:00Z"),
    ("GIT_COMMITTER_NAME", "storyhook"),
    ("GIT_COMMITTER_EMAIL", "storyhook@localhost"),
    ("GIT_COMMITTER_DATE", "1970-01-01T00:00:00Z"),
];

/// The answer to one trial merge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrialMerge {
    /// The merge is clean.
    Clean {
        /// The merged tree.
        tree: String,
    },
    /// The merge conflicts.
    Conflict {
        /// Every conflicted path, in Git's order.
        paths: Vec<String>,
    },
}

/// Resolves revisions and merges commits without changing the repository.
pub trait TrialMerger {
    /// The commit `rev` names, or `None` when it names nothing.
    fn resolve(&mut self, rev: &str) -> Result<Option<String>, AppError>;
    /// Merges the commit `head` onto the commit `onto`.
    fn merge(&mut self, onto: &str, head: &str) -> Result<TrialMerge, AppError>;
    /// Records the clean merge of `head` onto `onto`, whose tree is `tree`,
    /// as a merge commit with the parents `onto` and `head`.
    fn commit(&mut self, onto: &str, head: &str, tree: &str) -> Result<String, AppError>;
}

/// The production [`TrialMerger`]: Git plumbing in private object storage.
/// The merge commits it records live only as long as the value.
pub struct PrivateTrialMerger {
    objects: PrivateObjects,
    deadline: Option<Instant>,
    cancellation: Option<Cancellation>,
}

impl PrivateTrialMerger {
    /// Opens private object storage for the repository that holds `checkout`.
    pub fn open(checkout: &Path) -> Result<Self, AppError> {
        Ok(Self {
            objects: PrivateObjects::open(checkout, LABEL, "storyhook-trial-merge-")?,
            deadline: None,
            cancellation: None,
        })
    }

    /// Stops every later Git command at `deadline`.
    #[must_use]
    pub fn with_deadline(mut self, deadline: Instant) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Stops every later Git command as soon as `cancellation` fires.
    #[must_use]
    pub fn with_cancellation(mut self, cancellation: Cancellation) -> Self {
        self.cancellation = Some(cancellation);
        self
    }

    fn query(
        &self,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Result<crate::process::Captured, AppError> {
        let cancelled = || {
            self.cancellation
                .as_ref()
                .is_some_and(Cancellation::is_cancelled)
        };
        self.objects
            .query(args, env, ANSWERS, self.deadline, &cancelled)
    }
}

impl TrialMerger for PrivateTrialMerger {
    fn resolve(&mut self, rev: &str) -> Result<Option<String>, AppError> {
        // Callers name refs and object ids; an option-shaped word is neither.
        if rev.is_empty() || rev.starts_with('-') {
            return Err(AppError::Validation(format!(
                "trial merge cannot resolve {rev:?}"
            )));
        }
        let result = self.query(
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{rev}^{{commit}}"),
            ],
            &QUERY_ENV,
        )?;
        match result.status.code() {
            Some(0) => oid(&result.stdout, rev).map(Some),
            Some(1) => Ok(None),
            _ => Err(AppError::Storage(format!(
                "trial merge could not resolve {rev}: {}",
                String::from_utf8_lossy(&result.stderr).trim()
            ))),
        }
    }

    fn merge(&mut self, onto: &str, head: &str) -> Result<TrialMerge, AppError> {
        pinned(onto)?;
        pinned(head)?;
        let result = self.query(
            &[
                "merge-tree",
                "--write-tree",
                "--name-only",
                "--no-messages",
                "-z",
                onto,
                head,
            ],
            &QUERY_ENV,
        )?;
        merge_answer(&result, LABEL, onto, head)
    }

    fn commit(&mut self, onto: &str, head: &str, tree: &str) -> Result<String, AppError> {
        pinned(onto)?;
        pinned(head)?;
        pinned(tree)?;
        // `--no-gpg-sign` overrides `commit.gpgSign` wherever a Git version
        // lets it reach commit-tree: a signer inside the daemon could prompt,
        // hang to the deadline, or fail.
        let result = self.query(
            &[
                "commit-tree",
                "--no-gpg-sign",
                "-p",
                onto,
                "-p",
                head,
                "-m",
                "storyhook trial merge",
                tree,
            ],
            &COMMIT_ENV,
        )?;
        if !result.status.success() {
            return Err(AppError::Storage(format!(
                "trial merge could not record the merge of {head} onto {onto}: {}",
                String::from_utf8_lossy(&result.stderr).trim()
            )));
        }
        oid(&result.stdout, "the merge commit")
    }
}

/// Reads the answer of `git merge-tree --write-tree --name-only
/// --no-messages -z <onto> <head>`: exit 0 is a clean tree, exit 1 a conflict
/// with its paths, anything else a failure. `label` names the caller.
pub(crate) fn merge_answer(
    result: &crate::process::Captured,
    label: &str,
    onto: &str,
    head: &str,
) -> Result<TrialMerge, AppError> {
    // `-z` output: the tree, then each conflicted path, each NUL-terminated.
    let mut fields = result.stdout.split(|byte| *byte == 0);
    let tree = fields.next().unwrap_or_default();
    match result.status.code() {
        Some(0) => Ok(TrialMerge::Clean {
            tree: answer_oid(tree, label, "the merged tree")?,
        }),
        Some(1) => Ok(TrialMerge::Conflict {
            paths: fields
                .take_while(|path| !path.is_empty())
                .map(|path| String::from_utf8_lossy(path).into_owned())
                .collect(),
        }),
        _ => Err(AppError::Storage(format!(
            "{label} of {head} onto {onto} failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ))),
    }
}

/// Refuses anything but a full object id where Git is handed one.
pub(crate) fn require_pinned(oid: &str, label: &str) -> Result<(), AppError> {
    if is_pinned_oid(oid) {
        Ok(())
    } else {
        Err(AppError::Validation(format!(
            "{label} requires a full object id, not {oid:?}"
        )))
    }
}

/// The object id Git printed for `what`, refused unless it is a full one.
pub(crate) fn answer_oid(bytes: &[u8], label: &str, what: &str) -> Result<String, AppError> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    if !is_pinned_oid(text) {
        return Err(AppError::Storage(format!(
            "{label} Git gave no object id for {what}: {text:?}"
        )));
    }
    Ok(text.to_owned())
}

fn pinned(oid: &str) -> Result<(), AppError> {
    require_pinned(oid, LABEL)
}

fn oid(bytes: &[u8], what: &str) -> Result<String, AppError> {
    answer_oid(bytes, LABEL, what)
}
