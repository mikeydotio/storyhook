//! Teardown removes what reset proves the story owns; everything else is residue.
//!
//! Each identity check that once refused a reset now withholds authority over
//! the one resource it protects. Teardown removes what it may, records what it
//! left and why, and never stops the reset from finishing (SH-886). Removal
//! still never reaches a resource the story cannot be shown to own.
use crate::error::AppError;
use crate::service::resources::{ResourceReport, git, tmux};
use crate::service::workspace_lock::{self, WorkspaceLock};
use crate::store::{ResetPathIdentity, ResetRecovery, ResetResidue};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(test)]
#[path = "cleanup_revivify_tests.rs"]
mod revivify_tests;

/// Attempts for one removal whose failure may be transient.
const STEP_ATTEMPTS: u32 = 3;

/// Pause between attempts of one removal.
const STEP_PAUSE: Duration = Duration::from_secs(1);

/// The removals teardown may perform, each proven separately.
#[derive(Debug, Default)]
pub(super) struct Authority {
    /// The repository whose worktree and branch are in scope.
    repository: Option<PathBuf>,
    /// Remove the registered worktree, including a registration whose
    /// directory is already gone.
    worktree: bool,
    /// Delete the directory of a worktree that Git no longer registers.
    orphan_directory: bool,
    /// Delete the local branch.
    branch: bool,
}

/// Accumulates residue, one entry per resource.
#[derive(Debug, Default)]
pub(super) struct Residue(Vec<ResetResidue>);

impl Residue {
    /// Records a resource the reset left, once; later reasons are dropped.
    pub(super) fn leave(&mut self, resource: impl Into<String>, reason: impl Into<String>) {
        let resource = resource.into();
        if !self.0.iter().any(|entry| entry.resource == resource) {
            self.0.push(ResetResidue {
                resource,
                reason: reason.into(),
                blocks_dispatch: false,
            });
        }
    }

    /// Marks (or adds) a resource the next dispatch would collide with.
    fn blocks_dispatch(&mut self, resource: impl Into<String>, reason: impl Into<String>) {
        let resource = resource.into();
        if let Some(entry) = self.0.iter_mut().find(|entry| entry.resource == resource) {
            entry.blocks_dispatch = true;
        } else {
            self.0.push(ResetResidue {
                resource,
                reason: reason.into(),
                blocks_dispatch: true,
            });
        }
    }

    /// The entries, in the order teardown found them.
    pub(super) fn into_entries(self) -> Vec<ResetResidue> {
        self.0
    }
}

fn window_resource(report: &ResourceReport) -> String {
    format!("tmux window {}", report.window_name)
}

fn worktree_resource(worktree: &Path) -> String {
    format!("worktree {}", worktree.display())
}

fn branch_resource(branch: &str) -> String {
    format!("local branch {branch}")
}

/// Repeats one removal a few times, so a transient failure does not leave
/// residue; the last error becomes the reason when every attempt fails.
fn attempt<T>(mut step: impl FnMut() -> Result<T, AppError>) -> Result<T, AppError> {
    let mut outcome = step();
    for _ in 1..STEP_ATTEMPTS {
        if outcome.is_ok() {
            break;
        }
        std::thread::sleep(STEP_PAUSE);
        outcome = step();
    }
    outcome
}

/// Decides which Git resources teardown may remove. Every failed proof
/// withholds authority over the resource it protects and records why.
pub(super) fn authorize(
    report: &ResourceReport,
    paths: &[ResetPathIdentity],
    caller: &Path,
    env: &crate::env::Environment,
    residue: &mut Residue,
) -> Authority {
    let mut authority = Authority::default();
    if !matches!(report.status.as_str(), "resolved" | "absent") {
        residue.leave(
            "worktree and branch",
            format!(
                "resource identity is {}: {}",
                report.status,
                report.diagnostics.join("; ")
            ),
        );
        return authority;
    }
    let Some(repository) = &report.repository else {
        if report.worktree.is_some() || !report.candidates.is_empty() {
            residue.leave(
                "worktree and branch",
                "resources were found without a repository",
            );
        }
        return authority;
    };
    // Observation stays in scope even where removal is withheld below.
    authority.repository = Some(repository.clone());
    let mut worktree = report.worktree.is_some();
    let mut branch = report.branch.is_some();
    for (path, reason) in super::identity::replaced(paths) {
        if path.removable {
            worktree = false;
            branch = false;
        } else {
            residue.leave("worktree and branch", reason.clone());
            return authority;
        }
        if let Some(target) = &report.worktree {
            residue.leave(worktree_resource(target), reason);
        }
    }
    let common = match git::text(
        repository,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ) {
        Ok(common) => common,
        Err(error) => {
            residue.leave("worktree and branch", error.to_string());
            return authority;
        }
    };
    if let Err(reason) = installed_artifact_guard(repository, common.trim(), report, env) {
        if let Some(target) = &report.worktree {
            residue.leave(worktree_resource(target), reason.clone());
        }
        if let Some(name) = &report.branch {
            residue.leave(branch_resource(name), reason);
        }
        return authority;
    }
    let records = match git::inventory(repository) {
        Ok(records) => records,
        Err(error) => {
            residue.leave("worktree and branch", error.to_string());
            return authority;
        }
    };
    match git::canonical(&records[0].path) {
        Ok(actual) if &actual == repository => {}
        _ => {
            residue.leave(
                "worktree and branch",
                "the repository's identity changed after reset began",
            );
            return authority;
        }
    }
    if let Some(name) = &report.branch
        && let Some(reason) = protected_branch(repository, &records, name)
    {
        residue.leave(branch_resource(name), reason);
        branch = false;
    }
    if let Some(target) = &report.worktree {
        match worktree_authority(report, target, repository, &records, caller) {
            WorktreeAuthority::Remove => {}
            WorktreeAuthority::OrphanDirectory => {
                worktree = false;
                authority.orphan_directory = paths
                    .iter()
                    .any(|pinned| &pinned.path == target && pinned.removable);
                if !authority.orphan_directory {
                    residue.leave(
                        worktree_resource(target),
                        "the directory exists but Git no longer registers it, and its \
                         identity was not pinned when reset began",
                    );
                }
            }
            WorktreeAuthority::Withhold { reason, branch_too } => {
                residue.leave(worktree_resource(target), reason.clone());
                worktree = false;
                if branch_too {
                    branch = false;
                    if let Some(name) = &report.branch {
                        residue.leave(branch_resource(name), reason);
                    }
                }
            }
        }
        if let Some(name) = &report.branch
            && records
                .iter()
                .any(|row| row.path != *target && row.branch.as_ref() == Some(name))
        {
            residue.leave(
                branch_resource(name),
                "another worktree has this branch checked out",
            );
            branch = false;
        }
    }
    authority.worktree = worktree;
    authority.branch = branch;
    authority
}

enum WorktreeAuthority {
    Remove,
    OrphanDirectory,
    Withhold { reason: String, branch_too: bool },
}

fn worktree_authority(
    report: &ResourceReport,
    worktree: &Path,
    repository: &Path,
    records: &[git::WorktreeRecord],
    caller: &Path,
) -> WorktreeAuthority {
    let withhold = |reason: &str, branch_too: bool| WorktreeAuthority::Withhold {
        reason: reason.into(),
        branch_too,
    };
    match git::canonical(worktree) {
        Ok(path) if path == worktree && worktree != repository => {}
        _ => return withhold("the path changed or is the main checkout", true),
    }
    match git::canonical(caller) {
        Ok(caller) if caller.starts_with(worktree) => {
            return withhold("the reset caller's working directory is inside it", true);
        }
        Ok(_) => {}
        Err(_) => return withhold("the reset caller's working directory is unreadable", true),
    }
    match std::env::current_exe()
        .map_err(AppError::from)
        .and_then(|exe| git::canonical(&exe))
    {
        Ok(executable) if executable.starts_with(worktree) => {
            return withhold("the running storyhook executable is inside it", true);
        }
        Ok(_) => {}
        Err(_) => return withhold("the running storyhook executable cannot be located", true),
    }
    let Some(row) = records.iter().find(|row| row.path == worktree) else {
        return match worktree.try_exists() {
            Ok(false) => WorktreeAuthority::Remove,
            Ok(true) => WorktreeAuthority::OrphanDirectory,
            Err(error) => WorktreeAuthority::Withhold {
                reason: format!("cannot observe the path: {error}"),
                branch_too: false,
            },
        };
    };
    if row.branch != report.branch {
        return withhold("its checked-out branch changed after reset began", true);
    }
    // A registration whose directory is gone has no marker left to compare;
    // its private Git directory identity is pinned instead.
    if !matches!(worktree.try_exists(), Ok(false)) {
        let expected = report
            .candidates
            .iter()
            .find(|candidate| candidate.worktree.as_deref() == Some(worktree))
            .and_then(|candidate| candidate.lease.as_ref());
        if let Some(lease) = expected {
            let unchanged = crate::service::resources::validate_lease(lease).is_ok()
                && super::super::cleanup_lease::marker_at_registered(worktree)
                    .is_ok_and(|marker| marker.as_ref() == Some(lease));
            if !unchanged {
                return withhold("its cleanup lease marker changed after reset began", true);
            }
        }
    }
    WorktreeAuthority::Remove
}

/// The reason a branch must survive, when it is protected.
fn protected_branch(
    repository: &Path,
    records: &[git::WorktreeRecord],
    branch: &str,
) -> Option<String> {
    if !git::branch_exists(repository, branch).unwrap_or(true) {
        return None;
    }
    if matches!(branch, "main" | "master") || records[0].branch.as_deref() == Some(branch) {
        return Some(format!("{branch} is a protected branch"));
    }
    // The cached origin default only: reset never waits on the network.
    match local_origin_default(repository) {
        Some(default) if default == branch => Some(format!("{branch} is origin's default branch")),
        _ => None,
    }
}

/// Origin's default branch as last fetched, read without a network call.
fn local_origin_default(repository: &Path) -> Option<String> {
    git::text(
        repository,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .ok()
    .and_then(|name| name.trim().strip_prefix("origin/").map(str::to_string))
}

/// Runs the installed-artifact guard: a worktree holding installed StoryHook
/// artifacts is never removed.
fn installed_artifact_guard(
    repository: &Path,
    common: &str,
    report: &ResourceReport,
    env: &crate::env::Environment,
) -> Result<(), String> {
    let mut guard = std::process::Command::new("python3");
    crate::env::spawn_env::apply_dispatch_allowlist(&mut guard);
    guard
        .envs(env.child_vars())
        .args([
            "-c",
            include_str!("../../../plugins/story/lib/artifact-resources.py"),
        ])
        .arg(repository)
        .arg(common)
        .arg(report.worktree.as_deref().unwrap_or(Path::new("")));
    let captured = crate::process::run_captured_quiescent(
        guard,
        env.subprocess_bound(crate::service::engine::TMUX_TIMEOUT),
        crate::process::TerminationPolicy::Kill,
    )
    .map_err(|error| format!("installed artifact guard: {}", error.detail()))?;
    if !captured.status.success() {
        return Err(format!(
            "installed artifact guard: {}",
            String::from_utf8_lossy(&captured.stderr).trim()
        ));
    }
    Ok(())
}

/// Counts what removal will discard, before anything is removed.
pub(super) fn recovery(report: &ResourceReport, authority: &Authority) -> ResetRecovery {
    let mut record = ResetRecovery::default();
    if let (true, Some(worktree)) = (authority.worktree, &report.worktree)
        && matches!(worktree.try_exists(), Ok(true))
        && let Ok(status) = git::text(
            worktree,
            &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
        )
    {
        let (dirty, untracked) = count_changes(&status);
        record.dirty = Some(dirty);
        record.untracked = Some(untracked);
    }
    if let (true, Some(repository), Some(branch)) =
        (authority.branch, &authority.repository, &report.branch)
        && let Ok(tip) = git::text(
            repository,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}^{{commit}}"),
            ],
        )
    {
        let tip = tip.trim().to_string();
        record.unpushed = git::text(
            repository,
            &[
                "rev-list",
                "--count",
                &tip,
                "--not",
                // `--exclude` before `--branches` takes the name without refs/heads/.
                &format!("--exclude={branch}"),
                "--branches",
                "--remotes",
                "--tags",
            ],
        )
        .ok()
        .and_then(|count| count.trim().parse().ok());
        record.branch = Some(branch.clone());
        record.tip = Some(tip);
    }
    record
}

/// Tracked changes and untracked paths in `git status --porcelain=v2 -z`.
fn count_changes(status: &str) -> (u64, u64) {
    let (mut dirty, mut untracked) = (0, 0);
    let mut fields = status.split('\0');
    while let Some(field) = fields.next() {
        match field.as_bytes().first() {
            Some(b'1' | b'u') => dirty += 1,
            Some(b'2') => {
                dirty += 1;
                // A rename or copy is followed by its original path.
                fields.next();
            }
            Some(b'?') => untracked += 1,
            _ => {}
        }
    }
    (dirty, untracked)
}

/// Removes every resource `authority` covers; failures become residue.
pub(super) fn remove(
    report: &ResourceReport,
    paths: &[ResetPathIdentity],
    authority: &Authority,
    env: &crate::env::Environment,
    workspace: Option<&WorkspaceLock>,
    residue: &mut Residue,
) {
    close_window(report, env, workspace, residue);
    let Some(repository) = &authority.repository else {
        return;
    };
    // Identity is checked again right before each destructive Git step.
    let unchanged = || super::identity::replaced(paths).is_empty();
    if let Some(worktree) = &report.worktree {
        let path = worktree.to_string_lossy().to_string();
        if authority.worktree && unchanged() {
            let removed = attempt(|| {
                let registered = git::inventory(repository)?
                    .iter()
                    .any(|record| &record.path == worktree);
                if registered {
                    workspace_lock::git(
                        repository,
                        &["worktree", "remove", "--force", "--force", "--", &path],
                        workspace,
                    )?;
                }
                Ok(())
            });
            if let Err(error) = removed {
                residue.leave(worktree_resource(worktree), error.to_string());
            }
        }
        if authority.orphan_directory
            && unchanged()
            && let Err(error) = attempt(|| std::fs::remove_dir_all(worktree).map_err(Into::into))
        {
            residue.leave(
                worktree_resource(worktree),
                format!("removing the unregistered directory: {error}"),
            );
        }
    }
    if let (true, Some(branch)) = (authority.branch, &report.branch)
        && unchanged()
    {
        let deleted = attempt(|| {
            if git::branch_exists(repository, branch)? {
                workspace_lock::git(repository, &["branch", "-D", "--", branch], workspace)?;
            }
            Ok(())
        });
        if let Err(error) = deleted {
            residue.leave(branch_resource(branch), error.to_string());
        }
    }
}

/// Closes the exact pinned window; any doubt about its identity leaves it.
fn close_window(
    report: &ResourceReport,
    env: &crate::env::Environment,
    workspace: Option<&WorkspaceLock>,
    residue: &mut Residue,
) {
    let resource = window_resource(report);
    let Some(socket) = &report.socket_path else {
        if report.pane.is_some() {
            residue.leave(resource, "the tmux window has no recorded socket");
        }
        return;
    };
    let names = BTreeSet::from([report.window_name.clone()]);
    let (target, panes) = match attempt(|| tmux::resolved_panes(env, socket, &names)) {
        Ok(observed) => observed,
        Err(error) => return residue.leave(resource, error.to_string()),
    };
    if panes.is_empty() {
        return;
    }
    if target.protected && target.endpoint != *socket {
        return residue.leave(
            resource,
            "the tmux server generation changed after reset began",
        );
    }
    let Some(expected) = &report.pane else {
        return residue.leave(resource, "a new tmux window appeared after reset began");
    };
    if panes.len() != 1
        || panes[0].window_id != expected.window_id
        || panes[0].pane_id != expected.pane_id
        || panes[0].pid != expected.pid
    {
        return residue.leave(
            resource,
            "the tmux window's identity changed after reset began",
        );
    }
    let closed = attempt(|| {
        let mut command = std::process::Command::new("tmux");
        crate::env::spawn_env::apply_dispatch_allowlist(&mut command);
        target.apply(&mut command, Some(socket));
        command.args(["kill-window", "-t", &expected.window_id]);
        if let Some(workspace) = workspace {
            workspace.command(&mut command);
        }
        let output = crate::process::run_captured_quiescent(
            command,
            env.subprocess_bound(crate::service::engine::TMUX_TIMEOUT),
            crate::process::TerminationPolicy::Kill,
        )
        .map_err(|error| {
            AppError::Validation(format!("closing tmux window: {}", error.detail()))
        })?;
        if !output.status.success()
            && tmux::panes(env, socket, &names)?
                .iter()
                .any(|pane| pane.window_id == expected.window_id)
        {
            return Err(AppError::Validation(format!(
                "tmux kill-window {}: {}",
                expected.window_id,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(())
    });
    if let Err(error) = closed {
        residue.leave(resource, error.to_string());
    }
}

/// Marks what the next dispatch would collide with: the story's window,
/// worktree path and local branch, and a same-named branch on origin that
/// the default branch does not contain (a fresh branch would be rejected when
/// pushed). Observation is local and best effort; it never fails the reset.
pub(super) fn dispatch_overlap(
    report: &ResourceReport,
    authority: &Authority,
    env: &crate::env::Environment,
    residue: &mut Residue,
) {
    if let Some(socket) = &report.socket_path {
        let names = BTreeSet::from([report.window_name.clone()]);
        match tmux::panes(env, socket, &names) {
            Ok(panes) if panes.is_empty() => {}
            Ok(_) => residue.blocks_dispatch(window_resource(report), "the window is still open"),
            Err(error) => residue.leave(
                window_resource(report),
                format!("cannot confirm the window closed: {error}"),
            ),
        }
    }
    let Some(repository) = &authority.repository else {
        if !matches!(report.status.as_str(), "resolved" | "absent")
            || report.worktree.is_some()
            || !report.candidates.is_empty()
        {
            residue.blocks_dispatch(
                "worktree and branch",
                "reset could not identify the story's Git resources",
            );
        }
        return;
    };
    if let Some(worktree) = &report.worktree {
        let registered = git::inventory(repository)
            .map(|records| records.iter().any(|record| &record.path == worktree))
            .unwrap_or(true);
        if registered || std::fs::symlink_metadata(worktree).is_ok() {
            residue.blocks_dispatch(worktree_resource(worktree), "the worktree is still present");
        }
    }
    let Some(branch) = &report.branch else {
        return;
    };
    if git::branch_exists(repository, branch).unwrap_or(true) {
        residue.blocks_dispatch(branch_resource(branch), "the local branch still exists");
    }
    if let Some(reason) = unmerged_origin_branch(repository, branch) {
        residue.blocks_dispatch(format!("remote branch origin/{branch}"), reason);
    }
}

/// Why a surviving `origin/<branch>` would reject the next dispatch's push.
fn unmerged_origin_branch(repository: &Path, branch: &str) -> Option<String> {
    let remote = format!("refs/remotes/origin/{branch}");
    git::text(repository, &["rev-parse", "--verify", "--quiet", &remote]).ok()?;
    let mut bases: Vec<String> = local_origin_default(repository)
        .map(|default| format!("refs/remotes/origin/{default}"))
        .into_iter()
        .collect();
    if let Ok(records) = git::inventory(repository)
        && let Some(primary) = &records[0].branch
    {
        bases.push(format!("refs/remotes/origin/{primary}"));
        bases.push(format!("refs/heads/{primary}"));
    }
    let merged = bases
        .iter()
        .any(|base| git::text(repository, &["merge-base", "--is-ancestor", &remote, base]).is_ok());
    (!merged).then(|| {
        format!(
            "origin/{branch} holds commits the default branch does not contain; a fresh \
             dispatch's push of the same branch name would be rejected. Delete or merge the \
             remote branch, or dispatch with --resume."
        )
    })
}
