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

use std::{path::PathBuf, process::Command};

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

/// Runs the actual declarations without dispatching or starting any services.
fn readiness(ostype: &str, attempts: Option<&str>, delay: Option<&str>) -> (u64, f64) {
    let script = read("plugins/story/bin/story.sh");
    let declarations = script
        .split_once("# Readiness budget defaults (SH-820).")
        .expect("the readiness declaration boundary")
        .1
        .split_once("READY_FALLBACK_DELAY=")
        .expect("the end of the poll declarations")
        .0;
    let mut command = Command::new("bash");
    command
        .args(["-c", &format!("OSTYPE=\"$1\"\n{declarations}\nprintf '%s %s\\n' \"$READY_ATTEMPTS\" \"$READY_DELAY\""), "readiness-fixture", ostype])
        .env_remove("STORY_READY_ATTEMPTS")
        .env_remove("STORY_READY_DELAY");
    if let Some(value) = attempts {
        command.env("STORY_READY_ATTEMPTS", value);
    }
    if let Some(value) = delay {
        command.env("STORY_READY_DELAY", value);
    }
    let result = command.output().expect("evaluating readiness declarations");
    assert!(result.status.success(), "{result:?}");
    let output = String::from_utf8(result.stdout).unwrap();
    let fields: Vec<&str> = output.split_whitespace().collect();
    assert_eq!(fields.len(), 2, "{output}");
    (fields[0].parse().unwrap(), fields[1].parse().unwrap())
}

#[test]
fn the_dispatch_readiness_poll_budget_is_derived_from_spawn_lock_deadline_not_hand_copied() {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux-gnu",
        other => other,
    };
    let (attempts, delay) = readiness(platform, None, None);

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

#[test]
fn platform_defaults_and_explicit_readiness_overrides_are_preserved() {
    for (platform, lock_secs) in [("darwin25", 115), ("linux-gnu", 70), ("freebsd", 30)] {
        let (attempts, delay) = readiness(platform, None, None);
        assert_eq!(delay, 0.25);
        assert_eq!(attempts as f64 * delay, (lock_secs + MARGIN_SECS) as f64);
        assert_eq!(readiness(platform, Some("2"), Some("0.1")), (2, 0.1));
        assert_eq!(readiness(platform, Some(""), Some("")), (attempts, delay));
    }
}
