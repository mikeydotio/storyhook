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
        /// Every conflicted path, in Git's order, each once.
        paths: Vec<String>,
        /// What Git reported about the conflict (SH-834).
        shape: ConflictShape,
    },
}

/// A conflicted merge as `merge-tree --write-tree -z` reports it: the tree
/// it wrote, the index entries of every conflicted path, and its
/// informational records.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConflictShape {
    /// The merged tree, each conflicted file written with diff3 markers.
    pub tree: String,
    /// Every index entry of a conflicted path, in Git's order.
    pub stages: Vec<StageEntry>,
    /// Every informational record, in Git's order.
    pub records: Vec<ConflictRecord>,
}

impl ConflictShape {
    /// Every conflicted path, in Git's order, each once: what
    /// `merge-tree --name-only` lists.
    #[must_use]
    pub fn paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = Vec::new();
        for entry in &self.stages {
            if !paths.contains(&entry.path) {
                paths.push(entry.path.clone());
            }
        }
        paths
    }
}

/// One index entry of a conflicted path: `<mode> <object> <stage> <path>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageEntry {
    /// The entry's mode, such as `100644`.
    pub mode: String,
    /// The entry's object id.
    pub oid: String,
    /// 1 for the merge base, 2 for the side merged onto, 3 for the side
    /// merged in.
    pub stage: u8,
    /// The path, unquoted.
    pub path: String,
}

/// One informational record of a merge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictRecord {
    /// Git's stable conflict type, such as `Auto-merging` or
    /// `CONFLICT (contents)`.
    pub kind: String,
    /// The paths (or branch names) the record names.
    pub paths: Vec<String>,
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
    /// The blobs its merges wrote and read, for conflict smoothing (SH-834);
    /// `None`, the default, when this merger cannot read blobs, which
    /// smooths nothing.
    fn blobs(&mut self) -> Option<&mut dyn BlobSource> {
        None
    }
}

/// Reads blobs and committed files, for conflict smoothing (SH-834).
pub trait BlobSource {
    /// The content of the blob `oid`, a full object id.
    fn blob(&mut self, oid: &str) -> Result<Vec<u8>, AppError>;
    /// The content of the file at `path` in the commit or tree `treeish` (a
    /// full object id), or `None` when it has no entry there.
    fn file(&mut self, treeish: &str, path: &str) -> Result<Option<Vec<u8>>, AppError>;
}

/// The production [`TrialMerger`]: Git plumbing in private object storage.
/// The merge commits it records live only as long as the value.
pub struct PrivateTrialMerger {
    objects: PrivateObjects,
    deadline: Option<Instant>,
    cancellation: Option<Cancellation>,
}

impl PrivateTrialMerger {
    /// Opens a private trial under one caller deadline, including Git setup.
    pub(crate) fn open_controlled(
        checkout: &Path,
        deadline: Instant,
        cancellation: Cancellation,
    ) -> Result<Self, AppError> {
        Ok(Self {
            objects: PrivateObjects::open_controlled(
                checkout,
                LABEL,
                "storyhook-trial-merge-",
                Some(deadline),
                &|| cancellation.is_cancelled(),
            )?,
            deadline: Some(deadline),
            cancellation: Some(cancellation),
        })
    }

    /// Explicit cleanup receipt for native diagnostic/integration ownership.
    pub(crate) fn close(self) -> Result<(), AppError> {
        self.objects.close()
    }
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
        let cancelled = || {
            self.cancellation
                .as_ref()
                .is_some_and(Cancellation::is_cancelled)
        };
        let result = self
            .objects
            .merge([onto, head], true, self.deadline, &cancelled)?;
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

    fn blobs(&mut self) -> Option<&mut dyn BlobSource> {
        Some(self)
    }
}

impl BlobSource for PrivateTrialMerger {
    fn blob(&mut self, oid: &str) -> Result<Vec<u8>, AppError> {
        pinned(oid)?;
        let result = self.query(&["cat-file", "blob", oid], &QUERY_ENV)?;
        blob_answer(&result, LABEL, oid)
    }

    fn file(&mut self, treeish: &str, path: &str) -> Result<Option<Vec<u8>>, AppError> {
        pinned(treeish)?;
        let Some(oid) = self.entry(treeish, path)? else {
            return Ok(None);
        };
        self.blob(&oid).map(Some)
    }
}

impl PrivateTrialMerger {
    /// The object at `path` in `treeish`, or `None` when it has none.
    fn entry(&self, treeish: &str, path: &str) -> Result<Option<String>, AppError> {
        let spec = entry_spec(treeish, path)?;
        let result = self.query(&["rev-parse", "--verify", "--quiet", &spec], &QUERY_ENV)?;
        entry_answer(&result, LABEL, &spec)
    }
}

/// `<treeish>:<path>`, refused for a path that is empty, absolute or
/// option-shaped: callers pass paths Git reported or constants.
pub(crate) fn entry_spec(treeish: &str, path: &str) -> Result<String, AppError> {
    if path.is_empty() || path.starts_with('/') || path.starts_with('-') || path.contains('\0') {
        return Err(AppError::Validation(format!(
            "a path in a tree must be relative, not {path:?}"
        )));
    }
    Ok(format!("{treeish}:{path}"))
}

/// Reads `rev-parse --verify --quiet <spec>`: exit 0 is the object, exit 1
/// no entry, anything else a failure.
pub(crate) fn entry_answer(
    result: &crate::process::Captured,
    label: &str,
    spec: &str,
) -> Result<Option<String>, AppError> {
    match result.status.code() {
        Some(0) => answer_oid(&result.stdout, label, spec).map(Some),
        Some(1) => Ok(None),
        _ => Err(AppError::Storage(format!(
            "{label} could not read {spec}: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ))),
    }
}

/// Reads `cat-file blob <oid>`: its output on success, a failure otherwise
/// (a missing object, or an object that is not a blob).
pub(crate) fn blob_answer(
    result: &crate::process::Captured,
    label: &str,
    oid: &str,
) -> Result<Vec<u8>, AppError> {
    if result.status.success() {
        Ok(result.stdout.clone())
    } else {
        Err(AppError::Storage(format!(
            "{label} could not read the blob {oid}: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        )))
    }
}

/// Reads an isolated NUL-delimited merge answer: exit 0 is a clean
/// tree, exit 1 a conflict with its shape, anything else a failure. `label`
/// names the caller.
pub(crate) fn merge_answer(
    result: &crate::process::Captured,
    label: &str,
    onto: &str,
    head: &str,
) -> Result<TrialMerge, AppError> {
    match result.status.code() {
        Some(0) => {
            let tree = result.stdout.split(|byte| *byte == 0).next();
            Ok(TrialMerge::Clean {
                tree: answer_oid(tree.unwrap_or_default(), label, "the merged tree")?,
            })
        }
        Some(1) => {
            let shape = parse_conflict(&result.stdout).map_err(|why| {
                AppError::Storage(format!(
                    "{label} of {head} onto {onto} reported a conflict Git's output does \
                     not describe: {why}"
                ))
            })?;
            Ok(TrialMerge::Conflict {
                paths: shape.paths(),
                shape,
            })
        }
        _ => Err(AppError::Storage(format!(
            "{label} of {head} onto {onto} failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ))),
    }
}

/// Parses the `-z` output of a conflicted `merge-tree --write-tree`: the
/// tree, then each conflicted index entry (`<mode> <oid> <stage>\t<path>`),
/// then an empty field, then each informational record (`<count>`, that many
/// paths, the conflict type, the message). Every field is NUL-terminated.
fn parse_conflict(stdout: &[u8]) -> Result<ConflictShape, String> {
    let text = |field: &[u8]| {
        std::str::from_utf8(field)
            .map(str::to_owned)
            .map_err(|_| format!("a field is not UTF-8: {:?}", String::from_utf8_lossy(field)))
    };
    // Every field ends with NUL; without the last one the output was cut.
    let body = stdout
        .strip_suffix(&[0])
        .ok_or("the output does not end with NUL")?;
    let mut fields = body.split(|byte| *byte == 0);
    let tree = text(fields.next().unwrap_or_default())?;
    if !is_pinned_oid(&tree) {
        return Err(format!("no tree object id: {tree:?}"));
    }
    let mut stages = Vec::new();
    loop {
        let Some(field) = fields.next() else {
            return Err("the conflicted entries do not end".into());
        };
        if field.is_empty() {
            break;
        }
        let entry = text(field)?;
        let (info, path) = entry
            .split_once('\t')
            .ok_or_else(|| format!("an entry has no path: {entry:?}"))?;
        let mut parts = info.split(' ');
        let (Some(mode), Some(oid), Some(stage), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(format!("an entry is not `<mode> <oid> <stage>`: {entry:?}"));
        };
        let stage: u8 = stage
            .parse()
            .ok()
            .filter(|stage| (1..=3).contains(stage))
            .ok_or_else(|| format!("an entry has no stage 1, 2 or 3: {entry:?}"))?;
        if !is_pinned_oid(oid) || path.is_empty() {
            return Err(format!("an entry is malformed: {entry:?}"));
        }
        stages.push(StageEntry {
            mode: mode.to_owned(),
            oid: oid.to_owned(),
            stage,
            path: path.to_owned(),
        });
    }
    if stages.is_empty() {
        return Err("a conflict with no conflicted entry".into());
    }
    let mut records = Vec::new();
    while let Some(count) = fields.next() {
        let count: usize = text(count)?
            .parse()
            .map_err(|_| "a record does not start with its path count".to_owned())?;
        let paths = (0..count)
            .map(|_| {
                fields
                    .next()
                    .ok_or_else(|| "a record ends inside its paths".to_owned())
                    .and_then(text)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let kind = text(
            fields
                .next()
                .ok_or_else(|| "a record has no conflict type".to_owned())?,
        )?;
        fields
            .next()
            .ok_or_else(|| "a record has no message".to_owned())?;
        records.push(ConflictRecord { kind, paths });
    }
    Ok(ConflictShape {
        tree,
        stages,
        records,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(digit: char) -> String {
        digit.to_string().repeat(40)
    }

    #[test]
    fn a_conflict_reads_its_tree_entries_and_records() {
        let (tree, a, b, c) = (oid('0'), oid('1'), oid('2'), oid('3'));
        let stdout = format!(
            "{tree}\0100644 {a} 2\tboth.md\0100644 {b} 3\tboth.md\0\
             100644 {a} 1\tdoc.md\0100644 {b} 2\tdoc.md\0100644 {c} 3\tdoc.md\0\0\
             1\0both.md\0Auto-merging\0Auto-merging both.md\n\0\
             1\0both.md\0CONFLICT (contents)\0CONFLICT (add/add): Merge conflict in both.md\n\0\
             2\0old.md\0new.md\0CONFLICT (rename/delete)\0old.md renamed to new.md\0"
        );
        let shape = parse_conflict(stdout.as_bytes()).unwrap();
        assert_eq!(shape.tree, tree);
        assert_eq!(shape.paths(), ["both.md", "doc.md"]);
        assert_eq!(shape.stages.len(), 5);
        assert_eq!(
            shape.stages[2],
            StageEntry {
                mode: "100644".into(),
                oid: a,
                stage: 1,
                path: "doc.md".into(),
            }
        );
        let kinds: Vec<_> = shape.records.iter().map(|r| r.kind.as_str()).collect();
        assert_eq!(
            kinds,
            [
                "Auto-merging",
                "CONFLICT (contents)",
                "CONFLICT (rename/delete)"
            ]
        );
        assert_eq!(shape.records[2].paths, ["old.md", "new.md"]);
    }

    #[test]
    fn a_conflict_with_no_records_still_reads() {
        let stdout = format!("{}\0100644 {} 2\ta\0\0", oid('0'), oid('1'));
        let shape = parse_conflict(stdout.as_bytes()).unwrap();
        assert_eq!(shape.paths(), ["a"]);
        assert!(shape.records.is_empty());
    }

    #[test]
    fn malformed_conflict_output_is_refused_not_guessed() {
        let (tree, a) = (oid('0'), oid('1'));
        for bad in [
            String::new(),
            "not-a-tree\0".to_owned(),
            format!("{tree}\0"),
            format!("{tree}\0\0"),
            format!("{tree}\0100644 {a} 2 a\0\0"),
            format!("{tree}\0100644 {a} 4\ta\0\0"),
            format!("{tree}\0100644 short 2\ta\0\0"),
            format!("{tree}\0100644 {a} 2\t\0\0"),
            format!("{tree}\0100644 {a} 2\ta\0\0x\0"),
            format!("{tree}\0100644 {a} 2\ta\0\02\0a\0"),
            format!("{tree}\0100644 {a} 2\ta\0\01\0a\0CONFLICT (contents)\0"),
            format!("{tree}\0100644 {a} 2\ta\0\01\0a\0t\0m\0\0x\0"),
        ] {
            assert!(parse_conflict(bad.as_bytes()).is_err(), "accepted {bad:?}");
        }
    }
}
