//! Checked Git observations shared by resource discovery and cleanup.
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::env::git_env;
use crate::error::AppError;
use crate::process::run_captured;

/// One complete record from Git's NUL-delimited worktree inventory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorktreeRecord {
    /// Absolute path registered by Git, including missing worktrees.
    pub path: PathBuf,
    /// Local branch, absent for a detached or bare worktree.
    pub branch: Option<String>,
    /// Whether Git protects the registration with a lock.
    pub locked: bool,
    /// Whether Git reports stale administrative data.
    pub prunable: bool,
}

/// Runs one read-only Git observation with contextual, bounded failure.
pub fn text(cwd: &Path, args: &[&str]) -> Result<String, AppError> {
    let mut command = git_env::command(cwd);
    command.args(args);
    let output = run_captured(command, Duration::from_secs(60)).map_err(|error| {
        AppError::Validation(format!(
            "git {} in {}: {}",
            args.join(" "),
            cwd.display(),
            error.detail()
        ))
    })?;
    if !output.status.success() {
        return Err(AppError::Validation(format!(
            "git {} in {}: {}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    String::from_utf8(output.stdout).map_err(|error| {
        AppError::Validation(format!(
            "Git resource observation in {} is not UTF-8: {error}",
            cwd.display()
        ))
    })
}

/// Reads every registration without treating an unreadable inventory as empty.
pub fn inventory(repository: &Path) -> Result<Vec<WorktreeRecord>, AppError> {
    parse(&text(
        repository,
        &["worktree", "list", "--porcelain", "-z"],
    )?)
}

fn parse(listing: &str) -> Result<Vec<WorktreeRecord>, AppError> {
    let mut records = Vec::new();
    let mut record: Option<WorktreeRecord> = None;
    for field in listing.split('\0') {
        if let Some(path) = field.strip_prefix("worktree ") {
            if let Some(previous) = record.take() {
                records.push(previous);
            }
            if !Path::new(path).is_absolute() {
                return Err(AppError::Validation(format!(
                    "Git worktree path is not absolute: {path:?}"
                )));
            }
            record = Some(WorktreeRecord {
                path: path.into(),
                ..Default::default()
            });
        } else if let Some(current) = record.as_mut() {
            if let Some(branch) = field.strip_prefix("branch refs/heads/") {
                current.branch = Some(branch.into());
            }
            if field == "locked" || field.starts_with("locked ") {
                current.locked = true;
            }
            if field == "prunable" || field.starts_with("prunable ") {
                current.prunable = true;
            }
        } else if !field.is_empty() {
            return Err(AppError::Validation(
                "Git inventory has attributes without a worktree".into(),
            ));
        }
    }
    if let Some(record) = record {
        records.push(record);
    }
    if records.is_empty() {
        return Err(AppError::Validation(
            "Git returned no worktree records".into(),
        ));
    }
    Ok(records)
}

/// Tests an exact local branch while preserving operational errors.
pub fn branch_exists(repository: &Path, branch: &str) -> Result<bool, AppError> {
    let reference = format!("refs/heads/{branch}");
    let refs = text(
        repository,
        &["for-each-ref", "--format=%(refname)", &reference],
    )?;
    Ok(refs.lines().any(|line| line == reference))
}

/// Canonicalizes existing aliases; absent final components retain their identity.
pub fn canonical(path: &Path) -> Result<PathBuf, AppError> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if std::fs::symlink_metadata(path).is_ok() {
                return Err(AppError::Validation(format!(
                    "dangling or inaccessible resource symlink {}",
                    path.display()
                )));
            }
            let parent = path.parent().ok_or_else(|| {
                AppError::Validation(format!("cannot resolve {}", path.display()))
            })?;
            let name = path.file_name().ok_or_else(|| {
                AppError::Validation(format!("cannot resolve {}", path.display()))
            })?;
            Ok(canonical(parent)?.join(name))
        }
        Err(error) => Err(AppError::Validation(format!(
            "cannot resolve {}: {error}",
            path.display()
        ))),
    }
}
