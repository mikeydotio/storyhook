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
use crate::domain::{CLEANUP_LEASE_VERSION, StoryCleanupLease, SuperState};
use crate::error::AppError;
use crate::process::{Captured, run_captured};
use crate::store::{ReadOps, Store, StoryQuery};

mod dropped;

use super::Ctx;
use super::verification::{ReapMarker, VerificationGeneration, latest_generation};

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
    ///
    /// There is no remote counterpart: the verifier's merge step deletes the
    /// remote branch and cleanup never reads or writes it (SH-653).
    pub removed_local_branch: bool,
    /// Whether the exact dropped-story window was removed, or would be removed.
    #[serde(default)]
    pub removed_tmux_window: bool,
    /// Whether the local branch was deliberately retained for recovery.
    #[serde(default)]
    pub retained_local_branch: bool,
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
        let mut stories: BTreeMap<String, StoryFacts> = BTreeMap::new();
        for row in rows {
            let events = self
                .ctx
                .store()
                .read(|tx| tx.events_for(project.id, row.story_no))?;
            let generation = latest_generation(&events);
            let expected = row.story_no.to_id(&project.prefix);
            if row.state == "dropped" {
                if let Some(lease) = events.iter().rev().find_map(|e| match e.known() {
                    Some(crate::domain::StoryEvent::StoryCleanupLeaseRecorded {
                        lease, ..
                    }) => Some(lease.as_ref().clone()),
                    _ => None,
                }) {
                    if lease.story_id != expected {
                        conflicts.insert(expected.clone());
                        leases.remove(&expected);
                        skipped.push(CleanupSkip {
                            story_id: expected.clone(),
                            reason: "invalid-lease".into(),
                            detail: format!(
                                "story history carries a cleanup lease for {}",
                                lease.story_id
                            ),
                        });
                    } else {
                        insert_lease(
                            &project.slug,
                            lease,
                            &mut leases,
                            &mut conflicts,
                            &mut skipped,
                        );
                    }
                }
                if let Some(record) = self
                    .ctx
                    .store()
                    .read(|tx| tx.dropped_cleanup(project.id, row.story_no))?
                    && (!record.released || Some(record.generation) == dropped::generation(&events))
                {
                    insert_lease(
                        &project.slug,
                        record.lease,
                        &mut leases,
                        &mut conflicts,
                        &mut skipped,
                    );
                }
            }
            if row.state != "dropped"
                && let Some(lease) = generation.as_ref().and_then(|found| found.lease.clone())
            {
                if lease.story_id != expected {
                    skipped.push(CleanupSkip {
                        story_id: expected.clone(),
                        reason: "invalid-lease".into(),
                        detail: format!(
                            "story history carries a cleanup lease for `{}`",
                            lease.story_id
                        ),
                    });
                } else {
                    insert_lease(
                        &project.slug,
                        lease,
                        &mut leases,
                        &mut conflicts,
                        &mut skipped,
                    );
                }
            }
            stories.insert(
                expected,
                StoryFacts {
                    state: row.state,
                    superstate: row.superstate,
                    generation,
                },
            );
        }
        for run in self.ctx.store().read(|tx| tx.engine_runs(&project.slug))? {
            for lane in self.ctx.store().read(|tx| tx.engine_lanes(&run.id))? {
                if let Some(lease) = lane.cleanup_lease
                    && stories
                        .get(&lease.story_id)
                        .is_some_and(|facts| facts.state == "dropped")
                {
                    if lane.story_id.as_deref() != Some(lease.story_id.as_str()) {
                        conflicts.insert(lease.story_id.clone());
                        leases.remove(&lease.story_id);
                        skipped.push(CleanupSkip {
                            story_id: lease.story_id,
                            reason: "invalid-lease".into(),
                            detail: "engine lane story differs from its cleanup lease".into(),
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
            if stories
                .get(&lease.story_id)
                .is_some_and(|facts| facts.state == "dropped")
            {
                match dropped::run(self.ctx, &repository, &lease, dry_run) {
                    Ok(Some(removal)) => removed.push(removal),
                    Ok(None) => {}
                    Err(issue) if is_operational_failure(&issue.reason) => {
                        failed.push(CleanupFailure {
                            story_id: issue.story_id,
                            reason: issue.reason,
                            detail: issue.detail,
                        })
                    }
                    Err(issue) => skipped.push(issue),
                }
                continue;
            }
            // The verifier's release is read from the store before any git
            // or network work on the candidate: a refused story costs no
            // fetch, and a story the verifier has not finished with is never
            // inspected on disk at all.
            let marker = match verifier_release(&lease.story_id, stories.get(&lease.story_id)) {
                Ok(marker) => marker,
                Err(skip) => {
                    skipped.push(skip);
                    continue;
                }
            };
            // A lease whose resources are already gone is not a removal —
            // decided from local facts alone, before the fetch. After a
            // COMPLETE it is what the verifier already verified and is not
            // worth a line on every pass; after a REQUIRED it is a retry
            // with nothing left to retry, which is worth saying.
            if nothing_left(&repository, &lease) {
                if marker == ReapMarker::Required {
                    skipped.push(CleanupSkip {
                        story_id: lease.story_id.clone(),
                        reason: "already-clean".into(),
                        detail: "exact worktree and local branch are absent; nothing to retry"
                            .into(),
                    });
                }
                continue;
            }
            let options = super::resources::ResourceOptions {
                lease_json: Some(serde_json::to_string(&lease)?),
                ..Default::default()
            };
            let observed =
                super::resources::ResourceService::new(self.ctx).resolve(&lease.story_id, &options);
            let result = match observed {
                Ok(report) if report.status == "resolved" && report.pane.is_none() => {
                    // Not `&lease.repository_path`: `resolve` above only
                    // established that this repository is *associated* with
                    // the project (by origin URL or UUID pointer), which a
                    // second, unregistered clone of the same origin also
                    // satisfies. `clean_candidate`'s own repository-mismatch
                    // guard exists to refuse exactly that clone; passing the
                    // lease's own path here would make the guard compare a
                    // value against itself (SH-653).
                    clean_candidate(&repository, &lease, dry_run)
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

/// What the store says about the story a lease names, read once per pass.
struct StoryFacts {
    state: String,
    superstate: SuperState,
    generation: Option<VerificationGeneration>,
}

/// The verifier's release of a leased workspace to cleanup (SH-653, D-H).
///
/// Cleanup is the reap's retry path, never an independent reaper: a workspace
/// is touchable only when its story is CLOSED and the verifier wrote a
/// CLEANUP COMPLETE or CLEANUP REQUIRED marker for the story's **latest**
/// verification generation. Anything else is refused with a reason a person
/// can read back from `--dry-run`. The marker is read through the same
/// [`latest_generation`] the verifier's own retry uses, so the two cannot
/// disagree about which workspace the verifier owns.
fn verifier_release(story_id: &str, facts: Option<&StoryFacts>) -> Result<ReapMarker, CleanupSkip> {
    let refuse = |reason: &str, detail: String| CleanupSkip {
        story_id: story_id.to_string(),
        reason: reason.into(),
        detail,
    };
    let Some(facts) = facts else {
        return Err(refuse(
            "unknown-story",
            format!("no story `{story_id}` in this project; a lease is not a story"),
        ));
    };
    if facts.superstate != SuperState::Closed {
        return Err(refuse(
            "story-open",
            format!(
                "`{story_id}` is `{}`; cleanup touches only a CLOSED story's workspace",
                facts.state
            ),
        ));
    }
    match facts
        .generation
        .as_ref()
        .and_then(|generation| generation.reap_marker)
    {
        Some(marker) => Ok(marker),
        None => Err(refuse(
            "not-verifier-released",
            "no CENTRAL VERIFICATION CLEANUP COMPLETE/REQUIRED comment on its latest \
             verification; the verifier owns the first reap"
                .into(),
        )),
    }
}

/// Whether every resource cleanup would remove is already absent: the
/// worktree path, its registration, and the local branch. Local facts only,
/// so a pass over an already-reaped backlog costs no network.
fn nothing_left(repository: &Path, lease: &StoryCleanupLease) -> bool {
    if lease.worktree_path.exists()
        || ref_exists(repository, &format!("refs/heads/{}", lease.branch))
    {
        return false;
    }
    super::resources::git::inventory(repository).is_ok_and(|records| {
        records
            .iter()
            .all(|record| record.path != lease.worktree_path)
    })
}

fn is_operational_failure(reason: &str) -> bool {
    matches!(
        reason,
        "dropped-cleanup-failed"
            | "fetch-failed"
            | "remove-worktree-failed"
            | "delete-local-branch-failed"
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

    // Only what cleanup itself would delete has to be reachable: the worktree
    // and the local branch. The remote branch is neither read nor written —
    // `land-pr.sh` deletes it at merge time, and nothing on the remote can be
    // lost by a tool that never touches it.
    let local_ref = format!("refs/heads/{}", lease.branch);
    let base_ref = format!("refs/remotes/origin/{default_branch}");
    let mut tips = BTreeSet::new();
    if worktree_exists {
        tips.insert(
            git_text(&lease.worktree_path, &["rev-parse", "HEAD"])
                .map_err(|detail| refuse("worktree-unverifiable", detail))?,
        );
    }
    if ref_exists(repository, &local_ref) {
        tips.insert(
            git_text(repository, &["rev-parse", &local_ref])
                .map_err(|detail| refuse("branch-unverifiable", detail))?,
        );
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
            removed_tmux_window: false,
            retained_local_branch: false,
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
    let registration_remains = super::resources::git::inventory(repository)
        .map_err(|error| refuse("postcondition-unverifiable", error.to_string()))?
        .iter()
        .any(|record| record.path == lease.worktree_path);
    if registration_remains || lease.worktree_path.exists() || ref_exists(repository, &local_ref) {
        return Err(refuse(
            "postcondition-failed",
            "worktree path or local branch remains".into(),
        ));
    }
    Ok(CleanupRemoval {
        story_id: lease.story_id.clone(),
        worktree: lease.worktree_path.clone(),
        branch: lease.branch.clone(),
        removed_worktree,
        removed_local_branch,
        removed_tmux_window: false,
        retained_local_branch: false,
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
pub(crate) fn origin_default_branch(repository: &Path) -> Result<String, String> {
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
        workspace: storyhook_test_support::StoryWorkspace,
        checkout: PathBuf,
        worktree: PathBuf,
        lease: StoryCleanupLease,
    }

    impl Repo {
        fn new(merged: bool) -> Self {
            let workspace = storyhook_test_support::StoryWorkspace::new("SH-7", merged);
            let lease = StoryCleanupLease {
                version: CLEANUP_LEASE_VERSION,
                project_slug: "fixture".into(),
                story_id: workspace.story_id.clone(),
                repository_path: workspace.checkout.clone(),
                worktree_path: workspace.worktree.clone(),
                branch: workspace.branch.clone(),
                tmux: TmuxCleanupTarget {
                    socket_path: workspace.root.path().join("no-tmux.sock"),
                },
            };
            Self {
                checkout: workspace.checkout.clone(),
                worktree: workspace.worktree.clone(),
                lease,
                workspace,
            }
        }

        fn root(&self) -> &Path {
            self.workspace.root.path()
        }
    }

    fn facts(superstate: SuperState, marker: Option<ReapMarker>) -> StoryFacts {
        StoryFacts {
            state: if superstate == SuperState::Closed {
                "done".into()
            } else {
                "in-progress".into()
            },
            superstate,
            generation: Some(VerificationGeneration {
                lease: None,
                landed: true,
                overridden: false,
                reap_marker: marker,
            }),
        }
    }

    #[test]
    fn only_a_closed_story_with_a_verifier_marker_on_its_latest_generation_is_released() {
        assert_eq!(
            verifier_release("SH-7", None).unwrap_err().reason,
            "unknown-story"
        );
        let open = verifier_release(
            "SH-7",
            Some(&facts(SuperState::Open, Some(ReapMarker::Required))),
        )
        .unwrap_err();
        assert_eq!(open.reason, "story-open");
        assert!(open.detail.contains("in-progress"), "{}", open.detail);
        assert_eq!(
            verifier_release("SH-7", Some(&facts(SuperState::Closed, None)))
                .unwrap_err()
                .reason,
            "not-verifier-released"
        );
        let never_verified = StoryFacts {
            state: "done".into(),
            superstate: SuperState::Closed,
            generation: None,
        };
        assert_eq!(
            verifier_release("SH-7", Some(&never_verified))
                .unwrap_err()
                .reason,
            "not-verifier-released"
        );
        assert_eq!(
            verifier_release(
                "SH-7",
                Some(&facts(SuperState::Closed, Some(ReapMarker::Complete)))
            ),
            Ok(ReapMarker::Complete)
        );
        assert_eq!(
            verifier_release(
                "SH-7",
                Some(&facts(SuperState::Closed, Some(ReapMarker::Required)))
            ),
            Ok(ReapMarker::Required)
        );
    }

    #[test]
    fn merged_inactive_story_removes_the_worktree_and_local_branch_and_never_the_remote() {
        let repo = Repo::new(true);
        assert!(repo.workspace.origin_has_branch(), "fixture control");
        let removal = clean_candidate(&repo.checkout, &repo.lease, false).unwrap();
        assert!(removal.removed_worktree);
        assert!(removal.removed_local_branch);
        assert!(removal.reclaimed_bytes >= 4096);
        assert!(!repo.worktree.exists());
        assert!(!repo.workspace.local_branch_exists());
        assert!(
            repo.workspace.origin_has_branch(),
            "the remote branch is the verifier's merge step's to delete, never cleanup's"
        );

        let retry = clean_candidate(&repo.checkout, &repo.lease, false).unwrap();
        assert!(!retry.removed_worktree);
        assert!(!retry.removed_local_branch);
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
        assert!(repo.workspace.local_branch_exists());
    }

    #[test]
    fn a_locked_worktree_is_preserved_and_a_divergent_remote_is_left_alone() {
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
        let clone = divergent.root().join("remote-writer");
        run_git(
            divergent.root(),
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

        // Nothing on the remote can be lost by a tool that never writes it:
        // the local worktree and branch are reachable from the default branch
        // and go; the remote-only commit stays exactly where it was pushed.
        let removal = clean_candidate(&divergent.checkout, &divergent.lease, false).unwrap();
        assert!(removal.removed_worktree);
        assert!(!divergent.worktree.exists());
        assert!(divergent.workspace.origin_has_branch());
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
        protected.worktree_path = repo.root().join("already-absent");
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
        protected.worktree_path = repo.root().join("already-absent");
        protected.branch = "dev".into();
        let refusal = clean_candidate(&repo.checkout, &protected, false).unwrap_err();
        assert_eq!(refusal.reason, "protected-branch");
    }

    /// SH-691: an origin whose HEAD is detached advertises no default branch;
    /// that is unverifiable, never a guess of `main`.
    #[test]
    fn an_origin_without_a_default_branch_is_unverifiable_never_guessed() {
        let repo = Repo::new(true);
        let origin = repo.root().join("origin.git");
        let tip = git_text(&origin, &["rev-parse", "refs/heads/dev"]).unwrap();
        run_git(&origin, &["update-ref", "--no-deref", "HEAD", &tip]).unwrap();
        let mut lease = repo.lease.clone();
        lease.worktree_path = repo.root().join("already-absent");
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
        let marker = repo.workspace.worktree_git_dir().join(CLEANUP_LEASE_MARKER);
        let mut misdirected = repo.lease.clone();
        misdirected.worktree_path = repo.root().join("different-worktree");
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
        let socket = repo.root().join("tmux.sock");
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
