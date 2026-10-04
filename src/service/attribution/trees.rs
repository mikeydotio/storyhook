//! Native pinned-tree preparation. Tree facts alone do not prove detector semantics.

mod directory;
use super::{DetectorRelation, ProbeSide};
use crate::{
    error::AppError,
    process::Cancellation,
    service::{
        private_objects::PrivateObjects,
        trial_merge::{answer_oid, require_pinned},
    },
    store::GateInputs,
};
pub use directory::PreparedDirectory;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Instant,
};

const LABEL: &str = "causal tree preparation";
const CLEAN_GIT: [(&str, &str); 6] = [
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_CONFIG_SYSTEM", "/dev/null"),
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_ATTR_NOSYSTEM", "1"),
    ("GIT_NO_LAZY_FETCH", "1"),
    ("GIT_NO_REPLACE_OBJECTS", "1"),
];

/// The exact intervention to apply after independently resolving the failed merge.
#[derive(Clone, Debug)]
pub enum TreeIntervention {
    /// Run the pinned base with byte-identical protected detector inputs.
    Unchanged,
    /// Copy these exact candidate paths onto the pinned base.
    Transplant(Vec<String>),
    /// Restore these exact paths from the base, preserving all other candidate inputs.
    Ablation(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    mode: String,
    oid: String,
}
type Entries = BTreeMap<String, Entry>;

/// Privately owned Git facts. This cannot construct causal repair authority.
pub struct PreparedTrees {
    objects: PrivateObjects,
    index: tempfile::TempDir,
    candidate: String,
    control: String,
    detector: String,
    patch: Vec<u8>,
    relation: DetectorRelation,
    deadline: Instant,
    cancellation: Cancellation,
}

impl PreparedTrees {
    /// Recompute the failed merge and construct a detector-preserving tree intervention.
    /// The adapter still must establish that protected inputs cover the complete detector.
    pub fn prepare(
        checkout: &Path,
        inputs: &GateInputs,
        protected: &[String],
        intervention: TreeIntervention,
        deadline: Instant,
        cancellation: &Cancellation,
    ) -> Result<Self, AppError> {
        let head = pinned(inputs.head.as_deref())?;
        let base = pinned(inputs.base.as_deref())?;
        let failed_tree = pinned(inputs.tree.as_deref())?;
        let protected = paths(protected)?;
        let objects = PrivateObjects::open_controlled(
            checkout,
            LABEL,
            "storyhook-diagnosis-objects-",
            Some(deadline),
            &|| cancellation.is_cancelled(),
        )?;
        let merged = objects.merge([base, head], false, Some(deadline), &|| {
            cancellation.is_cancelled()
        })?;
        if !merged.status.success()
            || answer_oid(&merged.stdout, LABEL, "failed merge")? != failed_tree
        {
            return Err(invalid(
                "pinned inputs do not reproduce the retained failed merge tree",
            ));
        }
        let index = tempfile::Builder::new()
            .prefix("storyhook-diagnosis-index-")
            .tempdir()
            .map_err(|e| invalid(&format!("private index: {e}")))?;
        let directory = index
            .path()
            .to_str()
            .ok_or_else(|| invalid("private administration is not UTF-8"))?;
        let format = if failed_tree.len() == 40 {
            "--object-format=sha1"
        } else {
            "--object-format=sha256"
        };
        let initialized = objects.query(
            &[
                "init",
                "--bare",
                "--quiet",
                "--template=",
                format,
                directory,
            ],
            &CLEAN_GIT,
            &[],
            Some(deadline),
            &|| cancellation.is_cancelled(),
        )?;
        if !initialized.status.success() {
            return Err(invalid(&format!(
                "private Git administration: {}",
                String::from_utf8_lossy(&initialized.stderr)
            )));
        }
        std::fs::create_dir(index.path().join("empty-worktree"))
            .map_err(|e| invalid(&format!("private index work-tree: {e}")))?;
        let mut prepared = Self {
            objects,
            index,
            candidate: failed_tree.into(),
            control: String::new(),
            detector: String::new(),
            patch: vec![],
            relation: DetectorRelation::Unchanged,
            deadline,
            cancellation: cancellation.clone(),
        };
        let candidate = prepared.entries(failed_tree)?;
        let base_tree = prepared.query(&["rev-parse", "--verify", &format!("{base}^{{tree}}")])?;
        let base_tree = answer_oid(&base_tree, LABEL, "base tree")?;
        let base_entries = prepared.entries(&base_tree)?;
        for path in &protected {
            regular(
                candidate
                    .get(*path)
                    .ok_or_else(|| invalid(&format!("missing detector input {path:?}")))?,
            )?;
        }
        prepared.control = match &intervention {
            TreeIntervention::Unchanged => base_tree,
            TreeIntervention::Transplant(selected) | TreeIntervention::Ablation(selected) => {
                let selected = paths(selected)?;
                let transplant = matches!(intervention, TreeIntervention::Transplant(_));
                let (start, from, to) = if transplant {
                    (base_tree.as_str(), &candidate, &base_entries)
                } else {
                    (failed_tree, &base_entries, &candidate)
                };
                prepared.query(&["read-tree", start])?;
                for path in selected {
                    if from.get(path) == to.get(path) {
                        return Err(invalid(&format!("intervention does not change {path:?}")));
                    }
                    if let Some(entry) = from.get(path) {
                        regular(entry)?;
                        prepared.query(&[
                            "update-index",
                            "--add",
                            "--cacheinfo",
                            &entry.mode,
                            &entry.oid,
                            path,
                        ])?;
                    } else {
                        prepared.query(&["update-index", "--force-remove", "--", path])?;
                    }
                }
                answer_oid(&prepared.query(&["write-tree"])?, LABEL, "control tree")?
            }
        };
        if prepared.control == prepared.candidate {
            return Err(invalid("control is identical to candidate"));
        }
        let control = prepared.entries(&prepared.control)?;
        let mut digest = Sha256::new();
        for path in protected {
            if candidate.get(path) != control.get(path) {
                return Err(invalid(&format!(
                    "control changes protected detector input {path:?}"
                )));
            }
            let entry = &candidate[path];
            for value in [path.as_str(), entry.mode.as_str(), entry.oid.as_str()] {
                digest.update(value.as_bytes());
                digest.update([0]);
            }
        }
        prepared.detector = format!("sha256:{:x}", digest.finalize());
        prepared.patch = prepared.query(&[
            "diff",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-textconv",
            &prepared.candidate,
            &prepared.control,
            "--",
        ])?;
        let patch = format!("sha256:{:x}", Sha256::digest(&prepared.patch));
        prepared.relation = match intervention {
            TreeIntervention::Unchanged => DetectorRelation::Unchanged,
            TreeIntervention::Transplant(_) => DetectorRelation::Transplant { patch },
            TreeIntervention::Ablation(_) => DetectorRelation::Ablation { patch },
        };
        prepared.check()?;
        Ok(prepared)
    }

    /// Exact candidate and control tree object IDs, respectively.
    pub fn trees(&self) -> (&str, &str) {
        (&self.candidate, &self.control)
    }

    /// Content identity of every protected detector path, mode and blob.
    pub fn detector(&self) -> &str {
        &self.detector
    }

    /// Exact binary patch from candidate to control, including full object IDs.
    pub fn patch(&self) -> &[u8] {
        &self.patch
    }

    /// How the control was prepared, with the retained patch digest when applicable.
    pub fn relation(&self) -> DetectorRelation {
        self.relation.clone()
    }

    /// Explicitly settle private Git state; both removals are attempted on failure.
    pub fn close(self) -> Result<(), AppError> {
        let objects = self.objects.close();
        let index = self
            .index
            .close()
            .map_err(|e| invalid(&format!("private Git cleanup: {e}")));
        match (objects, index) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(a), Err(b)) => Err(invalid(&format!("{a}; {b}"))),
            (Err(e), _) | (_, Err(e)) => Err(e),
        }
    }

    /// Materialize a tree in a fresh owned directory, refusing links and submodules.
    pub fn materialize(&self, side: ProbeSide) -> Result<PreparedDirectory, AppError> {
        let tree = if side == ProbeSide::Candidate {
            &self.candidate
        } else {
            &self.control
        };
        directory::materialize(self, &self.entries(tree)?)
    }

    fn check(&self) -> Result<(), AppError> {
        if self.cancellation.is_cancelled() || Instant::now() >= self.deadline {
            return Err(invalid("diagnosis cancelled or active deadline reached"));
        }
        Ok(())
    }

    fn query(&self, args: &[&str]) -> Result<Vec<u8>, AppError> {
        self.check()?;
        let index = self.index.path().join("index");
        let index = index
            .to_str()
            .ok_or_else(|| invalid("private index is not UTF-8"))?;
        let directory = self
            .index
            .path()
            .to_str()
            .ok_or_else(|| invalid("private administration is not UTF-8"))?;
        let mut env = CLEAN_GIT.to_vec();
        let worktree = self.index.path().join("empty-worktree");
        let worktree = worktree
            .to_str()
            .ok_or_else(|| invalid("private index work-tree is not UTF-8"))?;
        env.extend([
            ("GIT_DIR", directory),
            ("GIT_INDEX_FILE", index),
            ("GIT_WORK_TREE", worktree),
        ]);
        let result = self
            .objects
            .query(args, &env, &[], Some(self.deadline), &|| {
                self.cancellation.is_cancelled()
            })?;
        if !result.status.success() {
            return Err(invalid(&format!(
                "Git {args:?}: {}",
                String::from_utf8_lossy(&result.stderr)
            )));
        }
        self.check()?;
        Ok(result.stdout)
    }

    fn entries(&self, tree: &str) -> Result<Entries, AppError> {
        let output = self.query(&["ls-tree", "-rz", "--full-tree", tree])?;
        let mut entries = Entries::new();
        for row in output
            .split(|byte| *byte == 0)
            .filter(|row| !row.is_empty())
        {
            let row = std::str::from_utf8(row)
                .map_err(|e| invalid(&format!("tree is not UTF-8: {e}")))?;
            let (header, path) = row
                .split_once('\t')
                .ok_or_else(|| invalid("malformed tree entry"))?;
            path_ok(path)?;
            let header: Vec<_> = header.split(' ').collect();
            let [mode, kind, oid] = header.as_slice() else {
                return Err(invalid("malformed tree header"));
            };
            require_pinned(oid, LABEL)?;
            if !matches!(
                (*mode, *kind),
                ("100644" | "100755" | "120000", "blob") | ("160000", "commit")
            ) || entries
                .insert(
                    path.into(),
                    Entry {
                        mode: (*mode).into(),
                        oid: (*oid).into(),
                    },
                )
                .is_some()
            {
                return Err(invalid("unsupported or duplicate tree entry"));
            }
        }
        Ok(entries)
    }
}

fn invalid(detail: &str) -> AppError {
    AppError::Validation(format!("{LABEL}: {detail}"))
}

fn pinned(value: Option<&str>) -> Result<&str, AppError> {
    let value = value.ok_or_else(|| invalid("missing pinned merge input"))?;
    require_pinned(value, LABEL)?;
    Ok(value)
}

fn paths(values: &[String]) -> Result<BTreeSet<&String>, AppError> {
    let mut result = BTreeSet::new();
    for value in values {
        path_ok(value)?;
        if !result.insert(value) {
            return Err(invalid("duplicate intervention or detector path"));
        }
    }
    if result.is_empty() {
        return Err(invalid("empty intervention or detector closure"));
    }
    Ok(result)
}

fn path_ok(path: &str) -> Result<(), AppError> {
    if path
        .chars()
        .any(|c| c.is_control() || matches!(c, '\\' | '*' | '?' | '[' | ']'))
        || path
            .split('/')
            .any(|part| matches!(part, "" | "." | "..") || part.eq_ignore_ascii_case(".git"))
    {
        return Err(invalid(&format!(
            "unsupported literal repository path {path:?}"
        )));
    }
    Ok(())
}

fn regular(entry: &Entry) -> Result<(), AppError> {
    if matches!(entry.mode.as_str(), "100644" | "100755") {
        Ok(())
    } else {
        Err(invalid(
            "symbolic links and submodules cannot establish native input identity",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancelled_preparation_does_not_open_the_repository() {
        let cancellation = Cancellation::default();
        cancellation.cancel();
        let inputs = GateInputs {
            head: Some("a".repeat(40)),
            base: Some("b".repeat(40)),
            tree: Some("c".repeat(40)),
            ..GateInputs::default()
        };
        let result = PreparedTrees::prepare(
            Path::new("/nonexistent-sh870-repository"),
            &inputs,
            &["tests/check.rs".into()],
            TreeIntervention::Unchanged,
            Instant::now() + std::time::Duration::from_millis(super::super::MAX_DIAGNOSIS_MS),
            &cancellation,
        );
        assert!(matches!(result, Err(error) if error.to_string().contains("authority expired")));
    }
}
