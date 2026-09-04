//! Restart reconciliation: quarantine interrupted lanes, never resume them
//! (SH-466, `docs/spec/full-auto-engine.md` D11).
//!
//! `tests/engine_reconcile.rs` proves the ordinary, steady-state pass. This
//! file proves the two places [`ReconcilePass::Restart`] diverges from it —
//! a dead window means `Interrupted` rather than `WindowGone`, and the stall
//! clock is re-seeded rather than read across the outage (SH-372) — plus
//! that a restart pass never fills an idle lane and never terminates a run,
//! both of which are `ReconcilePass::Steady`'s job on the very next pass.
//!
//! Same two-mechanism shape `tests/engine_reconcile.rs` uses (SH-365): a
//! pure table over [`classify`] proves the rule, and a real store plus
//! [`FakeDispatcher`] proves the rule is reached.

mod store_support;

use storyhook::service::engine::{
    BREAKER_TRIPPED, EngineService, HardStopKind, LaneClassification, LaneObservation,
    ReconcilePass, STALL_CEILING_SECS, StartRequest, classify,
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
// The pure half: `classify` under `ReconcilePass::Restart`
// ---------------------------------------------------------------------------

/// A lane that is working normally: story open, agent quiet about it, window
/// alive, and its seq moved since the last pass. Copied from
/// `engine_reconcile.rs` rather than shared — integration test binaries do
/// not share modules, and it is four lines.
fn progressing() -> LaneObservation {
    LaneObservation {
        story_closed: false,
        story_verifying: false,
        agent_blocked: false,
        window_alive: true,
        head_global_seq: Some(200),
        last_progress_seq: Some(100),
        seconds_since_progress: Some(5),
        awaiting_reason: None,
    }
}

/// The row this story exists to add: a dead window at restart is
/// `Interrupted`, never `WindowGone` — nobody watched it close, the daemon
/// that would have watched just restarted.
#[test]
fn a_dead_window_at_restart_is_interrupted() {
    let observation = LaneObservation {
        window_alive: false,
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Restart),
        LaneClassification::HardStop(HardStopKind::Interrupted)
    );
}

/// The control for the test above: the identical observation under
/// `ReconcilePass::Steady` is still `WindowGone`. The pass is the entire
/// difference between the two outcomes.
#[test]
fn the_identical_dead_window_under_steady_is_still_window_gone() {
    let observation = LaneObservation {
        window_alive: false,
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::HardStop(HardStopKind::WindowGone)
    );
}

/// A closed story still wins over a dead window at restart — completion is a
/// store fact regardless of which pass observes it (D3, SH-226).
#[test]
fn a_closed_story_wins_over_a_dead_window_at_restart() {
    let observation = LaneObservation {
        story_closed: true,
        window_alive: false,
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Restart),
        LaneClassification::Completed
    );
}

/// The verifying handoff still wins over a dead window at restart: a story
/// held for centralized verification is not interrupted, it is exactly
/// where it is supposed to be, restart or not (SH-521).
#[test]
fn the_verifying_handoff_wins_over_a_dead_window_at_restart() {
    let observation = LaneObservation {
        story_verifying: true,
        window_alive: false,
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Restart),
        LaneClassification::Verifying
    );
}

/// An agent's own diagnosis still surfaces over a dead window at restart —
/// `AgentBlocked` is tested first regardless of pass, so a story the agent
/// itself blocked before the crash keeps its own reason rather than being
/// relabeled `Interrupted`.
#[test]
fn an_agent_block_wins_over_a_dead_window_at_restart() {
    let observation = LaneObservation {
        agent_blocked: true,
        window_alive: false,
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Restart),
        LaneClassification::HardStop(HardStopKind::AgentBlocked)
    );
}

/// The other divergence: at restart, a window that is still alive is
/// `Progressing` even when the seq has not moved past the ceiling — the
/// stall check is skipped entirely, because a daemon that was down cannot
/// have observed anything during the outage, and reading the pre-outage
/// clock would misreport an untouched, healthy lane as stalled (SH-372).
#[test]
fn an_unmoved_seq_past_the_ceiling_is_not_a_stall_at_restart() {
    let observation = LaneObservation {
        head_global_seq: Some(100),
        last_progress_seq: Some(100),
        seconds_since_progress: Some(STALL_CEILING_SECS + 1),
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Restart),
        LaneClassification::Progressing,
        "a live window across an outage of unknown length proves nothing stalled"
    );
}

/// The control: the identical observation under `Steady` is still `Stalled`.
/// Without this the test above would only prove that `Restart` never
/// stalls, not that the pass is what changed the answer.
#[test]
fn the_identical_unmoved_seq_under_steady_is_still_a_stall() {
    let observation = LaneObservation {
        head_global_seq: Some(100),
        last_progress_seq: Some(100),
        seconds_since_progress: Some(STALL_CEILING_SECS + 1),
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::HardStop(HardStopKind::Stalled)
    );
}

// ---------------------------------------------------------------------------
// The wired half: the restart pass through a real store and FakeDispatcher
// ---------------------------------------------------------------------------

/// A run with `lanes` lanes and no stories claimable, so `fill` finds
/// nothing unless the test seeded something. Copied from
/// `engine_reconcile.rs`'s own helper of the same name.
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

/// Puts `story` into lane `index` as if a dispatch had succeeded before the
/// daemon went down.
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

fn streak(fixture: &ServiceFixture, run_id: &str) -> u32 {
    fixture
        .store()
        .read(|tx| tx.engine_run(run_id))
        .unwrap()
        .unwrap()
        .consecutive_hard_stops
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

fn awaiting_of(fixture: &ServiceFixture, number: i64) -> Option<String> {
    fixture
        .store()
        .read(|tx| {
            tx.story(
                fixture.project(),
                storyhook::store::ids::StoryNo::new(number),
            )
        })
        .unwrap()
        .unwrap()
        .awaiting
}

/// A `reconcile_after_restart` whose clock reads `now`. Mirrors
/// `engine_reconcile.rs`'s `reconcile_at`, calling the restart entry point
/// instead of the steady one.
fn reconcile_restart_at(
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
        .reconcile_after_restart(&run_id.to_string())
        .unwrap()
}

fn reconcile_steady_at(
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

/// The central case: a lane occupied when the daemon went down is
/// quarantined `Interrupted`, and the reason names the kind and the run.
#[test]
fn an_interrupted_lane_is_quarantined_and_names_itself() {
    let fixture = ServiceFixture::new();
    // No `WindowAlive` step scripted for "gone" — `window_alive` on the real
    // `ShellDispatcher` answers `false` for a session that does not exist;
    // the fake needs a step regardless, since `observe_lanes` still probes.
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "=fixture:=story-SH-1".into(),
        alive: false,
    }]);
    let story = new_story(&fixture, "lane work", &[]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);

    let report = reconcile_restart_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.quarantined, [(0, HardStopKind::Interrupted)]);
    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(lane.state, EngineLaneState::Quarantined);
    assert_eq!(lane.outcome.as_deref(), Some("interrupted"));
    let awaiting = awaiting_of(&fixture, 1).expect("a quarantined story carries a reason");
    assert!(
        awaiting.contains("interrupted") && awaiting.contains(&run_id),
        "the reason names the kind and the run so a human can act on it: {awaiting}"
    );
}

/// Restart preserves evidence during its special pass, then the first steady
/// pass releases the lane and resumes normal queue work below the breaker.
#[test]
fn the_first_steady_pass_after_restart_continues_with_a_fresh_story() {
    let fixture = ServiceFixture::new();
    let interrupted = new_story(&fixture, "interrupted", &[]);
    let next = new_story(&fixture, "next", &[]);
    let restart = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: format!("=fixture:=story-{interrupted}"),
        alive: false,
    }]);
    let run_id = started_run(&fixture, &restart, 1);
    occupy(&fixture, &run_id, 0, &interrupted);
    reconcile_restart_at(&fixture, &restart, &run_id, FIXTURE_NOW);
    assert_eq!(
        lane_at(&fixture, &run_id, 0).state,
        EngineLaneState::Quarantined
    );

    let steady = FakeDispatcher::new([DispatcherStep::Dispatch(
        storyhook::service::engine::DispatchOutcome::from_payload(serde_json::json!({
            "ok": true,
            "pane": "%113",
            "window_name": next,
            "worktree_path": "/tmp/wt/next"
        })),
    )]);
    let report = reconcile_steady_at(&fixture, &steady, &run_id, FIXTURE_NOW);

    assert_eq!(report.filled, [(0, next.clone())]);
    assert_eq!(lane_at(&fixture, &run_id, 0).story_id, Some(next));
}

/// D11's own text: worktree, branch and window are preserved, and the
/// engine never resumes or resets — it never calls `unclaim` or
/// `kill_window` for an interrupted lane. There is no `Dispatcher` method
/// for `story reset` at all; the absence of any destructive call is the
/// whole of what "never reset" can mean at this seam.
#[test]
fn an_interrupted_lane_preserves_its_worktree_and_window_and_is_never_torn_down() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "=fixture:=story-SH-1".into(),
        alive: false,
    }]);
    let story = new_story(&fixture, "lane work", &[]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);

    reconcile_restart_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(
        lane.worktree_path.as_deref(),
        Some(format!("/tmp/wt/{story}").as_str()),
        "D11 preserves a crashed agent's worktree for a human to look at"
    );
    assert!(
        lane.window_name.is_some(),
        "the window name is diagnostic evidence and must survive quarantine"
    );
    assert!(
        !fake.calls().iter().any(|call| matches!(
            call,
            DispatcherCall::Unclaim(_) | DispatcherCall::KillWindow(_)
        )),
        "never resume, never reset: the engine must not unclaim or kill an interrupted lane's window"
    );
    let awaiting = awaiting_of(&fixture, 1).expect("a quarantined story carries a reason");
    assert!(
        awaiting.contains("preserved"),
        "D11's own promise — the worktree is preserved for inspection — must reach the human: {awaiting}"
    );
}

/// A lane whose window survived the restart (a daemon-only restart, tmux
/// left running) is not touched: still `Working`, not quarantined, streak
/// untouched, and its stall clock is re-seeded to the restart's own `now`
/// rather than left at whatever it was before the outage.
///
/// The lane's `last_progress_seq` is deliberately set to the story's OWN
/// current `head_global_seq` before the pass runs, so the seq genuinely
/// reads as UNMOVED — the `(Some(_), None) => true` fallback arm in
/// `record_progress` cannot supply "moved" on its own here, the way it does
/// for a lane observed for the first time. Only the restart-pass override
/// can be the reason this reseeds; a test that left `last_progress_seq`
/// unset would still pass with that override deleted, proving nothing.
#[test]
fn a_lane_whose_window_survived_the_restart_is_untouched_and_its_clock_is_reseeded() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "=fixture:=story-SH-1".into(),
        alive: true,
    }]);
    let story = new_story(&fixture, "lane work", &[]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);
    let head = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), storyhook::store::ids::StoryNo::new(1)))
        .unwrap()
        .unwrap()
        .head_global_seq;
    let mut lane = lane_at(&fixture, &run_id, 0);
    lane.last_progress_seq = Some(head);
    lane.last_progress_at = Some(FIXTURE_NOW.to_string());
    fixture
        .store()
        .write(|tx| tx.put_engine_lane(&lane))
        .unwrap();
    // Far enough past FIXTURE_NOW that a Steady pass at this same clock,
    // over this same unmoved seq, would call the lane Stalled (the outage
    // plus however long the daemon was down).
    let now = "2026-01-01T01:00:00Z"; // one hour after FIXTURE_NOW

    let report = reconcile_restart_at(&fixture, &fake, &run_id, now);

    assert!(report.quarantined.is_empty());
    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(lane.state, EngineLaneState::Working);
    assert_eq!(
        streak(&fixture, &run_id),
        0,
        "a surviving lane is not a hard stop"
    );
    assert_eq!(
        lane.last_progress_at.as_deref(),
        Some(now),
        "the stall clock is re-seeded to this pass, not left at whatever predates the outage"
    );
}

/// A restart pass never fills an idle lane, even with a ready story queued —
/// `story.sh` calls back into this daemon over `/api/v1/invoke`, which is
/// not yet answering this early in startup. The fake carries no `Dispatch`
/// step, so an attempt to fill would panic.
#[test]
fn a_restart_pass_never_fills_an_idle_lane_even_with_ready_work_queued() {
    let fixture = ServiceFixture::new();
    let _story = new_story(&fixture, "claim me", &[]);
    let fake = FakeDispatcher::default();
    let run_id = started_run(&fixture, &fake, 1);

    let report = reconcile_restart_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert!(
        report.filled.is_empty(),
        "the fake had no scripted dispatch step and would have panicked if the pass tried to fill"
    );
    assert!(
        fake.calls().is_empty(),
        "a restart pass probes nothing idle"
    );
}

/// A restart pass never terminates the run: an idle lane with nothing
/// claimable would ordinarily drain the run (`QUEUE_DRAINED`), but D11 says
/// the run continues with fresh lanes on the *next*, steady pass — not this
/// one.
#[test]
fn a_restart_pass_never_terminates_the_run() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let run_id = started_run(&fixture, &fake, 1);

    let report = reconcile_restart_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.run_state, EngineRunState::Running);
    assert_eq!(report.stop_reason, None);
    assert_eq!(run_state(&fixture, &run_id), EngineRunState::Running);
}

/// The breaker still runs on a restart pass, deliberately: three lanes
/// interrupted by one reboot is three consecutive hard stops, which halts
/// the run exactly as three ordinary hard stops would (D10). A machine that
/// just rebooted mid-run deserves a human look before it starts merging
/// again.
#[test]
fn three_interrupted_lanes_halt_the_run() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowAlive {
            window: "=fixture:=story-SH-1".into(),
            alive: false,
        },
        DispatcherStep::WindowAlive {
            window: "=fixture:=story-SH-2".into(),
            alive: false,
        },
        DispatcherStep::WindowAlive {
            window: "=fixture:=story-SH-3".into(),
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

    let report = reconcile_restart_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.quarantined.len(), 3);
    assert!(
        report
            .quarantined
            .iter()
            .all(|(_, kind)| *kind == HardStopKind::Interrupted)
    );
    assert_eq!(report.run_state, EngineRunState::Halted);
    assert_eq!(report.stop_reason.as_deref(), Some(BREAKER_TRIPPED));
}

/// The half above needs its control: two interrupted lanes do not trip the
/// breaker, so the run keeps going and the third lane, if any, is left for a
/// deliberate look rather than swept in by an accident of the counter.
#[test]
fn two_interrupted_lanes_leave_the_run_running() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowAlive {
            window: "=fixture:=story-SH-1".into(),
            alive: false,
        },
        DispatcherStep::WindowAlive {
            window: "=fixture:=story-SH-2".into(),
            alive: false,
        },
    ]);
    let a = new_story(&fixture, "a", &[]);
    let b = new_story(&fixture, "b", &[]);
    let run_id = started_run(&fixture, &fake, 2);
    occupy(&fixture, &run_id, 0, &a);
    occupy(&fixture, &run_id, 1, &b);

    let report = reconcile_restart_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.quarantined.len(), 2);
    assert_eq!(report.run_state, EngineRunState::Running);
    assert_eq!(report.stop_reason, None);
    assert_eq!(streak(&fixture, &run_id), 2);
}

/// A completed story in the same restart pass zeroes the streak before the
/// hard stops in that same pass are added — the identical rule
/// `apply_breaker` applies to a steady pass, proven here through the
/// restart entry point rather than assumed to carry over.
#[test]
fn a_completion_zeroes_the_streak_before_this_passs_hard_stops_are_added() {
    let fixture = ServiceFixture::new();
    let a = new_story(&fixture, "a", &[]);
    let b = new_story(&fixture, "b", &[]);
    StoryService::new(&fixture.ctx())
        .set_state(&a, "done", None, None, None)
        .unwrap();
    let fake = FakeDispatcher::new([
        // A closed story is still probed before `classify` short-circuits on
        // `story_closed`, so the fake needs an answer even though it is
        // irrelevant to the outcome.
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{a}"),
            alive: true,
        },
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{b}"),
            alive: false,
        },
    ]);
    let run_id = started_run(&fixture, &fake, 2);
    occupy(&fixture, &run_id, 0, &a);
    occupy(&fixture, &run_id, 1, &b);

    let report = reconcile_restart_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.completed, [0]);
    assert_eq!(report.quarantined, [(1, HardStopKind::Interrupted)]);
    assert_eq!(
        streak(&fixture, &run_id),
        1,
        "the completion zeroes any existing streak, then this pass's one hard stop is added"
    );
    assert_eq!(report.run_state, EngineRunState::Running);
}

/// An idle lane (never occupied) is left entirely alone by a restart pass:
/// no window probe, no store write, no quarantine.
#[test]
fn an_idle_lane_is_untouched_by_a_restart_pass() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let run_id = started_run(&fixture, &fake, 1);

    let report = reconcile_restart_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert!(report.quarantined.is_empty());
    assert!(fake.calls().is_empty(), "an idle lane is never probed");
    assert_eq!(lane_at(&fixture, &run_id, 0).state, EngineLaneState::Idle);
}

/// A second restart pass over an already-quarantined lane is idempotent:
/// `observe_lanes` skips `Quarantined` lanes entirely, so the streak does
/// not grow a second time and the fake is never asked about a window that
/// was already given up on.
#[test]
fn a_second_restart_pass_does_not_re_quarantine_or_grow_the_streak() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "=fixture:=story-SH-1".into(),
        alive: false,
    }]);
    let story = new_story(&fixture, "lane work", &[]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);

    let first = reconcile_restart_at(&fixture, &fake, &run_id, FIXTURE_NOW);
    assert_eq!(first.quarantined, [(0, HardStopKind::Interrupted)]);
    assert_eq!(streak(&fixture, &run_id), 1);

    let fake2 = FakeDispatcher::default();
    let second = reconcile_restart_at(&fixture, &fake2, &run_id, FIXTURE_NOW);

    assert!(
        second.quarantined.is_empty(),
        "an already-quarantined lane is not observed a second time"
    );
    assert_eq!(
        streak(&fixture, &run_id),
        1,
        "the streak does not grow twice for one lane"
    );
    assert!(fake2.calls().is_empty());
}
