//! Capture-side validation for dispatch cleanup leases.
//!
//! Dispatch writes the marker into a linked worktree's private Git directory.
//! This module is the only Rust reader: it proves that every repository value
//! in the marker still describes the caller's real linked worktree before the
//! story service is allowed to make that identity durable.

use std::fs;
use std::path::{Path, PathBuf};

use crate::domain::{CLEANUP_LEASE_MARKER, CLEANUP_LEASE_VERSION, StoryCleanupLease};
use crate::env::git_env;
use crate::error::AppError;

/// Reads and validates the cleanup marker for `cwd`'s linked worktree.
///
/// A main checkout, a non-Git directory, or a linked worktree with no marker
/// is an intentional legacy/manual submission and returns `None`. Once a
/// marker exists, any unreadable, malformed, unsupported, or contradictory
/// value fails loudly; silently degrading a claimed lease to legacy cleanup
/// would recreate the false-success class this contract removes.
pub(super) fn marker_at(cwd: &Path) -> Result<Option<StoryCleanupLease>, AppError> {
    if git_env::output(cwd, &["rev-parse", "--show-toplevel"]).is_none() {
        return Ok(None);
    }
    marker_at_registered(cwd)
}

/// Reads a known Git worktree without degrading failed queries to no marker.
pub(super) fn marker_at_registered(cwd: &Path) -> Result<Option<StoryCleanupLease>, AppError> {
    let query = |args: &[&str]| -> Result<String, AppError> {
        let value = super::resources::git::text(cwd, args)?;
        Ok(value.strip_suffix('\n').unwrap_or(&value).to_string())
    };
    let toplevel = query(&["rev-parse", "--show-toplevel"])?;
    let git_dir = query(&["rev-parse", "--absolute-git-dir"])?;
    let common_dir = query(&["rev-parse", "--path-format=absolute", "--git-common-dir"])?;

    let git_dir = canonical_existing(Path::new(&git_dir), "private Git directory")?;
    let common_dir = canonical_existing(Path::new(&common_dir), "Git common directory")?;
    if git_dir == common_dir {
        return Ok(None);
    }

    let marker_path = git_dir.join(CLEANUP_LEASE_MARKER);
    match fs::symlink_metadata(&marker_path) {
        Ok(meta) if !meta.file_type().is_file() => {
            return Err(AppError::Validation(format!(
                "cleanup marker {} is not a regular private file",
                marker_path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(AppError::Validation(format!(
                "cannot inspect {}: {error}",
                marker_path.display()
            )));
        }
    }
    let encoded = match fs::read(&marker_path) {
        Ok(encoded) => encoded,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(AppError::Validation(format!(
                "cannot read cleanup lease marker `{}`: {error}",
                marker_path.display()
            )));
        }
    };
    let lease: StoryCleanupLease = serde_json::from_slice(&encoded).map_err(|error| {
        AppError::Validation(format!(
            "cleanup lease marker `{}` is malformed: {error}",
            marker_path.display()
        ))
    })?;
    if lease.version != CLEANUP_LEASE_VERSION {
        return Err(AppError::Validation(format!(
            "cleanup lease marker `{}` uses unsupported version {}; this binary requires {}",
            marker_path.display(),
            lease.version,
            CLEANUP_LEASE_VERSION
        )));
    }

    let actual_worktree = canonical_existing(Path::new(&toplevel), "linked worktree")?;
    let actual_repository = main_worktree(cwd)?;
    let actual_branch =
        git_env::output(cwd, &["symbolic-ref", "--short", "HEAD"]).ok_or_else(|| {
            AppError::Validation(format!(
                "cleanup lease marker `{}` belongs to a detached or unreadable worktree",
                marker_path.display()
            ))
        })?;

    validate_path(
        "repository",
        &lease.repository_path,
        &actual_repository,
        &marker_path,
    )?;
    validate_path(
        "worktree",
        &lease.worktree_path,
        &actual_worktree,
        &marker_path,
    )?;
    if lease.branch != actual_branch {
        return Err(marker_mismatch(
            &marker_path,
            "branch",
            &lease.branch,
            &actual_branch,
        ));
    }
    validate_tmux_target(&lease, &marker_path)?;
    Ok(Some(lease))
}

fn canonical_existing(path: &Path, label: &str) -> Result<PathBuf, AppError> {
    path.canonicalize().map_err(|error| {
        AppError::Validation(format!(
            "cannot resolve cleanup lease {label} `{}`: {error}",
            path.display()
        ))
    })
}

fn main_worktree(cwd: &Path) -> Result<PathBuf, AppError> {
    let records = super::resources::git::inventory(cwd)?;
    canonical_existing(&records[0].path, "repository")
}

fn validate_path(
    label: &str,
    claimed: &Path,
    actual: &Path,
    marker: &Path,
) -> Result<(), AppError> {
    let claimed = super::resources::git::canonical(claimed)?;
    if claimed != actual {
        return Err(marker_mismatch(
            marker,
            label,
            &claimed.display().to_string(),
            &actual.display().to_string(),
        ));
    }
    Ok(())
}

fn validate_tmux_target(lease: &StoryCleanupLease, marker: &Path) -> Result<(), AppError> {
    let tmux = &lease.tmux;
    if !tmux.socket_path.is_absolute() {
        return Err(AppError::Validation(format!(
            "cleanup lease marker `{}` carries an invalid tmux cleanup target",
            marker.display()
        )));
    }
    Ok(())
}

fn marker_mismatch(marker: &Path, field: &str, claimed: &str, actual: &str) -> AppError {
    AppError::Validation(format!(
        "cleanup lease marker `{}` {field} mismatch: marker says `{claimed}`, caller is `{actual}`",
        marker.display()
    ))
}
