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
    inventory(socket, names, true)
}

/// Inspects every pane so cleanup never hides another pane behind the active one.
pub(crate) fn all_panes(
    socket: &Path,
    names: &BTreeSet<String>,
) -> Result<Vec<ResourcePane>, AppError> {
    inventory(socket, names, false)
}

fn inventory(
    socket: &Path,
    names: &BTreeSet<String>,
    active_only: bool,
) -> Result<Vec<ResourcePane>, AppError> {
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
    // ASCII locales make tmux replace tabs with underscores unless UTF-8 is explicit.
    command.args(["-u", "-S"]).arg(socket).args(["list-panes", "-a", "-F", "#{window_name}\t#{window_id}\t#{pane_id}\t#{pane_pid}\t#{pane_dead}\t#{@storyhook-agent}\t#{pane_active}\t#{pane_current_path}"]);
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
        if active_only && fields[6] != "1" {
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

/// The `list-panes` format a conflict-reconcile hold asks the story's tmux
/// server for (SH-770): pane id, window name, and the four liveness fields
/// [`crate::service::engine::pane_state`] classifies, tab-separated, in the
/// order [`story_panes_probe`] reads them.
pub(crate) const HOLD_PROBE_FORMAT: &str = "#{pane_id}\t#{window_name}\t#{pane_pid}\t#{pane_current_command}\t#{pane_dead}\t#{window_activity}";

/// Whether the agent in a window named for a story still runs on `socket`,
/// the tmux server its cleanup lease records (SH-770).
///
/// Every pane of every matching window is judged, because a person can split
/// an agent's window or link it into a second session, so the combination
/// errs toward the agent being present:
/// - **Alive** when any pane runs the agent it was launched with;
/// - **Gone** only when the server or every matching window is gone, or
///   every matching pane is dead;
/// - **Unanswered** when tmux cannot be asked, or a pane runs something else
///   (the identity pattern is this daemon's, not the pane's own record).
pub(crate) fn probe_story_panes(
    socket: &Path,
    names: &BTreeSet<String>,
    cancellation: &crate::process::Cancellation,
) -> crate::service::engine::WindowProbe {
    use crate::service::engine::WindowProbe;
    match socket.try_exists() {
        Ok(true) => {}
        Ok(false) => {
            return WindowProbe::Gone {
                detail: format!("tmux server socket {} no longer exists", socket.display()),
            };
        }
        Err(error) => {
            return WindowProbe::Unanswered {
                detail: format!("cannot inspect tmux socket {}: {error}", socket.display()),
            };
        }
    }
    let mut command = Command::new("tmux");
    crate::env::spawn_env::apply_dispatch_allowlist(&mut command);
    // ASCII locales make tmux replace tabs with underscores unless UTF-8 is explicit.
    command
        .args(["-u", "-S"])
        .arg(socket)
        .args(["list-panes", "-a", "-F", HOLD_PROBE_FORMAT]);
    let output = match crate::process::run_captured_cancellable(
        command,
        super::super::engine::TMUX_TIMEOUT,
        crate::process::TerminationPolicy::Kill,
        cancellation,
        |_| Ok(()),
    ) {
        Ok(output) => output,
        Err(error) => {
            return WindowProbe::Unanswered {
                detail: format!(
                    "tmux {} did not answer the agent probe: {}",
                    socket.display(),
                    error.detail()
                ),
            };
        }
    };
    if !output.status.success() {
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        // tmux emits this exact answer for ECONNREFUSED after its final window
        // exits. A leftover socket entry does not imply a listening server.
        if output.stdout.is_empty()
            && diagnostic.trim_end() == format!("no server running on {}", socket.display())
        {
            return WindowProbe::Gone {
                detail: format!("no tmux server runs on {}", socket.display()),
            };
        }
        return WindowProbe::Unanswered {
            detail: format!(
                "tmux {} exited {} answering the agent probe: {}",
                socket.display(),
                output.status,
                diagnostic.trim()
            ),
        };
    }
    story_panes_probe(
        &String::from_utf8_lossy(&output.stdout),
        names,
        socket,
        |target, pid, command, dead, activity| {
            crate::service::engine::pane_state(target, pid, command, dead, activity)
        },
    )
}

/// Combines the rows of a [`HOLD_PROBE_FORMAT`] answer into one verdict, as
/// [`probe_story_panes`] documents. `classify` judges one pane from its
/// target, pid, current command, dead flag and activity stamp.
fn story_panes_probe(
    answer: &str,
    names: &BTreeSet<String>,
    socket: &Path,
    classify: impl Fn(&str, &str, &str, &str, &str) -> crate::service::engine::PaneState,
) -> crate::service::engine::WindowProbe {
    use crate::service::engine::{PaneState, WindowProbe};
    let mut panes = std::collections::BTreeMap::new();
    for line in answer.lines() {
        let fields: Vec<_> = line.split('\t').collect();
        if !fields.get(1).is_some_and(|name| names.contains(*name)) {
            continue;
        }
        let [pane, name, pid, command, dead, activity] = fields[..] else {
            return WindowProbe::Unanswered {
                detail: format!("tmux answered the agent probe with {line:?}, not six fields"),
            };
        };
        // A window linked into a second session lists its panes once per link.
        panes.insert(
            pane,
            classify(&format!("{name}:{pane}"), pid, command, dead, activity),
        );
    }
    if panes.is_empty() {
        let names = names.iter().cloned().collect::<Vec<_>>().join(", ");
        return WindowProbe::Gone {
            detail: format!(
                "no window named {names} on tmux server {}",
                socket.display()
            ),
        };
    }
    let live = panes
        .values()
        .filter_map(|state| match state {
            PaneState::Live { last_output_at } => Some(*last_output_at),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !live.is_empty() {
        return WindowProbe::Alive {
            last_output_at: live.into_iter().flatten().max(),
        };
    }
    let detail =
        |states: &mut dyn Iterator<Item = &String>| states.cloned().collect::<Vec<_>>().join("; ");
    let doubtful = detail(&mut panes.values().filter_map(|state| match state {
        PaneState::Foreign { detail } | PaneState::Unreadable { detail } => Some(detail),
        _ => None,
    }));
    if !doubtful.is_empty() {
        return WindowProbe::Unanswered { detail: doubtful };
    }
    WindowProbe::Gone {
        detail: detail(&mut panes.values().filter_map(|state| match state {
            PaneState::Dead { detail } => Some(detail),
            _ => None,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::engine::{PaneState, WindowProbe};

    const SOCKET: &str = "/tmp/storyhook-test/default";

    fn names() -> BTreeSet<String> {
        BTreeSet::from(["SH-1".to_string()])
    }

    /// Classifies by the command column alone: `claude` is the agent, `dead`
    /// is a dead pane, `?` is unreadable, anything else runs something else.
    fn by_command(
        target: &str,
        _pid: &str,
        command: &str,
        _dead: &str,
        activity: &str,
    ) -> PaneState {
        match command {
            "claude" => PaneState::Live {
                last_output_at: activity.parse().ok(),
            },
            "dead" => PaneState::Dead {
                detail: format!("{target} dead"),
            },
            "?" => PaneState::Unreadable {
                detail: format!("{target} unreadable"),
            },
            other => PaneState::Foreign {
                detail: format!("{target} runs {other}"),
            },
        }
    }

    fn probe(rows: &[&str]) -> WindowProbe {
        story_panes_probe(&rows.join("\n"), &names(), Path::new(SOCKET), by_command)
    }

    #[test]
    fn an_agent_pane_beside_a_shell_split_is_alive_with_the_latest_output() {
        assert_eq!(
            probe(&[
                "%1\tSH-1\t10\tclaude\t0\t100",
                "%2\tSH-1\t11\tzsh\t0\t200",
                "%3\tSH-2\t12\tclaude\t0\t900",
            ]),
            WindowProbe::Alive {
                last_output_at: Some(100)
            },
            "another story's window says nothing about this one"
        );
    }

    #[test]
    fn linked_duplicate_rows_are_one_pane() {
        assert_eq!(
            probe(&["%1\tSH-1\t10\tdead\t1\t", "%1\tSH-1\t10\tdead\t1\t"]),
            WindowProbe::Gone {
                detail: "SH-1:%1 dead".into()
            }
        );
    }

    #[test]
    fn no_matching_window_is_gone() {
        assert_eq!(
            probe(&["%3\tSH-2\t12\tclaude\t0\t900"]),
            WindowProbe::Gone {
                detail: format!("no window named SH-1 on tmux server {SOCKET}")
            }
        );
        assert!(matches!(probe(&[]), WindowProbe::Gone { .. }));
    }

    #[test]
    fn a_pane_running_something_else_or_unreadable_is_no_evidence() {
        for rows in [
            vec!["%1\tSH-1\t10\tzsh\t0\t100"],
            vec!["%1\tSH-1\t10\tdead\t1\t", "%2\tSH-1\t11\tzsh\t0\t100"],
            vec!["%1\tSH-1\tx\t?\t0\t100"],
        ] {
            assert!(
                matches!(probe(&rows), WindowProbe::Unanswered { .. }),
                "{rows:?}"
            );
        }
    }

    #[test]
    fn a_malformed_matching_row_is_no_evidence() {
        assert!(matches!(
            probe(&["%1\tSH-1\t10\tclaude"]),
            WindowProbe::Unanswered { .. }
        ));
    }

    #[test]
    fn a_missing_socket_is_gone_without_asking_tmux() {
        let root = storyhook_test_support::scratch_dir();
        assert!(matches!(
            probe_story_panes(
                &root.path().join("absent"),
                &names(),
                &crate::process::Cancellation::default()
            ),
            WindowProbe::Gone { .. }
        ));
    }
}
