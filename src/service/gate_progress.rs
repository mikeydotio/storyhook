//! Pure fold and render for the SH-524 gate progress journal.
//!
//! `scripts/gate-progress.sh` and its producers (`scripts/leg.sh`,
//! `scripts/run-tests.sh`, `plugins/story/tests/run-tests.sh`,
//! `scripts/run-e2e.sh`, `e2e/gate-progress-reporter.ts`,
//! `scripts/verify-pr.sh`) append one JSON object per line to a journal file
//! named by `$STORYHOOK_GATE_PROGRESS`. [`fold`] turns that raw text into a
//! tree of [`ProgressItem`]s; [`render`] turns the tree into the markdown
//! checklist body the SH-524 progress comment carries. Both are pure — no
//! clock, no I/O — so both are exhaustively table-tested.
//!
//! # The path tree
//!
//! A line's `path` is a `/`-separated address ("release gate/rust-suite").
//! [`fold`] builds a tree by walking each segment, creating a
//! [`ProgressItem`] at every level it has not seen before — so a parent like
//! `"release gate"` exists in the tree the moment any of its children does,
//! even though nothing ever emits an `item` line naming it directly.
//!
//! # Two kinds of leaf
//!
//! A leg like `fmt`/`clippy`/`build` never gets a `case` line — it is a
//! single pass/fail unit. A suite like `rust-suite`/`plugin`/an `e2e`
//! project does, one per test. [`ProgressItem::contribution`] treats both
//! uniformly: a leaf with no recorded cases and no explicit total
//! contributes `(1, 1)` once it reaches a passing terminal status and
//! `(0, 1)` otherwise, so a parent's rolled-up fraction (`release gate`'s
//! "N/7 legs", the top-level header's overall count) is a sum over
//! whichever kind each child happens to be.

use serde::Deserialize;

/// One parsed line of the journal. An unrecognised `kind` (a future producer
/// this binary predates) folds to nothing, the same SH-54 doctrine every
/// other wire format in this codebase already follows.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum JournalLine {
    Item {
        path: String,
        status: String,
        #[serde(default)]
        seconds: Option<u64>,
        #[serde(default)]
        total: Option<u64>,
    },
    Case {
        path: String,
        outcome: String,
    },
    #[serde(other)]
    Unknown,
}

/// The state of one checklist row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemStatus {
    /// Declared (a parent implied by a child, or an e2e project named ahead
    /// of its turn) but nothing has happened to it yet.
    Pending,
    /// In flight.
    Running,
    /// Finished, green.
    Passed,
    /// Finished, red.
    Failed,
    /// Deliberately not run this tier (`scripts/leg.sh --skipped`).
    Skipped,
    /// Reused from a prior gate run (`scripts/leg.sh --reuse`, or the
    /// already-certified merge-preflight shortcut).
    Reused,
}

impl ItemStatus {
    fn parse(word: &str) -> Option<Self> {
        match word {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "passed" => Some(Self::Passed),
            "failed" => Some(Self::Failed),
            "skipped" => Some(Self::Skipped),
            "reused" => Some(Self::Reused),
            _ => None,
        }
    }

    /// Whether this status is a final answer — nothing further will change
    /// it without a fresh `item` line.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Passed | Self::Failed | Self::Skipped | Self::Reused
        )
    }

    /// Whether this status counts as "done and not red" for a rollup.
    fn counts_as_passing(self) -> bool {
        matches!(self, Self::Passed | Self::Skipped | Self::Reused)
    }

    fn checkbox(self) -> &'static str {
        if self.counts_as_passing() {
            "[x]"
        } else {
            "[ ]"
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Reused => "reused (cached)",
        }
    }
}

/// Per-test pass/fail tally for a leaf that receives `case` lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub passed: u32,
    pub failed: u32,
    /// A denominator a producer already knew synchronously — `plugins/story/
    /// tests/run-tests.sh`'s file count, `scripts/run-e2e.sh`'s Playwright
    /// `--list` count. `None` means the only available denominator is
    /// however many cases have been *seen* so far, which is an ESTIMATE
    /// until the item reaches a terminal status.
    pub explicit_total: Option<u32>,
}

impl Counts {
    fn record(&mut self, outcome: &str) {
        match outcome {
            "pass" => self.passed += 1,
            "fail" => self.failed += 1,
            _ => {}
        }
    }

    #[must_use]
    pub fn seen(self) -> u32 {
        self.passed + self.failed
    }
}

/// One checklist row, and everything nested under it.
#[derive(Debug, Clone)]
pub struct ProgressItem {
    pub label: String,
    pub status: ItemStatus,
    /// Whether an `item` line ever named this exact path — as opposed to
    /// existing only because a child's path implied it. An explicit status
    /// always wins over one derived from children.
    explicit: bool,
    pub counts: Counts,
    pub seconds: Option<u64>,
    pub children: Vec<ProgressItem>,
}

impl ProgressItem {
    fn new(label: String) -> Self {
        Self {
            label,
            status: ItemStatus::Pending,
            explicit: false,
            counts: Counts::default(),
            seconds: None,
            children: Vec::new(),
        }
    }

    /// This item's status: its own, if ever explicitly set, otherwise
    /// derived from its children's — failed beats running beats "some
    /// terminal, some not" (still running overall) beats "all terminal"
    /// (passed) beats "all pending" (pending).
    #[must_use]
    pub fn effective_status(&self) -> ItemStatus {
        if self.explicit || self.children.is_empty() {
            return self.status;
        }
        let statuses: Vec<ItemStatus> = self.children.iter().map(Self::effective_status).collect();
        if statuses.contains(&ItemStatus::Failed) {
            return ItemStatus::Failed;
        }
        if statuses.contains(&ItemStatus::Running) {
            return ItemStatus::Running;
        }
        if statuses.iter().all(|s| *s == ItemStatus::Pending) {
            return ItemStatus::Pending;
        }
        if statuses.iter().all(|s| s.is_terminal()) {
            return ItemStatus::Passed;
        }
        // A mix of pending and terminal children, none running, none
        // failed: the parent as a whole has not finished, so it reads as
        // still in progress even though nothing is active this instant.
        ItemStatus::Running
    }

    /// This item's (passed, total, is-estimated) contribution to a rollup.
    ///
    /// A parent's contribution is the sum of its children's. A leaf that
    /// has ever recorded a `case` or carries an explicit total counts real
    /// tests; a bare status-only leaf (`fmt`, `clippy`, `build`) counts as
    /// one unit, passing or not.
    #[must_use]
    pub fn contribution(&self) -> (u32, u32, bool) {
        if !self.children.is_empty() {
            return self
                .children
                .iter()
                .map(Self::contribution)
                .fold((0, 0, false), |(passed, total, estimated), (p, t, e)| {
                    (passed + p, total + t, estimated || e)
                });
        }
        if self.counts.seen() > 0 || self.counts.explicit_total.is_some() {
            let total = self
                .counts
                .explicit_total
                .unwrap_or_else(|| self.counts.seen());
            let estimated =
                self.counts.explicit_total.is_none() && !self.effective_status().is_terminal();
            return (self.counts.passed, total, estimated);
        }
        if self.effective_status().counts_as_passing() {
            (1, 1, false)
        } else {
            (0, 1, false)
        }
    }
}

/// The whole tree read from one journal.
#[derive(Debug, Clone, Default)]
pub struct GateProgress {
    pub items: Vec<ProgressItem>,
}

impl GateProgress {
    /// Finds or creates the node addressed by `path`, creating every
    /// ancestor segment along the way.
    fn item_mut(&mut self, path: &str) -> &mut ProgressItem {
        let mut segments = path.split('/');
        let Some(first) = segments.next() else {
            unreachable!("str::split always yields at least one segment")
        };
        let index = Self::index_of(&mut self.items, first);
        let mut node = &mut self.items[index];
        for segment in segments {
            let index = Self::index_of(&mut node.children, segment);
            node = &mut node.children[index];
        }
        node
    }

    fn index_of(items: &mut Vec<ProgressItem>, label: &str) -> usize {
        match items.iter().position(|item| item.label == label) {
            Some(index) => index,
            None => {
                items.push(ProgressItem::new(label.to_string()));
                items.len() - 1
            }
        }
    }
}

/// Folds a journal's raw text into a tree.
///
/// A line that fails to parse as JSON at all — truncated by a concurrent
/// write, or simply garbage — is skipped rather than treated as an error:
/// the journal is read while it may still be appended to, and a reader must
/// tolerate its own last line being a half-written fragment.
#[must_use]
pub fn fold(journal_text: &str) -> GateProgress {
    let mut progress = GateProgress::default();
    for line in journal_text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<JournalLine>(line) else {
            continue;
        };
        match parsed {
            JournalLine::Item {
                path,
                status,
                seconds,
                total,
            } => {
                let Some(status) = ItemStatus::parse(&status) else {
                    continue;
                };
                let item = progress.item_mut(&path);
                item.explicit = true;
                item.status = status;
                if let Some(seconds) = seconds {
                    item.seconds = Some(seconds);
                }
                if let Some(total) = total {
                    item.counts.explicit_total = Some(total as u32);
                }
            }
            JournalLine::Case { path, outcome } => {
                progress.item_mut(&path).counts.record(&outcome);
            }
            JournalLine::Unknown => {}
        }
    }
    progress
}

/// Renders a duration the way [`crate::output`]'s `format_elapsed` does —
/// reused rather than re-spelled, so "how long has this been running" is
/// the identical phrase everywhere it appears.
fn elapsed(seconds: u64) -> String {
    crate::output::format_elapsed(seconds)
}

fn fraction(passed: u32, total: u32, estimated: bool) -> String {
    if estimated {
        format!("{passed}/~{total}")
    } else {
        format!("{passed}/{total}")
    }
}

/// Renders one checklist line and its children, indented two spaces per
/// depth, CommonMark task-list syntax the dashboard already understands
/// (`docs/spec/markdown-in-the-dashboard.md`).
fn render_item(item: &ProgressItem, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    let status = item.effective_status();
    let (passed, total, estimated) = item.contribution();
    let mut detail = Vec::new();
    if total > 0 {
        let noun = if item.children.is_empty() {
            ""
        } else {
            " legs"
        };
        detail.push(format!("{}{noun}", fraction(passed, total, estimated)));
    }
    if let Some(seconds) = item.seconds {
        detail.push(elapsed(seconds));
    }
    match status {
        ItemStatus::Running => detail.push("running".to_string()),
        ItemStatus::Pending if total == 0 => detail.push("pending".to_string()),
        ItemStatus::Skipped | ItemStatus::Reused | ItemStatus::Failed => {
            detail.push(status.word().to_string());
        }
        _ => {}
    }
    let suffix = if detail.is_empty() {
        String::new()
    } else {
        format!(" ({})", detail.join(", "))
    };
    out.push_str(&format!(
        "{indent}- {} {}{suffix}\n",
        status.checkbox(),
        item.label
    ));
    for child in &item.children {
        render_item(child, depth + 1, out);
    }
}

/// One story's verification state, as the publisher sees it (SH-524).
pub enum VerificationProgressView<'a> {
    /// This story is in `verifying` but is not the candidate the release
    /// gate is currently running for.
    Queued {
        /// 1-based position in the serial queue.
        position: usize,
        /// Candidates strictly ahead of it, split by why they sort first.
        ahead_higher_priority: usize,
        ahead_equal_priority_older: usize,
    },
    /// This story is the one candidate the gate is presently working on.
    Running {
        progress: &'a GateProgress,
        /// Elapsed seconds since this story last entered `verifying`.
        elapsed_seconds: Option<u64>,
        /// Elapsed seconds since the journal's last line, when known — the
        /// staleness signal a wedged run shows up as.
        seconds_since_last_event: Option<u64>,
    },
}

/// The self-identifying prefix every SH-524 progress comment starts with.
pub const GATE_PROGRESS_PREFIX: &str = "CENTRAL VERIFICATION PROGRESS —";

/// Renders one story's whole progress comment body.
#[must_use]
pub fn render(view: &VerificationProgressView<'_>, now: &str) -> String {
    let mut out = format!("{GATE_PROGRESS_PREFIX} updated {now}\n\n");
    match view {
        VerificationProgressView::Queued {
            position,
            ahead_higher_priority,
            ahead_equal_priority_older,
        } => {
            out.push_str(&format!("Verification — QUEUED (position {position})\n"));
            out.push_str(&format!(
                "Ahead of it: {ahead_higher_priority} candidate{} of higher priority, {ahead_equal_priority_older} of equal priority and older.\n",
                if *ahead_higher_priority == 1 { "" } else { "s" }
            ));
            out.push_str("- [ ] release gate — not started\n");
        }
        VerificationProgressView::Running {
            progress,
            elapsed_seconds,
            seconds_since_last_event,
        } => {
            let (passed, total, estimated) = progress
                .items
                .iter()
                .map(ProgressItem::contribution)
                .fold((0, 0, false), |(p, t, e), (cp, ct, ce)| {
                    (p + cp, t + ct, e || ce)
                });
            let mut header = format!("Verification ({}", fraction(passed, total, estimated));
            if let Some(elapsed_seconds) = elapsed_seconds {
                header.push_str(&format!(", {}", elapsed(*elapsed_seconds)));
            }
            // `Iterator::all` on an empty journal (the gate has been handed
            // the candidate but has not emitted its first line yet) is
            // vacuously true — this view is only ever built for the one
            // candidate actually running, so "no items yet" must still read
            // as running, not silently drop the word.
            let all_terminal = !progress.items.is_empty()
                && progress
                    .items
                    .iter()
                    .all(|item| item.effective_status().is_terminal());
            if !all_terminal {
                header.push_str(", running");
            }
            if let Some(stale) = seconds_since_last_event.filter(|s| *s > STALE_GATE_THRESHOLD_SECS)
            {
                header.push_str(&format!(", NO GATE OUTPUT FOR {}", elapsed(stale)));
            }
            header.push_str(")\n");
            out.push_str(&header);
            for item in &progress.items {
                render_item(item, 0, &mut out);
            }
        }
    }
    out
}

/// Below this many silent seconds, a running gate is unremarkable. Above it,
/// silence itself becomes the signal a wedged run shows up as. Three times
/// the publisher's own base one-minute publish interval (`crate::daemon::
/// verification_progress::PUBLISH_INTERVAL`) rather than a bare literal
/// (SH-394): three consecutive missed publishes is what "no gate output"
/// means, not a picked number of seconds.
pub const STALE_GATE_THRESHOLD_SECS: u64 = 180;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_item_line_sets_status_and_seconds() {
        let progress = fold(
            r#"{"kind":"item","path":"release gate/fmt","status":"passed","at":"2026-01-01T00:00:00Z","seconds":2}"#,
        );
        assert_eq!(progress.items.len(), 1);
        let root = &progress.items[0];
        assert_eq!(root.label, "release gate");
        assert_eq!(root.children.len(), 1);
        let fmt = &root.children[0];
        assert_eq!(fmt.label, "fmt");
        assert_eq!(fmt.status, ItemStatus::Passed);
        assert_eq!(fmt.seconds, Some(2));
    }

    #[test]
    fn a_case_line_tallies_pass_and_fail_without_needing_an_item_line_first() {
        let progress = fold(
            "{\"kind\":\"case\",\"path\":\"release gate/rust-suite\",\"outcome\":\"pass\"}\n\
             {\"kind\":\"case\",\"path\":\"release gate/rust-suite\",\"outcome\":\"fail\"}\n\
             {\"kind\":\"case\",\"path\":\"release gate/rust-suite\",\"outcome\":\"pass\"}\n",
        );
        let suite = &progress.items[0].children[0];
        assert_eq!(suite.counts.passed, 2);
        assert_eq!(suite.counts.failed, 1);
        assert_eq!(suite.counts.seen(), 3);
    }

    #[test]
    fn a_truncated_final_line_is_skipped_not_fatal() {
        let progress = fold(
            "{\"kind\":\"item\",\"path\":\"release gate/fmt\",\"status\":\"passed\",\"at\":\"x\"}\n\
             {\"kind\":\"item\",\"path\":\"release gate/clippy\",\"status\":\"run",
        );
        let root = &progress.items[0];
        assert_eq!(
            root.children.len(),
            1,
            "the truncated second line must not appear at all"
        );
        assert_eq!(root.children[0].label, "fmt");
    }

    #[test]
    fn an_unrecognised_kind_is_ignored_rather_than_fatal() {
        let progress = fold("{\"kind\":\"phase\",\"path\":\"whatever\"}\n");
        assert!(progress.items.is_empty());
    }

    #[test]
    fn an_unrecognised_status_word_is_ignored_rather_than_fatal() {
        // Checked before the path is created at all: a line this binary
        // cannot understand contributes nothing, the same as an
        // unrecognised `kind` -- not a Pending placeholder for a path
        // nobody has actually reported on yet (SH-372: absence states
        // nothing).
        let progress =
            fold(r#"{"kind":"item","path":"release gate/fmt","status":"exploded","at":"x"}"#);
        assert!(progress.items.is_empty());
    }

    #[test]
    fn a_leaf_with_no_cases_and_no_total_contributes_one_unit() {
        let progress =
            fold(r#"{"kind":"item","path":"release gate/fmt","status":"passed","at":"x"}"#);
        assert_eq!(progress.items[0].children[0].contribution(), (1, 1, false));
        let progress =
            fold(r#"{"kind":"item","path":"release gate/fmt","status":"running","at":"x"}"#);
        assert_eq!(progress.items[0].children[0].contribution(), (0, 1, false));
    }

    #[test]
    fn an_explicit_total_is_never_estimated_even_mid_run() {
        let progress = fold(
            "{\"kind\":\"item\",\"path\":\"release gate/plugin\",\"status\":\"running\",\"at\":\"x\",\"total\":10}\n\
             {\"kind\":\"case\",\"path\":\"release gate/plugin\",\"outcome\":\"pass\"}\n",
        );
        assert_eq!(progress.items[0].children[0].contribution(), (1, 10, false));
    }

    #[test]
    fn no_explicit_total_is_estimated_until_a_terminal_status_arrives() {
        let progress = fold(
            "{\"kind\":\"item\",\"path\":\"release gate/rust-suite\",\"status\":\"running\",\"at\":\"x\"}\n\
             {\"kind\":\"case\",\"path\":\"release gate/rust-suite\",\"outcome\":\"pass\"}\n\
             {\"kind\":\"case\",\"path\":\"release gate/rust-suite\",\"outcome\":\"pass\"}\n",
        );
        assert_eq!(progress.items[0].children[0].contribution(), (2, 2, true));
        let progress = fold(
            "{\"kind\":\"item\",\"path\":\"release gate/rust-suite\",\"status\":\"running\",\"at\":\"x\"}\n\
             {\"kind\":\"case\",\"path\":\"release gate/rust-suite\",\"outcome\":\"pass\"}\n\
             {\"kind\":\"case\",\"path\":\"release gate/rust-suite\",\"outcome\":\"pass\"}\n\
             {\"kind\":\"item\",\"path\":\"release gate/rust-suite\",\"status\":\"passed\",\"at\":\"x\",\"seconds\":9}\n",
        );
        assert_eq!(progress.items[0].children[0].contribution(), (2, 2, false));
    }

    #[test]
    fn a_parent_never_explicitly_named_derives_its_status_from_its_children() {
        let progress = fold(
            "{\"kind\":\"item\",\"path\":\"release gate/fmt\",\"status\":\"passed\",\"at\":\"x\"}\n\
             {\"kind\":\"item\",\"path\":\"release gate/clippy\",\"status\":\"running\",\"at\":\"x\"}\n",
        );
        assert_eq!(progress.items[0].effective_status(), ItemStatus::Running);
    }

    #[test]
    fn a_derived_parent_reads_failed_ahead_of_running_ahead_of_a_settling_mix() {
        let all_pending =
            fold("{\"kind\":\"item\",\"path\":\"a/x\",\"status\":\"pending\",\"at\":\"t\"}\n");
        assert_eq!(all_pending.items[0].effective_status(), ItemStatus::Pending);

        let mixed = fold(
            "{\"kind\":\"item\",\"path\":\"a/x\",\"status\":\"passed\",\"at\":\"t\"}\n\
             {\"kind\":\"item\",\"path\":\"a/y\",\"status\":\"pending\",\"at\":\"t\"}\n",
        );
        assert_eq!(mixed.items[0].effective_status(), ItemStatus::Running);

        let all_terminal = fold(
            "{\"kind\":\"item\",\"path\":\"a/x\",\"status\":\"passed\",\"at\":\"t\"}\n\
             {\"kind\":\"item\",\"path\":\"a/y\",\"status\":\"skipped\",\"at\":\"t\"}\n",
        );
        assert_eq!(all_terminal.items[0].effective_status(), ItemStatus::Passed);

        let one_failed = fold(
            "{\"kind\":\"item\",\"path\":\"a/x\",\"status\":\"passed\",\"at\":\"t\"}\n\
             {\"kind\":\"item\",\"path\":\"a/y\",\"status\":\"failed\",\"at\":\"t\"}\n\
             {\"kind\":\"item\",\"path\":\"a/z\",\"status\":\"running\",\"at\":\"t\"}\n",
        );
        assert_eq!(one_failed.items[0].effective_status(), ItemStatus::Failed);
    }

    #[test]
    fn an_explicit_status_on_a_path_with_children_wins_over_derivation() {
        // "release gate" reused wholesale (already-certified merge preflight
        // shortcut) -- explicit, with no children at all in that run.
        let progress = fold(r#"{"kind":"item","path":"release gate","status":"reused","at":"x"}"#);
        assert_eq!(progress.items[0].effective_status(), ItemStatus::Reused);
    }

    #[test]
    fn render_queued_names_position_and_what_is_ahead() {
        let view = VerificationProgressView::Queued {
            position: 3,
            ahead_higher_priority: 2,
            ahead_equal_priority_older: 0,
        };
        let body = render(&view, "2026-08-31T18:04:00Z");
        assert!(body.starts_with(GATE_PROGRESS_PREFIX));
        assert!(body.contains("QUEUED (position 3)"));
        assert!(body.contains("2 candidates of higher priority, 0 of equal priority and older"));
    }

    #[test]
    fn render_running_nests_children_as_a_markdown_task_list() {
        let progress = fold(
            "{\"kind\":\"item\",\"path\":\"release gate/fmt\",\"status\":\"passed\",\"at\":\"t\",\"seconds\":2}\n\
             {\"kind\":\"item\",\"path\":\"release gate/rust-suite\",\"status\":\"running\",\"at\":\"t\"}\n\
             {\"kind\":\"case\",\"path\":\"release gate/rust-suite\",\"outcome\":\"pass\"}\n",
        );
        let view = VerificationProgressView::Running {
            progress: &progress,
            elapsed_seconds: Some(125),
            seconds_since_last_event: Some(3),
        };
        let body = render(&view, "2026-08-31T18:04:00Z");
        assert!(body.contains("Verification ("));
        assert!(body.contains("2m 5s"));
        assert!(body.contains("- [x] fmt (1/1, 2s)\n"));
        assert!(body.contains("  - [ ] rust-suite (1/~1, running)\n"));
        assert!(
            !body.contains("NO GATE OUTPUT"),
            "3s of silence is unremarkable"
        );
    }

    #[test]
    fn a_stale_running_gate_names_its_own_silence() {
        let progress =
            fold(r#"{"kind":"item","path":"release gate/rust-suite","status":"running","at":"t"}"#);
        let view = VerificationProgressView::Running {
            progress: &progress,
            elapsed_seconds: Some(4 * 3600),
            seconds_since_last_event: Some(3 * 3600 + 51 * 60),
        };
        let body = render(&view, "2026-08-31T18:04:00Z");
        assert!(body.contains("NO GATE OUTPUT FOR 3h 51m"));
    }

    /// The running candidate's journal can be empty for a moment — handed
    /// to the gate, but `verify-pr.sh` has not emitted its first line yet.
    /// `Iterator::all` over that empty list is vacuously true, which must
    /// not read as "not running": this view is only ever built for the one
    /// candidate the gate is actually working on.
    #[test]
    fn a_running_candidate_with_no_journal_lines_yet_still_reads_as_running() {
        let progress = fold("");
        let view = VerificationProgressView::Running {
            progress: &progress,
            elapsed_seconds: Some(1),
            seconds_since_last_event: None,
        };
        let body = render(&view, "2026-08-31T18:04:00Z");
        assert!(body.contains(", running)"), "{body}");
    }
}
