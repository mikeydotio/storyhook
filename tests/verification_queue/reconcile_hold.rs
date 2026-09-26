//! SH-770: a conflict-reconcile hold ends when its reconcile has stopped.
//!
//! The verifier keeps a project's slot for a story it returned on a merge
//! conflict until that story resubmits (D-E). These tests drive the
//! production wait through a real conflict return and prove that a store
//! fact saying the reconcile stopped releases the queue, while the existing
//! reservation tests prove a live reconcile still cannot be overtaken.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use storyhook::daemon::bus::ChangeBus;
use storyhook::daemon::lifecycle::CONTROL_DEADLINE;
use storyhook::daemon::verification::{
    HoldRelease, ReconcileWait, VERIFICATION_HOLD_RELEASED_PREFIX, wait_for_reconciled_candidate,
};
use storyhook::service::RelationService;

/// Changes the store with `act`, then runs the production wait for
/// `reserved`. A hold that never ends is stopped after [`CONTROL_DEADLINE`],
/// so it fails the caller's assertion instead of hanging the suite.
fn wait_after(
    fixture: &ServiceFixture,
    reserved: &VerificationCandidate,
    act: impl FnOnce(),
) -> Result<ReconcileWait, AppError> {
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    let stop = AtomicBool::new(false);
    act();
    let (done, finished) = channel::<()>();
    thread::scope(|scope| {
        let stop = &stop;
        scope.spawn(move || {
            if matches!(
                finished.recv_timeout(CONTROL_DEADLINE),
                Err(RecvTimeoutError::Timeout)
            ) {
                stop.store(true, Ordering::Relaxed);
            }
        });
        let result = wait_for_reconciled_candidate(fixture.store(), &subscription, stop, reserved);
        let _ = done.send(());
        result
    })
}

fn conflict() -> VerificationOutcome {
    VerificationOutcome::Conflict {
        detail: "both modified src/lib.rs".into(),
    }
}

fn certified() -> VerificationOutcome {
    VerificationOutcome::Certified {
        head: "a".repeat(40),
        tree: "b".repeat(40),
        detail: "landed".into(),
        gate: GateCommand::DEFAULT.into(),
    }
}

fn sequenced(outcomes: impl IntoIterator<Item = VerificationOutcome>) -> SequencedActuator {
    SequencedActuator {
        outcomes: Mutex::new(outcomes.into_iter().collect()),
        verified: Mutex::new(Vec::new()),
        notified: Mutex::new(Vec::new()),
        reaped: Mutex::new(Vec::new()),
    }
}

/// The hold-release comments on `id`, oldest first.
fn release_comments(fixture: &ServiceFixture, id: &str) -> Vec<String> {
    story_row(fixture, id)
        .snapshot
        .comments
        .iter()
        .filter(|comment| comment.text.starts_with(VERIFICATION_HOLD_RELEASED_PREFIX))
        .map(|comment| comment.text.clone())
        .collect()
}

struct Board {
    fixture: ServiceFixture,
    activity: VerificationActivity,
    inflight: InFlight,
}

impl Board {
    fn new() -> Self {
        let fixture = ServiceFixture::new();
        fixture.github_checkout("https://github.com/acme/widgets");
        std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
        let inflight = InFlight::new(fixture.env().clone());
        Self {
            fixture,
            activity: VerificationActivity::new(),
            inflight,
        }
    }

    /// One verifier tick whose conflict hold runs the production wait after
    /// `act` changes the store. Returns the tick result and how the wait ended.
    fn tick_releasing(
        &self,
        actuator: &SequencedActuator,
        act: impl FnOnce(&VerificationCandidate),
    ) -> (TickResult, ReconcileWait) {
        let mut act = Some(act);
        let mut ended = None;
        let result = tick_with_reconciliation(
            self.fixture.store(),
            self.fixture.env(),
            actuator,
            &self.activity,
            &self.inflight,
            self.fixture.project(),
            |reserved| {
                let act = act
                    .take()
                    .expect("one conflict hold per tick in these tests");
                let wait = wait_after(&self.fixture, reserved, || act(reserved))?;
                ended = Some(wait.clone());
                Ok(wait)
            },
        )
        .unwrap();
        (
            result,
            ended.expect("the tick must reach its conflict hold"),
        )
    }

    /// One verifier tick that must not reach a conflict hold.
    fn tick_without_hold(&self, actuator: &SequencedActuator) -> TickResult {
        tick_with_reconciliation(
            self.fixture.store(),
            self.fixture.env(),
            actuator,
            &self.activity,
            &self.inflight,
            self.fixture.project(),
            |_| panic!("this tick must not hold for a reconcile"),
        )
        .unwrap()
    }
}

/// The confirmed defect: the Full Auto stall watchdog quarantines a lane by
/// setting `awaiting` on its story, and the hold ignored it, so one dead
/// reconcile blocked the project's queue until a person stopped the verifier.
#[test]
fn the_engines_stall_verdict_releases_the_hold_and_the_queue_moves_on() {
    let board = Board::new();
    let held = submitted(&board.fixture, "reconciling", Priority::High, PR_ONE);
    let waiting = submitted(&board.fixture, "queued behind", Priority::Medium, PR_TWO);
    let actuator = sequenced([conflict(), certified()]);
    let quarantine = "Full Auto: stalled on lane 0 of run R (window SH-1). Worktree, branch and window are preserved.";

    let (result, ended) = board.tick_releasing(&actuator, |reserved| {
        assert_eq!(reserved.story_id, held);
        StoryService::new(&board.fixture.ctx())
            .set_awaiting(&held, quarantine)
            .unwrap();
    });

    assert_eq!(
        ended,
        ReconcileWait::Released(HoldRelease::StoryBlocked {
            reason: quarantine.into()
        }),
        "a story its watchdog quarantined can no longer resubmit on its own"
    );
    assert_eq!(result, TickResult::Returned);
    assert_eq!(board.activity.active_for(board.fixture.project()), None);
    let comments = release_comments(&board.fixture, &held);
    assert_eq!(comments.len(), 1, "{comments:?}");
    assert!(comments[0].contains(quarantine), "{}", comments[0]);

    assert_eq!(board.tick_without_hold(&actuator), TickResult::Completed);
    assert_eq!(
        actuator.verified.lock().unwrap().as_slice(),
        [held.as_str(), waiting.as_str()],
        "the released queue verifies the next story"
    );
}

/// `story block` during a reconcile: the agent says it cannot proceed.
#[test]
fn story_block_during_a_reconcile_releases_the_hold() {
    let board = Board::new();
    let held = submitted(&board.fixture, "reconciling", Priority::High, PR_ONE);
    let actuator = sequenced([conflict()]);

    let (result, ended) = board.tick_releasing(&actuator, |_| {
        StoryService::new(&board.fixture.ctx())
            .set_awaiting(&held, "needs a product decision")
            .unwrap();
    });

    assert_eq!(
        ended,
        ReconcileWait::Released(HoldRelease::StoryBlocked {
            reason: "needs a product decision".into()
        })
    );
    assert_eq!(result, TickResult::Returned);
}

/// The deadlock: the reconciling story is blocked by a story that waits in
/// the queue behind its own hold. Without a release neither can move.
#[test]
fn a_hold_blocked_by_a_story_queued_behind_it_releases_and_the_blocker_lands() {
    let board = Board::new();
    let held = submitted(&board.fixture, "reconciling", Priority::High, PR_ONE);
    let blocker = submitted(&board.fixture, "must land first", Priority::Low, PR_TWO);
    let actuator = sequenced([conflict(), certified()]);

    let (result, ended) = board.tick_releasing(&actuator, |_| {
        RelationService::new(&board.fixture.ctx())
            .relate(&held, "blocked-by", &blocker, false)
            .unwrap();
    });

    let ReconcileWait::Released(HoldRelease::StoryBlocked { reason }) = ended else {
        panic!("a hold blocked by its own queue must release, got {ended:?}");
    };
    assert!(reason.contains(&blocker), "{reason}");
    assert_eq!(result, TickResult::Returned);
    assert_eq!(board.tick_without_hold(&actuator), TickResult::Completed);
    assert_eq!(
        actuator.verified.lock().unwrap().as_slice(),
        [held.as_str(), blocker.as_str()]
    );
    assert_eq!(
        story_row(&board.fixture, &held).snapshot.awaiting,
        None,
        "a release never parks the story"
    );
}

/// A person moves the reserved story out of `in-progress`: nobody will
/// resubmit it, so the hold has nothing left to wait for.
#[test]
fn moving_the_reserved_story_out_of_in_progress_releases_the_hold() {
    for state in ["todo", "done"] {
        let board = Board::new();
        let held = submitted(&board.fixture, "reconciling", Priority::High, PR_ONE);
        let actuator = sequenced([conflict()]);

        let (result, ended) = board.tick_releasing(&actuator, |_| {
            StoryService::new(&board.fixture.ctx())
                .set_state(&held, state, None, Some("in-progress"), None)
                .unwrap();
        });

        assert_eq!(
            ended,
            ReconcileWait::Released(HoldRelease::StoryLeft {
                state: state.into()
            }),
            "{state}"
        );
        assert_eq!(result, TickResult::Returned, "{state}");
        assert_eq!(release_comments(&board.fixture, &held).len(), 1, "{state}");
    }
}

/// A resubmission while `awaiting` is still set is filtered out of the
/// queue, so the generation check alone never saw it and the hold never
/// ended.
#[test]
fn a_resubmission_with_awaiting_still_set_releases_the_hold() {
    let board = Board::new();
    let held = submitted(&board.fixture, "reconciling", Priority::High, PR_ONE);
    let actuator = sequenced([conflict()]);

    let (result, ended) = board.tick_releasing(&actuator, |_| {
        StoryService::new(&board.fixture.ctx())
            .set_awaiting(&held, "blocked on review")
            .unwrap();
        StoryService::new(&board.fixture.ctx())
            .set_state(&held, "verifying", None, Some("in-progress"), None)
            .unwrap();
    });

    assert_eq!(
        ended,
        ReconcileWait::Released(HoldRelease::StoryBlocked {
            reason: "blocked on review".into()
        })
    );
    assert_eq!(result, TickResult::Returned);
}

/// A released story is an ordinary queue member when it resubmits: it is
/// verified in priority order, and a second release of the same story is
/// recorded again rather than de-duplicated into silence.
#[test]
fn a_released_story_resubmits_in_priority_order_and_each_release_is_recorded() {
    let board = Board::new();
    let held = submitted(&board.fixture, "reconciling", Priority::Low, PR_ONE);
    let actuator = sequenced([conflict(), conflict(), certified()]);

    for round in 0..2 {
        let (result, ended) = board.tick_releasing(&actuator, |_| {
            StoryService::new(&board.fixture.ctx())
                .set_awaiting(&held, "waiting on a person")
                .unwrap();
        });
        assert!(
            matches!(
                ended,
                ReconcileWait::Released(HoldRelease::StoryBlocked { .. })
            ),
            "round {round}: {ended:?}"
        );
        assert_eq!(result, TickResult::Returned);
        StoryService::new(&board.fixture.ctx())
            .clear_awaiting(&held)
            .unwrap();
        StoryService::new(&board.fixture.ctx())
            .set_state(&held, "verifying", None, Some("in-progress"), None)
            .unwrap();
    }
    assert_eq!(
        release_comments(&board.fixture, &held).len(),
        2,
        "each generation's release is its own record"
    );

    let arrival = submitted(
        &board.fixture,
        "urgent",
        Priority::Critical,
        "https://github.com/acme/widgets/pull/3",
    );
    assert_eq!(board.tick_without_hold(&actuator), TickResult::Completed);
    assert_eq!(
        actuator.verified.lock().unwrap().last().map(String::as_str),
        Some(arrival.as_str()),
        "a released story holds no claim on the queue when it returns"
    );
}
