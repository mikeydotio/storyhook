//! Read gate configuration from the exact proposed merge, never mutable source files.

use super::gate_command::gate_command_from_bytes;
use super::project_fault::{GateEntryRefusal, ProjectFault, is_pinned_oid};
use crate::error::AppError;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

/// Store-free inspection result used by the bundled verifier.
#[derive(Debug, Serialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum GateInspection {
    /// The committed configuration selected a valid executable argv.
    GateReady {
        /// Exact argv, never a shell expression.
        argv: Vec<String>,
        /// Digest of configuration presence and bytes.
        configuration: String,
    },
    /// The pinned tree contains an actionable project configuration fault.
    ProjectFault {
        /// Typed evidence; no command was executed or tree certified.
        fault: ProjectFault,
    },
}

/// Inspect the proposed merge without contacting the store or mutating a checkout.
/// Git, object identity, and filesystem failures remain errors rather than project faults.
pub fn inspect(
    checkout: &Path,
    base: &str,
    head: &str,
    tree: &str,
) -> Result<GateInspection, AppError> {
    if ![base, head, tree].iter().all(|oid| is_pinned_oid(oid)) {
        return Err(AppError::Validation(
            "gate inspection requires pinned base, head and tree object IDs".into(),
        ));
    }
    let head_tree = run_git(
        checkout,
        None,
        &["rev-parse", "--verify", &format!("{head}^{{tree}}")],
    )?;
    let head_tree = String::from_utf8(head_tree)
        .map_err(|error| AppError::Storage(format!("reading committed head tree: {error}")))?;
    let head_tree = head_tree.trim();
    if !is_pinned_oid(head_tree) {
        return Err(AppError::Storage(
            "Git returned an invalid committed head tree".into(),
        ));
    }
    let common = run_git(
        checkout,
        None,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = String::from_utf8(common).map_err(|error| {
        AppError::Storage(format!("Git common directory is not UTF-8: {error}"))
    })?;
    let common = PathBuf::from(common.strip_suffix('\n').unwrap_or(&common));
    let objects = tempfile::Builder::new()
        .prefix("storyhook-gate-config-")
        .tempdir()
        .map_err(|error| {
            AppError::Storage(format!(
                "creating private gate configuration objects: {error}"
            ))
        })?;
    let snapshot = Snapshot {
        checkout,
        objects: objects.path(),
        source: common.join("objects"),
        tree,
    };
    let computed = snapshot.git(&["merge-tree", "--write-tree", base, head])?;
    if computed.strip_suffix(b"\n").unwrap_or(&computed) != tree.as_bytes() {
        return Err(AppError::Storage(format!(
            "gate configuration merge tree differs from expected {tree}"
        )));
    }
    let pointer = snapshot.resolve(".storyhook.toml")?;
    let raw = pointer
        .as_ref()
        .map(|entry| snapshot.blob(entry))
        .transpose()?;
    let mut digest = Sha256::new();
    digest.update([u8::from(raw.is_some())]);
    if let Some(raw) = &raw {
        digest.update(raw);
    }
    let configuration = format!("{:x}", digest.finalize());
    let gate = match gate_command_from_bytes(raw.as_deref()) {
        Ok(gate) => gate,
        Err(error) => {
            return Ok(GateInspection::ProjectFault {
                fault: ProjectFault::InvalidGateConfiguration {
                    locus: ".storyhook.toml#verify.gate".into(),
                    tree: tree.into(),
                    base: base.into(),
                    head: head.into(),
                    head_tree: head_tree.into(),
                    configuration,
                    detail: error.to_string(),
                },
            });
        }
    };
    let executable = &gate.argv()[0];
    if executable.contains('/') && Path::new(executable).is_relative() {
        let locus = normalize(Path::new(executable))?;
        let entry = snapshot.resolve(&locus)?;
        let reason = match entry {
            None => Some(GateEntryRefusal::Missing),
            Some(entry) if entry.mode != "100755" => Some(GateEntryRefusal::NotExecutable),
            Some(_) => None,
        };
        if let Some(reason) = reason {
            return Ok(GateInspection::ProjectFault {
                fault: ProjectFault::MissingGateCommand {
                    locus: locus.clone(),
                    tree: tree.into(),
                    base: base.into(),
                    head: head.into(),
                    head_tree: head_tree.into(),
                    configuration,
                    gate: gate.display(),
                    detail: format!(
                        "configured gate entry {locus} is {reason:?} in merge tree {tree}"
                    ),
                    reason,
                },
            });
        }
    }
    Ok(GateInspection::GateReady {
        argv: gate.argv().to_vec(),
        configuration,
    })
}

struct Entry {
    mode: String,
    kind: String,
    oid: String,
}

struct Snapshot<'a> {
    checkout: &'a Path,
    objects: &'a Path,
    source: PathBuf,
    tree: &'a str,
}

impl Snapshot<'_> {
    fn git(&self, args: &[&str]) -> Result<Vec<u8>, AppError> {
        run_git(self.checkout, Some((self.objects, &self.source)), args)
    }

    fn entry(&self, path: &str) -> Result<Option<Entry>, AppError> {
        let bytes = self.git(&[
            "--literal-pathspecs",
            "ls-tree",
            "--full-tree",
            "-z",
            self.tree,
            "--",
            path,
        ])?;
        if bytes.is_empty() {
            return Ok(None);
        }
        let record = bytes
            .strip_suffix(&[0])
            .ok_or_else(|| AppError::Storage("unterminated Git tree entry".into()))?;
        let tab = record
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| AppError::Storage("invalid Git tree entry".into()))?;
        if &record[tab + 1..] != path.as_bytes() {
            return Err(AppError::Storage(
                "Git tree lookup returned a different path".into(),
            ));
        }
        let fields: Vec<_> = std::str::from_utf8(&record[..tab])
            .map_err(|error| AppError::Storage(error.to_string()))?
            .split(' ')
            .collect();
        if fields.len() != 3 || !is_pinned_oid(fields[2]) {
            return Err(AppError::Storage("invalid Git tree object metadata".into()));
        }
        Ok(Some(Entry {
            mode: fields[0].into(),
            kind: fields[1].into(),
            oid: fields[2].into(),
        }))
    }

    fn blob(&self, entry: &Entry) -> Result<Vec<u8>, AppError> {
        if entry.kind != "blob" {
            return Err(AppError::Storage(
                "gate configuration path is not a committed file".into(),
            ));
        }
        self.git(&["cat-file", "blob", &entry.oid])
    }

    // Resolve only committed symlinks. A path escaping the tree cannot borrow
    // host files as configuration or be diagnosed as a repository-owned fault.
    fn resolve(&self, path: &str) -> Result<Option<Entry>, AppError> {
        let mut path = normalize(Path::new(path))?;
        let mut followed = false;
        for _ in 0..40 {
            let parts: Vec<_> = path.split('/').collect();
            let mut redirected = false;
            for (index, _) in parts.iter().enumerate() {
                let prefix = parts[..=index].join("/");
                let Some(entry) = self.entry(&prefix)? else {
                    return if followed {
                        Err(AppError::Storage(format!(
                            "committed gate path contains a dangling symlink: {path}"
                        )))
                    } else {
                        Ok(None)
                    };
                };
                if entry.mode == "120000" {
                    let target = String::from_utf8(self.blob(&entry)?).map_err(|error| {
                        AppError::Storage(format!("gate symlink is not UTF-8: {error}"))
                    })?;
                    if Path::new(&target).is_absolute() {
                        return Err(AppError::Storage(
                            "gate symlink points outside the repository".into(),
                        ));
                    }
                    let redirected_path = Path::new(&parts[..index].join("/"))
                        .join(target)
                        .join(parts[index + 1..].join("/"));
                    path = normalize(&redirected_path)?;
                    followed = true;
                    redirected = true;
                    break;
                }
                if index == parts.len() - 1 {
                    return Ok(Some(entry));
                }
                if entry.kind != "tree" {
                    return Ok(None);
                }
            }
            if !redirected {
                break;
            }
        }
        Err(AppError::Storage(
            "committed gate symlink resolution exceeded 40 links".into(),
        ))
    }
}

fn normalize(path: &Path) -> Result<String, AppError> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part),
            Component::CurDir => {}
            Component::ParentDir if !parts.is_empty() => {
                parts.pop();
            }
            _ => {
                return Err(AppError::Storage(
                    "gate path escapes the committed repository tree".into(),
                ));
            }
        }
    }
    if parts.is_empty() {
        return Err(AppError::Storage(
            "gate path names the repository root".into(),
        ));
    }
    let path: PathBuf = parts.iter().collect();
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| AppError::Storage("gate path is not UTF-8".into()))
}

fn run_git(
    checkout: &Path,
    objects: Option<(&Path, &Path)>,
    args: &[&str],
) -> Result<Vec<u8>, AppError> {
    let mut command = crate::env::git_env::command(checkout);
    if let Some((objects, source)) = objects {
        command
            .env("GIT_OBJECT_DIRECTORY", objects)
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", source);
    }
    command.args(args);
    let result = crate::process::run_captured_private(command, std::time::Duration::from_secs(30))
        .map_err(|error| {
            AppError::Storage(format!(
                "gate configuration Git {args:?}: {}",
                error.detail()
            ))
        })?;
    if !result.status.success() {
        return Err(AppError::Storage(format!(
            "gate configuration Git {args:?} failed: {}",
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    Ok(result.stdout)
}
