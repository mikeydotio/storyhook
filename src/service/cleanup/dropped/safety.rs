//! Checks are local: retaining the branch removes any need for merge evidence.
use super::issue;
use crate::domain::StoryCleanupLease;
use crate::error::AppError;
use crate::service::{
    CleanupSkip, Ctx,
    resources::{ResourcePane, ResourceReport, git, tmux},
};
use crate::store::Store;
use std::collections::BTreeSet;
use std::path::Path;

/// Reads every pane on the leased server that claims the exact story name.
pub(super) fn panes(lease: &StoryCleanupLease) -> Result<Vec<ResourcePane>, AppError> {
    tmux::all_panes(
        &lease.tmux.socket_path,
        &BTreeSet::from([lease.story_id.clone()]),
    )
}

/// Refuses any change to the pane snapshot captured before cleanup.
pub(super) fn same_pane(
    lease: &StoryCleanupLease,
    report: &ResourceReport,
) -> Result<(), AppError> {
    let current = panes(lease)?;
    match (&report.pane, current.as_slice()) {
        (None, []) => Ok(()),
        (Some(expected), [actual]) if actual == expected => Ok(()),
        _ => Err(AppError::Validation(
            "dropped cleanup pane identity changed; preserved resources".into(),
        )),
    }
}

/// Treats either a registered worktree or a filesystem object as remaining work.
pub(super) fn worktree_present(lease: &StoryCleanupLease) -> Result<bool, AppError> {
    let registered = git::inventory(&lease.repository_path)?
        .iter()
        .any(|r| r.path == lease.worktree_path);
    let present = match std::fs::symlink_metadata(&lease.worktree_path) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(e.into()),
    };
    Ok(registered || present)
}

/// Checks resource identity and protection, optionally requiring clean work.
pub(super) fn validate<S: Store>(
    ctx: &Ctx<'_, S>,
    repository: &Path,
    lease: &StoryCleanupLease,
    report: &ResourceReport,
    require_clean: bool,
) -> Result<(), CleanupSkip> {
    let refuse = |reason, detail: String| issue(lease, reason, detail);
    let probe = |e: AppError| refuse("resource-unverifiable", e.to_string());
    if !matches!(report.status.as_str(), "resolved" | "absent") {
        return Err(refuse(
            "resource-identity-unsafe",
            format!("{}: {}", report.status, report.diagnostics.join("; ")),
        ));
    }
    if lease.repository_path != repository || report.repository.as_deref() != Some(repository) {
        return Err(refuse(
            "repository-mismatch",
            "lease does not name the registered checkout".into(),
        ));
    }
    crate::service::resources::validate_lease(lease).map_err(probe)?;
    let records = git::inventory(repository).map_err(probe)?;
    if records.first().map(|r| r.path.as_path()) != Some(repository) {
        return Err(refuse(
            "repository-mismatch",
            "registered repository identity changed".into(),
        ));
    }
    if matches!(lease.branch.as_str(), "main" | "master")
        || records.first().and_then(|r| r.branch.as_ref()) == Some(&lease.branch)
    {
        return Err(refuse("protected-branch", lease.branch.clone()));
    }
    let worktree = &lease.worktree_path;
    if worktree == repository
        || git::canonical(ctx.cwd())
            .map_err(probe)?
            .starts_with(worktree)
        || git::canonical(&std::env::current_exe().map_err(|e| probe(e.into()))?)
            .map_err(probe)?
            .starts_with(worktree)
    {
        return Err(refuse("protected-worktree", worktree.display().to_string()));
    }
    let common = git::text(
        repository,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .map_err(probe)?;
    let mut guard = std::process::Command::new("python3");
    crate::env::spawn_env::apply_dispatch_allowlist(&mut guard);
    guard
        .envs(ctx.env().child_vars())
        .args([
            "-c",
            include_str!("../../../../plugins/story/lib/artifact-resources.py"),
        ])
        .arg(repository)
        .arg(common.trim())
        .arg(worktree);
    let guarded = crate::process::run_captured(guard, crate::service::engine::TMUX_TIMEOUT)
        .map_err(|e| refuse("artifact-unverifiable", e.detail()))?;
    if !guarded.status.success() {
        return Err(refuse(
            "protected-artifact",
            String::from_utf8_lossy(&guarded.stderr).into_owned(),
        ));
    }
    let registration = records.iter().find(|r| r.path == *worktree);
    if let Some(row) = registration {
        if row.locked {
            return Err(refuse("locked-worktree", worktree.display().to_string()));
        }
        if row.branch.as_ref() != Some(&lease.branch) {
            return Err(refuse(
                "worktree-mismatch",
                "worktree branch changed or detached".into(),
            ));
        }
    }
    if worktree.try_exists().map_err(|e| probe(e.into()))? {
        if git::canonical(worktree).map_err(probe)? != *worktree || registration.is_none() {
            return Err(refuse(
                "worktree-mismatch",
                "worktree is not its original canonical registration".into(),
            ));
        }
        if crate::service::cleanup_lease::marker_at_registered(worktree)
            .map_err(probe)?
            .as_ref()
            != Some(lease)
        {
            return Err(refuse(
                "worktree-mismatch",
                "original cleanup lease marker changed".into(),
            ));
        }
        if require_clean
            && !git::text(
                worktree,
                &["status", "--porcelain", "--untracked-files=all"],
            )
            .map_err(probe)?
            .is_empty()
        {
            return Err(refuse("dirty-worktree", worktree.display().to_string()));
        }
        let head = git::text(worktree, &["rev-parse", "HEAD"]).map_err(probe)?;
        let branch = git::text(
            repository,
            &["rev-parse", &format!("refs/heads/{}", lease.branch)],
        )
        .map_err(probe)?;
        if head != branch {
            return Err(refuse(
                "worktree-mismatch",
                "worktree commits are not retained by the leased branch".into(),
            ));
        }
    } else if registration.is_some() {
        return Err(refuse(
            "worktree-unavailable",
            "registered worktree is missing; preserve its metadata for recovery".into(),
        ));
    }
    let current = panes(lease).map_err(probe)?;
    if current.len() > 1 {
        return Err(refuse(
            "resource-identity-unsafe",
            "multiple windows or panes claim the story".into(),
        ));
    }
    if current.first().is_some_and(|p| p.cwd != *worktree) {
        return Err(refuse(
            "resource-identity-unsafe",
            "pane working directory does not match the leased worktree".into(),
        ));
    }
    Ok(())
}
