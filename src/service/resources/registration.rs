//! Per-registration evidence for resource discovery.

use super::{ResourceObservation, git};
use crate::domain::StoryCleanupLease;
use crate::error::AppError;
use std::path::Path;

/// One registration's public observation and private marker claim.
pub(super) struct CheckedRegistration {
    /// Facts and diagnostics reported without granting mutation authority.
    pub observation: ResourceObservation,
    /// Readable private claim, retained even when its validation fails.
    pub marker: Option<StoryCleanupLease>,
}

/// Corroborates Git inventory with private administration, even after `.git` is lost.
pub(super) fn inspect(
    repository: &Path,
    records: &[git::WorktreeRecord],
) -> Result<Vec<CheckedRegistration>, AppError> {
    let administrations = git::administrations(repository)?;
    let mut checked = Vec::new();
    for record in records.iter().filter(|record| record.path != repository) {
        let path = git::canonical(&record.path)?;
        let gitfile = git::canonical(&path.join(".git"))?;
        let matches: Vec<_> = administrations
            .iter()
            .filter(|admin| admin.gitfile == gitfile)
            .collect();
        let mut observation = ResourceObservation {
            repository: repository.to_path_buf(),
            path: path.clone(),
            branch: record.branch.clone(),
            status: "healthy".into(),
            owner_project: None,
            owner_story_id: None,
            diagnostics: Vec::new(),
        };
        let admin = match matches.as_slice() {
            [admin] => Some(*admin),
            [] => {
                mark(
                    &mut observation,
                    "unknown",
                    format!(
                        "no private Git administration matches registration {}",
                        path.display()
                    ),
                );
                None
            }
            _ => {
                return Err(AppError::Validation(format!(
                    "multiple private Git administrations claim registration {}; shared identity is unavailable",
                    path.display()
                )));
            }
        };
        let mut marker = None;
        if let Some(admin) = admin {
            match super::super::cleanup_lease::marker_in_private_admin(&admin.path) {
                Ok(Some(lease)) => {
                    observation.owner_project = Some(lease.project_slug.clone());
                    observation.owner_story_id = Some(lease.story_id.clone());
                    match (
                        git::canonical(&lease.repository_path),
                        git::canonical(&lease.worktree_path),
                    ) {
                        (Ok(claimed_repository), Ok(claimed_worktree))
                            if claimed_repository == repository
                                && claimed_worktree == path
                                && record.branch.as_deref() == Some(lease.branch.as_str()) => {}
                        (Err(error), _) | (_, Err(error)) => {
                            mark(
                                &mut observation,
                                "invalid",
                                format!(
                                    "private cleanup marker at {} has an invalid claimed path: {error}",
                                    admin.path.display()
                                ),
                            );
                        }
                        _ => mark(
                            &mut observation,
                            "invalid",
                            format!(
                                "private cleanup marker at {} contradicts Git registration {}",
                                admin.path.display(),
                                path.display()
                            ),
                        ),
                    }
                    marker = Some(lease);
                }
                Ok(None) => {}
                Err(error) => mark(&mut observation, "invalid", error.to_string()),
            }
        }
        let exists = match path.try_exists() {
            Ok(exists) => exists,
            Err(error) => {
                mark(
                    &mut observation,
                    "unknown",
                    format!(
                        "cannot inspect registered worktree {}: {error}",
                        path.display()
                    ),
                );
                false
            }
        };
        let link = if exists {
            match std::fs::symlink_metadata(path.join(".git")) {
                Ok(metadata) if metadata.file_type().is_file() => true,
                Ok(_) => {
                    mark(
                        &mut observation,
                        "invalid",
                        format!(
                            "worktree .git link at {} is not a regular file",
                            path.display()
                        ),
                    );
                    false
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => {
                    mark(
                        &mut observation,
                        "unknown",
                        format!(
                            "cannot inspect worktree .git link at {}: {error}",
                            path.display()
                        ),
                    );
                    false
                }
            }
        } else {
            false
        };
        if !exists || record.prunable || !link {
            mark(
                &mut observation,
                "stale",
                format!(
                    "stale registration at {}; inspect private Git administration and repair registration before reuse or cleanup",
                    path.display()
                ),
            );
        } else if let Some(admin) = admin {
            match git::text(&path, &["rev-parse", "--absolute-git-dir"]) {
                Ok(actual) => match git::canonical(Path::new(actual.trim_end_matches('\n'))) {
                    Ok(actual) if actual == admin.path => {}
                    Ok(_) => mark(
                        &mut observation,
                        "invalid",
                        format!(
                            "worktree .git link at {} points to different private administration",
                            path.display()
                        ),
                    ),
                    Err(error) => mark(&mut observation, "invalid", error.to_string()),
                },
                Err(error) => mark(&mut observation, "invalid", error.to_string()),
            }
        }
        checked.push(CheckedRegistration {
            observation,
            marker,
        });
    }
    for admin in &administrations {
        if !records.iter().any(|record| {
            git::canonical(&record.path.join(".git")).is_ok_and(|gitfile| gitfile == admin.gitfile)
        }) {
            return Err(AppError::Validation(format!(
                "private Git administration {} has no worktree inventory record; ownership is unavailable",
                admin.path.display()
            )));
        }
    }
    Ok(checked)
}

fn mark(observation: &mut ResourceObservation, status: &str, diagnostic: String) {
    fn rank(status: &str) -> u8 {
        match status {
            "healthy" => 0,
            "stale" => 1,
            "unknown" => 2,
            "invalid" => 3,
            _ => 4,
        }
    }
    if rank(status) > rank(&observation.status) {
        observation.status = status.into();
    }
    observation.diagnostics.push(diagnostic);
}
