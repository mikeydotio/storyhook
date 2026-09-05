//! Pins `story.sh`'s dispatch-readiness poll budget to this project's own
//! documented daemon-latency tolerance (SH-544).
//!
//! `plugins/story/bin/story.sh`'s `READY_ATTEMPTS`/`READY_DELAY` bound how
//! long both `wait_ready` (Codex's screen-scrape gate) and
//! `wait_ready_sentinel` (Claude's sentinel-file gate,
//! `plugins/story/lib/session.sh`) will poll before refusing a dispatch as
//! `pane-not-ready`. That budget used to be a bare 15s (`60 * 0.25`) with
//! nothing tying it to anything — well under
//! [`storyhook::daemon::lifecycle::SPAWN_LOCK_DEADLINE`], this project's own
//! documented tolerance for ordinary daemon contention, even though
//! `wait_ready_sentinel` polls for a file a daemon request has to complete to
//! produce. A dispatch whose SessionStart request got queued behind ordinary
//! contention on the daemon's own bounded worker pool could time out with
//! `no-sentinel`, force-remove a still-live worktree and roll back the claim,
//! even though nothing about the launched agent was wrong.
//!
//! The fix derives the budget from `SPAWN_LOCK_DEADLINE` plus a stated
//! margin rather than hand-copying a second number — this repository has
//! already been bitten by exactly that drift shape (SH-136,
//! `tests/dashboard_mutation_deadline.rs`'s own header). This test is the
//! catch: it fails if `story.sh`'s own two literals move without the
//! derivation moving with them.

use std::path::PathBuf;

/// The repository root, which is this package's manifest directory (see
/// `tests/dashboard_mutation_deadline.rs`'s identical helper).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    let path: PathBuf = repo_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

/// The margin over `SPAWN_LOCK_DEADLINE`, in seconds: tmux round trips (a
/// handful of `tmux display-message`/`capture-pane` calls per poll) and the
/// time Claude Code itself takes to reach its first render, both of which run
/// *before* a SessionStart hook's own request ever reaches the daemon. Stated
/// here, once, rather than left for `story.sh`'s own comment alone to assert.
const MARGIN_SECS: u64 = 15;

/// Parses a `NAME="${VAR:-default}"` shell assignment's own default value out
/// of `plugins/story/bin/story.sh`'s source text. Plain string parsing rather
/// than a shell parser dependency: the declaration is one fixed shape this
/// project controls, not untrusted input (matching
/// `dashboard_mutation_timeout_ms`'s own precedent in
/// `tests/dashboard_mutation_deadline.rs`).
fn shell_default(script: &str, marker: &str) -> String {
    let after = script
        .split(marker)
        .nth(1)
        .unwrap_or_else(|| panic!("plugins/story/bin/story.sh must declare `{marker}<default>}}`"));
    after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect()
}

#[test]
fn the_dispatch_readiness_poll_budget_is_derived_from_spawn_lock_deadline_not_hand_copied() {
    let script = read("plugins/story/bin/story.sh");

    let attempts: u64 = shell_default(&script, "READY_ATTEMPTS=\"${STORY_READY_ATTEMPTS:-")
        .parse()
        .expect("READY_ATTEMPTS's default must parse as a whole number of polls");
    let delay: f64 = shell_default(&script, "READY_DELAY=\"${STORY_READY_DELAY:-")
        .parse()
        .expect("READY_DELAY's default must parse as a number of seconds");

    let actual_secs = (attempts as f64) * delay;
    let expected_secs =
        (storyhook::daemon::lifecycle::SPAWN_LOCK_DEADLINE.as_secs() + MARGIN_SECS) as f64;

    assert!(
        (actual_secs - expected_secs).abs() < 0.001,
        "story.sh's READY_ATTEMPTS ({attempts}) * READY_DELAY ({delay}) = {actual_secs}s has \
         drifted from SPAWN_LOCK_DEADLINE + {MARGIN_SECS}s margin ({expected_secs}s) — raising \
         (or lowering) SPAWN_LOCK_DEADLINE without moving story.sh's own poll budget to match \
         reopens the SH-544 no-sentinel-under-contention race this test exists to catch."
    );
}
