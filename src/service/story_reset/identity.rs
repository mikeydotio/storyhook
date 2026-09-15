//! Names locate resources; filesystem identity prevents retrying against replacements.
use crate::error::AppError;
use crate::service::resources::{ResourceReport, git};
use crate::store::ResetPathIdentity;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

fn observe(path: PathBuf, removable: bool) -> Result<ResetPathIdentity, AppError> {
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|e| AppError::Storage(format!("reset identity {}: {e}", path.display())))?;
    if !metadata.is_dir() {
        return Err(AppError::Validation(format!(
            "reset identity is not a directory: {}",
            path.display()
        )));
    }
    Ok(ResetPathIdentity {
        path,
        device: metadata.dev(),
        inode: metadata.ino(),
        removable,
    })
}

/// Captures the repository and each existing worktree object before cleanup.
pub(crate) fn capture(report: &ResourceReport) -> Result<Vec<ResetPathIdentity>, AppError> {
    let Some(repository) = &report.repository else {
        return Ok(Vec::new());
    };
    let common = git::text(
        repository,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let mut paths = vec![observe(PathBuf::from(common.trim()), false)?];
    if let Some(worktree) = &report.worktree
        && worktree
            .try_exists()
            .map_err(|e| AppError::Storage(format!("reset identity {}: {e}", worktree.display())))?
    {
        paths.push(observe(worktree.clone(), true)?);
        let private = git::text(worktree, &["rev-parse", "--absolute-git-dir"])?;
        paths.push(observe(PathBuf::from(private.trim()), true)?);
    }
    Ok(paths)
}

/// Refuses replacements; expected removal does not invalidate a retry.
pub(crate) fn validate(paths: &[ResetPathIdentity]) -> Result<(), AppError> {
    for expected in paths {
        match std::fs::symlink_metadata(&expected.path) {
            Ok(metadata)
                if metadata.is_dir()
                    && metadata.dev() == expected.device
                    && metadata.ino() == expected.inode => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && expected.removable => {}
            Ok(_) => return Err(changed(&expected.path)),
            Err(error) => {
                return Err(AppError::Storage(format!(
                    "reset identity {}: {error}",
                    expected.path.display()
                )));
            }
        }
    }
    Ok(())
}

fn changed(path: &Path) -> AppError {
    AppError::Validation(format!(
        "reset refused: filesystem identity changed at {}",
        path.display()
    ))
}
