//! Forced cleanup retains identity checks; force only waives recoverability.
use crate::error::AppError;
use crate::service::resources::{ResourceReport, git, tmux};
use std::collections::BTreeSet;
use std::path::Path;

fn refuse(message: impl Into<String>) -> AppError {
    AppError::Validation(format!("reset refused: {}", message.into()))
}

pub(super) fn validate(
    report: &ResourceReport,
    caller: &Path,
    env: &crate::env::Environment,
) -> Result<(), AppError> {
    if !matches!(report.status.as_str(), "resolved" | "absent") {
        return Err(refuse(format!(
            "resource identity {}: {}",
            report.status,
            report.diagnostics.join("; ")
        )));
    }
    let Some(repository) = &report.repository else {
        if report.worktree.is_some() || report.pane.is_some() || !report.candidates.is_empty() {
            return Err(refuse("resources exist without a repository"));
        }
        return Ok(());
    };
    let common = git::text(
        repository,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let mut guard = std::process::Command::new("python3");
    crate::env::spawn_env::apply_dispatch_allowlist(&mut guard);
    guard
        .envs(env.child_vars())
        .args([
            "-c",
            include_str!("../../../plugins/story/lib/artifact-resources.py"),
        ])
        .arg(repository)
        .arg(common.trim())
        .arg(report.worktree.as_deref().unwrap_or(Path::new("")));
    let captured = crate::process::run_captured(guard, crate::service::engine::TMUX_TIMEOUT)
        .map_err(|error| refuse(format!("installed artifact guard: {}", error.detail())))?;
    if !captured.status.success() {
        return Err(refuse(format!(
            "installed artifact guard: {}",
            String::from_utf8_lossy(&captured.stderr)
        )));
    }
    let records = git::inventory(repository)?;
    let actual_repository = git::canonical(&records[0].path)?;
    if &actual_repository != repository {
        return Err(refuse("repository identity changed"));
    }
    if let Some(branch) = &report.branch
        && git::branch_exists(repository, branch)?
    {
        if matches!(branch.as_str(), "main" | "master")
            || records[0].branch.as_ref() == Some(branch)
        {
            return Err(refuse(format!("protected branch {branch}")));
        }
        if git::text(repository, &["remote"])?
            .lines()
            .any(|remote| remote == "origin")
        {
            let default =
                super::super::cleanup::origin_default_branch(repository).map_err(refuse)?;
            if branch == &default {
                return Err(refuse(format!("protected branch {branch}")));
            }
        }
    }
    if let Some(worktree) = &report.worktree {
        if git::canonical(worktree)? != *worktree || worktree == repository {
            return Err(refuse("worktree path changed or targets the main checkout"));
        }
        let caller = git::canonical(caller)?;
        if caller.starts_with(worktree) {
            return Err(refuse("cannot remove the calling worktree"));
        }
        let executable = std::env::current_exe()
            .map_err(|e| refuse(format!("cannot identify running executable: {e}")))?;
        if git::canonical(&executable)?.starts_with(worktree) {
            return Err(refuse("cannot remove the running daemon executable"));
        }
        let registration = records.iter().find(|row| &row.path == worktree);
        if let Some(row) = registration {
            if row.branch != report.branch {
                return Err(refuse("worktree branch identity changed"));
            }
            let expected = report
                .candidates
                .iter()
                .find(|candidate| candidate.worktree.as_ref() == Some(worktree))
                .and_then(|candidate| candidate.lease.as_ref());
            if let Some(lease) = expected {
                crate::service::resources::validate_lease(lease)?;
                let marker = super::super::cleanup_lease::marker_at_registered(worktree)?;
                if marker.as_ref() != Some(lease) {
                    return Err(refuse("original cleanup lease marker changed"));
                }
            }
        } else if worktree.try_exists().map_err(|e| refuse(e.to_string()))? {
            return Err(refuse(
                "worktree path exists without its original registration",
            ));
        }
        if records
            .iter()
            .any(|row| row.path != *worktree && row.branch == report.branch)
        {
            return Err(refuse("branch is now checked out elsewhere"));
        }
    }
    Ok(())
}

pub(super) fn remove(
    report: &ResourceReport,
    paths: &[crate::store::ResetPathIdentity],
    caller: &Path,
    env: &crate::env::Environment,
) -> Result<(), AppError> {
    super::identity::validate(paths)?;
    validate(report, caller, env)?;
    if let Some(socket) = &report.socket_path {
        let names = BTreeSet::from([report.window_name.clone()]);
        let panes = tmux::panes(socket, &names)?;
        if !panes.is_empty() {
            let expected = report
                .pane
                .as_ref()
                .ok_or_else(|| refuse("a new tmux window appeared after reset was reserved"))?;
            if panes.len() != 1
                || panes[0].window_id != expected.window_id
                || panes[0].pane_id != expected.pane_id
                || panes[0].pid != expected.pid
            {
                return Err(refuse("tmux window identity changed"));
            }
            let mut command = std::process::Command::new("tmux");
            crate::env::spawn_env::apply_dispatch_allowlist(&mut command);
            command
                .arg("-S")
                .arg(socket)
                .args(["kill-window", "-t", &expected.window_id]);
            let output =
                crate::process::run_captured(command, crate::service::engine::TMUX_TIMEOUT)
                    .map_err(|e| refuse(format!("closing tmux window: {}", e.detail())))?;
            if !output.status.success() {
                return Err(refuse(format!(
                    "tmux kill-window {}: {}",
                    expected.window_id,
                    String::from_utf8_lossy(&output.stderr)
                )));
            }
        }
        if !tmux::panes(socket, &names)?.is_empty() {
            return Err(refuse("tmux story window remains"));
        }
    } else if report.pane.is_some() {
        return Err(refuse("tmux window has no recorded socket"));
    }
    super::identity::validate(paths)?;
    validate(report, caller, env)?;
    let Some(repository) = &report.repository else {
        return Ok(());
    };
    if let Some(worktree) = &report.worktree {
        if git::inventory(repository)?
            .iter()
            .any(|record| &record.path == worktree)
        {
            let path = worktree
                .to_str()
                .ok_or_else(|| refuse("worktree path is not UTF-8"))?;
            git::text(
                repository,
                &["worktree", "remove", "--force", "--force", "--", path],
            )?;
        }
        if git::inventory(repository)?
            .iter()
            .any(|record| &record.path == worktree)
            || std::fs::symlink_metadata(worktree).is_ok()
        {
            return Err(refuse("worktree registration or path remains"));
        }
    }
    super::identity::validate(paths)?;
    if let Some(branch) = &report.branch {
        if git::branch_exists(repository, branch)? {
            git::text(repository, &["branch", "-D", "--", branch])?;
        }
        if git::branch_exists(repository, branch)? {
            return Err(refuse("local branch remains"));
        }
    }
    if let Some(socket) = &report.socket_path
        && !tmux::panes(socket, &BTreeSet::from([report.window_name.clone()]))?.is_empty()
    {
        return Err(refuse("tmux story window reappeared"));
    }
    Ok(())
}
