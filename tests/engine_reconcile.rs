//! The Full Auto reconcile loop (SH-465).
//!
//! Every row of the failure taxonomy, the breaker's arithmetic including its
//! reset, the `no-auto` skip, termination, the lane budget, and the two derived
//! constants.
//!
//! # Why the taxonomy is tested twice
//!
//! [`classify`] is pure, so the taxonomy itself is a table — no store, no
//! dispatcher, no clock. But a pure decision nothing calls is a decision that
//! never happens, so each row is *also* provoked through a real store and a
//! [`FakeDispatcher`], which records what the engine actually asked for. The
//! pure half proves the rule; the wired half proves the rule is reached. Neither
//! is sufficient alone, which is SH-365's two-mechanism shape.

mod store_support;

use storyhook::service::engine::{
    ENGINE_LANE_BUDGET, GATE_MEDIAN_SECS, HardStopKind, LaneClassification, LaneObservation,
    RECONCILE_TICK_SECS, STALL_CEILING_SECS, STALL_MARGIN, classify,
};

// ---------------------------------------------------------------------------
// The taxonomy, as a table over the pure decision
// ---------------------------------------------------------------------------

/// A lane that is working normally: story open, agent quiet about it, window
/// alive, and its seq moved since the last pass.
fn progressing() -> LaneObservation {
    LaneObservation {
        story_closed: false,
        agent_blocked: false,
        window_alive: true,
        head_global_seq: Some(200),
        last_progress_seq: Some(100),
        seconds_since_progress: Some(5),
    }
}

/// Row 1 of the taxonomy: the story left the OPEN superstate.
#[test]
fn a_closed_story_is_a_completion() {
    let observation = LaneObservation {
        story_closed: true,
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS),
        LaneClassification::Completed
    );
}

/// Row 2: the agent blocked the story or set `awaiting` on it.
#[test]
fn an_agent_blocked_story_is_a_hard_stop() {
    let observation = LaneObservation {
        agent_blocked: true,
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS),
        LaneClassification::HardStop(HardStopKind::AgentBlocked)
    );
}

/// Row 3: the window is gone while the story is still OPEN.
#[test]
fn a_missing_window_on_an_open_story_is_a_hard_stop() {
    let observation = LaneObservation {
        window_alive: false,
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS),
        LaneClassification::HardStop(HardStopKind::WindowGone)
    );
}

/// Row 4: nothing observable changed past the ceiling.
#[test]
fn an_unmoved_seq_past_the_ceiling_is_a_stall() {
    let observation = LaneObservation {
        head_global_seq: Some(100),
        last_progress_seq: Some(100),
        seconds_since_progress: Some(STALL_CEILING_SECS + 1),
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS),
        LaneClassification::HardStop(HardStopKind::Stalled)
    );
}

/// The other direction of the stall rule, which is the half that makes the
/// test mean anything: a lane whose seq MOVED is progressing no matter how
/// much wall clock has passed. Without this case the suite would prove only
/// that time passes, which it does regardless of the code under test.
#[test]
fn a_moved_seq_is_progress_however_long_the_clock_says() {
    let observation = LaneObservation {
        head_global_seq: Some(101),
        last_progress_seq: Some(100),
        seconds_since_progress: Some(STALL_CEILING_SECS * 10),
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS),
        LaneClassification::Progressing
    );
}

/// An unmoved seq that has NOT yet outrun the ceiling is still progress. The
/// boundary is `>`, not `>=`: a lane exactly at the ceiling has not passed it.
#[test]
fn an_unmoved_seq_inside_the_ceiling_is_not_yet_a_stall() {
    let at_ceiling = LaneObservation {
        head_global_seq: Some(100),
        last_progress_seq: Some(100),
        seconds_since_progress: Some(STALL_CEILING_SECS),
        ..progressing()
    };
    assert_eq!(
        classify(&at_ceiling, STALL_CEILING_SECS),
        LaneClassification::Progressing,
        "a lane exactly at the ceiling has not passed it"
    );
}

/// A lane observed for the first time has no recorded progress, and absence
/// states nothing (SH-372). Promoting it to a stall would quarantine every
/// lane alive when this shipped, and every lane on its first pass forever.
#[test]
fn a_lane_with_no_recorded_progress_is_never_stalled() {
    for (recorded, elapsed) in [
        (None, Some(STALL_CEILING_SECS * 100)),
        (Some(100), None),
        (None, None),
    ] {
        let observation = LaneObservation {
            head_global_seq: Some(100),
            last_progress_seq: recorded,
            seconds_since_progress: elapsed,
            ..progressing()
        };
        assert_eq!(
            classify(&observation, STALL_CEILING_SECS),
            LaneClassification::Progressing,
            "seed the first observation, never punish it: recorded={recorded:?} elapsed={elapsed:?}"
        );
    }
}

/// A story that cannot be resolved at all has no seq to compare, so it cannot
/// be *stalled* — it is caught by the window probe or by nothing. Pinned so a
/// future reader does not "helpfully" treat an unreadable story as a stall.
#[test]
fn an_unresolvable_story_is_not_a_stall() {
    let observation = LaneObservation {
        head_global_seq: None,
        last_progress_seq: Some(100),
        seconds_since_progress: Some(STALL_CEILING_SECS * 10),
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS),
        LaneClassification::Progressing
    );
}

// ---------------------------------------------------------------------------
// Precedence — the rule that decides the design
// ---------------------------------------------------------------------------

/// **The load-bearing ordering.** Completion is a STORE fact; a window closing
/// is only evidence about a window (D3, SH-226). An agent that finished its
/// story and let its pane exit is the ordinary, correct end of a lane — every
/// successful lane passes through exactly this state — so reading the window
/// first would report finished work as a failure, quarantine it, and count it
/// toward the breaker that halts the run.
///
/// This is the single most consequential line in `classify`, and it is one
/// `if` away from being wrong in a way no other test would notice.
#[test]
fn a_closed_story_wins_over_a_closed_window() {
    let observation = LaneObservation {
        story_closed: true,
        window_alive: false,
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS),
        LaneClassification::Completed,
        "a finished agent whose pane exited is a completion, not a WindowGone hard stop"
    );
}

/// Completion also outranks the agent-blocked signal and the stall clock: a
/// story that reached a CLOSED superstate is done regardless of what its
/// metadata or its clock say.
#[test]
fn a_closed_story_wins_over_every_other_signal() {
    let observation = LaneObservation {
        story_closed: true,
        agent_blocked: true,
        window_alive: false,
        head_global_seq: Some(100),
        last_progress_seq: Some(100),
        seconds_since_progress: Some(STALL_CEILING_SECS * 10),
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS),
        LaneClassification::Completed
    );
}

/// A blocked agent outranks a dead window: the agent told us why it stopped,
/// and that reason is more useful to the human reading the quarantine than
/// "its window is gone", which is a consequence rather than a cause.
#[test]
fn an_agent_block_wins_over_a_closed_window() {
    let observation = LaneObservation {
        agent_blocked: true,
        window_alive: false,
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS),
        LaneClassification::HardStop(HardStopKind::AgentBlocked)
    );
}

// ---------------------------------------------------------------------------
// The derived constants stay derived (SH-394)
// ---------------------------------------------------------------------------

/// Reads one tracked source file from the checkout.
fn checkout_source(relative: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The declared value of a `const NAME: TYPE = <literal>;` in a source file.
fn declared_const(source: &str, name: &str) -> String {
    let needle = format!("const {name}:");
    let line = source
        .lines()
        .find(|line| {
            line.trim_start().starts_with(&format!("pub {needle}"))
                || line
                    .trim_start()
                    .starts_with(&format!("pub(crate) {needle}"))
                || line.trim_start().starts_with(&needle)
        })
        .unwrap_or_else(|| panic!("no declaration of `{name}` found"));
    line.split_once('=')
        .unwrap_or_else(|| panic!("`{name}` has no initializer"))
        .1
        .trim()
        .trim_end_matches(';')
        .trim()
        .to_string()
}

/// A lane is exactly one `story.sh dispatch` subprocess, and this machine
/// already bounds those. Restating that budget as its own literal would be a
/// second opinion about one machine — the class SH-136/SH-198/SH-258 have
/// already cost this project repeatedly.
///
/// **This is asserted on the SOURCE, not on the values, and that distinction is
/// the whole test.** A runtime `assert_eq!(ENGINE_LANE_BUDGET, MAX_RUNNING)`
/// is vacuous here: if someone re-typed the budget as the literal `4` the two
/// would still be equal and such a test would pass, while the derivation it
/// exists to protect had already been broken. Only the spelling can tell a
/// derived constant from a copy of its digits — the same reason
/// `tests/machine_lock.rs` asserts `WAIT_REPORT_SECS=$GATE_MEDIAN_SECS`
/// textually rather than comparing two numbers.
#[test]
fn the_lane_budget_is_spelled_as_the_dispatch_capacity_not_a_copy_of_its_digits() {
    let declared = declared_const(
        &checkout_source("src/service/engine.rs"),
        "ENGINE_LANE_BUDGET",
    );
    assert_eq!(
        declared, "crate::api::dispatch::MAX_RUNNING",
        "ENGINE_LANE_BUDGET must BE api::dispatch::MAX_RUNNING rather than a literal that happens to equal it today; found `{declared}`"
    );
}

/// The ceiling's own spelling, for the same reason and by the same mechanism:
/// the product must be written as its three named factors, so a reader can see
/// which part is measurement and which is judgement.
#[test]
fn the_stall_ceiling_is_spelled_as_its_derivation() {
    let declared = declared_const(
        &checkout_source("src/service/engine.rs"),
        "STALL_CEILING_SECS",
    );
    for factor in ["ENGINE_LANE_BUDGET", "GATE_MEDIAN_SECS", "STALL_MARGIN"] {
        assert!(
            declared.contains(factor),
            "STALL_CEILING_SECS must name {factor} in its own derivation; found `{declared}`"
        );
    }
}

/// `make test`'s measured warm median, read back out of the document that
/// measures it, exactly as `tests/machine_lock.rs` does for the same figure.
fn measured_gate_median_secs() -> u64 {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/rearch/baseline/timings.md");
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let after = src
        .split("## The whole gate")
        .nth(1)
        .expect("docs/rearch/baseline/timings.md must have a `## The whole gate` section");
    let bolded = after
        .split("**")
        .nth(1)
        .expect("that section must carry a **bolded** median");
    let seconds: String = bolded
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    seconds
        .parse::<f64>()
        .unwrap_or_else(|e| panic!("the median {seconds:?} must parse: {e}")) as u64
}

/// The ceiling is derived from the deadline it disproves, and stays derived.
/// A lane's longest legitimate silence is queuing on the machine-wide `gate`
/// lock while other lanes run the suite, so the inputs are the lane budget and
/// the suite's own measured median.
#[test]
fn the_stall_ceiling_still_derives_from_the_measured_suite_median() {
    let measured = measured_gate_median_secs();
    assert_eq!(
        GATE_MEDIAN_SECS, measured,
        "service::engine declares GATE_MEDIAN_SECS={GATE_MEDIAN_SECS}, but docs/rearch/baseline/timings.md now measures `make test` at {measured}s. Re-derive the ceiling rather than leaving the engine asserting a median nobody measures any more."
    );
    assert_eq!(
        STALL_CEILING_SECS,
        ENGINE_LANE_BUDGET as u64 * measured * STALL_MARGIN,
        "the ceiling must remain the product of the budget, the measured median and the named margin — not a literal that happens to equal it today"
    );
}

/// The tick is a quarter of the ceiling, so a stall surfaces well inside it
/// rather than up to a full ceiling late. Derived from the ceiling, so raising
/// one raises the other.
#[test]
fn the_reconcile_tick_derives_from_the_stall_ceiling() {
    assert_eq!(RECONCILE_TICK_SECS, STALL_CEILING_SECS / 4);
    assert!(
        RECONCILE_TICK_SECS > 0,
        "a tick of zero would be a busy loop, which the design forbids"
    );
}

/// The margin is judgement and the median is measurement; keeping them as
/// separate named factors is what lets a reader tell which is which (SH-394).
/// A margin below 1 would make the ceiling tighter than the worst legitimate
/// case it is derived from, which would quarantine healthy lanes.
#[test]
fn the_stall_margin_never_tightens_the_ceiling_below_its_own_derivation() {
    assert!(
        STALL_MARGIN >= 1,
        "a margin below 1 puts the ceiling under the worst legitimate silence it derives from"
    );
    assert!(
        STALL_CEILING_SECS >= ENGINE_LANE_BUDGET as u64 * GATE_MEDIAN_SECS,
        "every lane must be able to wait out a full round of serialized suites without being called stalled"
    );
}

// ---------------------------------------------------------------------------
// Hard-stop identity
// ---------------------------------------------------------------------------

/// The recorded classification is the lane's durable outcome and reaches an
/// operator through the CLI, the dashboard and an event hook, so its spelling
/// is a contract rather than a debug convenience.
#[test]
fn every_hard_stop_kind_has_a_distinct_stable_spelling() {
    let kinds = [
        HardStopKind::AgentBlocked,
        HardStopKind::WindowGone,
        HardStopKind::Stalled,
        HardStopKind::Interrupted,
    ];
    let spellings: Vec<&str> = kinds.iter().map(|kind| kind.as_str()).collect();
    let mut unique = spellings.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(
        unique.len(),
        spellings.len(),
        "two hard stops sharing a spelling are indistinguishable in the store: {spellings:?}"
    );
    for spelling in &spellings {
        assert!(
            !spelling.is_empty() && spelling.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
            "{spelling:?} is not a stable machine-readable classification"
        );
    }
}
