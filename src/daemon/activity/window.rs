//! Supervise persistent project readers independently of verification phases.

use crate::{
    env::Environment,
    process::run_captured_quiet,
    store::{ReadOps, Store},
};
use std::{
    ffi::OsStr,
    path::Path,
    process::Command,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);

/// Opens or repairs only the explicitly owned project view.
///
/// The reconcile child is journaled only when it fails (SH-761): it runs
/// every [`RECONCILE_INTERVAL`] under the project's own journal scope, so
/// announcing each success would fill the window it exists to keep alive.
pub(crate) fn open(env: &Environment, project: &str, directory: &Path) {
    if !env.verifier_mirror_enabled() {
        return;
    }
    let result = (|| -> Result<(), String> {
        let binary = std::env::current_exe().map_err(|error| error.to_string())?;
        let mut command = Command::new("python3");
        command
            .args(["-c", include_str!("../../../scripts/verification-view.py")])
            .arg(project)
            .arg(directory)
            .arg(binary)
            .env("HOME", env.home())
            .envs(env.child_vars())
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        let captured =
            run_captured_quiet(command, Duration::from_secs(5)).map_err(|error| error.detail())?;
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

/// Reconstructs activated project views on startup and repairs idle readers.
pub(crate) fn poll(store: &impl Store, env: &Environment, stop: &AtomicBool) {
    if !env.verifier_mirror_enabled() {
        return;
    }
    while !stop.load(Ordering::Relaxed) {
        let projects = store.read(|tx| {
            let mut projects = Vec::new();
            for project in tx.projects()? {
                if let Some(checkout) = tx.checkout_path(project.id)? {
                    projects.push((project.slug, checkout.join(".storyhook/logs")));
                }
            }
            Ok(projects)
        });
        match projects {
            Ok(projects) => {
                for (project, directory) in projects {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if directory.is_dir() {
                        let _scope = super::context::enter(Some(super::context::LogContext {
                            directory: directory.clone(),
                            label: format!("project={project} reader"),
                        }));
                        open(env, &project, &directory);
                    }
                }
            }
            Err(error) => super::emit(
                "WARN",
                "tmux",
                "event",
                "",
                &format!("cannot list project verification views: {error}"),
            ),
        }
        let until = Instant::now() + RECONCILE_INTERVAL;
        while !stop.load(Ordering::Relaxed) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
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
