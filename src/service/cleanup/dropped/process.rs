//! Binary-owned helpers and durable process journals fence workspace removal.
use crate::error::AppError;
use crate::service::{Ctx, resources::ResourceReport, workspace_lock::WorkspaceLock};
use crate::store::{DroppedCleanup, Store};
use std::process::Command;
use std::time::Duration;

/// Pins a live pane incarnation before cleanup can reserve or signal it.
pub(super) fn capture(report: &ResourceReport) -> Result<Option<String>, AppError> {
    let Some(pane) = &report.pane else {
        return Ok(None);
    };
    if pane.dead {
        return Err(AppError::Validation(
            "dead pane has no captured process identity; preserve the window and worktree".into(),
        ));
    }
    let mut command = Command::new("python3");
    crate::env::spawn_env::apply_dispatch_allowlist(&mut command);
    let source = format!(
        "{}\nprint(process_identity(int(sys.argv[1]))['start'])",
        include_str!("../../../../plugins/story/lib/process_identity.py")
    );
    command.args(["-c", &source, &pane.pid]);
    let output = crate::process::run_captured(command, Duration::from_secs(10))
        .map_err(|e| AppError::Validation(e.detail()))?;
    if !output.status.success() {
        return Err(AppError::Validation(format!(
            "cannot capture pane {} process identity: {}",
            pane.pane_id,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let start =
        String::from_utf8(output.stdout).map_err(|e| AppError::Validation(e.to_string()))?;
    if start.trim().is_empty() {
        return Err(AppError::Validation(
            "empty pane process incarnation".into(),
        ));
    }
    Ok(Some(start.trim().into()))
}

/// Reconciles the process journal and proves the pinned window and writers absent.
pub(super) fn stop<S: Store>(
    ctx: &Ctx<'_, S>,
    record: &DroppedCleanup,
    workspace: &WorkspaceLock,
) -> Result<(), AppError> {
    let Some(pane) = &record.resources.pane else {
        return super::safety::same_pane(&record.lease, &record.resources);
    };
    let start = record
        .process_start
        .as_ref()
        .ok_or_else(|| AppError::Validation("cleanup has no captured pane incarnation".into()))?;
    let bundle = tempfile::Builder::new()
        .prefix("story-dropped-cleanup-")
        .tempdir_in("/tmp")?;
    for (name, source) in [
        (
            "dropped-cleanup-pane.py",
            include_str!("../../../../plugins/story/lib/dropped-cleanup-pane.py"),
        ),
        (
            "stop-dispatch-pane.py",
            include_str!("../../../../plugins/story/lib/stop-dispatch-pane.py"),
        ),
        (
            "process_identity.py",
            include_str!("../../../../plugins/story/lib/process_identity.py"),
        ),
        (
            "workspace_ownership.py",
            include_str!("../../../../plugins/story/lib/workspace_ownership.py"),
        ),
    ] {
        std::fs::write(bundle.path().join(name), source)?;
    }
    let directory = ctx.env().daemon_state_dir().join("dropped-cleanup");
    std::fs::create_dir_all(&directory)?;
    let journal = directory.join(format!("{}.json", record.token));
    let target = serde_json::json!({"socket": record.lease.tmux.socket_path, "window": pane.window_id,
        "name": record.lease.story_id, "pane": pane.pane_id, "pid": pane.pid, "start": start});
    let mut command = Command::new("python3");
    crate::env::spawn_env::apply_dispatch_allowlist(&mut command);
    command
        .envs(ctx.env().child_vars())
        .arg(bundle.path().join("dropped-cleanup-pane.py"))
        .arg(target.to_string())
        .arg(journal);
    workspace.dispatch_command(&mut command);
    let output = crate::process::run_captured_quiescent(
        command,
        Duration::from_secs(45),
        crate::process::TerminationPolicy::Kill,
    )
    .map_err(|e| AppError::Validation(format!("dropped pane cleanup uncertain: {}", e.detail())))?;
    if !output.status.success() {
        return Err(AppError::Validation(format!(
            "dropped pane cleanup failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let answer: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    if answer != serde_json::json!({"ok": true, "target": target}) {
        return Err(AppError::Validation(
            "dropped pane cleanup receipt did not prove its exact target".into(),
        ));
    }
    Ok(())
}
