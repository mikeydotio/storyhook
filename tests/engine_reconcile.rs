//! The Full Auto reconcile loop (SH-465).
//!
//! Every row of the failure taxonomy, the breaker's arithmetic including its
//! reset, the `no-auto` skip, termination, independent run capacity, and the
//! derived stall constants.
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

use storyhook::domain::{
    CLEANUP_LEASE_VERSION, LABEL_HUMAN_ONLY, LABEL_NO_AUTO, StoryCleanupLease, TmuxCleanupTarget,
};
use storyhook::lane_budget::WindowCensus;
use storyhook::service::engine::{
    AdmissionWait, BREAKER_TRIPPED, COMPLETED, ConfigureRequest, DispatchOutcome, Dispatcher,
    EngineService, HOST_TOOL_CALL_CEILING_SECS, HardStopKind, LaneClassification, LaneObservation,
    OPERATOR_STOPPED, QUEUE_DRAINED, RECONCILE_TICK_SECS, RESERVED, RESERVED_HOLD_PREFIX,
    ReconcilePass, STALL_CEILING_SECS, STALL_MARGIN, StartRequest, WindowProbe, classify,
};
use storyhook::service::{Clock, Ctx, NewStoryInput, StoryService};
use storyhook::store::{
    EngineAgent, EngineLaneRecord, EngineLaneState, EngineRunState, EngineScope, EngineSpeed,
    ReadOps, Store, WriteOps,
};
use storyhook_test_support::{
    DispatcherCall, DispatcherStep, FIXTURE_NOW, FakeDispatcher, ServiceFixture,
};

fn cleanup_lease(story: &str, worktree: &str) -> StoryCleanupLease {
    StoryCleanupLease {
        version: CLEANUP_LEASE_VERSION,
        project_slug: "fixture".into(),
        story_id: story.into(),
        repository_path: "/repos/original".into(),
        worktree_path: worktree.into(),
        branch: format!("worktree-{story}"),
        tmux: TmuxCleanupTarget {
            socket_path: "/tmp/tmux-original/default".into(),
        },
    }
}

// ---------------------------------------------------------------------------
// The taxonomy, as a table over the pure decision
// ---------------------------------------------------------------------------

/// A lane that is working normally: story open, agent quiet about it, window
/// alive, and its seq moved since the last pass.
fn progressing() -> LaneObservation {
    LaneObservation {
        story_closed: false,
        story_verifying: false,
        agent_blocked: false,
        window: WindowProbe::Alive {
            last_output_at: None,
        },
        head_global_seq: Some(200),
        last_progress_seq: Some(100),
        seconds_since_progress: Some(5),
        seconds_since_output: None,
        awaiting_reason: None,
        returned_for_repair: false,
        reserved_label: None,
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
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
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
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::HardStop(HardStopKind::AgentBlocked)
    );
}

/// Row 3: the window is gone while the story is still OPEN.
#[test]
fn a_missing_window_on_an_open_story_is_a_hard_stop() {
    let observation = LaneObservation {
        window: WindowProbe::Gone {
            detail: "scripted: gone".to_string(),
        },
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::HardStop(HardStopKind::WindowGone)
    );
}

/// SH-650: a window gone on a story the verifier has just returned for
/// repair is the verifier's own resume re-dispatch in flight, not a hard
/// stop. It contributes no evidence on a steady pass, exactly as an
/// unanswered probe does.
#[test]
fn a_missing_window_on_a_story_just_returned_for_repair_is_deferred() {
    let observation = LaneObservation {
        window: WindowProbe::Gone {
            detail: "scripted: gone".to_string(),
        },
        returned_for_repair: true,
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::Progressing
    );
}

/// SH-650: the deferral is bounded by the stall clock, which a re-dispatch
/// that never comes cannot advance — `DISPATCH_TIMEOUT` sits inside the
/// ceiling, so a re-dispatch has either shown a live pane or parked the story
/// with `awaiting` before this fires.
#[test]
fn a_deferred_missing_window_still_lets_the_stall_ceiling_catch_a_dead_lane() {
    assert!(
        storyhook::service::engine::DISPATCH_TIMEOUT.as_secs() < STALL_CEILING_SECS,
        "the resume re-dispatch must be able to finish inside the stall ceiling"
    );
    let observation = LaneObservation {
        window: WindowProbe::Gone {
            detail: "scripted: gone".to_string(),
        },
        returned_for_repair: true,
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

/// SH-650: a daemon that died mid-re-dispatch has nobody left to finish it,
/// so a restart pass still reports the missing window as `Interrupted`.
#[test]
fn a_missing_window_on_a_returned_story_is_interrupted_across_a_restart() {
    let observation = LaneObservation {
        window: WindowProbe::Gone {
            detail: "scripted: gone".to_string(),
        },
        returned_for_repair: true,
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Restart),
        LaneClassification::HardStop(HardStopKind::Interrupted)
    );
}

/// SH-650: the verifier's own refusal to re-dispatch sets `awaiting`, and
/// that still outranks the deferral — a parked story is a hard stop.
#[test]
fn an_agent_block_wins_over_a_deferred_missing_window() {
    let observation = LaneObservation {
        agent_blocked: true,
        window: WindowProbe::Gone {
            detail: "scripted: gone".to_string(),
        },
        returned_for_repair: true,
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::HardStop(HardStopKind::AgentBlocked)
    );
}

/// SH-626 (council verdict on the story): a probe tmux could not answer is
/// not a dead window. It contributes no evidence this pass, so a lane whose
/// story is moving keeps progressing under both passes.
#[test]
fn an_unanswered_probe_on_a_moving_story_is_not_a_hard_stop() {
    let observation = LaneObservation {
        window: WindowProbe::Unanswered {
            detail: "tmux exited 1: unbound variable".to_string(),
        },
        ..progressing()
    };
    for pass in [ReconcilePass::Steady, ReconcilePass::Restart] {
        assert_eq!(
            classify(&observation, STALL_CEILING_SECS, pass),
            LaneClassification::Progressing,
            "{pass:?}"
        );
    }
}

/// SH-626's backstop: with the probe unanswerable, the lane is judged by the
/// stall clock alone, so an unmoved seq past the ceiling is still caught —
/// later, and by the store fact a dead agent cannot forge.
#[test]
fn an_unanswered_probe_still_lets_the_stall_ceiling_catch_a_dead_lane() {
    let observation = LaneObservation {
        window: WindowProbe::Unanswered {
            detail: "tmux did not answer within 3s".to_string(),
        },
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
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
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
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
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
        classify(&at_ceiling, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::Progressing,
        "a lane exactly at the ceiling has not passed it"
    );
}

// ---------------------------------------------------------------------------
// The second stall channel (SH-657)
// ---------------------------------------------------------------------------

/// The incident's own row: the store has not moved past the ceiling — an
/// autonomous agent writes nothing to the store between its dispatch and its
/// plan comment — but its pane wrote to the terminal inside the ceiling. That
/// is a working agent, and 8 of the first 8 stall verdicts the engine ever
/// wrote were this row misread as a stall.
#[test]
fn a_store_silent_lane_whose_pane_wrote_inside_the_ceiling_is_progressing() {
    let observation = LaneObservation {
        head_global_seq: Some(100),
        last_progress_seq: Some(100),
        seconds_since_progress: Some(STALL_CEILING_SECS * 10),
        window: WindowProbe::Alive {
            last_output_at: Some(1_789_066_115),
        },
        seconds_since_output: Some(1),
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::Progressing,
        "a pane that wrote within the ceiling holds a live agent whatever the store says"
    );
}

/// The true stall, both channels: an agent that has neither written to the
/// store nor to its terminal for longer than the ceiling — a Claude parked at
/// an idle prompt, whose pane goes static for hours (measured for SH-657).
#[test]
fn a_lane_silent_on_both_channels_past_the_ceiling_is_a_stall() {
    let observation = LaneObservation {
        head_global_seq: Some(100),
        last_progress_seq: Some(100),
        seconds_since_progress: Some(STALL_CEILING_SECS + 1),
        window: WindowProbe::Alive {
            last_output_at: Some(1_789_066_115),
        },
        seconds_since_output: Some(STALL_CEILING_SECS + 1),
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::HardStop(HardStopKind::Stalled)
    );
}

/// The pty channel has the same boundary as the store channel: exactly AT the
/// ceiling is not past it, so output that old still rescues the lane.
#[test]
fn pane_output_exactly_at_the_ceiling_is_not_yet_silent() {
    let observation = LaneObservation {
        head_global_seq: Some(100),
        last_progress_seq: Some(100),
        seconds_since_progress: Some(STALL_CEILING_SECS * 10),
        window: WindowProbe::Alive {
            last_output_at: Some(1_789_066_115),
        },
        seconds_since_output: Some(STALL_CEILING_SECS),
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::Progressing
    );
}

/// A pty channel that could not be read contributes no evidence (SH-372), so
/// the store channel judges alone — which is the SH-626 backstop: a dead lane
/// behind a tmux that cannot be asked is still caught by store silence.
#[test]
fn unknown_pane_output_leaves_the_store_channel_to_judge_alone() {
    let store_silent = LaneObservation {
        head_global_seq: Some(100),
        last_progress_seq: Some(100),
        seconds_since_progress: Some(STALL_CEILING_SECS + 1),
        seconds_since_output: None,
        ..progressing()
    };
    assert_eq!(
        classify(&store_silent, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::HardStop(HardStopKind::Stalled),
        "no pty evidence is not pty evidence of life"
    );
    let store_moving = LaneObservation {
        seconds_since_output: None,
        ..progressing()
    };
    assert_eq!(
        classify(&store_moving, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::Progressing
    );
}

/// The store channel still wins on its own: a story that moved is progress
/// even when the pane has been static past the ceiling (a long foreground
/// tool call that ended with a `story comment`, say).
#[test]
fn a_moved_seq_is_progress_even_when_the_pane_is_silent() {
    let observation = LaneObservation {
        head_global_seq: Some(200),
        last_progress_seq: Some(100),
        seconds_since_progress: Some(STALL_CEILING_SECS * 10),
        seconds_since_output: Some(STALL_CEILING_SECS * 10),
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::Progressing
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
            classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
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
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::Progressing
    );
}

/// Row 5: the story reached the required `verifying` handoff (SH-521). The
/// agent's last action for a successful story execs `story move <n>
/// verifying` and stops, and the dispatch launch execs the agent process
/// directly rather than typing into a persistent shell
/// (`plugins/story/bin/story.sh`'s `remain-on-exit` rationale) — so the pane
/// is normally already dead the instant the story reaches this handoff,
/// exactly like an ordinary completion. That must read as `Verifying`, never
/// `WindowGone`.
#[test]
fn a_verifying_story_with_a_dead_window_is_held_not_window_gone() {
    let observation = LaneObservation {
        story_verifying: true,
        window: WindowProbe::Gone {
            detail: "scripted: gone".to_string(),
        },
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::Verifying
    );
}

/// D4's verification worker serializes `make test` machine-wide, so a
/// candidate can legitimately queue past any one lane's own
/// [`STALL_CEILING_SECS`]. That must read as `Verifying`, never `Stalled`.
#[test]
fn a_verifying_story_past_the_ceiling_is_held_not_stalled() {
    let observation = LaneObservation {
        story_verifying: true,
        head_global_seq: Some(100),
        last_progress_seq: Some(100),
        seconds_since_progress: Some(STALL_CEILING_SECS + 1),
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::Verifying,
        "the machine-wide verification queue can legitimately outrun one lane's own ceiling"
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
        window: WindowProbe::Gone {
            detail: "scripted: gone".to_string(),
        },
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::Completed,
        "a finished agent whose pane exited is a completion, not a WindowGone hard stop"
    );
}

/// Completion also outranks the agent-blocked signal, the verifying handoff
/// and the stall clock: a story that reached a CLOSED superstate is done
/// regardless of what its metadata or its clock say.
#[test]
fn a_closed_story_wins_over_every_other_signal() {
    let observation = LaneObservation {
        story_closed: true,
        story_verifying: true,
        agent_blocked: true,
        window: WindowProbe::Gone {
            detail: "scripted: gone".to_string(),
        },
        head_global_seq: Some(100),
        last_progress_seq: Some(100),
        seconds_since_progress: Some(STALL_CEILING_SECS * 10),
        seconds_since_output: Some(STALL_CEILING_SECS * 10),
        awaiting_reason: Some("the agent said why".to_string()),
        returned_for_repair: true,
        reserved_label: Some(LABEL_HUMAN_ONLY),
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
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
        window: WindowProbe::Gone {
            detail: "scripted: gone".to_string(),
        },
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::HardStop(HardStopKind::AgentBlocked)
    );
}

/// A closed story still wins over the verifying handoff: it is not reachable
/// in production (verifying is an OPEN state), but the precedence must not
/// depend on that being true.
#[test]
fn a_closed_story_wins_over_the_verifying_handoff() {
    let observation = LaneObservation {
        story_closed: true,
        story_verifying: true,
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::Completed
    );
}

/// An agent's own diagnosis still surfaces from the verifying handoff:
/// centralized verification's `return_for_repair` falls back to
/// `set_awaiting` when it cannot reach a dead pane (SH-521), and that must
/// read as `AgentBlocked`, not be silently held as `Verifying`.
#[test]
fn an_agent_block_wins_over_the_verifying_handoff() {
    let observation = LaneObservation {
        agent_blocked: true,
        story_verifying: true,
        ..progressing()
    };
    assert_eq!(
        classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
        LaneClassification::HardStop(HardStopKind::AgentBlocked)
    );
}

// ---------------------------------------------------------------------------
// A reserved label (SH-837, council D7)
// ---------------------------------------------------------------------------

/// Every observation that would end, quarantine or hold a lane for the
/// verifier — each row paired with the verdict it reaches without a label.
fn lane_endings() -> Vec<(
    &'static str,
    LaneObservation,
    ReconcilePass,
    LaneClassification,
)> {
    let gone = || WindowProbe::Gone {
        detail: "scripted: gone".to_string(),
    };
    let stalled = || LaneObservation {
        head_global_seq: Some(100),
        last_progress_seq: Some(100),
        seconds_since_progress: Some(STALL_CEILING_SECS + 1),
        seconds_since_output: Some(STALL_CEILING_SECS + 1),
        ..progressing()
    };
    vec![
        (
            "blocked or awaiting",
            LaneObservation {
                agent_blocked: true,
                ..progressing()
            },
            ReconcilePass::Steady,
            LaneClassification::HardStop(HardStopKind::AgentBlocked),
        ),
        (
            "verifying",
            LaneObservation {
                story_verifying: true,
                ..progressing()
            },
            ReconcilePass::Steady,
            LaneClassification::Verifying,
        ),
        (
            "verifying with its pane already gone",
            LaneObservation {
                story_verifying: true,
                window: gone(),
                ..progressing()
            },
            ReconcilePass::Steady,
            LaneClassification::Verifying,
        ),
        (
            "window gone",
            LaneObservation {
                window: gone(),
                ..progressing()
            },
            ReconcilePass::Steady,
            LaneClassification::HardStop(HardStopKind::WindowGone),
        ),
        (
            "window gone across a restart",
            LaneObservation {
                window: gone(),
                ..progressing()
            },
            ReconcilePass::Restart,
            LaneClassification::HardStop(HardStopKind::Interrupted),
        ),
        (
            "window gone just after a verifier return",
            LaneObservation {
                window: gone(),
                returned_for_repair: true,
                ..progressing()
            },
            ReconcilePass::Steady,
            LaneClassification::Progressing,
        ),
        (
            "silent on both channels past the ceiling",
            stalled(),
            ReconcilePass::Steady,
            LaneClassification::HardStop(HardStopKind::Stalled),
        ),
        (
            "stalled while tmux cannot be asked",
            LaneObservation {
                window: WindowProbe::Unanswered {
                    detail: "scripted: timeout".to_string(),
                },
                seconds_since_output: None,
                ..stalled()
            },
            ReconcilePass::Steady,
            LaneClassification::HardStop(HardStopKind::Stalled),
        ),
    ]
}

/// The run lets go of a reserved story at every point where it would
/// otherwise end, quarantine, defer or hold the lane — for either label, on
/// either pass — and never with a hard stop, which would set `awaiting` and
/// feed the breaker for what is an operator's decision.
///
/// The deferral row is the one a label changes from "wait": a story the
/// verifier just returned is normally waiting for the verifier's resume
/// re-dispatch (SH-650), and a reserved story never gets one.
#[test]
fn a_reserved_label_releases_the_lane_at_every_ending() {
    for (name, observation, pass, unlabelled) in lane_endings() {
        assert_eq!(
            classify(&observation, STALL_CEILING_SECS, pass),
            unlabelled,
            "control: {name}"
        );
        for label in [LABEL_HUMAN_ONLY, LABEL_NO_AUTO] {
            let reserved = LaneObservation {
                reserved_label: Some(label),
                ..observation.clone()
            };
            assert_eq!(
                classify(&reserved, STALL_CEILING_SECS, pass),
                LaneClassification::Reserved(label),
                "{label}: {name}"
            );
        }
    }
}

/// While its agent is live the run keeps a reserved story's lane (council
/// D7): capacity stays honest, nothing is interrupted, and removing the
/// label again changes nothing. An unanswered probe is no evidence either way
/// (SH-626), so it holds too, until the stall ceiling decides.
#[test]
fn a_reserved_story_with_a_live_agent_keeps_its_lane() {
    let live = [
        WindowProbe::Alive {
            last_output_at: None,
        },
        WindowProbe::Unanswered {
            detail: "scripted: timeout".to_string(),
        },
    ];
    for label in [LABEL_HUMAN_ONLY, LABEL_NO_AUTO] {
        for window in &live {
            for pass in [ReconcilePass::Steady, ReconcilePass::Restart] {
                let observation = LaneObservation {
                    window: window.clone(),
                    reserved_label: Some(label),
                    ..progressing()
                };
                assert_eq!(
                    classify(&observation, STALL_CEILING_SECS, pass),
                    LaneClassification::Progressing,
                    "{label}, {window:?}, {pass:?}"
                );
            }
        }
    }
}

/// Completion still comes first: a reserved story that closed frees its lane
/// as a completion, which also zeroes the streak.
#[test]
fn a_closed_story_wins_over_a_reserved_label() {
    for label in [LABEL_HUMAN_ONLY, LABEL_NO_AUTO] {
        let observation = LaneObservation {
            story_closed: true,
            window: WindowProbe::Gone {
                detail: "scripted: gone".to_string(),
            },
            reserved_label: Some(label),
            ..progressing()
        };
        assert_eq!(
            classify(&observation, STALL_CEILING_SECS, ReconcilePass::Steady),
            LaneClassification::Completed,
            "{label}"
        );
    }
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

/// SH-672: HTTP request concurrency and agent session capacity are distinct.
#[test]
fn engine_capacity_is_not_coupled_to_http_dispatch_capacity() {
    for path in ["src/service/engine.rs", "src/lane_budget.rs"] {
        let source = checkout_source(path);
        assert!(!source.contains("api::dispatch::MAX_RUNNING"), "{path}");
        assert!(!source.contains("pub const ENGINE_LANE_BUDGET"), "{path}");
    }
}

/// The ceiling's own spelling, for the same reason and by the same mechanism:
/// the product must be written as its two named factors, so a reader can see
/// which part is the deadline being disproved and which is judgement.
///
/// SH-657 replaced the factors. The ceiling used to be spelled
/// `ENGINE_LANE_BUDGET * GATE_MEDIAN_SECS * STALL_MARGIN` — a bound on a test
/// leg, which the clock never measured; it measured time between story
/// events, which has no legitimate bound at all. The deadline the ceiling
/// disproves now is one foreground tool call, the longest an agent can be
/// silent on BOTH channels the engine reads.
#[test]
fn the_stall_ceiling_is_spelled_as_its_derivation() {
    let declared = declared_const(
        &checkout_source("src/service/engine.rs"),
        "STALL_CEILING_SECS",
    );
    for factor in ["HOST_TOOL_CALL_CEILING_SECS", "STALL_MARGIN"] {
        assert!(
            declared.contains(factor),
            "STALL_CEILING_SECS must name {factor} in its own derivation; found `{declared}`"
        );
    }
    for retired in ["ENGINE_LANE_BUDGET", "GATE_MEDIAN_SECS"] {
        assert!(
            !declared.contains(retired),
            "STALL_CEILING_SECS must not derive from {retired} again: a test leg's duration bounds nothing the stall clock measures (SH-657); found `{declared}`"
        );
    }
}

/// The ceiling is derived from the deadline it disproves, and stays derived:
/// a live agent's longest silence on both channels is one foreground tool
/// call, which the host bounds at [`HOST_TOOL_CALL_CEILING_SECS`].
#[test]
fn the_stall_ceiling_still_derives_from_the_host_tool_call_ceiling() {
    assert_eq!(
        STALL_CEILING_SECS,
        HOST_TOOL_CALL_CEILING_SECS * STALL_MARGIN,
        "the ceiling must remain the product of the host's tool-call ceiling and the named margin — not a literal that happens to equal it today"
    );
    // The host bound is Claude Code's own: `timeout … max 600000` ms on its
    // Bash tool. Pinned so a change here is a decision, not drift.
    assert_eq!(
        HOST_TOOL_CALL_CEILING_SECS, 600,
        "Claude Code bounds one foreground Bash call at 600 s; re-measure before moving this"
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
        HardStopKind::DispatchRefused,
        HardStopKind::StoryMissing,
    ];
    // The lane outcomes that are not hard stops share the same column.
    let spellings: Vec<&str> = kinds
        .iter()
        .map(|kind| kind.as_str())
        .chain([COMPLETED, RESERVED])
        .collect();
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
            model: None,
            effort: None,
            speed: None,
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

/// Puts `story` into a new lane `index` beyond the run's configured lanes, as
/// if a dispatch had filled an overflow lane (SH-822).
fn occupy_overflow(fixture: &ServiceFixture, run_id: &str, index: u32, story: &str) {
    let mut lane = lane_at(fixture, run_id, 0);
    lane.lane_index = index;
    lane.adopted_identity = None;
    lane.cleanup_lease = None;
    lane.pane_id = None;
    lane.outcome = None;
    lane.outcome_detail = None;
    lane.probe_detail = None;
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

fn run_of(fixture: &ServiceFixture, run_id: &str) -> storyhook::store::EngineRunRecord {
    fixture
        .store()
        .read(|tx| tx.engine_run(run_id))
        .unwrap()
        .unwrap()
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

// ---------------------------------------------------------------------------
// Engine event hooks (SH-472)
// ---------------------------------------------------------------------------

/// Configures every engine hook to append its payload, one JSON line per
/// event, into `hooks.log` — the engine-scoped sibling of
/// `tests/service_story.rs`'s `record_all_hooks`.
fn record_engine_hooks(fixture: &ServiceFixture) {
    let events = [
        "on_engine_run_started",
        "on_engine_run_halted",
        "on_engine_run_drained",
        "on_engine_lane_quarantined",
    ];
    let body: String = events
        .iter()
        .map(|event| format!("{event} = {{ command = \"cat >> hooks.log; echo >> hooks.log\" }}\n"))
        .collect();
    fixture.write_hooks_toml(&body);
}

/// Every recorded payload whose `event_type` matches, in firing order.
///
/// Filtered by `event_type` rather than asserting the whole log, because one
/// test's flow can legitimately fire more than one kind of engine hook (a
/// `start()` followed by a `reconcile()` that drains, say) and each test
/// should only have to state what it is actually checking.
fn fired_engine_hooks(fixture: &ServiceFixture, event_type: &str) -> Vec<serde_json::Value> {
    let path = fixture.cwd().join("hooks.log");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|value| value.get("event_type").and_then(|v| v.as_str()) == Some(event_type))
        .collect()
}

/// `EngineService::start` fires `engine_run_started` once the run and its
/// lanes have committed, naming the run, its scope, lane count and agent.
#[test]
fn starting_a_run_fires_the_engine_run_started_hook() {
    let fixture = ServiceFixture::new();
    record_engine_hooks(&fixture);
    let fake = FakeDispatcher::new(Vec::<DispatcherStep>::new());
    let run_id = started_run(&fixture, &fake, 2);

    let fired = fired_engine_hooks(&fixture, "engine_run_started");
    assert_eq!(fired.len(), 1);
    let payload = &fired[0];
    assert_eq!(payload["run_id"], run_id);
    assert_eq!(payload["scope"], "project");
    assert_eq!(payload["epic_id"], serde_json::Value::Null);
    assert_eq!(payload["lanes"], 2);
    assert_eq!(payload["agent"], "codex");
}

/// `no_hooks(true)` suppresses engine hooks exactly as it does every other
/// hook (`Ctx::hooks_enabled`) — engine events are not a second, ungated
/// notification path.
#[test]
fn no_hooks_suppresses_engine_hooks_too() {
    let fixture = ServiceFixture::new();
    record_engine_hooks(&fixture);
    let ctx = fixture.ctx().no_hooks(true);
    let fake = FakeDispatcher::new(Vec::<DispatcherStep>::new());
    EngineService::new(&ctx, &fake)
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 1,
            agent: EngineAgent::Codex,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap();

    assert!(
        fired_engine_hooks(&fixture, "engine_run_started").is_empty(),
        "no_hooks(true) must suppress engine hooks exactly like every other hook"
    );
}

/// Row 1, wired: the story closed, so the lane frees and the run drains.
#[test]
fn a_completed_story_frees_its_lane_and_drains_the_run() {
    let fixture = ServiceFixture::new();
    record_engine_hooks(&fixture);
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "=fixture:=story-SH-1".into(),
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

    let drained = fired_engine_hooks(&fixture, "engine_run_drained");
    assert_eq!(
        drained.len(),
        1,
        "the queue organically running dry must fire engine_run_drained"
    );
    assert_eq!(drained[0]["run_id"], run_id);
    assert!(
        fired_engine_hooks(&fixture, "engine_run_halted").is_empty(),
        "a completion is never a hard stop, so the breaker never fires here"
    );
}

/// Row 2, wired: the agent set `awaiting`, so the hard stop and its evidence
/// are preserved even after the lane returns to service.
///
/// This also covers the SH-120 relay rule for quarantine's own message:
/// `quarantine_lane` must not overwrite the agent's own diagnosis with a
/// generic lane/run/window sentence composed here — it must relay the
/// original verbatim and append provenance. Without that fix, this assertion
/// fails with the agent's own text replaced entirely by "Full Auto:
/// agent-blocked on lane 0 of run ...".
#[test]
fn an_agent_blocked_story_quarantines_the_lane_and_preserves_its_evidence() {
    let fixture = ServiceFixture::new();
    record_engine_hooks(&fixture);
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "=fixture:=story-SH-1".into(),
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
    assert_eq!(lane.state, EngineLaneState::Idle);
    assert_eq!(lane.outcome.as_deref(), Some("agent-blocked"));
    let quarantine = fixture
        .store()
        .read(|tx| tx.engine_run(&run_id))
        .unwrap()
        .unwrap()
        .recent_quarantines
        .pop()
        .unwrap();
    assert_eq!(
        quarantine.worktree_path.as_deref(),
        Some(format!("/tmp/wt/{story}").as_str()),
        "D11 preserves a stopped agent's worktree for a human to look at"
    );
    assert!(
        quarantine.window_name.is_some(),
        "the window name is diagnostic evidence and must survive quarantine"
    );
    let awaiting = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), storyhook::store::ids::StoryNo::new(1)))
        .unwrap()
        .unwrap()
        .awaiting
        .expect("a quarantined story carries a reason");
    assert!(
        awaiting.contains("the agent stopped and said why"),
        "the agent's own diagnosis must be relayed verbatim, never replaced by a message composed here (SH-120): {awaiting}"
    );
    assert!(
        awaiting.contains("agent-blocked") && awaiting.contains(&run_id),
        "lane/run provenance is appended, not the whole story: {awaiting}"
    );

    let quarantined_hooks = fired_engine_hooks(&fixture, "engine_lane_quarantined");
    assert_eq!(quarantined_hooks.len(), 1);
    let payload = &quarantined_hooks[0];
    assert_eq!(payload["run_id"], run_id);
    assert_eq!(payload["lane_index"], 0);
    assert_eq!(payload["story_id"], story);
    assert_eq!(payload["kind"], "agent-blocked");
    let reason = payload["reason"].as_str().expect("a reason string");
    assert!(
        reason.contains("the agent stopped and said why"),
        "the hook's own reason must carry the same relayed text as the story's awaiting, \
         never a message recomposed here: {reason}"
    );
}

/// Row 3, wired: the window is gone while the story is still OPEN.
#[test]
fn a_dead_window_on_an_open_story_quarantines_and_names_itself() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "=fixture:=story-SH-1".into(),
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
    // SH-626: the probe's own words travel with the verdict, so "window-gone"
    // can never again stand in for "tmux could not be asked".
    assert!(
        awaiting.contains("scripted: tmux reports `=fixture:=story-SH-1` gone"),
        "the reason carries what tmux actually said: {awaiting}"
    );
}

/// SH-650, wired: the fact is read from the story's own state history. A
/// lane whose story went `verifying` → `in-progress` (the verifier's return)
/// keeps a dead window out of the verdict on a steady pass and reports the
/// deferral; once the story changes state again the same dead window is a
/// hard stop; and a restart pass never defers.
#[test]
fn a_dead_window_on_a_story_the_verifier_just_returned_is_deferred_not_quarantined() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "returned for repair", &[]);
    let gone = || DispatcherStep::WindowAlive {
        window: format!("=fixture:=story-{story}"),
        alive: false,
    };
    let fake = FakeDispatcher::new([gone(), gone(), gone()]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    service
        .set_state(&story, "verifying", None, None, None)
        .unwrap();
    service
        .set_state(&story, "in-progress", None, None, None)
        .unwrap();

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);
    assert_eq!(report.quarantined, [], "the return is not a hard stop");
    assert_eq!(
        report.deferred.len(),
        1,
        "the deferral is reported, never silent"
    );
    assert_eq!(report.deferred[0].0, 0);
    assert!(
        report.deferred[0].1.contains("gone"),
        "{:?}",
        report.deferred
    );
    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(lane.state, EngineLaneState::Working);
    assert!(
        lane.probe_detail
            .as_deref()
            .is_some_and(|detail| detail.contains("gone")),
        "status surfaces what tmux said: {:?}",
        lane.probe_detail
    );
    assert_eq!(awaiting_of(&fixture, 1), None);

    // A restart pass never defers: nobody is left to finish the re-dispatch.
    let restart_ctx = Ctx::new(
        fixture.store(),
        fixture.project(),
        fixture.cwd(),
        fixture.env().clone(),
    )
    .clock(Clock::Fixed(FIXTURE_NOW.to_string()));
    let report = EngineService::new(&restart_ctx, &fake)
        .reconcile_after_restart(&run_id.to_string())
        .unwrap();
    assert_eq!(report.quarantined, [(0, HardStopKind::Interrupted)]);
}

/// SH-650, wired: the deferral ends with the story's next state change. A
/// story a person moved out of `in-progress` after the return is judged by
/// its window again.
#[test]
fn a_dead_window_is_a_hard_stop_again_once_the_returned_story_moves_on() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "moved on after the return", &[]);
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: format!("=fixture:=story-{story}"),
        alive: false,
    }]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    service
        .set_state(&story, "verifying", None, None, None)
        .unwrap();
    service
        .set_state(&story, "in-progress", None, None, None)
        .unwrap();
    service.set_state(&story, "todo", None, None, None).unwrap();
    service
        .set_state(&story, "in-progress", None, None, None)
        .unwrap();

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);
    assert_eq!(report.quarantined, [(0, HardStopKind::WindowGone)]);
    assert_eq!(report.deferred, []);
}

/// D10 is a continuation policy, not only a counter: below the breaker
/// threshold the failed story keeps its diagnosis while the lane immediately
/// takes the next ready story (SH-542).
#[test]
fn a_one_lane_run_continues_after_one_hard_stop() {
    let fixture = ServiceFixture::new();
    let failed = new_story(&fixture, "fails", &[]);
    let next = new_story(&fixture, "continues", &[]);
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{failed}"),
            alive: false,
        },
        DispatcherStep::Dispatch(DispatchOutcome::from_payload(serde_json::json!({
            "ok": true,
            "pane": "%113",
            "window_name": next,
            "worktree_path": "/tmp/wt/next"
        }))),
    ]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &failed);

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.quarantined, [(0, HardStopKind::WindowGone)]);
    assert_eq!(report.filled, [(0, next.clone())]);
    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(lane.state, EngineLaneState::Working);
    assert_eq!(lane.story_id, Some(next));
    assert_eq!(lane.pane_id.as_deref(), Some("%113"));
    assert_eq!(streak(&fixture, &run_id), 1);
    assert!(
        fixture
            .store()
            .read(|tx| tx.story(fixture.project(), storyhook::store::ids::StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .awaiting
            .is_some(),
        "the failed story retains the durable quarantine diagnosis"
    );
}

/// Reusing one lane must not erase the evidence the breaker needs to explain
/// why its third consecutive hard stop halted the run.
#[test]
fn three_sequential_hard_stops_in_one_lane_halt_with_all_three_reasons() {
    let fixture = ServiceFixture::new();
    record_engine_hooks(&fixture);
    let first = new_story(&fixture, "first", &[]);
    let second = new_story(&fixture, "second", &[]);
    let third = new_story(&fixture, "third", &[]);
    let run_id = started_run(&fixture, &FakeDispatcher::default(), 1);
    occupy(&fixture, &run_id, 0, &first);

    for (target, next, pane) in [
        (format!("=fixture:=story-{first}"), &second, "%201"),
        ("%201".to_string(), &third, "%202"),
    ] {
        let fake = FakeDispatcher::new([
            DispatcherStep::WindowAlive {
                window: target,
                alive: false,
            },
            DispatcherStep::Dispatch(DispatchOutcome::from_payload(serde_json::json!({
                "ok": true,
                "pane": pane,
                "window_name": next,
                "worktree_path": format!("/tmp/wt/{next}")
            }))),
        ]);
        let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);
        assert_eq!(report.filled, [(0, next.clone())]);
    }

    let final_stop = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "%202".into(),
        alive: false,
    }]);
    let report = reconcile_at(&fixture, &final_stop, &run_id, FIXTURE_NOW);

    assert_eq!(report.run_state, EngineRunState::Halted);
    let run = fixture
        .store()
        .read(|tx| tx.engine_run(&run_id))
        .unwrap()
        .unwrap();
    assert_eq!(run.recent_quarantines.len(), 3);
    assert_eq!(
        run.recent_quarantines
            .iter()
            .map(|item| item.story_id.as_deref())
            .collect::<Vec<_>>(),
        [
            Some(first.as_str()),
            Some(second.as_str()),
            Some(third.as_str())
        ]
    );
    assert_eq!(lane_at(&fixture, &run_id, 0).story_id, Some(third));

    let halted = fired_engine_hooks(&fixture, "engine_run_halted");
    assert_eq!(halted.len(), 1);
    assert_eq!(
        halted[0]["last_quarantine_reasons"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

/// The precedence case, wired end to end: a finished agent whose pane exited
/// is a COMPLETION, and must never be counted as a hard stop against the
/// breaker that halts the run.
#[test]
fn a_closed_story_with_a_dead_window_completes_rather_than_quarantining() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "=fixture:=story-SH-1".into(),
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
// Row 5, wired: the verifying handoff (SH-521)
// ---------------------------------------------------------------------------

/// A story that reached `verifying` holds its lane rather than being freed,
/// quarantined, or refilled: the story still owns a live worktree and window
/// that only the verifier's own reap reclaims. The pane is already dead, just
/// as it is for an ordinary completion — that must not read as a hard stop,
/// and it must not count toward the breaker.
#[test]
fn a_verifying_story_holds_its_lane_without_quarantine_or_refill() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "=fixture:=story-SH-1".into(),
        alive: false,
    }]);
    let story = new_story(&fixture, "lane work", &[]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);
    StoryService::new(&fixture.ctx())
        .set_state(&story, "verifying", None, None, None)
        .unwrap();

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.verifying, [0]);
    assert!(report.completed.is_empty());
    assert!(report.quarantined.is_empty());
    assert!(report.filled.is_empty());
    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(
        lane.state,
        EngineLaneState::Working,
        "the lane stays occupied through verification"
    );
    assert_eq!(lane.story_id.as_deref(), Some(story.as_str()));
    assert_eq!(
        streak(&fixture, &run_id),
        0,
        "a held handoff is not a hard stop and must not feed the breaker"
    );
    assert_eq!(
        report.run_state,
        EngineRunState::Running,
        "a run with a held lane has not drained"
    );
    assert!(
        !fake
            .calls()
            .iter()
            .any(|c| matches!(c, DispatcherCall::Dispatch(_))),
        "a held lane must never be refilled"
    );
}

/// A held lane does not block other lanes in the same run from filling.
#[test]
fn a_verifying_lane_does_not_block_other_lanes_from_filling() {
    let fixture = ServiceFixture::new();
    let held = new_story(&fixture, "in verification", &[]);
    let ready = new_story(&fixture, "claim me", &[]);
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{held}"),
            alive: false,
        },
        DispatcherStep::Dispatch(DispatchOutcome::from_payload(serde_json::json!({
            "ok": true,
            "window_name": "SH-2",
            "worktree_path": "/tmp/wt/SH-2"
        }))),
    ]);
    let run_id = started_run(&fixture, &fake, 2);
    occupy(&fixture, &run_id, 0, &held);
    StoryService::new(&fixture.ctx())
        .set_state(&held, "verifying", None, None, None)
        .unwrap();

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.verifying, [0]);
    assert_eq!(report.filled, [(1, ready.clone())]);
    let held_lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(held_lane.state, EngineLaneState::Working);
    assert_eq!(held_lane.story_id.as_deref(), Some(held.as_str()));
}

/// SH-822: a verifying story keeps its lane record — the verifier's resume,
/// the SH-650 deferral and drain still read it — but it no longer uses the
/// run's capacity. A one-lane run fills an overflow lane beside it.
#[test]
fn a_verifying_story_frees_capacity_for_an_overflow_lane() {
    let fixture = ServiceFixture::new();
    let held = new_story(&fixture, "in verification", &[]);
    let ready = new_story(&fixture, "claim me", &[]);
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{held}"),
            alive: false,
        },
        DispatcherStep::Dispatch(DispatchOutcome::from_payload(serde_json::json!({
            "ok": true,
            "window_name": "SH-2",
            "worktree_path": "/tmp/wt/SH-2"
        }))),
    ]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &held);
    StoryService::new(&fixture.ctx())
        .set_state(&held, "verifying", None, None, None)
        .unwrap();

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.verifying, [0]);
    assert_eq!(report.filled, [(1, ready.clone())]);
    assert_eq!(
        report.admission_wait,
        Some(AdmissionWait::LanesFull),
        "the new story fills the run's one active lane"
    );
    let held_lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(held_lane.state, EngineLaneState::Working);
    assert_eq!(held_lane.story_id.as_deref(), Some(held.as_str()));
    let overflow = lane_at(&fixture, &run_id, 1);
    assert_eq!(overflow.state, EngineLaneState::Working);
    assert_eq!(overflow.story_id.as_deref(), Some(ready.as_str()));
}

/// SH-822: a story the verifier returns for repair is active work again. It
/// counts against capacity, so a run over its limit admits nothing until a
/// lane frees — the same rule as lowering `--lanes` (SH-672).
#[test]
fn a_returned_story_counts_again_and_blocks_admission() {
    let fixture = ServiceFixture::new();
    let returned = new_story(&fixture, "returned for repair", &[]);
    let overflow = new_story(&fixture, "claimed while it verified", &[]);
    let waiting = new_story(&fixture, "waiting", &[]);
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{returned}"),
            alive: true,
        },
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{overflow}"),
            alive: true,
        },
    ]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &returned);
    occupy_overflow(&fixture, &run_id, 1, &overflow);
    StoryService::new(&fixture.ctx())
        .set_state(&returned, "verifying", None, None, None)
        .unwrap();
    StoryService::new(&fixture.ctx())
        .set_state(&returned, "in-progress", None, None, None)
        .unwrap();

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert!(report.verifying.is_empty());
    assert!(report.filled.is_empty(), "{waiting} must wait: {report:?}");
    assert_eq!(report.admission_wait, Some(AdmissionWait::LanesFull));
    assert!(
        !fake
            .calls()
            .iter()
            .any(|call| matches!(call, DispatcherCall::Dispatch(_))),
        "an over-subscribed run must not dispatch"
    );
}

/// SH-822, council D3: a run holds at most `lanes` stories that wait for
/// central verification. A further claim would add work in progress, not
/// landed stories, while the serial verifier is the bottleneck — and each
/// held worktree keeps its build products. Admission waits, and says why.
#[test]
fn the_verifying_backlog_is_bounded_by_the_lane_count() {
    let fixture = ServiceFixture::new();
    let first = new_story(&fixture, "first handoff", &[]);
    let second = new_story(&fixture, "second handoff", &[]);
    let waiting = new_story(&fixture, "waiting", &[]);
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{first}"),
            alive: false,
        },
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{second}"),
            alive: false,
        },
    ]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &first);
    occupy_overflow(&fixture, &run_id, 1, &second);
    for story in [&first, &second] {
        StoryService::new(&fixture.ctx())
            .set_state(story, "verifying", None, None, None)
            .unwrap();
    }

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.verifying, [0, 1]);
    assert!(report.filled.is_empty(), "{waiting} must wait: {report:?}");
    assert_eq!(
        report.admission_wait,
        Some(AdmissionWait::VerificationBacklog)
    );
    assert_eq!(
        report.census, None,
        "a pass that cannot admit anything does not ask tmux for a census"
    );
    assert_eq!(report.run_state, EngineRunState::Running);
}

/// SH-822: status names the lanes that wait for verification and why a
/// running run admits nothing, so the CLI and the dashboard can say so. A
/// run that is not running reports no admission wait.
#[test]
fn status_names_verifying_lanes_and_the_admission_wait() {
    let fixture = ServiceFixture::new();
    let first = new_story(&fixture, "first handoff", &[]);
    let second = new_story(&fixture, "second handoff", &[]);
    let fake = FakeDispatcher::default();
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &first);
    occupy_overflow(&fixture, &run_id, 1, &second);
    for story in [&first, &second] {
        StoryService::new(&fixture.ctx())
            .set_state(story, "verifying", None, None, None)
            .unwrap();
    }
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);

    let view = service.status(Some(&run_id)).unwrap().remove(0);
    assert_eq!(view.verifying, std::collections::BTreeSet::from([0, 1]));
    assert_eq!(
        view.admission_wait,
        Some(AdmissionWait::VerificationBacklog)
    );
    let cli = storyhook::output::EngineRunView::from_service(view, FIXTURE_NOW);
    assert!(cli.lanes.iter().all(|lane| lane.verifying), "{cli:?}");
    assert_eq!(cli.admission_wait.as_deref(), Some("verification-backlog"));

    service.pause(&run_id).unwrap();
    let paused = service.status(Some(&run_id)).unwrap().remove(0);
    assert_eq!(paused.admission_wait, None, "a paused run is not waiting");
    assert_eq!(paused.verifying.len(), 2);
}

/// SH-822: an overflow lane exists only while it holds a story. When that
/// story completes, the lane retires; the run's configured lanes stay.
#[test]
fn an_overflow_lane_retires_when_its_story_completes() {
    let fixture = ServiceFixture::new();
    let held = new_story(&fixture, "in verification", &[]);
    let finished = new_story(&fixture, "finished on the overflow lane", &[]);
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{held}"),
            alive: false,
        },
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{finished}"),
            alive: false,
        },
    ]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &held);
    occupy_overflow(&fixture, &run_id, 1, &finished);
    StoryService::new(&fixture.ctx())
        .set_state(&held, "verifying", None, None, None)
        .unwrap();
    StoryService::new(&fixture.ctx())
        .set_state(&finished, "verifying", None, None, None)
        .unwrap();
    StoryService::new(&fixture.ctx())
        .comment(
            &finished,
            "CENTRAL VERIFICATION GREEN — merge tree `abc123` passed `make test` and pull request https://github.com/acme/widgets/pull/2 landed.",
        )
        .unwrap();
    StoryService::new(&fixture.ctx())
        .set_state(&finished, "done", None, None, None)
        .unwrap();

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.completed, [1]);
    assert_eq!(report.verifying, [0]);
    let indexes: Vec<u32> = fixture
        .store()
        .read(|tx| tx.engine_lanes(&run_id))
        .unwrap()
        .into_iter()
        .map(|lane| lane.lane_index)
        .collect();
    assert_eq!(indexes, [0], "the overflow lane retires; lane 0 stays");
}

/// The handoff completes: once the verifier actually closes the story, the
/// lane frees exactly like any other completion.
#[test]
fn a_verified_story_that_closes_frees_its_lane() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "=fixture:=story-SH-1".into(),
        alive: false,
    }]);
    let story = new_story(&fixture, "lane work", &[]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);
    StoryService::new(&fixture.ctx())
        .set_state(&story, "verifying", None, None, None)
        .unwrap();
    let held = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);
    assert_eq!(held.verifying, [0], "the handoff is held first");

    // Central verification lands the PR and closes the story: its GREEN
    // verdict is what lets a `verifying` story complete (SH-692) — a bare
    // hand move is an override and is refused without a reason.
    StoryService::new(&fixture.ctx())
        .comment(
            &story,
            "CENTRAL VERIFICATION GREEN — merge tree `abc123` passed `make test` and pull request https://github.com/acme/widgets/pull/1 landed.",
        )
        .unwrap();
    StoryService::new(&fixture.ctx())
        .set_state(&story, "done", None, None, None)
        .unwrap();
    let fake2 = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "=fixture:=story-SH-1".into(),
        alive: true, // irrelevant -- `story_closed` short-circuits before this matters
    }]);
    let report = reconcile_at(&fixture, &fake2, &run_id, FIXTURE_NOW);

    assert_eq!(
        report.completed,
        [0],
        "the handoff completes once the story actually closes"
    );
    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(lane.state, EngineLaneState::Idle);
}

// ---------------------------------------------------------------------------
// A dispatch refusal is a refusal, not a dead window
// ---------------------------------------------------------------------------

/// `fill_idle_lanes` quarantines a claimed story the helper refused to
/// dispatch. The report's own kind must name what actually happened, matching
/// the lane's stored `outcome` — a refusal is not a dead window.
#[test]
fn a_refused_dispatch_is_reported_as_a_refusal_not_a_dead_window() {
    let fixture = ServiceFixture::new();
    record_engine_hooks(&fixture);
    let story = new_story(&fixture, "claim me", &[]);
    let fake = FakeDispatcher::new([DispatcherStep::Dispatch(DispatchOutcome::from_payload(
        serde_json::json!({"ok": false, "display": "worktree already exists"}),
    ))]);
    let run_id = started_run(&fixture, &fake, 1);

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(
        report.quarantined,
        [(0, HardStopKind::DispatchRefused)],
        "a dispatch refusal must not be reported as WindowGone"
    );
    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(lane.state, EngineLaneState::Idle);
    assert_eq!(
        lane.outcome.as_deref(),
        Some("dispatch-refused"),
        "the report and the lane's own stored outcome must spell the same event"
    );
    assert_eq!(lane.outcome_detail.as_deref(), Some(story.as_str()));
    assert_eq!(streak(&fixture, &run_id), 1);
    assert_eq!(report.run_state, EngineRunState::Running);
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.engine_run(&run_id))
            .unwrap()
            .unwrap()
            .recent_quarantines
            .len(),
        1
    );
    let awaiting = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), storyhook::store::ids::StoryNo::new(1)))
        .unwrap()
        .unwrap()
        .awaiting
        .expect("a refused dispatch leaves its diagnosis on the claimed story");
    assert!(awaiting.contains("worktree already exists"), "{awaiting}");

    // A refusal is a full hard-stop producer, including the hook.
    let quarantined_hooks = fired_engine_hooks(&fixture, "engine_lane_quarantined");
    assert_eq!(
        quarantined_hooks.len(),
        1,
        "fill_idle_lanes' own inline quarantine must also fire the hook"
    );
    let payload = &quarantined_hooks[0];
    assert_eq!(payload["kind"], "dispatch-refused");
    assert_eq!(payload["story_id"], story);
    assert_eq!(payload["reason"], "worktree already exists");
}

/// Graceful drain must not wait forever on a quarantined lane: no agent is
/// still running there, and the story already owns the durable diagnosis.
#[test]
fn a_draining_run_clears_a_hard_stopped_lane_and_finishes() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "lane work", &[]);
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: format!("=fixture:=story-{story}"),
        alive: false,
    }]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);
    EngineService::new(&fixture.ctx(), &fake)
        .stop(&run_id, false)
        .unwrap();

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.quarantined, [(0, HardStopKind::WindowGone)]);
    assert_eq!(report.run_state, EngineRunState::Finished);
    assert_eq!(lane_at(&fixture, &run_id, 0).state, EngineLaneState::Idle);
}

/// Pause preserves a stopped lane for inspection; resume makes the next
/// steady pass release it and continue with queued work.
#[test]
fn a_paused_quarantine_is_preserved_until_resume() {
    let fixture = ServiceFixture::new();
    let failed = new_story(&fixture, "failed", &[]);
    let next = new_story(&fixture, "next", &[]);
    let stopped = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: format!("=fixture:=story-{failed}"),
        alive: false,
    }]);
    let run_id = started_run(&fixture, &stopped, 1);
    occupy(&fixture, &run_id, 0, &failed);
    EngineService::new(&fixture.ctx(), &stopped)
        .pause(&run_id)
        .unwrap();

    let paused = reconcile_at(&fixture, &stopped, &run_id, FIXTURE_NOW);
    assert_eq!(paused.run_state, EngineRunState::Paused);
    assert_eq!(
        lane_at(&fixture, &run_id, 0).state,
        EngineLaneState::Quarantined
    );

    EngineService::new(&fixture.ctx(), &FakeDispatcher::default())
        .resume(&run_id)
        .unwrap();
    let resumed = FakeDispatcher::new([DispatcherStep::Dispatch(DispatchOutcome::from_payload(
        serde_json::json!({
            "ok": true,
            "pane": "%301",
            "window_name": next,
            "worktree_path": "/tmp/wt/next"
        }),
    ))]);
    let report = reconcile_at(&fixture, &resumed, &run_id, FIXTURE_NOW);

    assert_eq!(report.filled, [(0, next.clone())]);
    assert_eq!(lane_at(&fixture, &run_id, 0).story_id, Some(next));
}

/// A helper refusal is a hard stop at dispatch time, so three sequential
/// refusals trip the same breaker without needing a later observation pass.
#[test]
fn three_dispatch_refusals_halt_immediately_on_the_third() {
    let fixture = ServiceFixture::new();
    for title in ["first", "second", "third"] {
        new_story(&fixture, title, &[]);
    }
    let run_id = started_run(&fixture, &FakeDispatcher::default(), 1);

    for expected in 1..=3 {
        let refused = FakeDispatcher::new([DispatcherStep::Dispatch(
            DispatchOutcome::from_payload(serde_json::json!({
                "ok": false,
                "display": format!("refusal {expected}")
            })),
        )]);
        let report = reconcile_at(&fixture, &refused, &run_id, FIXTURE_NOW);
        assert_eq!(streak(&fixture, &run_id), expected);
        assert_eq!(
            report.run_state,
            if expected == 3 {
                EngineRunState::Halted
            } else {
                EngineRunState::Running
            }
        );
    }

    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(lane.state, EngineLaneState::Quarantined);
    assert_eq!(lane.outcome.as_deref(), Some("dispatch-refused"));
}

/// SH-774: a dispatch that failed outright (here a helper timeout) was
/// propagated with `?`, leaving the lane dispatching with the story claimed
/// forever. It is now quarantined like a refusal, with the failure as the
/// story's awaiting reason, and it counts toward the breaker.
#[test]
fn a_failed_dispatch_is_quarantined_and_counted_like_a_refusal() {
    let fixture = ServiceFixture::new();
    new_story(&fixture, "fails at dispatch", &[]);
    let run_id = started_run(&fixture, &FakeDispatcher::default(), 1);
    let failing = FakeDispatcher::new([DispatcherStep::DispatchFailure(
        "story.sh timed out after 180s".into(),
    )]);

    let report = reconcile_at(&fixture, &failing, &run_id, FIXTURE_NOW);

    assert_eq!(report.quarantined, [(0, HardStopKind::DispatchRefused)]);
    assert_eq!(streak(&fixture, &run_id), 1);
    // As after a refusal, a running pass then frees the quarantined lane;
    // the story keeps its claim and says why.
    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(lane.state, EngineLaneState::Idle);
    assert_eq!(lane.outcome.as_deref(), Some("dispatch-refused"));
    assert_eq!(lane.outcome_detail.as_deref(), Some("SH-1"));
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), storyhook::store::ids::StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "in-progress");
    assert_eq!(
        row.awaiting.as_deref(),
        Some("dispatch failed: story.sh timed out after 180s")
    );
}

/// SH-774: a card reset reserved while the engine dispatched made the
/// refusal's awaiting write fail ("reset in progress"), which left the lane
/// dispatching forever: observation skips a story under reset, and the card
/// reset's own quiesce then waited on that lane. The reset owns the story
/// now, so the refusal frees the lane instead of quarantining it.
#[test]
fn a_dispatch_refused_after_a_card_reset_reserved_its_story_releases_the_lane() {
    use storyhook::error::AppError;
    use storyhook::service::engine::{DispatchRequest, Dispatcher, UnclaimRequest, WindowProbe};
    use storyhook::service::story_reset::StoryResetService;

    struct ReservesThenRefuses<'a> {
        fixture: &'a ServiceFixture,
    }
    impl Dispatcher for ReservesThenRefuses<'_> {
        fn dispatch(&self, request: DispatchRequest) -> Result<DispatchOutcome, AppError> {
            StoryResetService::new(&self.fixture.ctx())
                .reserve(&request.story, &request.story)
                .unwrap();
            Ok(DispatchOutcome::from_payload(
                serde_json::json!({"ok": false, "display": "refused after the reset"}),
            ))
        }
        fn unclaim(&self, _: UnclaimRequest) -> Result<DispatchOutcome, AppError> {
            panic!("the engine never unclaims here")
        }
        fn probe_window(&self, _: &str) -> WindowProbe {
            panic!("no lane is observed")
        }
        fn kill_window(&self, _: &str) -> Result<(), AppError> {
            panic!("no window is proven owned")
        }
        fn census(&self) -> WindowCensus {
            WindowCensus::Counted { windows: vec![] }
        }
    }

    let fixture = ServiceFixture::new();
    new_story(&fixture, "reset while dispatching", &[]);
    let run_id = started_run(&fixture, &FakeDispatcher::default(), 1);
    let ctx = fixture.ctx();
    let dispatcher = ReservesThenRefuses { fixture: &fixture };

    let report = EngineService::new(&ctx, &dispatcher)
        .reconcile(&run_id)
        .unwrap();

    assert!(report.quarantined.is_empty(), "{:?}", report.quarantined);
    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(lane.state, EngineLaneState::Idle);
    assert_eq!(
        lane.outcome_detail.as_deref(),
        Some("refused after the reset")
    );
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), storyhook::store::ids::StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert!(row.awaiting.is_none(), "the card reset owns the story");
}

/// SH-774: the post-dispatch lane write was unconditional, so a dispatch
/// result could bring back a lane that another writer had released while
/// the helper ran. It is now a compare-and-swap, and a lost race fails loud
/// with the unowned window and worktree named.
#[test]
fn a_dispatch_result_never_overwrites_a_lane_released_meanwhile() {
    use storyhook::error::AppError;
    use storyhook::service::engine::{DispatchRequest, Dispatcher, UnclaimRequest, WindowProbe};

    struct ReleasedMeanwhile<'a> {
        fixture: &'a ServiceFixture,
        run_id: String,
    }
    impl Dispatcher for ReleasedMeanwhile<'_> {
        fn dispatch(&self, request: DispatchRequest) -> Result<DispatchOutcome, AppError> {
            let mut lane = lane_at(self.fixture, &self.run_id, 0);
            lane.state = EngineLaneState::Idle;
            lane.story_id = None;
            lane.dispatched_at = None;
            lane.outcome = Some("released elsewhere".into());
            self.fixture
                .store()
                .write(|tx| tx.put_engine_lane(&lane))
                .unwrap();
            Ok(DispatchOutcome::from_payload(serde_json::json!({
                "ok": true,
                "window_name": request.story,
                "worktree_path": "/owned/late",
            })))
        }
        fn unclaim(&self, _: UnclaimRequest) -> Result<DispatchOutcome, AppError> {
            panic!("the engine never unclaims here")
        }
        fn probe_window(&self, _: &str) -> WindowProbe {
            panic!("no lane is observed")
        }
        fn kill_window(&self, _: &str) -> Result<(), AppError> {
            panic!("no window is proven owned")
        }
        fn census(&self) -> WindowCensus {
            WindowCensus::Counted { windows: vec![] }
        }
    }

    let fixture = ServiceFixture::new();
    new_story(&fixture, "late result", &[]);
    let run_id = started_run(&fixture, &FakeDispatcher::default(), 1);
    let ctx = fixture.ctx();
    let dispatcher = ReleasedMeanwhile {
        fixture: &fixture,
        run_id: run_id.clone(),
    };

    let error = EngineService::new(&ctx, &dispatcher)
        .reconcile(&run_id)
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("changed while story `SH-1` dispatched"),
        "{error}"
    );
    assert!(error.contains("/owned/late"), "{error}");
    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(lane.state, EngineLaneState::Idle);
    assert_eq!(lane.outcome.as_deref(), Some("released elsewhere"));
}

/// SH-774: the breaker leaves the third refused lane quarantined, with its
/// story claimed and no cleanup lease. Stop Now on that halted run failed on
/// every attempt ("cannot reset legacy lane"); it now releases the lane,
/// keeps the refused story's claim and diagnosis, and finishes the run.
#[test]
fn stop_now_after_the_breaker_trips_on_refusals_finishes() {
    let fixture = ServiceFixture::new();
    for title in ["first", "second", "third"] {
        new_story(&fixture, title, &[]);
    }
    let run_id = started_run(&fixture, &FakeDispatcher::default(), 1);
    for expected in 1..=3 {
        let refused = FakeDispatcher::new([DispatcherStep::Dispatch(
            DispatchOutcome::from_payload(serde_json::json!({
                "ok": false,
                "display": format!("refusal {expected}")
            })),
        )]);
        reconcile_at(&fixture, &refused, &run_id, FIXTURE_NOW);
    }
    assert_eq!(lane_at(&fixture, &run_id, 0).cleanup_lease, None);

    let ctx = fixture.ctx();
    // Any helper call would be unscripted, and the fake would panic.
    let stopped = EngineService::new(&ctx, &FakeDispatcher::default())
        .stop(&run_id, true)
        .unwrap();

    assert_eq!(stopped.run.state, EngineRunState::Finished);
    assert_eq!(lane_at(&fixture, &run_id, 0).state, EngineLaneState::Idle);
    let third = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), storyhook::store::ids::StoryNo::new(3)))
        .unwrap()
        .unwrap();
    assert_eq!(third.state, "in-progress");
    assert!(
        third.awaiting.as_deref().unwrap().contains("refusal 3"),
        "{:?}",
        third.awaiting
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
    record_engine_hooks(&fixture);
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

    let quarantine_hooks = fired_engine_hooks(&fixture, "engine_lane_quarantined");
    assert_eq!(
        quarantine_hooks.len(),
        3,
        "each of the three hard stops fires its own lane hook"
    );

    let halted_hooks = fired_engine_hooks(&fixture, "engine_run_halted");
    assert_eq!(halted_hooks.len(), 1);
    let payload = &halted_hooks[0];
    assert_eq!(payload["run_id"], run_id);
    assert_eq!(payload["consecutive_hard_stops"], 3);
    let reasons = payload["last_quarantine_reasons"]
        .as_array()
        .expect("a reasons array");
    assert_eq!(
        reasons.len(),
        3,
        "all three quarantined lanes fit inside the last-three window"
    );
    for reason in reasons {
        let text = reason.as_str().expect("a reason line");
        assert!(
            text.contains("window-gone"),
            "each reason names its kind: {text}"
        );
    }
}

/// Two hard stops do not halt, and the run keeps going.
#[test]
fn two_hard_stops_leave_the_run_running() {
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
    let _human = new_story(&fixture, "human", &["no-auto"]);
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
    let _human = new_story(&fixture, "human", &["no-auto"]);
    let fake = FakeDispatcher::new([
        // Pass 1: two dead windows -> streak 2, one short of the breaker.
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{a}"),
            alive: false,
        },
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{b}"),
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
        window: format!("=fixture:=story-{c}"),
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
    assert!(
        fixture
            .store()
            .read(|tx| tx.engine_run(&run_id))
            .unwrap()
            .unwrap()
            .recent_quarantines
            .is_empty(),
        "a completion starts a new consecutive series"
    );
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

/// A run whose entire backlog is `no-auto` does not auto-finish. "Nothing
/// claimable" and "nothing left, ever" are different facts: the engine
/// skips `no-auto` deliberately for a human to act on (D12), and the run
/// must still be there for `engine status` to report it against when they
/// check.
///
/// This is the regression the CLI grammar suite found: before this fix, a
/// project whose only story was `no-auto` had its run finish the instant
/// `story engine start` created it — a store write wakes the reconcile
/// loop synchronously at the SAME request boundary that made it (SH-202),
/// so the run was routinely gone before even the operator's own next
/// command reached the daemon, leaving `engine status` reporting "no live
/// engine run" with no trace beyond `start`'s own JSON response.
#[test]
fn a_run_whose_only_backlog_is_no_auto_stays_running_rather_than_draining() {
    let fixture = ServiceFixture::new();
    let _parked = new_story(&fixture, "human work", &["no-auto"]);
    let fake = FakeDispatcher::default();
    let run_id = started_run(&fixture, &fake, 1);

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert!(report.filled.is_empty(), "the engine claimed nothing");
    assert_eq!(
        run_state(&fixture, &run_id),
        EngineRunState::Running,
        "a no-auto item is still actionable by a human, so the run must not finish out from under it"
    );
    assert_eq!(report.stop_reason, None);
}

/// The control for the fix above, exercising the OTHER caller of
/// [`EngineService::finish_if_drained`]: a `draining` run (an operator's own
/// graceful `stop` while a lane was still occupied) still finishes once
/// that lane frees, even with a `no-auto` item still parked in the backlog.
/// Graceful `stop` is an explicit decision to end the run; the guard this
/// story adds must gate only the AUTOMATIC "nothing claimable" path
/// (`Running`), never the operator's own wind-down (`Draining`) — proven
/// here by reaching `finish_if_drained` through reconcile's own draining
/// branch rather than through `stop` directly, since `stop(now: false)`
/// has its own separate, unaffected finish check and never calls
/// `finish_if_drained` at all.
#[test]
fn a_draining_run_still_finishes_once_its_lane_clears_despite_a_parked_no_auto_story() {
    let fixture = ServiceFixture::new();
    let _parked = new_story(&fixture, "human work", &["no-auto"]);
    let working = new_story(&fixture, "in flight", &[]);
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: format!("=fixture:=story-{working}"),
        alive: true,
    }]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &working);
    // The occupied lane keeps `stop` from finishing directly; it drains.
    let stopped = EngineService::new(&fixture.ctx(), &fake)
        .stop(&run_id, false)
        .unwrap();
    assert_eq!(stopped.run.state, EngineRunState::Draining);

    // The lane's story completes, freeing the only occupied lane.
    StoryService::new(&fixture.ctx())
        .set_state(&working, "done", None, None, None)
        .unwrap();
    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.completed, [0]);
    assert_eq!(
        report.run_state,
        EngineRunState::Finished,
        "a draining run is not gated on unclaimed no-auto work"
    );
    assert_eq!(report.stop_reason.as_deref(), Some(OPERATOR_STOPPED));
}

/// The stored row of story `number`, for before-and-after comparisons that
/// prove a pass wrote nothing to it.
fn story_row(fixture: &ServiceFixture, number: i64) -> storyhook::store::StoryRow {
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
}

fn label(fixture: &ServiceFixture, story: &str, label: &str) {
    StoryService::new(&fixture.ctx())
        .set_labels(story, &[label.to_string()], &[])
        .unwrap();
}

/// The live SH-837 case: engine-dispatched stories moved to `verifying`, then
/// an operator labelled them `human-only` as a verifier hold. The verifier
/// never touches such a story, so the held lane never freed — three of four
/// lanes of one run sat on them for 71 hours.
///
/// Now the lane is released to the operator, and nothing else happens: the
/// story's row is byte-for-byte what it was (no `awaiting`, no comment, no
/// state change), nothing is cleaned up or dispatched, and the breaker does
/// not move. With nothing else to do, the run then drains (decision D6).
#[test]
fn a_lane_whose_verifying_story_gains_human_only_is_released_without_cleanup() {
    let fixture = ServiceFixture::new();
    let window = "=fixture:=story-SH-1".to_string();
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowAlive {
            window: window.clone(),
            alive: false,
        },
        DispatcherStep::WindowAlive {
            window,
            alive: false,
        },
    ]);
    let story = new_story(&fixture, "lane work", &[]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);
    StoryService::new(&fixture.ctx())
        .set_state(&story, "verifying", None, None, None)
        .unwrap();
    let held = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);
    assert_eq!(
        held.verifying,
        [0],
        "control: an unlabelled handoff is held"
    );

    label(&fixture, &story, LABEL_HUMAN_ONLY);
    let before = story_row(&fixture, 1);
    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.reserved, [(0, LABEL_HUMAN_ONLY)]);
    assert!(report.verifying.is_empty());
    assert!(report.quarantined.is_empty());
    assert!(report.completed.is_empty());
    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(lane.state, EngineLaneState::Idle);
    assert_eq!(lane.story_id, None);
    assert_eq!(lane.outcome.as_deref(), Some(RESERVED));
    assert_eq!(lane.outcome_detail.as_deref(), Some(story.as_str()));
    assert_eq!(
        story_row(&fixture, 1),
        before,
        "the story is left exactly as the operator left it"
    );
    assert_eq!(streak(&fixture, &run_id), 0);
    assert!(
        fake.calls()
            .iter()
            .all(|call| matches!(call, DispatcherCall::WindowAlive(_))),
        "a release cleans up nothing and dispatches nothing: {:?}",
        fake.calls()
    );
    assert_eq!(report.run_state, EngineRunState::Finished);
    assert_eq!(report.stop_reason.as_deref(), Some(QUEUE_DRAINED));
}

/// Council D7: an in-progress story that gains `no-auto` while its agent is
/// live keeps its lane — the run does not interrupt it or overshoot its
/// capacity — and says so on the story exactly once, naming `story block`.
/// When the agent's pane goes away, the lane is released instead of
/// quarantined, and nothing ever relaunches the story.
#[test]
fn a_working_lane_whose_story_gains_no_auto_is_held_while_live_then_released() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "in flight", &[]);
    let window = format!("=fixture:=story-{story}");
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowAlive {
            window: window.clone(),
            alive: true,
        },
        DispatcherStep::WindowAlive {
            window: window.clone(),
            alive: true,
        },
        DispatcherStep::WindowAlive {
            window,
            alive: false,
        },
    ]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);
    StoryService::new(&fixture.ctx())
        .set_state(&story, "in-progress", None, None, None)
        .unwrap();
    label(&fixture, &story, LABEL_NO_AUTO);

    let holds = |fixture: &ServiceFixture| {
        story_row(fixture, 1)
            .snapshot
            .comments
            .iter()
            .filter(|comment| comment.text.starts_with(RESERVED_HOLD_PREFIX))
            .map(|comment| comment.text.clone())
            .collect::<Vec<_>>()
    };
    for pass in 0..2 {
        let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);
        assert!(report.reserved.is_empty(), "pass {pass}");
        assert!(report.quarantined.is_empty(), "pass {pass}");
        assert_eq!(
            lane_at(&fixture, &run_id, 0).state,
            EngineLaneState::Working,
            "pass {pass}: a live agent keeps its lane"
        );
        let holds = holds(&fixture);
        assert_eq!(holds.len(), 1, "pass {pass}: one comment, never more");
        for needle in [
            format!("run `{run_id}` lane 0"),
            format!("`{LABEL_NO_AUTO}`"),
            "window `story-SH-1`".to_string(),
            "Full Auto mode".to_string(),
            format!("`story block {story} <reason>`"),
        ] {
            assert!(holds[0].contains(&needle), "{needle:?} in {:?}", holds[0]);
        }
    }

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.reserved, [(0, LABEL_NO_AUTO)]);
    assert!(report.quarantined.is_empty());
    assert_eq!(lane_at(&fixture, &run_id, 0).state, EngineLaneState::Idle);
    assert_eq!(story_row(&fixture, 1).awaiting, None);
    assert_eq!(story_row(&fixture, 1).state, "in-progress");
    assert_eq!(streak(&fixture, &run_id), 0);
    assert!(
        !fake
            .calls()
            .iter()
            .any(|call| matches!(call, DispatcherCall::Dispatch(_))),
        "a reserved story is never relaunched: {:?}",
        fake.calls()
    );
}

/// A release is neither a failure nor a completion, so it must leave an
/// existing streak exactly where it was: a strike would halt runs over
/// operator decisions, and a reset would hide real failures.
#[test]
fn a_reserved_release_neither_strikes_nor_resets_the_breaker() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "held", &[]);
    // Keeps the run `Running` after the release, so the streak is read
    // from a live run.
    let _parked = new_story(&fixture, "human work", &[LABEL_NO_AUTO]);
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: format!("=fixture:=story-{story}"),
        alive: false,
    }]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);
    fixture
        .store()
        .write(|tx| {
            let mut run = tx.engine_run(&run_id)?.unwrap();
            run.consecutive_hard_stops = 2;
            tx.update_engine_run(&run)
        })
        .unwrap();
    label(&fixture, &story, LABEL_HUMAN_ONLY);

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.reserved, [(0, LABEL_HUMAN_ONLY)]);
    assert_eq!(streak(&fixture, &run_id), 2);
    assert_eq!(report.run_state, EngineRunState::Running);
}

/// A story whose agent blocked it keeps its own words: the release relays
/// nothing and replaces nothing (SH-120), where a quarantine would have
/// appended the engine's provenance to them.
#[test]
fn a_reserved_label_on_an_awaiting_story_leaves_its_reason_verbatim() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "blocked", &[]);
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: format!("=fixture:=story-{story}"),
        alive: true,
    }]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);
    StoryService::new(&fixture.ctx())
        .set_awaiting(&story, "the agent said why")
        .unwrap();
    label(&fixture, &story, LABEL_HUMAN_ONLY);

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.reserved, [(0, LABEL_HUMAN_ONLY)]);
    assert!(report.quarantined.is_empty());
    assert_eq!(
        awaiting_of(&fixture, 1).as_deref(),
        Some("the agent said why")
    );
}

/// A released lane is an idle lane: the same pass fills it with other ready
/// work, and never with the reserved story.
#[test]
fn a_released_reserved_lane_refills_with_other_ready_work() {
    let fixture = ServiceFixture::new();
    let held = new_story(&fixture, "held", &[]);
    let next = new_story(&fixture, "next", &[]);
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{held}"),
            alive: false,
        },
        DispatcherStep::Dispatch(DispatchOutcome::from_payload(serde_json::json!({
            "ok": true,
            "pane": "%7",
            "window_name": next,
            "worktree_path": format!("/tmp/wt/{next}"),
            "cleanup_lease": cleanup_lease(&next, &format!("/tmp/wt/{next}")),
        }))),
    ]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &held);
    StoryService::new(&fixture.ctx())
        .set_state(&held, "verifying", None, None, None)
        .unwrap();
    label(&fixture, &held, LABEL_HUMAN_ONLY);

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.reserved, [(0, LABEL_HUMAN_ONLY)]);
    assert_eq!(report.filled, [(0, next.clone())]);
    assert_eq!(lane_at(&fixture, &run_id, 0).story_id, Some(next));
}

/// An operator's graceful stop ends the run once its lanes clear, and a
/// reserved release clears a lane like any other ending.
#[test]
fn a_draining_run_finishes_once_its_lane_is_released_for_a_reserved_label() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "in flight", &[]);
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: format!("=fixture:=story-{story}"),
        alive: false,
    }]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);
    let stopped = EngineService::new(&fixture.ctx(), &fake)
        .stop(&run_id, false)
        .unwrap();
    assert_eq!(stopped.run.state, EngineRunState::Draining);
    StoryService::new(&fixture.ctx())
        .set_state(&story, "verifying", None, None, None)
        .unwrap();
    label(&fixture, &story, LABEL_HUMAN_ONLY);

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.reserved, [(0, LABEL_HUMAN_ONLY)]);
    assert_eq!(report.run_state, EngineRunState::Finished);
    assert_eq!(report.stop_reason.as_deref(), Some(OPERATOR_STOPPED));
}

/// A dispatcher that labels the story `no-auto` while the engine dispatches
/// it — the one window between the engine's claim, which admits no reserved
/// story, and `story.sh`, which refuses `--full-auto` for one.
struct LabelledDuringDispatch {
    store_path: std::path::PathBuf,
    project: storyhook::store::ProjectId,
    cwd: std::path::PathBuf,
    env: storyhook::env::Environment,
    inner: FakeDispatcher,
}

impl Dispatcher for LabelledDuringDispatch {
    fn dispatch(
        &self,
        request: storyhook::service::engine::DispatchRequest,
    ) -> Result<DispatchOutcome, storyhook::error::AppError> {
        let store = storyhook::store::SqliteStore::open(&self.store_path).unwrap();
        let ctx = Ctx::new(&store, self.project, &self.cwd, self.env.clone());
        StoryService::new(&ctx)
            .set_labels(&request.story, &[LABEL_NO_AUTO.to_string()], &[])
            .unwrap();
        self.inner.dispatch(request)
    }

    fn unclaim(
        &self,
        request: storyhook::service::engine::UnclaimRequest,
    ) -> Result<DispatchOutcome, storyhook::error::AppError> {
        self.inner.unclaim(request)
    }

    fn probe_window(&self, window: &str) -> WindowProbe {
        self.inner.probe_window(window)
    }

    fn kill_window(&self, window: &str) -> Result<(), storyhook::error::AppError> {
        self.inner.kill_window(window)
    }

    fn census(&self) -> WindowCensus {
        self.inner.census()
    }
}

/// Decision D4: when the label lands between the claim and the dispatch,
/// `story.sh` refuses, and the lane is released as `reserved` without a
/// strike or a quarantine. The refusal goes onto `awaiting` verbatim, so the
/// person the story is left for sees why a claimed story has no agent.
#[test]
fn a_reserved_label_refusal_after_claim_releases_without_a_strike() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "labelled mid-dispatch", &[]);
    let refusal = "story SH-1 carries `no-auto`; a Full Auto lane never works it.";
    let dispatcher = LabelledDuringDispatch {
        store_path: fixture.env().store_path().to_path_buf(),
        project: fixture.project(),
        cwd: fixture.cwd().to_path_buf(),
        env: fixture.env().clone(),
        inner: FakeDispatcher::new([DispatcherStep::Dispatch(DispatchOutcome::from_payload(
            serde_json::json!({"ok": false, "reason": "reserved-label", "display": refusal}),
        ))]),
    };
    let run_id = EngineService::new(&fixture.ctx(), &dispatcher)
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 1,
            agent: EngineAgent::Codex,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap()
        .id;

    let report = EngineService::new(&fixture.ctx(), &dispatcher)
        .reconcile(&run_id)
        .unwrap();

    assert!(report.filled.is_empty());
    assert!(report.quarantined.is_empty(), "{report:?}");
    assert_eq!(streak(&fixture, &run_id), 0);
    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(lane.state, EngineLaneState::Idle);
    assert_eq!(lane.outcome.as_deref(), Some(RESERVED));
    assert_eq!(lane.outcome_detail.as_deref(), Some(story.as_str()));
    let row = story_row(&fixture, 1);
    assert_eq!(row.awaiting.as_deref(), Some(refusal));
    assert_eq!(row.state, "in-progress", "the claim stays visible");
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
    let lease = cleanup_lease(&story, "/tmp/wt/SH-1");
    let fake = FakeDispatcher::new([DispatcherStep::Dispatch(DispatchOutcome::from_payload(
        serde_json::json!({
            "ok": true,
            "pane": "%112",
            "window_name": "SH-1",
            "worktree_path": "/tmp/wt/SH-1",
            "cleanup_lease": lease
        }),
    ))]);
    let run_id = started_run(&fixture, &fake, 1);

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.filled, [(0, story.clone())]);
    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(lane.state, EngineLaneState::Working);
    assert_eq!(lane.story_id.as_deref(), Some(story.as_str()));
    assert_eq!(lane.pane_id.as_deref(), Some("%112"));
    assert_eq!(lane.window_name.as_deref(), Some("SH-1"));
    assert_eq!(lane.worktree_path.as_deref(), Some("/tmp/wt/SH-1"));
    assert_eq!(
        lane.cleanup_lease,
        Some(cleanup_lease(&story, "/tmp/wt/SH-1"))
    );
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

#[test]
fn a_run_uses_the_latest_configuration_for_future_claims() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "configured claim", &[]);
    let fake = FakeDispatcher::new([DispatcherStep::Dispatch(DispatchOutcome::from_payload(
        serde_json::json!({
            "ok": true,
            "window_name": story,
            "worktree_path": "/tmp/wt/configured"
        }),
    ))]);
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);
    let run_id = service
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 1,
            agent: EngineAgent::Codex,
            model: Some("gpt-5.3-codex".into()),
            effort: Some("high".into()),
            speed: Some(EngineSpeed::Fast),
        })
        .unwrap()
        .id;
    service
        .configure(
            &run_id,
            ConfigureRequest {
                lanes: 1,
                agent: EngineAgent::Claude,
                model: Some("claude-opus-4-6".into()),
                effort: Some("max".into()),
                speed: Some(EngineSpeed::Standard),
            },
        )
        .unwrap();

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.filled, [(0, story.clone())]);
    assert!(fake.calls().contains(&DispatcherCall::Dispatch(
        storyhook::service::engine::DispatchRequest {
            project: "fixture".into(),
            story,
            agent: EngineAgent::Claude,
            model: Some("claude-opus-4-6".into()),
            effort: Some("max".into()),
            speed: Some(EngineSpeed::Standard),
        }
    )));
}

#[test]
fn a_reduced_lane_cap_blocks_refill_until_occupancy_falls_below_it() {
    let fixture = ServiceFixture::new();
    let occupied_a = new_story(&fixture, "occupied a", &[]);
    let occupied_b = new_story(&fixture, "occupied b", &[]);
    let waiting = new_story(&fixture, "waiting", &[]);
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{occupied_a}"),
            alive: true,
        },
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{occupied_b}"),
            alive: true,
        },
    ]);
    let run_id = started_run(&fixture, &fake, 3);
    occupy(&fixture, &run_id, 0, &occupied_a);
    occupy(&fixture, &run_id, 1, &occupied_b);
    let ctx = fixture.ctx();
    EngineService::new(&ctx, &fake)
        .configure(
            &run_id,
            ConfigureRequest {
                lanes: 2,
                agent: EngineAgent::Codex,
                model: None,
                effort: None,
                speed: None,
            },
        )
        .unwrap();

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert!(report.filled.is_empty());
    assert_eq!(run_state(&fixture, &run_id), EngineRunState::Running);
    assert!(
        !fake
            .calls()
            .iter()
            .any(|call| matches!(call, DispatcherCall::Dispatch(_))),
        "{waiting} must remain ready while occupancy equals the reduced cap"
    );
}

#[test]
fn a_surplus_occupied_lane_retires_after_its_story_completes() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "surplus occupied lane", &[]);
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: format!("=fixture:=story-{story}"),
        alive: true,
    }]);
    let run_id = started_run(&fixture, &fake, 2);
    occupy(&fixture, &run_id, 1, &story);
    let ctx = fixture.ctx();
    EngineService::new(&ctx, &fake)
        .configure(
            &run_id,
            ConfigureRequest {
                lanes: 1,
                agent: EngineAgent::Codex,
                model: None,
                effort: None,
                speed: None,
            },
        )
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&story, "done", None, None, None)
        .unwrap();

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert_eq!(report.completed, [1]);
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.engine_lanes(&run_id))
            .unwrap()
            .into_iter()
            .map(|lane| lane.lane_index)
            .collect::<Vec<_>>(),
        [0],
        "a lane above the new cap remains occupied until completion, then disappears"
    );
}

/// The helper returns the pane id that tmux guarantees is stable for the
/// pane lifetime. Reconciliation must use it instead of heuristically
/// resolving the human-readable window name (SH-542).
#[test]
fn a_dispatched_lane_is_observed_through_its_exact_pane_id() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "claim me", &[]);
    let dispatch = FakeDispatcher::new([DispatcherStep::Dispatch(DispatchOutcome::from_payload(
        serde_json::json!({
            "ok": true,
            "pane": "%112",
            "window_name": "SH-1",
            "worktree_path": "/tmp/wt/SH-1"
        }),
    ))]);
    let run_id = started_run(&fixture, &dispatch, 1);
    reconcile_at(&fixture, &dispatch, &run_id, FIXTURE_NOW);

    let observer = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "%112".into(),
        alive: true,
    }]);
    let report = reconcile_at(&fixture, &observer, &run_id, FIXTURE_NOW);

    assert!(report.quarantined.is_empty());
    assert_eq!(lane_at(&fixture, &run_id, 0).story_id, Some(story));
}

/// SH-626, wired: a probe tmux could not answer leaves the lane working,
/// blocks nothing, counts toward nothing, and is loud on the status surface
/// — the probe's own words are on the lane and in the pass report — until
/// tmux answers again, when the lane clears them.
#[test]
fn an_unanswered_probe_leaves_the_lane_working_and_names_itself_on_the_lane() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "lane behind a broken tmux", &[]);
    let detail = "tmux exited exit status: 1 answering the liveness probe for `%1`: FAKE_TMUX_IMPLEMENTATION: unbound variable";
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowUnanswered {
            window: format!("=fixture:=story-{story}"),
            detail: detail.to_string(),
        },
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{story}"),
            alive: true,
        },
    ]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);

    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);

    assert!(
        report.quarantined.is_empty(),
        "an unanswered probe is not a verdict"
    );
    assert_eq!(report.unanswered, [(0, detail.to_string())]);
    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(lane.state, EngineLaneState::Working);
    assert_eq!(lane.story_id.as_deref(), Some(story.as_str()));
    assert_eq!(lane.probe_detail.as_deref(), Some(detail));
    assert!(
        lane.last_progress_seq.is_some(),
        "the stall clock is seeded exactly as it would be for an answered probe"
    );
    assert_eq!(
        awaiting_of(&fixture, 1),
        None,
        "nothing was written onto the story"
    );
    assert_eq!(run_of(&fixture, &run_id).consecutive_hard_stops, 0);

    // tmux answers again: the lane clears what it said.
    let recovered = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);
    assert!(recovered.unanswered.is_empty());
    assert_eq!(lane_at(&fixture, &run_id, 0).probe_detail, None);
}

/// SH-626's backstop, wired: with the probe unanswerable for the whole life
/// of a lane, a story that stops moving is still quarantined at the stall
/// ceiling, and the block reason carries what tmux said so the reader knows
/// the window's own state was never observed.
#[test]
fn a_dead_lane_behind_an_unanswerable_probe_is_caught_by_the_stall_ceiling() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "dead lane behind a broken tmux", &[]);
    let detail = "tmux did not answer the liveness probe for `%1` within 3s";
    let unanswered = || DispatcherStep::WindowUnanswered {
        window: format!("=fixture:=story-{story}"),
        detail: detail.to_string(),
    };
    let fake = FakeDispatcher::new([unanswered(), unanswered()]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);

    let seeded = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);
    assert!(seeded.quarantined.is_empty());

    let past = "2026-01-02T00:00:00Z";
    let report = reconcile_at(&fixture, &fake, &run_id, past);

    assert_eq!(report.quarantined, [(0, HardStopKind::Stalled)]);
    let awaiting = awaiting_of(&fixture, 1).expect("a stalled story carries a reason");
    assert!(
        awaiting.contains("stalled") && awaiting.contains(detail),
        "the stall reason names the probe that could not be asked: {awaiting}"
    );
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
            window: format!("=fixture:=story-{story}"),
            alive: true,
        },
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{story}"),
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
            window: format!("=fixture:=story-{story}"),
            alive: true,
        },
        DispatcherStep::WindowAlive {
            window: format!("=fixture:=story-{story}"),
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

// ---------------------------------------------------------------------------
// The second stall channel, wired (SH-657)
// ---------------------------------------------------------------------------

/// The unix second of `FIXTURE_NOW`, for scripting a pty stamp against the
/// fixture clock.
fn fixture_now_unix() -> i64 {
    chrono::DateTime::parse_from_rfc3339(FIXTURE_NOW)
        .unwrap()
        .timestamp()
}

/// The incident replayed (SH-657, run a64495a6): a lane is filled, the story
/// gets its dispatch comment, and then the store hears nothing for longer than
/// the ceiling while the agent reads code and plans — its pane writing to the
/// terminal the whole time. Before this fix that was three quarantines, a
/// tripped breaker and a halted run; it is a working lane.
#[test]
fn a_store_silent_lane_whose_pane_keeps_writing_is_not_stalled() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "planning in silence", &[]);
    let window = format!("=fixture:=story-{story}");
    // Past the ceiling on the store, but the pane wrote one second before
    // the pass looked — the sub-second cadence measured on every working lane.
    let past_secs = i64::try_from(STALL_CEILING_SECS * 2).unwrap();
    let past = chrono::DateTime::from_timestamp(fixture_now_unix() + past_secs, 0)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let output_at = fixture_now_unix() + past_secs - 1;
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowAlive {
            window: window.clone(),
            alive: true,
        },
        DispatcherStep::WindowActive {
            window,
            last_output_at: output_at,
        },
    ]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);

    let seeded = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);
    assert!(seeded.quarantined.is_empty());
    let before = lane_at(&fixture, &run_id, 0);

    let report = reconcile_at(&fixture, &fake, &run_id, &past);

    assert!(
        report.quarantined.is_empty(),
        "a pane that wrote inside the ceiling is a working lane, however silent the store: {report:?}"
    );
    let lane = lane_at(&fixture, &run_id, 0);
    assert_eq!(lane.state, EngineLaneState::Working);
    assert_eq!(
        lane.last_progress_seq, before.last_progress_seq,
        "the seq did not move and the mark says so"
    );
    assert_eq!(
        lane.last_progress_at.as_deref(),
        Some(
            chrono::DateTime::from_timestamp(output_at, 0)
                .unwrap()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                .as_str()
        ),
        "the clock restarts from the pane's own output stamp, not from this observation"
    );
    assert_eq!(streak(&fixture, &run_id), 0, "no hard stop was counted");
    assert!(
        awaiting_of(&fixture, 1).is_none(),
        "the story was not blocked"
    );
}

/// The true stall, wired: the pane's stamp is as old as the store's silence.
/// The reason names what each channel measured, so the verdict can be checked
/// against the lane rather than taken on trust (SH-418).
#[test]
fn a_lane_silent_on_both_channels_is_quarantined_with_its_evidence() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "parked at a prompt", &[]);
    let window = format!("=fixture:=story-{story}");
    let past_secs = i64::try_from(STALL_CEILING_SECS * 2).unwrap();
    let past = chrono::DateTime::from_timestamp(fixture_now_unix() + past_secs, 0)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowActive {
            window: window.clone(),
            last_output_at: fixture_now_unix(),
        },
        DispatcherStep::WindowActive {
            window,
            last_output_at: fixture_now_unix(),
        },
    ]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);

    reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);
    let report = reconcile_at(&fixture, &fake, &run_id, &past);

    assert_eq!(report.quarantined, [(0, HardStopKind::Stalled)]);
    let awaiting = awaiting_of(&fixture, 1).expect("a stalled story carries a reason");
    let store_silence = STALL_CEILING_SECS * 2;
    assert!(
        awaiting.contains(&format!("no story event for {store_silence}s"))
            && awaiting.contains(&format!(
                "{store_silence}s since the pane last wrote to its terminal"
            ))
            && awaiting.contains(&format!("the ceiling is {STALL_CEILING_SECS}s")),
        "the stall reason names both channels' measurements and the ceiling: {awaiting}"
    );
}

/// The store channel still restarts the clock on its own, and a pane stamp
/// OLDER than the recorded mark never moves the mark backwards.
#[test]
fn a_stale_pane_stamp_never_rewinds_the_progress_mark() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "old stamp", &[]);
    let window = format!("=fixture:=story-{story}");
    let fake = FakeDispatcher::new([
        DispatcherStep::WindowActive {
            window: window.clone(),
            last_output_at: fixture_now_unix() - 3_600,
        },
        DispatcherStep::WindowActive {
            window,
            last_output_at: fixture_now_unix() - 3_600,
        },
    ]);
    let run_id = started_run(&fixture, &fake, 1);
    occupy(&fixture, &run_id, 0, &story);

    reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);
    assert_eq!(
        lane_at(&fixture, &run_id, 0).last_progress_at.as_deref(),
        Some(FIXTURE_NOW),
        "the first pass seeds from the observation, not from an hour-old stamp"
    );
    // Inside the ceiling, nothing moved, stamp still an hour old: the mark
    // holds at the seed rather than rewinding to the stamp.
    let soon = "2026-01-01T00:01:00Z";
    let report = reconcile_at(&fixture, &fake, &run_id, soon);
    assert!(report.quarantined.is_empty());
    assert_eq!(
        lane_at(&fixture, &run_id, 0).last_progress_at.as_deref(),
        Some(FIXTURE_NOW)
    );
}

/// SH-672: a configured six-lane run fills six lanes, independently of the
/// HTTP dispatch endpoint's in-flight request limit.
#[test]
fn a_six_lane_run_fills_six_lanes() {
    let fixture = ServiceFixture::new();
    for n in 0..8 {
        new_story(&fixture, &format!("story {n}"), &[]);
    }
    let fake = FakeDispatcher::new((0..6).map(|_| {
        DispatcherStep::Dispatch(DispatchOutcome::from_payload(
            serde_json::json!({"ok": true, "window_name": "w", "worktree_path": "/tmp/w"}),
        ))
    }));
    let run_id = started_run(&fixture, &fake, 6);
    let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);
    assert_eq!(report.filled.len(), 6, "all six configured lanes must fill");
    assert_eq!(
        fake.calls()
            .iter()
            .filter(|c| matches!(c, DispatcherCall::Dispatch(_)))
            .count(),
        6,
        "six sessions must actually be dispatched"
    );
}

/// SH-672: manual sessions are observable, but do not consume a run's lanes.
#[test]
fn census_is_reported_without_limiting_the_run() {
    for census in [
        WindowCensus::Counted {
            windows: Vec::new(),
        },
        WindowCensus::Counted {
            windows: (0..8).map(|n| format!("manual:SH-{n}")).collect(),
        },
        WindowCensus::Unanswered {
            detail: "no server running".into(),
        },
    ] {
        let fixture = ServiceFixture::new();
        for n in 0..8 {
            new_story(&fixture, &format!("story {n}"), &[]);
        }
        let fake = FakeDispatcher::new((0..6).map(|_| {
            DispatcherStep::Dispatch(DispatchOutcome::from_payload(
                serde_json::json!({"ok": true, "window_name": "w", "worktree_path": "/tmp/w"}),
            ))
        }));
        fake.set_census(census.clone());
        let run_id = started_run(&fixture, &fake, 6);
        let report = reconcile_at(&fixture, &fake, &run_id, FIXTURE_NOW);
        assert_eq!(report.filled.len(), 6, "{census:?}");
        assert_eq!(report.census, Some(census));
    }
}
