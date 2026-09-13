//! Safe cleanup of StoryHook-owned Git workspaces (SH-594).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[cfg(test)]
use crate::domain::CLEANUP_LEASE_MARKER;
use crate::domain::{CLEANUP_LEASE_VERSION, StoryCleanupLease, StoryEvent};
use crate::error::AppError;
use crate::process::{Captured, run_captured};
use crate::store::{ReadOps, Store, StoryQuery};

use super::Ctx;

/// One successfully cleaned story workspace.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CleanupRemoval {
    /// Canonical story id from the cleanup lease.
    pub story_id: String,
    /// Exact worktree path the lease owned.
    pub worktree: PathBuf,
    /// Exact branch the lease owned.
    pub branch: String,
    /// Whether the worktree existed before this pass.
    pub removed_worktree: bool,
    /// Whether the local branch existed before this pass.
    pub removed_local_branch: bool,
    /// Whether the remote branch existed before this pass.
    pub removed_remote_branch: bool,
    /// Bytes measured beneath the worktree before removal.
    pub reclaimed_bytes: u64,
}

/// One candidate cleanup deliberately preserved.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CleanupSkip {
    /// Story id, when the candidate carried a decodable lease.
    pub story_id: String,
    /// Stable, machine-readable refusal category.
    pub reason: String,
    /// Context sufficient for a person to repair or retry it.
    pub detail: String,
}

/// One candidate whose Git or filesystem operation failed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CleanupFailure {
    /// Story whose exact leased resources could not be fully cleaned.
    pub story_id: String,
    /// Stable, machine-readable failure category.
    pub reason: String,
    /// Underlying operation diagnostics for repair and retry.
    pub detail: String,
}

/// Result of one project cleanup pass.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CleanupReport {
    /// Project whose linked checkout was inspected.
    pub project: String,
    /// Whether this pass only previewed actions.
    pub dry_run: bool,
    /// Number of distinct valid leases considered.
    pub candidates: usize,
    /// Sum of worktree bytes removed, or eligible in a dry run.
    pub reclaimed_bytes: u64,
    /// Successful removals or dry-run actions.
    pub removed: Vec<CleanupRemoval>,
    /// Candidates preserved by a safety gate.
    pub skipped: Vec<CleanupSkip>,
    /// Candidates whose operational cleanup failed.
    pub failed: Vec<CleanupFailure>,
}

/// Project-scoped cleanup service.
pub struct CleanupService<'ctx, S: Store> {
    ctx: &'ctx Ctx<'ctx, S>,
}

impl<'ctx, S: Store> CleanupService<'ctx, S> {
    /// A cleanup service bound to one project and its registered checkout.
    #[must_use]
    pub fn new(ctx: &'ctx Ctx<'ctx, S>) -> Self {
        Self { ctx }
    }

    /// Cleans every eligible workspace owned by a valid cleanup lease.
    pub fn run(&self, dry_run: bool) -> Result<CleanupReport, AppError> {
        let (project, checkout, rows) = self.ctx.store().read(|tx| {
            let project = tx.project(self.ctx.project())?.ok_or_else(|| {
                crate::store::StoreError::NotFound("selected project disappeared".into())
            })?;
            let checkout = tx.checkout_path(project.id)?;
            let rows = tx.stories(project.id, &StoryQuery::all())?;
            Ok((project, checkout, rows))
        })?;
        let Some(checkout) = checkout else {
            return Err(AppError::Validation(format!(
                "project `{}` has no linked checkout; run `story project link checkout <path>` first",
                project.slug
            )));
        };
        let repository = canonical(&checkout).map_err(AppError::Validation)?;

        let mut leases: BTreeMap<String, StoryCleanupLease> = BTreeMap::new();
        let mut conflicts = BTreeSet::new();
        let mut skipped = Vec::new();
        for row in rows {
            let events = self
                .ctx
                .store()
                .read(|tx| tx.events_for(project.id, row.story_no))?;
            if let Some(lease) = events.iter().rev().find_map(|event| match event.known() {
                Some(StoryEvent::StoryCleanupLeaseRecorded { lease, .. }) => {
                    Some(lease.as_ref().clone())
                }
                _ => None,
            }) {
                let expected = row.story_no.to_id(&project.prefix);
                if lease.story_id != expected {
                    skipped.push(CleanupSkip {
                        story_id: expected,
                        reason: "invalid-lease".into(),
                        detail: format!(
                            "story history carries a cleanup lease for `{}`",
                            lease.story_id
                        ),
                    });
                    continue;
                }
                insert_lease(
                    &project.slug,
                    lease,
                    &mut leases,
                    &mut conflicts,
                    &mut skipped,
                );
            }
        }
        discover_worktree_markers(
            &repository,
            &project.slug,
            &mut leases,
            &mut conflicts,
            &mut skipped,
        );

        let candidates = leases.len();
        let mut removed = Vec::new();
        let mut failed = Vec::new();
        for lease in leases.into_values() {
            let options = super::resources::ResourceOptions {
                lease_json: Some(serde_json::to_string(&lease)?),
                ..Default::default()
            };
            let observed =
                super::resources::ResourceService::new(self.ctx).resolve(&lease.story_id, &options);
            let result = match observed {
                Ok(report) if report.status == "resolved" && report.pane.is_none() => {
                    clean_candidate(&lease.repository_path, &lease, dry_run)
                }
                Ok(report) => Err(CleanupSkip {
                    story_id: lease.story_id.clone(),
                    reason: "resource-identity-unsafe".into(),
                    detail: format!(
                        "resource status {}; panes {:?}; {}",
                        report.status,
                        report.pane,
                        report.diagnostics.join("; ")
                    ),
                }),
                Err(error) => Err(CleanupSkip {
                    story_id: lease.story_id.clone(),
                    reason: "resource-unverifiable".into(),
                    detail: error.to_string(),
                }),
            };
            match result {
                Ok(removal) => removed.push(removal),
                Err(issue) if is_operational_failure(&issue.reason) => {
                    failed.push(CleanupFailure {
                        story_id: issue.story_id,
                        reason: issue.reason,
                        detail: issue.detail,
                    });
                }
                Err(skip) => skipped.push(skip),
            }
        }
        let reclaimed_bytes = removed.iter().map(|item| item.reclaimed_bytes).sum();
        Ok(CleanupReport {
            project: project.slug,
            dry_run,
            candidates,
            reclaimed_bytes,
            removed,
            skipped,
            failed,
        })
    }
}

fn is_operational_failure(reason: &str) -> bool {
    matches!(
        reason,
        "fetch-failed"
            | "remote-unverifiable"
            | "remove-worktree-failed"
            | "delete-local-branch-failed"
            | "delete-remote-branch-failed"
            | "postcondition-unverifiable"
            | "postcondition-failed"
    )
}

fn insert_lease(
    project: &str,
    lease: StoryCleanupLease,
    leases: &mut BTreeMap<String, StoryCleanupLease>,
    conflicts: &mut BTreeSet<String>,
    skipped: &mut Vec<CleanupSkip>,
) {
    if lease.version != CLEANUP_LEASE_VERSION || lease.project_slug != project {
        skipped.push(CleanupSkip {
            story_id: lease.story_id,
            reason: "invalid-lease".into(),
            detail: "cleanup lease version or project does not match this pass".into(),
        });
        return;
    }
    if conflicts.contains(&lease.story_id) {
        return;
    }
    if let Some(existing) = leases.get(&lease.story_id) {
        if existing != &lease {
            leases.remove(&lease.story_id);
            conflicts.insert(lease.story_id.clone());
            skipped.push(CleanupSkip {
                story_id: lease.story_id,
                reason: "conflicting-lease".into(),
                detail: "multiple cleanup leases claim different resources for this story".into(),
            });
        }
        return;
    }
    leases.insert(lease.story_id.clone(), lease);
}

fn discover_worktree_markers(
    repository: &Path,
    project: &str,
    leases: &mut BTreeMap<String, StoryCleanupLease>,
    conflicts: &mut BTreeSet<String>,
    skipped: &mut Vec<CleanupSkip>,
) {
    let records = match super::resources::git::inventory(repository) {
        Ok(records) => records,
        Err(error) => {
            skipped.push(CleanupSkip {
                story_id: String::new(),
                reason: "worktree-unverifiable".into(),
                detail: error.to_string(),
            });
            return;
        }
    };
    for record in records {
        let path = record.path;
        if path == repository {
            continue;
        }
        let story_id = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        match super::cleanup_lease::marker_at_registered(&path) {
            Ok(Some(lease)) => insert_lease(project, lease, leases, conflicts, skipped),
            Ok(None) => skipped.push(CleanupSkip {
                story_id,
                reason: "missing-lease".into(),
                detail: format!("{} has no StoryHook cleanup lease", path.display()),
            }),
            Err(error) => skipped.push(CleanupSkip {
                story_id,
                reason: if error.to_string().contains("mismatch") {
                    "worktree-mismatch"
                } else {
                    "invalid-lease"
                }
                .into(),
                detail: error.to_string(),
            }),
        }
    }
}

fn clean_candidate(
    repository: &Path,
    lease: &StoryCleanupLease,
    dry_run: bool,
) -> Result<CleanupRemoval, CleanupSkip> {
    let refuse = |reason: &str, detail: String| CleanupSkip {
        story_id: lease.story_id.clone(),
        reason: reason.into(),
        detail,
    };
    let leased_repo = canonical(&lease.repository_path)
        .map_err(|detail| refuse("repository-unavailable", detail))?;
    if leased_repo != repository {
        return Err(refuse(
            "repository-mismatch",
            format!(
                "lease names {}, project uses {}",
                leased_repo.display(),
                repository.display()
            ),
        ));
    }
    let worktree_exists = lease.worktree_path.exists();
    if worktree_exists {
        let actual = canonical(&lease.worktree_path)
            .map_err(|detail| refuse("worktree-unavailable", detail))?;
        if actual != lease.worktree_path {
            return Err(refuse(
                "worktree-mismatch",
                format!("non-canonical path {}", lease.worktree_path.display()),
            ));
        }
        let status = git_text(&lease.worktree_path, &["status", "--porcelain"])
            .map_err(|detail| refuse("worktree-unverifiable", detail))?;
        if !status.is_empty() {
            return Err(refuse(
                "dirty-worktree",
                lease.worktree_path.display().to_string(),
            ));
        }
    }
    let records = super::resources::git::inventory(repository)
        .map_err(|error| refuse("worktree-unverifiable", error.to_string()))?;
    let record = records
        .iter()
        .find(|record| record.path == lease.worktree_path);
    if let Some(record) = record {
        if record.locked {
            return Err(refuse(
                "locked-worktree",
                lease.worktree_path.display().to_string(),
            ));
        }
        if record.branch.as_deref() != Some(lease.branch.as_str()) {
            return Err(refuse(
                "worktree-mismatch",
                format!(
                    "{} is checked out on {:?}",
                    lease.worktree_path.display(),
                    record.branch
                ),
            ));
        }
    } else if worktree_exists {
        return Err(refuse(
            "worktree-unregistered",
            lease.worktree_path.display().to_string(),
        ));
    }

    ensure_window_absent(lease).map_err(|detail| refuse("tmux-window-open", detail))?;
    let default_branch = origin_default_branch(repository)
        .map_err(|detail| refuse("default-branch-unverifiable", detail))?;
    let default_branch = default_branch.as_str();
    if matches!(lease.branch.as_str(), "main" | "master") || lease.branch == default_branch {
        return Err(refuse("protected-branch", lease.branch.clone()));
    }
    super::resources::validate_lease(lease)
        .map_err(|error| refuse("worktree-mismatch", error.to_string()))?;
    let default_spec = format!("+refs/heads/{default_branch}:refs/remotes/origin/{default_branch}");
    let fetch = git(repository, &["fetch", "--quiet", "origin", &default_spec])
        .map_err(|error| refuse("fetch-failed", error))?;
    if !fetch.status.success() {
        return Err(refuse("fetch-failed", stderr(&fetch)));
    }

    let local_ref = format!("refs/heads/{}", lease.branch);
    let remote_ref = format!("refs/remotes/origin/{}", lease.branch);
    let base_ref = format!("refs/remotes/origin/{default_branch}");
    let remote_output = git(
        repository,
        &["ls-remote", "--heads", "origin", &lease.branch],
    )
    .map_err(|error| refuse("remote-unverifiable", error))?;
    if !remote_output.status.success() {
        return Err(refuse("remote-unverifiable", stderr(&remote_output)));
    }
    let remote_exists = !remote_output.stdout.is_empty();
    if remote_exists {
        let branch_spec = format!("+refs/heads/{}:{remote_ref}", lease.branch);
        let branch_fetch = git(repository, &["fetch", "--quiet", "origin", &branch_spec])
            .map_err(|error| refuse("fetch-failed", error))?;
        if !branch_fetch.status.success() {
            return Err(refuse("fetch-failed", stderr(&branch_fetch)));
        }
    }
    let mut tips = BTreeSet::new();
    if worktree_exists {
        tips.insert(
            git_text(&lease.worktree_path, &["rev-parse", "HEAD"])
                .map_err(|detail| refuse("worktree-unverifiable", detail))?,
        );
    }
    for reference in [&local_ref, &remote_ref] {
        if ref_exists(repository, reference) {
            tips.insert(
                git_text(repository, &["rev-parse", reference])
                    .map_err(|detail| refuse("branch-unverifiable", detail))?,
            );
        }
    }
    for tip in tips {
        let answer = git(
            repository,
            &["merge-base", "--is-ancestor", &tip, &base_ref],
        )
        .map_err(|error| refuse("merge-unverifiable", error))?;
        if !answer.status.success() {
            return Err(refuse(
                "unmerged-work",
                format!("{tip} is not reachable from {base_ref}"),
            ));
        }
    }

    let removed_worktree = worktree_exists;
    let removed_local_branch = ref_exists(repository, &local_ref);
    let removed_remote_branch = remote_exists;
    let reclaimed_bytes = if worktree_exists {
        directory_size(&lease.worktree_path)
    } else {
        0
    };
    if dry_run {
        return Ok(CleanupRemoval {
            story_id: lease.story_id.clone(),
            worktree: lease.worktree_path.clone(),
            branch: lease.branch.clone(),
            removed_worktree,
            removed_local_branch,
            removed_remote_branch,
            reclaimed_bytes,
        });
    }
    if removed_worktree {
        run_git(
            repository,
            &[
                "worktree",
                "remove",
                lease.worktree_path.to_string_lossy().as_ref(),
            ],
        )
        .map_err(|detail| refuse("remove-worktree-failed", detail))?;
    }
    if removed_local_branch {
        run_git(repository, &["branch", "-D", &lease.branch])
            .map_err(|detail| refuse("delete-local-branch-failed", detail))?;
    }
    if removed_remote_branch {
        run_git(
            repository,
            &["push", "--quiet", "origin", "--delete", &lease.branch],
        )
        .map_err(|detail| refuse("delete-remote-branch-failed", detail))?;
    }
    let remote_after = git(
        repository,
        &["ls-remote", "--heads", "origin", &lease.branch],
    )
    .map_err(|error| refuse("postcondition-unverifiable", error))?;
    if !remote_after.status.success() {
        return Err(refuse("postcondition-unverifiable", stderr(&remote_after)));
    }
    let registration_remains = super::resources::git::inventory(repository)
        .map_err(|error| refuse("postcondition-unverifiable", error.to_string()))?
        .iter()
        .any(|record| record.path == lease.worktree_path);
    if registration_remains
        || lease.worktree_path.exists()
        || ref_exists(repository, &local_ref)
        || !remote_after.stdout.is_empty()
    {
        return Err(refuse(
            "postcondition-failed",
            "worktree path, local branch, or remote branch remains".into(),
        ));
    }
    Ok(CleanupRemoval {
        story_id: lease.story_id.clone(),
        worktree: lease.worktree_path.clone(),
        branch: lease.branch.clone(),
        removed_worktree,
        removed_local_branch,
        removed_remote_branch,
        reclaimed_bytes,
    })
}

fn ensure_window_absent(lease: &StoryCleanupLease) -> Result<(), String> {
    let names = super::resources::lease_names(lease, &BTreeSet::new());
    let panes = super::resources::tmux::panes(&lease.tmux.socket_path, &names)
        .map_err(|e| format!("cannot prove tmux window absence: {e}"))?;
    if panes.is_empty() {
        Ok(())
    } else {
        Err(format!("tmux windows are still open: {panes:?}"))
    }
}

fn canonical(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize()
        .map_err(|error| format!("cannot resolve {}: {error}", path.display()))
}
fn git(cwd: &Path, args: &[&str]) -> Result<Captured, String> {
    let mut command = crate::env::git_env::command(cwd);
    command.args(args);
    run_captured(command, Duration::from_secs(60))
        .map_err(|error| format!("git {} failed: {}", args.join(" "), error.detail()))
}
fn git_text(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let output = git(cwd, args)?;
    if !output.status.success() {
        return Err(stderr(&output));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The name of origin's default branch, asked of origin itself (`git
/// ls-remote --symref origin HEAD`) — never the local `refs/remotes/origin/HEAD`
/// cache, which git writes at clone time and no fetch refreshes, so it kept
/// answering `main` after this repository's default moved to `dev` (SH-691);
/// and never a literal. An origin that does not answer, or that advertises no
/// symbolic HEAD (unborn or detached: `ls-remote` then prints no `ref:` line
/// at exit 0), is an error naming why — absence is not an answer (SH-372).
/// The plugin's `default_branch` and the verifier bundle's
/// `origin-default-branch.sh` are this derivation's shell copies.
fn origin_default_branch(repository: &Path) -> Result<String, String> {
    let advertised = git_text(repository, &["ls-remote", "--symref", "origin", "HEAD"])
        .map_err(|detail| format!("origin did not answer: {detail}"))?;
    advertised
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .find(|(target, name)| *name == "HEAD" && target.starts_with("ref: "))
        .and_then(|(target, _)| target.strip_prefix("ref: refs/heads/"))
        .map(str::to_string)
        .ok_or_else(|| {
            "origin advertises no symbolic HEAD (its default branch is unborn or detached)"
                .to_string()
        })
}
fn run_git(cwd: &Path, args: &[&str]) -> Result<(), String> {
    let output = git(cwd, args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(stderr(&output))
    }
}
fn ref_exists(cwd: &Path, reference: &str) -> bool {
    git(cwd, &["show-ref", "--verify", "--quiet", reference]).is_ok_and(|out| out.status.success())
}
fn stderr(output: &Captured) -> String {
    let text = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if text.is_empty() {
        format!("process exited {}", output.status)
    } else {
        text
    }
}
fn directory_size(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_file() || metadata.file_type().is_symlink() {
        return metadata.len();
    }
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| directory_size(&entry.path()))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::TmuxCleanupTarget;

    struct Repo {
        _root: tempfile::TempDir,
        checkout: PathBuf,
        worktree: PathBuf,
        lease: StoryCleanupLease,
    }

    impl Repo {
        fn new(merged: bool) -> Self {
            let root = tempfile::Builder::new()
                .prefix("storyhook-cleanup-")
                .tempdir_in("/private/tmp")
                .unwrap();
            let origin = root.path().join("origin.git");
            let checkout = root.path().join("repo");
            let worktree = root.path().join("SH-7");
            run_git(
                root.path(),
                &["init", "--bare", origin.to_string_lossy().as_ref()],
            )
            .unwrap();
            run_git(
                root.path(),
                &[
                    "clone",
                    origin.to_string_lossy().as_ref(),
                    checkout.to_string_lossy().as_ref(),
                ],
            )
            .unwrap();
            run_git(&checkout, &["config", "user.name", "Cleanup Test"]).unwrap();
            run_git(&checkout, &["config", "user.email", "cleanup@example.test"]).unwrap();
            fs::write(checkout.join("README"), "base").unwrap();
            fs::write(checkout.join(".gitignore"), "target/\n").unwrap();
            run_git(&checkout, &["add", "README", ".gitignore"]).unwrap();
            run_git(&checkout, &["commit", "-m", "base"]).unwrap();
            run_git(&checkout, &["branch", "-M", "dev"]).unwrap();
            run_git(&checkout, &["push", "-u", "origin", "dev"]).unwrap();
            run_git(&origin, &["symbolic-ref", "HEAD", "refs/heads/dev"]).unwrap();
            run_git(&checkout, &["remote", "set-head", "origin", "dev"]).unwrap();
            run_git(
                &checkout,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "worktree-SH-7",
                    worktree.to_string_lossy().as_ref(),
                    "dev",
                ],
            )
            .unwrap();
            fs::write(worktree.join("work"), "merged work").unwrap();
            fs::create_dir(worktree.join("target")).unwrap();
            fs::write(worktree.join("target/artifact"), vec![0_u8; 4096]).unwrap();
            run_git(&worktree, &["add", "work"]).unwrap();
            run_git(&worktree, &["commit", "-m", "story work"]).unwrap();
            run_git(&worktree, &["push", "-u", "origin", "worktree-SH-7"]).unwrap();
            if merged {
                run_git(
                    &checkout,
                    &["merge", "--no-ff", "-m", "merge story", "worktree-SH-7"],
                )
                .unwrap();
                run_git(&checkout, &["push", "origin", "dev"]).unwrap();
            }
            let lease = StoryCleanupLease {
                version: CLEANUP_LEASE_VERSION,
                project_slug: "fixture".into(),
                story_id: "SH-7".into(),
                repository_path: checkout.canonicalize().unwrap(),
                worktree_path: worktree.canonicalize().unwrap(),
                branch: "worktree-SH-7".into(),
                tmux: TmuxCleanupTarget {
                    socket_path: root.path().join("no-tmux.sock"),
                },
            };
            Self {
                _root: root,
                checkout,
                worktree,
                lease,
            }
        }
    }

    #[test]
    fn merged_inactive_story_removes_worktree_artifacts_and_both_branches() {
        let repo = Repo::new(true);
        let removal = clean_candidate(&repo.checkout, &repo.lease, false).unwrap();
        assert!(removal.removed_worktree);
        assert!(removal.removed_local_branch);
        assert!(removal.removed_remote_branch);
        assert!(removal.reclaimed_bytes >= 4096);
        assert!(!repo.worktree.exists());
        assert!(!ref_exists(&repo.checkout, "refs/heads/worktree-SH-7"));
        let remote = git_text(
            &repo.checkout,
            &["ls-remote", "--heads", "origin", "worktree-SH-7"],
        )
        .unwrap();
        assert!(remote.is_empty());

        let retry = clean_candidate(&repo.checkout, &repo.lease, false).unwrap();
        assert!(!retry.removed_worktree);
        assert!(!retry.removed_local_branch);
        assert!(!retry.removed_remote_branch);
    }

    #[test]
    fn unmerged_or_dirty_work_is_preserved() {
        let unmerged = Repo::new(false);
        let refusal = clean_candidate(&unmerged.checkout, &unmerged.lease, false).unwrap_err();
        assert_eq!(refusal.reason, "unmerged-work");
        assert!(unmerged.worktree.exists());

        let dirty = Repo::new(true);
        fs::write(dirty.worktree.join("uncommitted"), "mine").unwrap();
        let refusal = clean_candidate(&dirty.checkout, &dirty.lease, false).unwrap_err();
        assert_eq!(refusal.reason, "dirty-worktree");
        assert!(dirty.worktree.exists());
    }

    #[test]
    fn dry_run_reports_bytes_without_removing_anything() {
        let repo = Repo::new(true);
        let removal = clean_candidate(&repo.checkout, &repo.lease, true).unwrap();
        assert!(removal.reclaimed_bytes >= 4096);
        assert!(repo.worktree.exists());
        assert!(ref_exists(&repo.checkout, "refs/heads/worktree-SH-7"));
    }

    #[test]
    fn locked_worktree_and_divergent_remote_are_preserved() {
        let locked = Repo::new(true);
        run_git(
            &locked.checkout,
            &[
                "worktree",
                "lock",
                locked.worktree.to_string_lossy().as_ref(),
            ],
        )
        .unwrap();
        let refusal = clean_candidate(&locked.checkout, &locked.lease, false).unwrap_err();
        assert_eq!(refusal.reason, "locked-worktree");

        let divergent = Repo::new(true);
        let remote = git_text(&divergent.checkout, &["remote", "get-url", "origin"]).unwrap();
        let clone = divergent._root.path().join("remote-writer");
        run_git(
            divergent._root.path(),
            &["clone", remote.as_str(), clone.to_string_lossy().as_ref()],
        )
        .unwrap();
        run_git(&clone, &["config", "user.name", "Cleanup Test"]).unwrap();
        run_git(&clone, &["config", "user.email", "cleanup@example.test"]).unwrap();
        run_git(&clone, &["checkout", "worktree-SH-7"]).unwrap();
        fs::write(clone.join("remote-only"), "not merged").unwrap();
        run_git(&clone, &["add", "remote-only"]).unwrap();
        run_git(&clone, &["commit", "-m", "remote divergence"]).unwrap();
        run_git(&clone, &["push", "origin", "worktree-SH-7"]).unwrap();

        let refusal = clean_candidate(&divergent.checkout, &divergent.lease, false).unwrap_err();
        assert_eq!(refusal.reason, "unmerged-work");
        assert!(divergent.worktree.exists());
    }

    #[test]
    fn remote_deletion_failure_never_claims_complete_cleanup() {
        use std::os::unix::fs::PermissionsExt;

        let repo = Repo::new(true);
        let remote =
            PathBuf::from(git_text(&repo.checkout, &["remote", "get-url", "origin"]).unwrap());
        let hook = remote.join("hooks/pre-receive");
        fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        let mut permissions = fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&hook, permissions).unwrap();

        let refusal = clean_candidate(&repo.checkout, &repo.lease, false).unwrap_err();

        assert_eq!(refusal.reason, "delete-remote-branch-failed");
        assert!(!repo.worktree.exists(), "earlier worktree leg did complete");
        assert!(!ref_exists(&repo.checkout, "refs/heads/worktree-SH-7"));
        assert!(
            !git_text(
                &repo.checkout,
                &["ls-remote", "--heads", "origin", "worktree-SH-7"]
            )
            .unwrap()
            .is_empty(),
            "the report must remain a failure while the remote ref survives"
        );
        assert!(is_operational_failure(&refusal.reason));
    }

    #[test]
    fn conflicting_leases_revoke_cleanup_authority() {
        let repo = Repo::new(true);
        let mut leases = BTreeMap::new();
        let mut conflicts = BTreeSet::new();
        let mut skipped = Vec::new();
        insert_lease(
            "fixture",
            repo.lease.clone(),
            &mut leases,
            &mut conflicts,
            &mut skipped,
        );
        let mut conflict = repo.lease.clone();
        conflict.branch = "different-branch".into();
        insert_lease(
            "fixture",
            conflict,
            &mut leases,
            &mut conflicts,
            &mut skipped,
        );

        assert!(leases.is_empty());
        assert!(conflicts.contains("SH-7"));
        assert_eq!(skipped[0].reason, "conflicting-lease");
    }

    #[test]
    fn lease_identity_and_default_branch_are_fail_closed() {
        let repo = Repo::new(true);
        let mut mismatched = repo.lease.clone();
        mismatched.branch = "different-branch".into();
        let refusal = clean_candidate(&repo.checkout, &mismatched, false).unwrap_err();
        assert_eq!(refusal.reason, "worktree-mismatch");

        let mut protected = repo.lease.clone();
        protected.worktree_path = repo._root.path().join("already-absent");
        protected.branch = "dev".into();
        let refusal = clean_candidate(&repo.checkout, &protected, false).unwrap_err();
        assert_eq!(refusal.reason, "protected-branch");
    }

    /// SH-691: the default branch is origin's own answer, not the local
    /// `refs/remotes/origin/HEAD` cache — here the cache is made to say `main`,
    /// a branch origin does not even have, and origin's `dev` stays protected.
    #[test]
    fn the_default_branch_is_asked_of_origin_not_the_local_cache() {
        let repo = Repo::new(true);
        run_git(
            &repo.checkout,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        )
        .unwrap();
        let mut protected = repo.lease.clone();
        protected.worktree_path = repo._root.path().join("already-absent");
        protected.branch = "dev".into();
        let refusal = clean_candidate(&repo.checkout, &protected, false).unwrap_err();
        assert_eq!(refusal.reason, "protected-branch");
    }

    /// SH-691: an origin whose HEAD is detached advertises no default branch;
    /// that is unverifiable, never a guess of `main`.
    #[test]
    fn an_origin_without_a_default_branch_is_unverifiable_never_guessed() {
        let repo = Repo::new(true);
        let origin = repo._root.path().join("origin.git");
        let tip = git_text(&origin, &["rev-parse", "refs/heads/dev"]).unwrap();
        run_git(&origin, &["update-ref", "--no-deref", "HEAD", &tip]).unwrap();
        let mut lease = repo.lease.clone();
        lease.worktree_path = repo._root.path().join("already-absent");
        let refusal = clean_candidate(&repo.checkout, &lease, false).unwrap_err();
        assert_eq!(refusal.reason, "default-branch-unverifiable");
        assert!(
            refusal.detail.contains("no symbolic HEAD"),
            "{}",
            refusal.detail
        );
    }

    #[test]
    fn malformed_or_misdirected_private_markers_never_authorize_cleanup() {
        let repo = Repo::new(true);
        let git_dir = git_text(&repo.worktree, &["rev-parse", "--absolute-git-dir"]).unwrap();
        let marker = Path::new(&git_dir).join(CLEANUP_LEASE_MARKER);
        let mut misdirected = repo.lease.clone();
        misdirected.worktree_path = repo._root.path().join("different-worktree");
        fs::write(&marker, serde_json::to_vec(&misdirected).unwrap()).unwrap();

        let mut leases = BTreeMap::new();
        let mut conflicts = BTreeSet::new();
        let mut skipped = Vec::new();
        discover_worktree_markers(
            &repo.checkout,
            "fixture",
            &mut leases,
            &mut conflicts,
            &mut skipped,
        );
        assert!(leases.is_empty());
        assert_eq!(skipped[0].reason, "worktree-mismatch");

        fs::write(&marker, "not json").unwrap();
        skipped.clear();
        discover_worktree_markers(
            &repo.checkout,
            "fixture",
            &mut leases,
            &mut conflicts,
            &mut skipped,
        );
        assert!(leases.is_empty());
        assert_eq!(skipped[0].reason, "invalid-lease");
    }

    #[test]
    fn tmux_gate_matches_the_exact_story_window_and_fails_closed() {
        let repo = Repo::new(true);
        let socket = repo._root.path().join("tmux.sock");
        let socket_text = socket.to_string_lossy();
        let started = Command::new("tmux")
            .args([
                "-S",
                socket_text.as_ref(),
                "new-session",
                "-d",
                "-s",
                "cleanup-test",
                "-n",
                "OTHER",
            ])
            .status()
            .unwrap();
        assert!(started.success());
        let mut lease = repo.lease.clone();
        lease.tmux.socket_path = socket.clone();
        let other_window = ensure_window_absent(&lease);
        let created = Command::new("tmux")
            .args([
                "-S",
                socket_text.as_ref(),
                "new-window",
                "-n",
                lease.story_id.as_str(),
            ])
            .status()
            .unwrap();
        assert!(created.success());
        let exact_window = ensure_window_absent(&lease);
        let _ = Command::new("tmux")
            .args(["-S", socket_text.as_ref(), "kill-server"])
            .status();

        assert!(other_window.is_ok());
        assert!(exact_window.unwrap_err().contains("still open"));

        fs::remove_file(&socket).unwrap();
        fs::write(&socket, "not a tmux socket").unwrap();
        assert!(
            ensure_window_absent(&lease)
                .unwrap_err()
                .contains("cannot prove tmux window absence")
        );
    }
}
