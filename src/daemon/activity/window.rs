//! Supervise persistent project readers independently of verification phases.

pub(crate) use super::window_requests::Requests;
use super::window_requests::Schedule;
use crate::{
    env::Environment,
    process::{CaptureError, Captured, run_captured_quiet_cancellable},
    store::{ProjectId, ReadOps, Store},
};

use std::{
    ffi::OsStr,
    path::Path,
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub(super) const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);
/// One 30 s probe operation plus 15 s for interpreter startup and exit (SH-808).
const VIEW_RECONCILE_TIMEOUT: Duration = Duration::from_secs(45);
/// Bound stop observation while idle, matching cancellable process capture.
const STOP_POLL: Duration = Duration::from_millis(100);

#[cfg(test)]
#[path = "window_tests.rs"]
mod tests;

/// The reconciler as the daemon runs it: the tmux server environment policy,
/// then the view program that calls it (SH-758). Composed rather than
/// imported because an installed binary carries no plugin files it could
/// rely on, and composed rather than copied so both launchers that can start
/// a tmux server apply one policy. The module defines names only, so its
/// position ahead of the view's own `__main__` block runs nothing extra.
const VIEW_PROGRAM: &str = concat!(
    include_str!("../../../plugins/story/lib/probe_budget.py"),
    "\nprobe_run = run\nprobe_operation = operation\n",
    include_str!("../../../plugins/story/lib/tmux_server_env.py"),
    "\n",
    include_str!("../../../scripts/verification-view.py")
);

/// Opens or repairs only the explicitly owned project view.
///
/// The reconcile child is journaled only when it fails (SH-761): it runs
/// every [`RECONCILE_INTERVAL`] under the project's own journal scope, so
/// announcing each success would fill the window it exists to keep alive.
///
/// The view only locks and reads the journal directory. This function
/// prepares it first, ignore file included, so the daemon is its only
/// creator on this path (SH-771).
fn open(env: &Environment, project: &str, directory: &Path, stop: &AtomicBool) {
    if !env.verifier_mirror_enabled() {
        return;
    }
    if let Err(error) = super::ignore::prepare(directory) {
        super::emit(
            "WARN",
            "tmux",
            "event",
            "",
            &format!(
                "project {project} verification view unavailable: journal {} cannot be prepared: {error}",
                directory.display()
            ),
        );
        return;
    }
    let result = (|| -> Result<(), String> {
        let binary = std::env::current_exe().map_err(|error| error.to_string())?;
        let mut command = Command::new("python3");
        command
            .args(["-c", VIEW_PROGRAM])
            .arg(project)
            .arg(directory)
            .arg(binary)
            .env("HOME", env.home())
            .envs(env.child_vars())
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        let captured = match run_view(command, stop) {
            Ok(captured) => captured,
            Err(CaptureError::Cancelled) => return Ok(()),
            Err(error) => {
                return Err(format!(
                    "project {project} verification view unavailable (outer bound {}s): {}",
                    VIEW_RECONCILE_TIMEOUT.as_secs(),
                    error.detail()
                ));
            }
        };
        if !captured.status.success() {
            return Err(format!(
                "project {project} verification view unavailable ({}): {}",
                captured.status,
                String::from_utf8_lossy(&captured.stderr).trim()
            ));
        }
        Ok(())
    })();
    if let Err(error) = result {
        super::emit("WARN", "tmux", "event", "", &error);
    }
}

/// The process boundary shared by production and the deadline/cancellation tests.
fn run_view(command: Command, stop: &AtomicBool) -> Result<Captured, CaptureError> {
    run_captured_quiet_cancellable(command, VIEW_RECONCILE_TIMEOUT, || {
        stop.load(Ordering::Acquire)
    })
}

/// Reconstructs activated project views and handles phase requests off verifier workers.
pub(crate) fn poll(store: &impl Store, env: &Environment, stop: &AtomicBool, requests: &Requests) {
    poll_with(store, env, stop, requests, open);
}

/// Inject only the external view operation; the supervisor and store flow stay real.
fn poll_with(
    store: &impl Store,
    env: &Environment,
    stop: &AtomicBool,
    requests: &Requests,
    mut reconcile: impl FnMut(&Environment, &str, &Path, &AtomicBool),
) {
    if !env.verifier_mirror_enabled() {
        return;
    }
    let mut schedule = Schedule::default();
    let mut next_scan = Instant::now();
    let mut catalog_retry = Instant::now();
    while !stop.load(Ordering::Acquire) {
        schedule.request(requests.take());
        let now = Instant::now();
        if now < catalog_retry {
            requests.wait(STOP_POLL.min(catalog_retry - now));
            continue;
        }
        let mut due = schedule.due(now, RECONCILE_INTERVAL);
        let periodic = now >= next_scan;
        if periodic || !due.is_empty() {
            match registered_views(store) {
                Ok(projects) => {
                    let valid = projects.iter().map(|(id, _, _)| *id).collect();
                    schedule.retain(&valid);
                    if periodic {
                        schedule.request(
                            projects
                                .iter()
                                .filter(|(_, _, directory)| directory.is_dir())
                                .map(|(id, _, _)| *id),
                        );
                        due.extend(schedule.due(now, RECONCILE_INTERVAL));
                        next_scan = now + RECONCILE_INTERVAL;
                    }
                    // The catalog read ends before subprocesses start. A request carries
                    // no path authority and cannot revive a deleted registration.
                    for (id, project, directory) in projects {
                        if stop.load(Ordering::Acquire) {
                            return;
                        }
                        if !due.contains(&id) {
                            continue;
                        }
                        let _scope = super::context::enter(Some(super::context::LogContext {
                            directory: directory.clone(),
                            label: format!("project={project} reader"),
                        }));
                        reconcile(env, &project, &directory, stop);
                        schedule.completed(id, Instant::now());
                        // Activation survives a failed first spawn or mkdir. The
                        // catalog and cooldown still govern every later attempt.
                        schedule.request([id]);
                    }
                }
                Err(error) => {
                    schedule.request(due);
                    catalog_retry = Instant::now() + RECONCILE_INTERVAL;
                    super::emit(
                        "WARN",
                        "tmux",
                        "event",
                        "",
                        &format!("cannot list project verification views: {error}"),
                    );
                }
            }
        }
        requests.wait(STOP_POLL);
    }
}

/// Only a currently registered, valid checkout can authorize a reader path.
fn registered_views(
    store: &impl Store,
) -> Result<Vec<(ProjectId, String, std::path::PathBuf)>, crate::store::StoreError> {
    store.read(|tx| {
        let mut projects = Vec::new();
        for project in tx.projects()? {
            if let Some(checkout) = tx.checkout_path(project.id)?
                && checkout.is_absolute()
                && checkout.is_dir()
            {
                projects.push((project.id, project.slug, super::project_journal(&checkout)));
            }
        }
        Ok(projects)
    })
}

/// Display only the executable or script path, never arbitrary arguments
/// (which may contain a notification body or credentials).
pub(crate) fn command_source(command: &Command) -> String {
    let program = command.get_program();
    if [OsStr::new("bash"), OsStr::new("sh")].contains(&program)
        && let Some(script) = command
            .get_args()
            .next()
            .filter(|arg| !arg.to_string_lossy().starts_with('-'))
    {
        return script.to_string_lossy().into_owned();
    }
    program.to_string_lossy().into_owned()
}
