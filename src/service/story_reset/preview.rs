//! Read-only reset plan. No reservation, locks, quiescing, or teardown.
use super::{StoryResetService, cleanup, identity};
use crate::domain::SuperState;
use crate::error::AppError;
use crate::service::reset::ResetCaller;
use crate::service::{project_prefix, resolve_open_story};
use crate::store::{ReadOps, ResetOrigin, ResetRecovery, ResetResidue, Store};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Exact terminal identity that the observed reset would close.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResetWindowPreview {
    /// Proven server endpoint, never an unqualified pane identifier.
    pub socket: PathBuf,
    /// Human-readable window name.
    pub name: String,
    /// Server-local window identifier.
    pub window_id: String,
    /// Exact pane used to prove the window identity.
    pub pane_id: String,
}

/// Observed removals, lost local work, and retained resources, not authority
/// for a later execution. Reset rechecks all identities when actually run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResetPreview {
    /// Canonical story identifier.
    pub story_id: String,
    /// State that would be replaced with Todo.
    pub original_state: String,
    /// Existing awaiting reason that reset would clear.
    pub awaiting: Option<String>,
    /// Existing unfinished reset, if this invocation would join one.
    pub existing_reset: Option<String>,
    /// Window proven removable at observation time.
    pub window: Option<ResetWindowPreview>,
    /// Worktree proven removable at observation time.
    pub worktree: Option<PathBuf>,
    /// Stale Git registration to remove when its worktree directory is absent.
    pub worktree_registration: Option<PathBuf>,
    /// Local branch proven removable at observation time.
    pub branch: Option<String>,
    /// Same discard counts and branch recovery facts used by execution.
    pub recovery: ResetRecovery,
    /// Predicted survivors, including resources that would hold dispatch.
    pub residue: Vec<ResetResidue>,
}

impl<S: Store> StoryResetService<'_, S> {
    /// Observes exactly the cleanup authority without reserving the story,
    /// replacing an old receipt, signalling a process, or taking a lock.
    pub fn preview(&self, id: &str, caller: &ResetCaller) -> Result<ResetPreview, AppError> {
        let (row, existing, origin) = self.ctx.store().read(|tx| {
            let project = self.ctx.project();
            let prefix = project_prefix(tx, project)?;
            let (story, row) = resolve_open_story(tx, project, &prefix, id)?;
            if row.snapshot.story_type.as_deref() == Some("epic") {
                return Err(AppError::Validation(
                    "Reset an ordinary child story, not an epic".into(),
                )
                .into());
            }
            let existing = tx
                .story_reset(project, story)?
                .filter(|reset| !reset.completed);
            if existing.is_none()
                && !tx
                    .state_map(project)?
                    .get("todo")
                    .is_some_and(|state| state.super_state == SuperState::Open)
            {
                return Err(
                    AppError::Validation("Reset requires the OPEN todo state".into()).into(),
                );
            }
            let origin = existing
                .as_ref()
                .map(|reset| reset.origin.clone())
                .unwrap_or(ResetOrigin {
                    automation_generation: tx.settings(project)?.automations_after,
                    caller: caller.clone(),
                    cwd: Some(self.ctx.cwd().to_path_buf()),
                    fire_hooks: self.ctx.hooks_enabled(),
                    hook_depth: self.ctx.depth(),
                    legacy_force: None,
                });
            Ok((row, existing, origin))
        })?;
        let mut report = existing
            .as_ref()
            .and_then(|reset| reset.resources.clone())
            .unwrap_or_else(|| self.identify(&row.snapshot.id));
        let paths = if let Some(reset) = &existing
            && reset.resources.is_some()
        {
            reset.paths.clone()
        } else {
            match identity::capture(&report) {
                Ok(paths) => paths,
                Err(error) => {
                    report.status = "unavailable".into();
                    report
                        .diagnostics
                        .push(format!("pinning filesystem identity: {error}"));
                    Vec::new()
                }
            }
        };
        let mut residue = cleanup::Residue::default();
        let authority = cleanup::authorize(
            &report,
            &paths,
            &origin,
            // Pending dashboard/legacy receipts execute from daemon home when
            // no cwd was recorded, never from this later preview's caller.
            origin.cwd.as_deref().unwrap_or(self.ctx.env().home()),
            self.ctx.env(),
            &mut residue,
        );
        let window = match cleanup::window_authority(&report, &origin.caller, self.ctx.env()) {
            Ok(Some((target, pane))) => Some(ResetWindowPreview {
                socket: target.endpoint,
                name: pane.window_name.clone(),
                window_id: pane.window_id.clone(),
                pane_id: pane.pane_id.clone(),
            }),
            Ok(None) => None,
            Err(reason) => {
                residue.leave(format!("tmux window {}", report.window_name), reason);
                None
            }
        };
        let recovery = cleanup::recovery(&report, &authority);
        cleanup::preview_overlap(
            &report,
            &authority,
            self.ctx.env(),
            &mut residue,
            window.is_some(),
        );
        // Authority permits an attempt; it does not prove the named resource
        // exists. In particular, pinned reports survive partial cleanup.
        let mut worktree = None;
        let mut worktree_registration = None;
        if (authority.worktree || authority.orphan_directory)
            && let Some(path) = &report.worktree
        {
            match std::fs::symlink_metadata(path) {
                Ok(_) => worktree = Some(path.clone()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    if let Some(repository) = &authority.repository {
                        match crate::service::resources::git::inventory(repository) {
                            Ok(records) if records.iter().any(|row| row.path == *path) => {
                                worktree_registration = Some(path.clone());
                            }
                            Ok(_) => {}
                            Err(error) => residue.blocks_dispatch(
                                format!("worktree registration {}", path.display()),
                                format!("cannot observe planned registration cleanup: {error}"),
                            ),
                        }
                    }
                }
                Err(error) => residue.blocks_dispatch(
                    format!("worktree {}", path.display()),
                    format!("cannot observe planned worktree removal: {error}"),
                ),
            }
        }
        let mut branch = None;
        if authority.branch
            && let (Some(repository), Some(name)) = (&authority.repository, &report.branch)
        {
            match crate::service::resources::git::branch_exists(repository, name) {
                Ok(true) => branch = Some(name.clone()),
                Ok(false) => {}
                Err(error) => residue.blocks_dispatch(
                    format!("local branch {name}"),
                    format!("cannot observe planned branch removal: {error}"),
                ),
            }
        }
        Ok(ResetPreview {
            story_id: row.snapshot.id,
            original_state: row.state,
            awaiting: row.awaiting,
            existing_reset: existing.map(|reset| reset.token),
            window,
            worktree,
            worktree_registration,
            branch,
            recovery,
            residue: residue.into_entries(),
        })
    }
}

impl ResetPreview {
    /// Human-facing preview; unknown counts stay unknown, never become zero.
    pub fn display(&self) -> String {
        let count = |value: Option<u64>| value.map_or_else(|| "unknown".into(), |n| n.to_string());
        let mut lines = vec![format!(
            "DRY RUN story reset {}: {} -> todo (no changes)",
            self.story_id, self.original_state
        )];
        if let Some(token) = &self.existing_reset {
            lines.push(format!(
                "Would join existing reset {token}; this preview does not start or resume it."
            ));
        }
        if let Some(window) = &self.window {
            lines.push(format!(
                "Would close window {} ({}, pane {}) on {}",
                window.name,
                window.window_id,
                window.pane_id,
                window.socket.display()
            ));
        }
        if let Some(path) = &self.worktree {
            lines.push(format!(
                "Would discard worktree {}: {} changed paths, {} untracked paths",
                path.display(),
                count(self.recovery.dirty),
                count(self.recovery.untracked)
            ));
        }
        if let Some(path) = &self.worktree_registration {
            lines.push(format!(
                "Would remove stale worktree registration {} (directory is absent)",
                path.display()
            ));
        }
        if let Some(branch) = &self.branch {
            lines.push(format!("Would delete local branch {branch}: tip {}; {} commits on no other branch, tag or remote", self.recovery.tip.as_deref().unwrap_or("unknown"), count(self.recovery.unpushed)));
        }
        lines.push(format!(
            "Awaiting reason to clear: {}",
            self.awaiting.as_deref().unwrap_or("none")
        ));
        for item in &self.residue {
            lines.push(format!(
                "Would retain {}: {}{}",
                item.resource,
                item.reason,
                if item.blocks_dispatch {
                    " (would hold the next dispatch)"
                } else {
                    ""
                }
            ));
        }
        lines.push("Remote branches and pull requests are preserved. Counts exclude ignored files, which worktree removal also discards. This is a current observation; execution rechecks ownership and can leave additional residue if cleanup fails.".into());
        lines.join("\n")
    }
}
