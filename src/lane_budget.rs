//! The machine lane budget, and the live agent windows counted against it
//! (SH-655).
//!
//! D14 (`docs/spec/full-auto-engine.md`) promises a *machine-wide* lane
//! budget, and until SH-655 the only thing that consulted it was the Full
//! Auto engine, over its own `engine_lanes` rows. A `/story do` typed by hand
//! is the same `cmd_dispatch`, opens the same worktree and window, compiles
//! the same workspace, and counted for nothing — seven of them were measured
//! at load 33 on a ten-core machine. This module is the census both doors
//! read: the engine from inside the daemon, and `story lane-budget` from the
//! operator's own shell, before a manual dispatch has claimed anything.
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
//! The census is three-valued (SH-626): counted, or unanswered with the
//! probe's own words. A caller that read "tmux could not be asked" as "no
//! sessions" would dispatch past the budget exactly when the machine is
//! least observable. `story lane-budget` reports an unanswered census with no
//! `live` and no `available` at all, and `cmd_dispatch` proceeds on it
//! loudly rather than refusing on it.

use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::process::{CaptureError, run_captured};
use crate::service::engine::{ENGINE_LANE_BUDGET, TMUX_TIMEOUT};

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

impl WindowCensus {
    /// The number of live sessions, when the server answered.
    pub fn live(&self) -> Option<usize> {
        match self {
            Self::Counted { windows } => Some(windows.len()),
            Self::Unanswered { .. } => None,
        }
    }
}

/// The machine-wide ceiling every dispatch door is measured against — the
/// engine's own number (`ENGINE_LANE_BUDGET`, itself derived from
/// `api::dispatch::MAX_RUNNING`), never a second copy (SH-136).
pub fn budget() -> usize {
    ENGINE_LANE_BUDGET
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
/// the census it fills lanes against is taken on the server its lanes live
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
    /// The machine-wide ceiling.
    pub budget: usize,
    /// `"counted"` or `"unanswered"`.
    pub probe: String,
    /// Live sessions, when counted. Absent on an unanswered census rather
    /// than zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<usize>,
    /// The live sessions' window addresses, when counted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub windows: Option<Vec<String>>,
    /// Whether another session fits under the budget. Absent when nothing
    /// was counted: no evidence is not permission.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available: Option<bool>,
    /// The probe's own words, when unanswered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl LaneBudgetView {
    /// Measures a census against the budget.
    pub fn from_census(census: WindowCensus) -> Self {
        let budget = budget();
        match census {
            WindowCensus::Counted { windows } => Self {
                budget,
                probe: "counted".to_string(),
                live: Some(windows.len()),
                available: Some(windows.len() < budget),
                windows: Some(windows),
                detail: None,
            },
            WindowCensus::Unanswered { detail } => Self {
                budget,
                probe: "unanswered".to_string(),
                live: None,
                windows: None,
                available: None,
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
                let mut text = format!(
                    "{live} of {} lanes in use on this machine{}\n",
                    self.budget,
                    if live < self.budget {
                        ""
                    } else {
                        " — at the budget"
                    }
                );
                for window in windows {
                    text.push_str("  ");
                    text.push_str(window);
                    text.push('\n');
                }
                text
            }
            (None, detail) => format!(
                "lane budget {}; live sessions unknown: {}\n",
                self.budget,
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
        assert_eq!(view.available, None);
        assert_eq!(view.probe, "unanswered");
        let json = serde_json::to_value(&view).unwrap();
        assert!(json.get("live").is_none(), "{json}");
        assert!(json.get("available").is_none(), "{json}");
    }
}
