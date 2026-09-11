//! Exact resource discovery, validation, and teardown for native reset.

use super::{ResetCaller, ResetReservation};
use crate::domain::{CLEANUP_LEASE_MARKER, CLEANUP_LEASE_VERSION, StoryCleanupLease, StoryEvent};
use crate::error::AppError;
use crate::service::Ctx;
use crate::service::workspace_lock::{WorkspaceLock, capture, git};
use crate::store::{ReadOps, Store, StoryNo};
use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Default)]
struct Worktree {
    path: PathBuf,
    branch: Option<String>,
    locked: bool,
}

fn worktrees(repository: &Path, lock: &WorkspaceLock) -> Result<Vec<Worktree>, AppError> {
    let listing = git(
        repository,
        &["worktree", "list", "--porcelain", "-z"],
        Some(lock),
    )?;
    let mut entries = Vec::new();
    let mut entry = Worktree::default();
    for field in listing.split('\0') {
        if field.is_empty() {
            if !entry.path.as_os_str().is_empty() {
                entries.push(std::mem::take(&mut entry));
            }
        } else if let Some(path) = field.strip_prefix("worktree ") {
            entry.path = path.into();
        } else if let Some(branch) = field.strip_prefix("branch refs/heads/") {
            entry.branch = Some(branch.into());
        } else if field == "locked" || field.starts_with("locked ") {
            entry.locked = true;
        }
    }
    Ok(entries)
}

pub(super) fn repository(checkout: &Path, lock: &WorkspaceLock) -> Result<PathBuf, AppError> {
    let list = worktrees(checkout, lock)?;
    list.first()
        .ok_or_else(|| AppError::Validation("Git returned no primary worktree".into()))?
        .path
        .canonicalize()
        .map_err(Into::into)
}

fn marker(path: &Path, lock: &WorkspaceLock) -> Result<Option<StoryCleanupLease>, AppError> {
    let directory = git(path, &["rev-parse", "--absolute-git-dir"], Some(lock))?;
    let file = Path::new(directory.trim()).join(CLEANUP_LEASE_MARKER);
    let metadata = match fs::symlink_metadata(&file) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(AppError::Validation(format!(
                "cannot inspect {}: {error}",
                file.display()
            )));
        }
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(AppError::Validation(format!(
            "{} is not a regular private cleanup marker",
            file.display()
        )));
    }
    serde_json::from_slice(&fs::read(&file)?)
        .map(Some)
        .map_err(|error| {
            AppError::Validation(format!(
                "invalid cleanup marker {}: {error}",
                file.display()
            ))
        })
}

pub(super) fn discover(
    ctx: &Ctx<'_, impl Store>,
    project: &str,
    number: StoryNo,
    id: &str,
    repository: &Path,
    lock: &WorkspaceLock,
) -> Result<Option<StoryCleanupLease>, AppError> {
    let mut candidates = ctx.store().read(|tx| {
        let events = tx.events_for(ctx.project(), number)?;
        let mut candidates = events
            .iter()
            .rev()
            .find_map(|event| match event.known() {
                Some(StoryEvent::StoryCleanupLeaseRecorded { lease, .. }) => {
                    Some(vec![lease.as_ref().clone()])
                }
                _ => None,
            })
            .unwrap_or_default();
        for run in tx.live_engine_runs()? {
            if run.project_slug != project {
                continue;
            }
            for lane in tx.engine_lanes(&run.id)? {
                if lane.story_id.as_deref() == Some(id) {
                    if lane.state == crate::store::EngineLaneState::Dispatching {
                        return Err(AppError::Validation(format!(
                            "{id} has an active dispatch preparation"
                        ))
                        .into());
                    }
                    if let Some(lease) = lane.cleanup_lease {
                        candidates.push(lease);
                    }
                }
            }
        }
        Ok(candidates)
    })?;
    for entry in worktrees(repository, lock)? {
        if entry.path == repository || !entry.path.exists() {
            continue;
        }
        match marker(&entry.path, lock)? {
            Some(lease) if lease.story_id == id => candidates.push(lease),
            None if entry.path.file_name().is_some_and(|name| name == id) => {
                return Err(AppError::Validation(format!(
                    "{} has no ownership marker; refusing to guess",
                    entry.path.display()
                )));
            }
            _ => {}
        }
    }
    candidates.dedup();
    let Some(first) = candidates.first().cloned() else {
        return Ok(None);
    };
    if candidates.iter().any(|candidate| candidate != &first) {
        return Err(AppError::Validation(format!(
            "{id} has conflicting cleanup leases; no resources were removed"
        )));
    }
    Ok(Some(first))
}

fn validate(
    lease: &StoryCleanupLease,
    id: &str,
    project: &str,
    repository: &Path,
    cwd: &Path,
    lock: &WorkspaceLock,
) -> Result<Option<Worktree>, AppError> {
    if lease.version != CLEANUP_LEASE_VERSION
        || lease.story_id != id
        || lease.project_slug != project
        || lease.repository_path != repository
    {
        return Err(AppError::Validation(
            "cleanup lease identity does not match this project and story".into(),
        ));
    }
    if !lease.worktree_path.is_absolute()
        || lease.worktree_path == repository
        || repository.starts_with(&lease.worktree_path)
    {
        return Err(AppError::Validation(
            "refusing to remove the primary checkout or its ancestor".into(),
        ));
    }
    if cwd.canonicalize()?.starts_with(&lease.worktree_path) {
        return Err(AppError::Validation(
            "refusing to reset the caller's worktree; run reset from another directory".into(),
        ));
    }
    if lease.worktree_path.exists() {
        if lease.worktree_path.canonicalize()? != lease.worktree_path {
            return Err(AppError::Validation(
                "worktree path is not canonical".into(),
            ));
        }
        if marker(&lease.worktree_path, lock)?.as_ref() != Some(lease) {
            return Err(AppError::Validation(
                "worktree cleanup marker does not match reset authority".into(),
            ));
        }
    } else if fs::symlink_metadata(&lease.worktree_path).is_ok() {
        return Err(AppError::Validation(
            "worktree path is a dangling link".into(),
        ));
    }
    let entry = worktrees(repository, lock)?
        .into_iter()
        .find(|entry| entry.path == lease.worktree_path);
    if let Some(entry) = &entry {
        if entry.branch.as_deref() != Some(&lease.branch) {
            return Err(AppError::Validation(
                "worktree branch changed; no resources were removed".into(),
            ));
        }
    } else if lease.worktree_path.exists() {
        return Err(AppError::Validation(
            "worktree directory is not registered with this repository".into(),
        ));
    }
    Ok(entry)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pane {
    window: String,
    pane: String,
    pid: String,
    path: PathBuf,
}

fn tmux(
    lease: &StoryCleanupLease,
    args: &[&str],
    lock: &WorkspaceLock,
) -> Result<String, AppError> {
    let mut command = Command::new("tmux");
    command
        .arg("-S")
        .arg(&lease.tmux.socket_path)
        .args(args)
        .env("LC_ALL", "C");
    let output = capture(command, Some(lock))?;
    if !output.status.success() {
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        // Closing the last window leaves a stale socket on some tmux versions.
        // Only this exact absence result is success; other probe errors stay loud.
        if args.first() == Some(&"list-panes")
            && (diagnostic.trim()
                == format!("no server running on {}", lease.tmux.socket_path.display())
                || !lease.tmux.socket_path.try_exists()?)
        {
            return Ok(String::new());
        }
        return Err(AppError::Validation(format!(
            "tmux {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| AppError::Validation(format!("invalid tmux output: {error}")))
}

fn panes(
    lease: &StoryCleanupLease,
    caller: &ResetCaller,
    lock: &WorkspaceLock,
) -> Result<Vec<Pane>, AppError> {
    if !lease.tmux.socket_path.is_absolute() {
        return Err(AppError::Validation(
            "tmux lease lacks an absolute socket".into(),
        ));
    }
    if !lease.tmux.socket_path.try_exists()? {
        return Ok(Vec::new());
    }
    if !fs::symlink_metadata(&lease.tmux.socket_path)?
        .file_type()
        .is_socket()
    {
        return Err(AppError::Validation(
            "tmux lease path is not a socket".into(),
        ));
    }
    let output = tmux(
        lease,
        &[
            "list-panes",
            "-a",
            "-F",
            "#{window_id}\t#{window_name}\t#{pane_id}\t#{pane_pid}\t#{?pane_dead,#{pane_start_path},#{pane_current_path}}",
        ],
        lock,
    )?;
    let mut answer = Vec::new();
    for line in output.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 5 {
            return Err(AppError::Validation(
                "unrecognized tmux pane inventory".into(),
            ));
        }
        if fields[1] != lease.story_id {
            continue;
        }
        if !fields[0].starts_with('@')
            || !fields[2].starts_with('%')
            || fields[3].parse::<u32>().is_err()
        {
            return Err(AppError::Validation(
                "tmux returned invalid pane identity".into(),
            ));
        }
        let path = PathBuf::from(fields[4]);
        if !path.starts_with(&lease.worktree_path) {
            return Err(AppError::Validation(format!(
                "window {} has an unrelated pane {}; refusing to close it",
                fields[0], fields[2]
            )));
        }
        if caller.pane.as_deref() == Some(fields[2])
            && caller.socket.as_ref().map(fs::canonicalize).transpose()?
                == Some(lease.tmux.socket_path.canonicalize()?)
        {
            return Err(AppError::Validation(
                "refusing to close the caller's own tmux window".into(),
            ));
        }
        answer.push(Pane {
            window: fields[0].into(),
            pane: fields[2].into(),
            pid: fields[3].into(),
            path,
        });
    }
    Ok(answer)
}

pub(super) fn preflight(
    reservation: &ResetReservation,
    id: &str,
    project: &str,
    repository: &Path,
    cwd: &Path,
    caller: &ResetCaller,
    lock: &WorkspaceLock,
) -> Result<(), AppError> {
    let Some(lease) = &reservation.lease else {
        return Ok(());
    };
    let entry = validate(lease, id, project, repository, cwd, lock)?;
    if !reservation.force {
        if entry.as_ref().is_some_and(|entry| entry.locked) {
            return Err(AppError::Validation(
                "worktree is locked; use --force to remove it".into(),
            ));
        }
        if lease.worktree_path.exists()
            && !git(
                &lease.worktree_path,
                &["status", "--porcelain", "-z", "--untracked-files=all"],
                Some(lock),
            )?
            .is_empty()
        {
            return Err(AppError::Validation(
                "worktree has uncommitted changes; use --force to discard them".into(),
            ));
        }
    }
    panes(lease, caller, lock)?;
    Ok(())
}

pub(super) fn remove(
    reservation: &ResetReservation,
    id: &str,
    project: &str,
    repository: &Path,
    cwd: &Path,
    caller: &ResetCaller,
    lock: &WorkspaceLock,
) -> Result<(), AppError> {
    let Some(lease) = &reservation.lease else {
        return Ok(());
    };
    preflight(reservation, id, project, repository, cwd, caller, lock)?;
    let targets = panes(lease, caller, lock)?;
    let windows: std::collections::BTreeSet<_> =
        targets.iter().map(|pane| pane.window.clone()).collect();
    for window in windows {
        let current = panes(lease, caller, lock)?;
        if current
            .iter()
            .filter(|pane| pane.window == window)
            .ne(targets.iter().filter(|pane| pane.window == window))
        {
            return Err(AppError::Validation(
                "tmux window identity changed during reset".into(),
            ));
        }
        tmux(lease, &["kill-window", "-t", &window], lock)?;
    }
    if !panes(lease, caller, lock)?.is_empty() {
        return Err(AppError::Validation(
            "owned tmux windows survived reset".into(),
        ));
    }
    if let Some(entry) = validate(lease, id, project, repository, cwd, lock)? {
        let mut args = vec!["worktree", "remove"];
        if reservation.force {
            args.push("--force");
            if entry.locked {
                args.push("--force");
            }
        }
        let path = lease
            .worktree_path
            .to_str()
            .ok_or_else(|| AppError::Validation("non-UTF-8 worktree path".into()))?;
        args.push(path);
        git(repository, &args, Some(lock))?;
    }
    if lease.worktree_path.try_exists()?
        || worktrees(repository, lock)?
            .iter()
            .any(|entry| entry.path == lease.worktree_path)
    {
        return Err(AppError::Validation(
            "worktree removal postcondition failed".into(),
        ));
    }
    if !panes(lease, caller, lock)?.is_empty() {
        return Err(AppError::Validation(
            "tmux windows reappeared during reset".into(),
        ));
    }
    Ok(())
}
