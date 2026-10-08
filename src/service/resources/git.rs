//! Checked Git observations shared by resource discovery and cleanup.
use std::fs;
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

/// Surviving private Git administration and its recorded worktree backlink.
#[derive(Clone, Debug)]
pub(super) struct WorktreeAdministration {
    /// Private Git directory under the common repository directory.
    pub path: PathBuf,
    /// Exact `.git` file named by Git's `gitdir` backlink.
    pub gitfile: PathBuf,
}

/// Runs one checked Git command with contextual, bounded failure.
/// Mutation callers must establish ownership before invoking this primitive.
pub fn text(cwd: &Path, args: &[&str]) -> Result<String, AppError> {
    text_with_bound(Duration::from_secs(60), cwd, args)
}

/// Uses the owning operation's declared subprocess allowance.
pub(crate) fn text_with_bound(
    bound: Duration,
    cwd: &Path,
    args: &[&str],
) -> Result<String, AppError> {
    let mut command = git_env::command(cwd);
    command.args(args);
    let output = run_captured(command, bound).map_err(|error| {
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
    inventory_with_bound(Duration::from_secs(60), repository)
}

/// Reads registrations with the owning operation's allowance.
pub(crate) fn inventory_with_bound(
    bound: Duration,
    repository: &Path,
) -> Result<Vec<WorktreeRecord>, AppError> {
    parse(&text_with_bound(
        bound,
        repository,
        &["worktree", "list", "--porcelain", "-z"],
    )?)
}

/// Requires strict Git discovery unless a readable tree proves no repository.
/// Partial Git metadata and orphan workspace containers are evidence, not absence.
pub(super) fn needs_inventory(
    checkout: &Path,
    additional_worktree_root: Option<&Path>,
) -> Result<bool, AppError> {
    let root = checkout
        .canonicalize()
        .map_err(|error| inspection_error(checkout, error))?;
    for directory in root.ancestors() {
        // The final dot also requires directory search permission, not just listing.
        let entries = fs::read_dir(directory.join("."))
            .map_err(|error| inspection_error(directory, error))?;
        for entry in entries {
            let name = entry
                .map_err(|error| inspection_error(directory, error))?
                .file_name();
            if [
                ".git",
                "HEAD",
                "objects",
                "refs",
                "commondir",
                "packed-refs",
                "reftable",
            ]
            .iter()
            .any(|marker| name == *marker)
            {
                return Ok(true);
            }
        }
    }
    for container in [
        Path::new(".claude/worktrees"),
        Path::new(".codex/worktrees"),
    ]
    .into_iter()
    .chain(additional_worktree_root)
    {
        // Resolve every ancestor so a dangling or inaccessible parent is not absence.
        let path = canonical(&root.join(container))?;
        match fs::symlink_metadata(&path) {
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(inspection_error(&path, error)),
        }
    }
    Ok(false)
}

fn inspection_error(path: &Path, error: std::io::Error) -> AppError {
    AppError::Validation(format!(
        "cannot inspect Git resource root {}: {error}",
        path.display()
    ))
}

/// Reads private administration without asking a possibly broken worktree to run Git.
pub(super) fn administrations(
    bound: Duration,
    repository: &Path,
) -> Result<Vec<WorktreeAdministration>, AppError> {
    let common = text_with_bound(
        bound,
        repository,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let root = canonical(Path::new(common.trim_end_matches('\n')))?.join("worktrees");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(AppError::Validation(format!(
                "cannot inspect private Git administrations {}: {error}",
                root.display()
            )));
        }
    };
    let mut administrations = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            AppError::Validation(format!(
                "cannot list private Git administrations {}: {error}",
                root.display()
            ))
        })?;
        if !entry
            .file_type()
            .map_err(|error| {
                AppError::Validation(format!(
                    "cannot inspect {}: {error}",
                    entry.path().display()
                ))
            })?
            .is_dir()
        {
            continue;
        }
        let path = entry.path();
        let backlink = path.join("gitdir");
        let raw = fs::read_to_string(&backlink).map_err(|error| {
            AppError::Validation(format!(
                "cannot read Git backlink {}: {error}",
                backlink.display()
            ))
        })?;
        let gitfile = Path::new(raw.trim_end_matches('\n'));
        if !gitfile.is_absolute() || gitfile.file_name().is_none_or(|name| name != ".git") {
            return Err(AppError::Validation(format!(
                "Git backlink {} does not name an absolute .git file",
                backlink.display()
            )));
        }
        administrations.push(WorktreeAdministration {
            path,
            gitfile: canonical(gitfile)?,
        });
    }
    administrations.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(administrations)
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
    branch_exists_with_bound(Duration::from_secs(60), repository, branch)
}

/// Observes the branch with the owning operation's allowance.
pub(crate) fn branch_exists_with_bound(
    bound: Duration,
    repository: &Path,
    branch: &str,
) -> Result<bool, AppError> {
    let reference = format!("refs/heads/{branch}");
    let refs = text_with_bound(
        bound,
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
