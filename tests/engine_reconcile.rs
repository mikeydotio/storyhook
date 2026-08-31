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
    BREAKER_TRIPPED, COMPLETED, DispatchOutcome, ENGINE_LANE_BUDGET, EngineService,
    GATE_MEDIAN_SECS, HardStopKind, LaneClassification, LaneObservation, QUEUE_DRAINED,
    RECONCILE_TICK_SECS, STALL_CEILING_SECS, STALL_MARGIN, StartRequest, classify,
};
use storyhook::service::{Clock, Ctx, NewStoryInput, StoryService};
use storyhook::store::{
    EngineAgent, EngineLaneRecord, EngineLaneState, EngineRunState, EngineScope, ReadOps, Store,
    WriteOps,
};
use storyhook_test_support::{
    DispatcherCall, DispatcherStep, FIXTURE_NOW, FakeDispatcher, ServiceFixture,
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
    // That the tick is non-zero is asserted at COMPILE time beside the
    // constant itself (`const _: () = assert!(...)`), because a runtime
    // assertion over a `const` folds away and proves nothing.
}

/// The margin is judgement and the median is measurement; keeping them as
/// separate named factors is what lets a reader tell which is which (SH-394).
/// A margin below 1 would make the ceiling tighter than the worst legitimate
/// case it is derived from, which would quarantine healthy lanes.
#[test]
fn the_stall_margin_never_tightens_the_ceiling_below_its_own_derivation() {
    // The margin's own floor is a compile-time assertion beside the constant.
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

// ---------------------------------------------------------------------------
// The wired half: the taxonomy through a real store and FakeDispatcher
// ---------------------------------------------------------------------------
//
// The table above proves the RULE. These prove the rule is REACHED — that
// `reconcile` reads the right facts, applies the right transition, and asks
// the dispatcher for exactly what it should. Neither half is sufficient alone
// (SH-365): a pure decision nothing calls is a decision that never happens.

/// A run with `lanes` lanes and no stories claimable, so `fill` finds nothing
/// unless the test seeded something.
fn started_run(fixture: &ServiceFixture, fake: &FakeDispatcher, lanes: u32) -> String {
    let ctx = fixture.ctx();
    EngineService::new(&ctx, fake)
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes,
            agent: EngineAgent::Codex,
        })
        .unwrap()
        .id
}

/// Puts `story` into lane `index` as if a dispatch had succeeded.
fn occupy(fixture: &ServiceFixture, run_id: &str, index: u32, story: &str) {
    let mut lane = lane_at(fixture, run_id, index);
    lane.state = EngineLaneState::Working;
    lane.story_id = Some(story.to_string());
    lane.window_name = Some(format!("story-{story}"));
    lane.worktree_path = Some(format!("/tmp/wt/{story}"));
    lane.dispatched_at = Some(FIXTURE_NOW.to_string());
    lane.last_observed_at = FIXTURE_NOW.to_string();
    lane.last_progress_seq = None;
    lane.last_progress_at = None;
    fixture
        .store()
        .write(|tx| tx.put_engine_lane(&lane))
        .unwrap();
}

fn lane_at(fixture: &ServiceFixture, run_id: &str, index: u32) -> EngineLaneRecord {
    fixture
        .store()
        .read(|tx| tx.engine_lanes(run_id))
        .unwrap()
        .into_iter()
        .find(|lane| lane.lane_index == index)
        .unwrap()
}

fn run_state(fixture: &ServiceFixture, run_id: &str) -> EngineRunState {
    fixture
        .store()
        .read(|tx| tx.engine_run(run_id))
        .unwrap()
        .unwrap()
        .state
}

fn new_story(fixture: &ServiceFixture, title: &str, labels: &[&str]) -> String {
    StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: title.into(),
            labels: Some(labels.iter().map(|l| (*l).to_string()).collect()),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id
}

/// A `reconcile` whose clock reads `now`, so a stall can be provoked without
/// sleeping. The fixture's own clock is fixed at `FIXTURE_NOW`.
fn reconcile_at(
    fixture: &ServiceFixture,
    fake: &FakeDispatcher,
    run_id: &str,
    now: &str,
) -> storyhook::service::engine::ReconcileReport {
    let ctx = Ctx::new(
        fixture.store(),
        fixture.project(),
        fixture.cwd(),
        fixture.env().clone(),
    )
    .clock(Clock::Fixed(now.to_string()));
    EngineService::new(&ctx, fake)
        .reconcile(&run_id.to_string())
        .unwrap()
}

/// Row 1, wired: the story closed, so the lane frees and the run drains.
#[test]
fn a_completed_story_frees_its_lane_and_drains_the_run() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "story-SH-1".into(),
        alive: true,
    }]);
    let story = new_story(&fixture, "lane work", &[]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);
    StoryService::new(&fixture.ctx())
        .set_state(&story, "done", None, None, None)
        .unwrap();

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.completed, [0]);
    assert!(report.quarantined.is_empty());
    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(lane.state, EngineLaneState::Idle);
    assert_eq!(lane.story_id, None, "an idle lane holds no story");
    assert_eq!(lane.outcome.as_deref(), Some(COMPLETED));
    assert_eq!(
        run_state(&fixture, &run_id),
        EngineRunState::Finished,
        "nothing claimable and every lane idle ends the run"
    );
    assert_eq!(report.stop_reason.as_deref(), Some(QUEUE_DRAINED));
}

/// Row 2, wired: the agent set `awaiting`, so the lane is quarantined and its
/// evidence preserved.
#[test]
fn an_agent_blocked_story_quarantines_the_lane_and_preserves_its_evidence() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "story-SH-1".into(),
        alive: true,
    }]);
    let story = new_story(&fixture, "lane work", &[]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);
    StoryService::new(&fixture.ctx())
        .set_awaiting(&story, "the agent stopped and said why")
        .unwrap();

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.quarantined, [(0, HardStopKind::AgentBlocked)]);
    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(lane.state, EngineLaneState::Quarantined);
    assert_eq!(lane.outcome.as_deref(), Some("agent-blocked"));
    assert_eq!(
        lane.worktree_path.as_deref(),
        Some(format!("/tmp/wt/{story}").as_str()),
        "D11 preserves a stopped agent's worktree for a human to look at"
    );
    assert!(
        lane.window_name.is_some(),
        "the window name is diagnostic evidence and must survive quarantine"
    );
}

/// Row 3, wired: the window is gone while the story is still OPEN.
#[test]
fn a_dead_window_on_an_open_story_quarantines_and_names_itself() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "story-SH-1".into(),
        alive: false,
    }]);
    let story = new_story(&fixture, "lane work", &[]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.quarantined, [(0, HardStopKind::WindowGone)]);
    let awaiting = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), storyhook::store::ids::StoryNo::new(1)))
        .unwrap()
        .unwrap()
        .awaiting;
    let awaiting = awaiting.expect("a quarantined story carries a reason");
    assert!(
        awaiting.contains("window-gone") && awaiting.contains(&run_id),
        "the reason names the kind and the run so a human can act on it: {awaiting}"
    );
}

/// The precedence case, wired end to end: a finished agent whose pane exited
/// is a COMPLETION, and must never be counted as a hard stop against the
/// breaker that halts the run.
#[test]
fn a_closed_story_with_a_dead_window_completes_rather_than_quarantining() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "story-SH-1".into(),
        alive: false,
    }]);
    let story = new_story(&fixture, "lane work", &[]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);
    StoryService::new(&fixture.ctx())
        .set_state(&story, "done", None, None, None)
        .unwrap();

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.completed, [0]);
    assert!(
        report.quarantined.is_empty(),
        "an agent that finished and let its pane exit is the ordinary end of a lane"
    );
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.engine_run(&run_id))
            .unwrap()
            .unwrap()
            .consecutive_hard_stops,
        0
    );
}

// ---------------------------------------------------------------------------
// The breaker
// ---------------------------------------------------------------------------

/// Three consecutive hard stops halt the run (D10), and a halted pass claims
/// nothing — the fake would panic on an unscripted `dispatch` if it tried.
#[test]
fn three_consecutive_hard_stops_halt_the_run_and_stop_it_claiming() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowAlive {
            window: "story-SH-1".into(),
            alive: false,
        },
        DispatcherStep::WindowAlive {
            window: "story-SH-2".into(),
            alive: false,
        },
        DispatcherStep::WindowAlive {
            window: "story-SH-3".into(),
            alive: false,
        },
    ]);
    let a = new_story(&fixture, "a", &[]);
    let b = new_story(&fixture, "b", &[]);
    let c = new_story(&fixture, "c", &[]);
    let run_id = started_run(&fixture, &fake, 3);
    occupy(&fixture, &run_id, 0, &a);
    occupy(&fixture, &run_id, 1, &b);
    occupy(&fixture, &run_id, 2, &c);

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.quarantined.len(), 3);
    assert_eq!(report.run_state, EngineRunState::Halted);
    assert_eq!(report.stop_reason.as_deref(), Some(BREAKER_TRIPPED));
    assert!(
        report.filled.is_empty(),
        "a halted run claims nothing; the fake has no dispatch step and would panic if it tried"
    );
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.engine_run(&run_id))
            .unwrap()
            .unwrap()
            .acknowledged_at,
        None,
        "a fresh halt is unacknowledged, so D13's banner has something to raise"
    );
}

/// Two hard stops do not halt, and the run keeps going.
#[test]
fn two_hard_stops_leave_the_run_running() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowAlive {
            window: "story-SH-1".into(),
            alive: false,
        },
        DispatcherStep::WindowAlive {
            window: "story-SH-2".into(),
            alive: false,
        },
    ]);
    let a = new_story(&fixture, "a", &[]);
    let b = new_story(&fixture, "b", &[]);
    let run_id = started_run(&fixture, &fake, 2);
    occupy(&fixture, &run_id, 0, &a);
    occupy(&fixture, &run_id, 1, &b);

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.quarantined.len(), 2);
    assert_eq!(report.run_state, EngineRunState::Running);
    assert_eq!(report.stop_reason, None);
}

/// **The reset is the half that makes the breaker a breaker rather than a
/// counter**, and it needs a streak that already EXISTS to be reset.
///
/// An earlier version of this test ran one pass with a hard stop and a
/// completion and asserted the streak read 1. That passes with the reset
/// deleted, because the streak was already 0 — the assertion agreed with the
/// code for the wrong reason, and a mutation caught it where review had not
/// (the SH-364 shape). So: build a real streak of two, then complete
/// something, and demand it actually drops.
#[test]
fn a_completion_zeroes_an_existing_streak_so_the_breaker_never_trips() {
    let fixture = ServiceFixture::new();
    let a = new_story(&fixture, "a", &[]);
    let b = new_story(&fixture, "b", &[]);
    let fake = FakeDispatcher::new([
        // Pass 1: two dead windows -> streak 2, one short of the breaker.
        DispatcherStep::WindowAlive {
            window: format!("story-{a}"),
            alive: false,
        },
        DispatcherStep::WindowAlive {
            window: format!("story-{b}"),
            alive: false,
        },
    ]);
    let run_id = started_run(&fixture, &fake, 2);
    occupy(&fixture, &run_id, 0, &a);
    occupy(&fixture, &run_id, 1, &b);

    let first = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);
    assert_eq!(first.quarantined.len(), 2);
    assert_eq!(
        streak(&fixture, &run_id),
        2,
        "two hard stops, one short of halting"
    );
    assert_eq!(first.run_state, EngineRunState::Running);

    // Pass 2: a completion on a freshly occupied lane. Without the reset the
    // streak would stay at 2 and the very next hard stop would halt the run.
    let c = new_story(&fixture, "c", &[]);
    let fake2 = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: format!("story-{c}"),
        alive: true,
    }]);
    occupy(&fixture, &run_id, 0, &c);
    StoryService::new(&fixture.ctx())
        .set_state(&c, "done", None, None, None)
        .unwrap();

    let second = reconcile_at(&fixture, &fake2, &run_id, FIXTURE_NOW);

    assert_eq!(second.completed, [0]);
    assert_eq!(
        streak(&fixture, &run_id),
        0,
        "a completion must zero an EXISTING streak, not merely fail to add to it"
    );
    assert_eq!(second.run_state, EngineRunState::Running);
}

/// The run's current consecutive-hard-stop count.
fn streak(fixture: &ServiceFixture, run_id: &str) -> u32 {
    fixture
        .store()
        .read(|tx| tx.engine_run(run_id))
        .unwrap()
        .unwrap()
        .consecutive_hard_stops
}

// ---------------------------------------------------------------------------
// The reserved label
// ---------------------------------------------------------------------------

/// D12: `no-auto` is still returned by `story next` and still claimable by
/// hand, but the engine never dispatches it.
///
/// Asserted through `FakeDispatcher::calls()`, which records exactly what the
/// engine asked for — a fill that skipped the story is indistinguishable from
/// one that never ran unless you look at the calls.
#[test]
fn a_no_auto_story_is_never_dispatched_though_story_next_still_returns_it() {
    let fixture = ServiceFixture::new();
    let parked = new_story(&fixture, "human work", &["no-auto"]);
    let fake = FakeDispatcher::default();
    let run_id = started_run(&fixture, &fake, 1);

    // `story next` still offers it — the label filters the ENGINE, not the queue.
    let offered = fixture
        .store()
        .read(|tx| {
            Ok(
                storyhook::service::QueryService::new(tx, fixture.project(), FIXTURE_NOW)
                    .next_filtered(5, storyhook::service::ReadyQueueFilters::default())
                    .expect("the ready queue must answer"),
            )
        })
        .unwrap();
    assert!(
        offered.iter().any(|c| c.story.id == parked),
        "a no-auto story stays in the ready queue for a human"
    );

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert!(report.filled.is_empty(), "the engine claimed nothing");
    assert!(
        !fake
            .calls()
            .iter()
            .any(|c| matches!(c, DispatcherCall::Dispatch(_))),
        "the engine must never dispatch a no-auto story: {:?}",
        fake.calls()
    );
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.story(fixture.project(), storyhook::store::ids::StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .state,
        "todo",
        "and must not have claimed it either"
    );
}

// ---------------------------------------------------------------------------
// Fill, stall, and the budget
// ---------------------------------------------------------------------------

/// A ready story is claimed and dispatched, and the lane records the window
/// and worktree the helper reported.
#[test]
fn an_idle_lane_claims_and_dispatches_a_ready_story() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "claim me", &[]);
    let fake = FakeDispatcher::new([DispatcherStep::Dispatch(DispatchOutcome::from_payload(
        serde_json::json!({
            "ok": true,
            "window_name": "SH-1",
            "worktree_path": "/tmp/wt/SH-1"
        }),
    ))]);
    let run_id = started_run(&fixture, &fake, 1);

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.filled, [(0, story.clone())]);
    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(lane.state, EngineLaneState::Working);
    assert_eq!(lane.story_id.as_deref(), Some(story.as_str()));
    assert_eq!(lane.window_name.as_deref(), Some("SH-1"));
    assert_eq!(lane.worktree_path.as_deref(), Some("/tmp/wt/SH-1"));
    assert_eq!(
        run_state(&fixture, &run_id),
        EngineRunState::Running,
        "a run that just filled a lane has not drained"
    );
    // The claim really happened: the story left the neutral state.
    let state = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), storyhook::store::ids::StoryNo::new(1)))
        .unwrap()
        .unwrap()
        .state;
    assert_eq!(state, "in-progress");
}

/// The stall row, wired: the seq has not moved and the ceiling has passed.
///
/// Provoked by moving the CLOCK rather than by sleeping — a test that slept
/// `STALL_CEILING_SECS` would take minutes and would be a wall-clock
/// assertion of exactly the kind SH-394 forbids.
#[test]
fn a_lane_whose_story_has_not_moved_past_the_ceiling_is_quarantined_as_stalled() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "quiet lane", &[]);
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowAlive {
            window: format!("story-{story}"),
            alive: true,
        },
        DispatcherStep::WindowAlive {
            window: format!("story-{story}"),
            alive: true,
        },
    ]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);

    // Pass 1 seeds the progress mark — absence states nothing, so the first
    // observation can never be a stall however old the lane looks.
    let seeded = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);
    assert!(
        seeded.quarantined.is_empty(),
        "the first observation seeds, never punishes"
    );
    let lane = lane_at(&fixture, &run_id, 0);
    assert!(lane.last_progress_seq.is_some(), "the seq mark is recorded");
    assert_eq!(lane.last_progress_at.as_deref(), Some(FIXTURE_NOW));

    // Pass 2, past the ceiling, with the story untouched in between.
    let past = "2026-01-02T00:00:00Z"; // a full day, far beyond the ceiling
    let report = reconcile_at(&fixture, &fake, &run_id, past);

    assert_eq!(report.quarantined, [(0, HardStopKind::Stalled)]);
    assert_eq!(
        lane_at(&fixture, &run_id, 0).outcome.as_deref(),
        Some("stalled")
    );
}

/// The other direction, wired: the same elapsed clock, but the story MOVED.
/// Without this the stall test would prove only that time passes.
#[test]
fn a_lane_whose_story_moved_is_not_stalled_however_long_the_clock_says() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "busy lane", &[]);
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowAlive {
            window: format!("story-{story}"),
            alive: true,
        },
        DispatcherStep::WindowAlive {
            window: format!("story-{story}"),
            alive: true,
        },
    ]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);

    reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);
    let seeded = lane_at(&fixture, &run_id, 0).last_progress_seq;

    // The agent does something: any write moves the story's global seq.
    StoryService::new(&fixture.ctx())
        .comment(&story, "still working")
        .unwrap();

    let past = "2026-01-02T00:00:00Z";
    let report = reconcile_at(&fixture, &fake, &run_id, past);

    assert!(
        report.quarantined.is_empty(),
        "a story that moved is progressing no matter how much wall clock passed"
    );
    let lane = lane_at(&fixture, &run_id, 0);
    assert_ne!(
        lane.last_progress_seq, seeded,
        "the mark advanced with the story"
    );
    assert_eq!(
        lane.last_progress_at.as_deref(),
        Some(past),
        "and the stall clock restarts from the CHANGE, not from the observation"
    );
}

/// D14: total lanes filled across a pass are bounded by the machine budget,
/// even when more lanes sit idle and more stories are claimable.
#[test]
fn fill_stops_at_the_machine_lane_budget() {
    let fixture = ServiceFixture::new();
    let over = ENGINE_LANE_BUDGET + 2;
    for n in 0..over {
        new_story(&fixture, &format!("story {n}"), &[]);
    }
    let steps: Vec<DispatcherStep> = (0..ENGINE_LANE_BUDGET)
        .map(|_| {
            DispatcherStep::Dispatch(DispatchOutcome::from_payload(
                serde_json::json!({"ok": true, "window_name": "w", "worktree_path": "/tmp/w"}),
            ))
        })
        .collect();
    let fake = FakeDispatcher::new(steps);
    let run_id = started_run(&fixture, &fake, u32::try_from(over).unwrap());

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(
        report.filled.len(),
        ENGINE_LANE_BUDGET,
        "the budget bounds the pass even with {over} idle lanes and {over} claimable stories"
    );
    assert_eq!(
        fake.calls()
            .iter()
            .filter(|c| matches!(c, DispatcherCall::Dispatch(_)))
            .count(),
        ENGINE_LANE_BUDGET,
        "and the engine asked the dispatcher exactly that many times"
    );
}
