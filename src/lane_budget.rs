//! Informational census of live agent windows (SH-655, SH-672).
//!
//! The Full Auto engine and `story lane-budget` share this probe. A census
//! describes sessions; it does not choose a budget or authorize dispatch.
//! The engine limits each run by its configured lane count. Outside the
//! engine, concurrency is between the operator and the agent.
//!
//! # What a live agent session IS
//!
//! Ask what a process is, never what it is spelled (SH-226, SH-239). Every
//! dispatched window — engine-filled or manual — carries the
//! `@storyhook-agent` window option `cmd_dispatch` sets, and every one is
//! `remain-on-exit on`, so a session that has finished leaves a window whose
//! pane tmux reports dead. A live session is therefore a window with the
//! option set AND `pane_dead` clear. A worktree with no window, a window on
//! another tmux socket, and an agent started outside `cmd_dispatch` are all
//! outside the census, and the As-built section on SH-655 says so.
//!
//! # No evidence is not zero
//!
//! An unanswered probe carries its own words and no live count. It must
//! never be interpreted as an empty server.

use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::process::{CaptureError, run_captured};
use crate::service::engine::TMUX_TIMEOUT;

/// The one `list-windows` format the census asks for: the window's address,
/// its `@storyhook-agent` option (empty when unset), and whether tmux itself
/// considers the pane dead, tab-separated. `-a` asks every session on the
/// server, because a manual dispatch may target a session other than the
/// engine's.
pub const CENSUS_FORMAT: &str = "#{session_name}:#{window_name}\t#{@storyhook-agent}\t#{pane_dead}";

/// What the tmux server said when asked which agent windows are live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowCensus {
    /// The server answered; these windows carry `@storyhook-agent` and a
    /// pane that is not dead.
    Counted { windows: Vec<String> },
    /// The server could not be asked, or answered in a shape this build
    /// does not understand. `detail` carries the probe's own words. This is
    /// **no evidence**, never a count of zero.
    Unanswered { detail: String },
}

/// Asks the tmux server the caller's environment names for its live agent
/// windows.
///
/// Deliberately a plain `tmux` on the caller's `PATH` with the caller's
/// environment intact — never `ShellDispatcher`'s allowlisted spawn, which
/// strips `TMUX` and would ask the default socket on behalf of a client that
/// is attached somewhere else.
pub fn count_live_agent_windows() -> WindowCensus {
    census_through(Command::new("tmux"))
}

/// The same census through a caller-prepared `tmux` command — the engine's
/// `ShellDispatcher` passes its own program and allowlisted environment, so
/// the census is taken on the server its lanes live
/// on. One parser, one error vocabulary, two doors (SH-136).
pub fn census_through(mut command: Command) -> WindowCensus {
    command.args(["list-windows", "-a", "-F", CENSUS_FORMAT]);
    let captured = match run_captured(command, TMUX_TIMEOUT) {
        Ok(captured) => captured,
        Err(CaptureError::Timeout(_)) => {
            return WindowCensus::Unanswered {
                detail: format!(
                    "tmux did not answer the window census within {}s",
                    TMUX_TIMEOUT.as_secs()
                ),
            };
        }
        Err(error) => {
            return WindowCensus::Unanswered {
                detail: format!(
                    "tmux could not be run for the window census: {}",
                    error.detail()
                ),
            };
        }
    };
    if !captured.status.success() {
        let stderr = String::from_utf8_lossy(&captured.stderr).trim().to_string();
        return WindowCensus::Unanswered {
            detail: format!(
                "tmux exited {} answering the window census: {}",
                captured.status,
                if stderr.is_empty() {
                    "(no stderr)"
                } else {
                    &stderr
                }
            ),
        };
    }
    parse_census(&String::from_utf8_lossy(&captured.stdout))
}

/// Folds one `list-windows` answer in [`CENSUS_FORMAT`] into a census.
///
/// A line with fewer than three fields is a shape this build does not
/// understand — a tmux whose format vocabulary changed — and is reported
/// rather than skipped, because a skipped line is a session that silently
/// stopped counting.
pub fn parse_census(answer: &str) -> WindowCensus {
    let mut windows = Vec::new();
    for line in answer.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let mut fields = line.splitn(3, '\t');
        let (Some(window), Some(agent), Some(dead)) = (fields.next(), fields.next(), fields.next())
        else {
            return WindowCensus::Unanswered {
                detail: format!(
                    "tmux answered the window census in a shape this build does not understand: {line:?}"
                ),
            };
        };
        if !agent.is_empty() && dead.trim() == "0" {
            windows.push(window.to_string());
        }
    }
    WindowCensus::Counted { windows }
}

/// `story lane-budget`'s answer, in both renderings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LaneBudgetView {
    /// `"counted"` or `"unanswered"`.
    pub probe: String,
    /// Live sessions, when counted. Absent on an unanswered census rather
    /// than zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<usize>,
    /// The live sessions' window addresses, when counted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub windows: Option<Vec<String>>,
    /// The probe's own words, when unanswered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl LaneBudgetView {
    /// Reports the census without assigning a budget.
    pub fn from_census(census: WindowCensus) -> Self {
        match census {
            WindowCensus::Counted { windows } => Self {
                probe: "counted".to_string(),
                live: Some(windows.len()),
                windows: Some(windows),
                detail: None,
            },
            WindowCensus::Unanswered { detail } => Self {
                probe: "unanswered".to_string(),
                live: None,
                windows: None,
                detail: Some(detail),
            },
        }
    }

    /// Asks the caller's tmux server and measures the answer.
    pub fn measure() -> Self {
        Self::from_census(count_live_agent_windows())
    }

    /// The rendering a person reads.
    pub fn render_human(&self) -> String {
        match (&self.windows, &self.detail) {
            (Some(windows), _) => {
                let live = windows.len();
                let mut text = format!("{live} live agent sessions on this tmux server\n");
                for window in windows {
                    text.push_str("  ");
                    text.push_str(window);
                    text.push('\n');
                }
                text
            }
            (None, detail) => format!(
                "live agent sessions unknown: {}\n",
                detail.as_deref().unwrap_or("the census was not answered")
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tagged_live_windows_count() {
        let census = parse_census("a:x\tclaude\t0\nb:y\t\t0\nc:z\tcodex\t1\n\nd:w\tclaude\t0\n");
        assert_eq!(
            census,
            WindowCensus::Counted {
                windows: vec!["a:x".to_string(), "d:w".to_string()]
            }
        );
    }

    #[test]
    fn a_line_with_too_few_fields_is_unanswered_not_skipped() {
        let census = parse_census("a:x\tclaude\t0\nbroken line\n");
        assert!(
            matches!(census, WindowCensus::Unanswered { .. }),
            "{census:?}"
        );
    }

    #[test]
    fn an_empty_answer_is_a_counted_zero() {
        assert_eq!(parse_census(""), WindowCensus::Counted { windows: vec![] });
    }

    #[test]
    fn an_unanswered_census_carries_neither_a_count_nor_permission() {
        let view = LaneBudgetView::from_census(WindowCensus::Unanswered {
            detail: "no server".to_string(),
        });
        assert_eq!(view.live, None);
        assert_eq!(view.probe, "unanswered");
        let json = serde_json::to_value(&view).unwrap();
        assert!(json.get("live").is_none(), "{json}");
        assert!(json.get("available").is_none(), "{json}");
        assert!(json.get("budget").is_none(), "{json}");
    }
}
