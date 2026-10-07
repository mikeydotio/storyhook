//! Binary-owned helpers and durable process journals fence workspace removal.
use crate::error::AppError;
use crate::service::{Ctx, resources::ResourceReport, workspace_lock::WorkspaceLock};
use crate::store::{DroppedCleanup, Store};
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// How long `dropped-cleanup-pane.py` may run before it is killed, with no
/// SIGTERM first. The helper's per-operation probe budget
/// (`plugins/story/lib/probe_budget.py`) must end well inside it, or the kill
/// could land while the helper holds processes frozen.
const CLEANUP_HELPER_TIMEOUT: Duration = Duration::from_secs(45);

/// How long the python3 process-identity read may take: one `ps` of one pid.
const IDENTITY_TIMEOUT: Duration = Duration::from_secs(10);

/// Pins a live pane incarnation before cleanup can reserve or signal it.
pub(super) fn capture(
    env: &crate::env::Environment,
    report: &ResourceReport,
) -> Result<Option<String>, AppError> {
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
    let output = crate::process::run_captured(command, env.subprocess_bound(IDENTITY_TIMEOUT))
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
        return super::safety::same_pane(ctx.env(), &record.lease, &record.resources);
    };
    let start = record
        .process_start
        .as_ref()
        .ok_or_else(|| AppError::Validation("cleanup has no captured pane incarnation".into()))?;
    let directory = ctx.env().daemon_state_dir().join("dropped-cleanup");
    std::fs::create_dir_all(&directory)?;
    let journal = directory.join(format!("{}.json", record.token));
    // Named by the reservation, not the attempt: a traceback names the
    // helper's files, and a path that changed on every retry made each one
    // read as a new failure that posted another comment (SH-881).
    let bundle = directory.join(format!("{}.bundle", record.token));
    prepare_bundle(&bundle)?;
    let target = serde_json::json!({"socket": record.lease.tmux.socket_path, "window": pane.window_id,
        "name": record.lease.story_id, "pane": pane.pane_id, "pid": pane.pid, "start": start});
    let mut command = Command::new("python3");
    crate::env::spawn_env::apply_dispatch_allowlist(&mut command);
    command
        .envs(ctx.env().child_vars())
        .arg(bundle.join("dropped-cleanup-pane.py"))
        .arg(target.to_string())
        .arg(journal);
    workspace.dispatch_command(&mut command);
    let result = run_helper(command, &target);
    match (result, std::fs::remove_dir_all(&bundle)) {
        (result, Ok(())) => result,
        (Ok(()), Err(error)) => Err(AppError::Storage(format!(
            "dropped pane cleanup could not remove its helper bundle {}: {error}",
            bundle.display()
        ))),
        (Err(failure), Err(error)) => Err(failure.with_context(&format!(
            "the helper bundle {} could not be removed after this failure: {error}",
            bundle.display()
        ))),
    }
}

/// Runs the helper and requires a receipt for exactly `target`.
fn run_helper(command: Command, target: &serde_json::Value) -> Result<(), AppError> {
    let output = crate::process::run_captured_quiescent(
        command,
        CLEANUP_HELPER_TIMEOUT,
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

/// Writes the helper and everything it can import to `bundle`, a private
/// directory: the whole plugin library this binary embeds, never a list of
/// the files it is thought to need (SH-881).
///
/// Whatever an interrupted attempt left there is replaced. Only this
/// reservation's attempt uses the path, under the story's executor and
/// workspace locks, and a helper that outlived its attempt still holds the
/// inherited workspace lock, so nothing can be running from a leftover.
fn prepare_bundle(bundle: &Path) -> Result<(), AppError> {
    match std::fs::remove_dir_all(bundle) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(AppError::Storage(format!(
                "cannot replace the dropped-pane helper bundle {}: {error}",
                bundle.display()
            )));
        }
        _ => {}
    }
    std::fs::DirBuilder::new().mode(0o700).create(bundle)?;
    crate::plugin::library::project(bundle)
}

#[cfg(test)]
mod tests {
    use super::{CLEANUP_HELPER_TIMEOUT, prepare_bundle};
    use std::process::Command;

    /// The helper runs from the bundle exactly as `stop` starts it, so its
    /// whole import graph loads before it reads its arguments. Missing
    /// arguments are its own reported error; a missing module is a traceback
    /// that never reaches that handler (SH-881). `-E` keeps an inherited
    /// `PYTHONPATH` from supplying what the bundle lacks; `-I` cannot be used
    /// because it also drops the script's directory from `sys.path`.
    #[test]
    fn the_helper_bundle_resolves_every_module_the_helper_imports() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = storyhook_test_support::scratch_dir();
        let bundle = scratch.path().join("bundle");
        // What an interrupted attempt left behind must not survive.
        std::fs::create_dir(&bundle).unwrap();
        std::fs::write(bundle.join("json.py"), "raise SystemExit('stale')").unwrap();
        prepare_bundle(&bundle).unwrap();
        let output = Command::new("python3")
            .args(["-E", "-s", "-B"])
            .arg(bundle.join("dropped-cleanup-pane.py"))
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
        assert!(stderr.is_empty(), "the helper did not load: {stderr}");
        let answer: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(answer["ok"], false, "{answer}");
        assert!(
            crate::embedded::matches(&crate::plugin::library::files(), &bundle),
            "the bundle must be exactly the embedded plugin library"
        );
        let mode = std::fs::metadata(&bundle).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
    }

    /// The cleanup helper's probe budget leaves a third of its kill bound for
    /// interpreter start and exit under load, so its own cleanup resumes any
    /// frozen process before the kill, which has no SIGTERM first (SH-766).
    #[test]
    fn helper_probe_budget_ends_inside_the_cleanup_bound() {
        let budget = crate::process::plugin_probe_budget();
        assert!(
            budget * 3 <= CLEANUP_HELPER_TIMEOUT * 2,
            "probe budget {budget:?} is more than two thirds of CLEANUP_HELPER_TIMEOUT \
             {CLEANUP_HELPER_TIMEOUT:?}"
        );
    }
}
