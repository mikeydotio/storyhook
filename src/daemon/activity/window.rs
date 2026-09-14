//! The fixed tmux attach point follows the journal, not an attempt's log.

use crate::{env::Environment, process::run_captured};
use std::{ffi::OsStr, process::Command, time::Duration};

pub(super) fn open(env: &Environment) {
    let result = (|| -> Result<(), String> {
        let binary = std::env::current_exe().map_err(|error| error.to_string())?;
        let mut command = Command::new("bash");
        let script = format!(
            "{}\nverifier_window_logs \"$1\" \"$2\"",
            include_str!("../../../scripts/verify-window.sh")
        );
        command
            .args(["-c", &script, "verify-window"])
            .arg(binary)
            .arg(env.store_path())
            .env("HOME", env.home())
            .envs(env.child_vars())
            .env_remove("TMUX")
            .env_remove("TMUX_PANE");
        // `run_captured` observes this helper as well, including refusals.
        let captured =
            run_captured(command, Duration::from_secs(5)).map_err(|error| error.detail())?;
        if !captured.status.success() {
            return Err(format!(
                "tmux activity view unavailable ({})",
                captured.status
            ));
        }
        Ok(())
    })();
    if let Err(error) = result {
        super::emit("WARN", "tmux", "event", "", &error);
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
