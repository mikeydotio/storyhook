//! Read-only, socket-bound terminal identity. No provider is launched here.
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use crate::error::AppError;
use crate::process::run_captured;
use serde::{Deserialize, Serialize};

/// One server-local pane belonging to an exact named window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourcePane {
    /// Server-local window identifier.
    pub window_id: String,
    /// Exact window name.
    pub window_name: String,
    /// Server-local pane identifier.
    pub pane_id: String,
    /// Observed process identifier, needed to detect respawns.
    pub pid: String,
    /// Whether the pane's process has exited.
    pub dead: bool,
    /// Provider recorded by dispatch, never supplied by the querying agent.
    pub provider: Option<String>,
    /// Working directory observed from the pane, including a removed worktree.
    pub cwd: std::path::PathBuf,
}

/// Inspects a recorded server, retaining duplicate-window evidence.
pub fn panes(socket: &Path, names: &BTreeSet<String>) -> Result<Vec<ResourcePane>, AppError> {
    match socket.try_exists() {
        Ok(false) => return Ok(Vec::new()),
        Ok(true) => {}
        Err(error) => {
            return Err(AppError::Validation(format!(
                "cannot inspect tmux socket {}: {error}",
                socket.display()
            )));
        }
    }
    let mut command = Command::new("tmux");
    crate::env::spawn_env::apply_dispatch_allowlist(&mut command);
    command.arg("-S").arg(socket).args(["list-panes", "-a", "-F", "#{window_name}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{@storyhook-agent}\t#{pane_active}\t#{pane_current_path}"]);
    let output = run_captured(command, super::super::engine::TMUX_TIMEOUT)
        .map_err(|e| AppError::Validation(format!("tmux {}: {}", socket.display(), e.detail())))?;
    if !output.status.success() {
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        // tmux emits this exact answer for ECONNREFUSED after its final window
        // exits. A leftover socket entry does not imply a listening server.
        if output.stdout.is_empty()
            && diagnostic.trim_end() == format!("no server running on {}", socket.display())
        {
            return Ok(Vec::new());
        }
        return Err(AppError::Validation(format!(
            "cannot query recorded tmux server {}: {}",
            socket.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let text = String::from_utf8(output.stdout)
        .map_err(|e| AppError::Validation(format!("invalid tmux inventory: {e}")))?;
    let mut result = Vec::new();
    for line in text.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        if !fields.first().is_some_and(|name| names.contains(*name)) {
            continue;
        }
        if fields.len() != 8 {
            return Err(AppError::Validation(format!(
                "malformed tmux pane: {line:?}"
            )));
        }
        if !fields[1]
            .strip_prefix('@')
            .is_some_and(|n| n.parse::<u64>().is_ok())
            || !fields[2]
                .strip_prefix('%')
                .is_some_and(|n| n.parse::<u64>().is_ok())
            || !fields[3].parse::<u32>().is_ok_and(|pid| pid > 0)
            || !matches!(fields[4], "0" | "1")
            || !matches!(fields[6], "0" | "1")
            || !Path::new(fields[7]).is_absolute()
        {
            return Err(AppError::Validation(format!(
                "invalid tmux identity: {line:?}"
            )));
        }
        if fields[6] != "1" {
            continue;
        }
        result.push(ResourcePane {
            window_name: fields[0].into(),
            window_id: fields[1].into(),
            pane_id: fields[2].into(),
            pid: fields[3].into(),
            dead: fields[4] == "1",
            provider: (!fields[5].is_empty()).then(|| fields[5].into()),
            cwd: super::git::canonical(Path::new(fields[7]))?,
        });
    }
    result.sort_by(|a, b| a.window_id.cmp(&b.window_id));
    result.dedup();
    Ok(result)
}
