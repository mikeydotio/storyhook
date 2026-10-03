//! Native observations use the same ownership protocol as the plugin (SH-825).
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::env::Environment;
use crate::error::AppError;
use crate::process::Cancellation;

const INSPECT_PROGRAM: &str = concat!(
    include_str!("../../plugins/story/lib/probe_budget.py"),
    "\n",
    include_str!("../../plugins/story/lib/tmux_server_env.py"),
    "\n",
    include_str!("../../plugins/story/lib/tmux_target.py"),
    r#"
import sys
try:
    with operation(float(sys.argv[2])):
        target = resolve_target(sys.argv[1] or None, os.environ, run,
                                client_environment(os.environ))
        print(json.dumps(target))
except (RuntimeError, OSError, subprocess.TimeoutExpired) as error:
    print(str(error), file=sys.stderr)
    sys.exit(1)
"#
);

/// One operation's immutable server selection. Resolving ownership does not
/// prove that a pane number from a predecessor generation still names its agent.
#[derive(Debug, Deserialize)]
pub(crate) struct Target {
    /// Whether RV-10 owns this logical server.
    pub(crate) protected: bool,
    /// The canonical logical server identity.
    pub(crate) socket: PathBuf,
    /// The private generation endpoint, or the unmanaged logical socket.
    pub(crate) endpoint: PathBuf,
}

impl Target {
    /// Pin a protected client without permitting tmux to start a replacement.
    /// An unmanaged caller retains its original socket selection conventions.
    pub(crate) fn apply(&self, command: &mut Command, socket: Option<&Path>) {
        if self.protected {
            command.arg("-N").arg("-S").arg(&self.endpoint);
        } else if let Some(socket) = socket {
            command.arg("-S").arg(socket);
        }
    }
}

/// Read the active generation, without startup or restore authority, within
/// the caller's one absolute deadline and cancellation scope.
pub(crate) fn inspect(
    env: &Environment,
    socket: Option<&Path>,
    deadline: Instant,
    cancellation: &Cancellation,
) -> Result<Target, AppError> {
    let timeout = remaining(deadline)?;
    let mut command = Command::new("python3");
    crate::env::spawn_env::apply_dispatch_allowlist(&mut command);
    command
        .args(["-c", INSPECT_PROGRAM])
        .arg(socket.unwrap_or_else(|| Path::new("")))
        .arg(timeout.as_secs_f64().to_string())
        .env("HOME", env.home())
        .envs(env.child_vars());
    let captured = crate::process::run_captured_cancellable(
        command,
        remaining(deadline)?,
        crate::process::TerminationPolicy::Kill,
        cancellation,
        |_| Ok(()),
    )
    .map_err(|error| {
        AppError::Validation(format!("tmux ownership inspection: {}", error.detail()))
    })?;
    if !captured.status.success() {
        return Err(AppError::Validation(format!(
            "tmux ownership inspection exited {}: {}",
            captured.status,
            String::from_utf8_lossy(&captured.stderr).trim()
        )));
    }
    serde_json::from_slice(&captured.stdout)
        .map_err(|error| AppError::Validation(format!("invalid tmux ownership response: {error}")))
}

/// Remaining time for both discovery and the terminal operation it authorizes.
pub(crate) fn remaining(deadline: Instant) -> Result<Duration, AppError> {
    let timeout = deadline.saturating_duration_since(Instant::now());
    if timeout.is_zero() {
        Err(AppError::Validation(
            "tmux ownership operation budget exhausted".into(),
        ))
    } else {
        Ok(timeout)
    }
}

#[cfg(test)]
#[path = "tmux_target_tests.rs"]
pub(crate) mod tests;
