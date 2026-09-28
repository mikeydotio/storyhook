//! SH-770: a conflict-reconcile hold ends when its reconcile has stopped.
//!
//! The verifier keeps a project's slot for a story it returned on a merge
//! conflict until that story resubmits (D-E). These tests drive the
//! production wait through a real conflict return and prove that a store fact
//! or an agent that is gone or silent releases the queue, while a live
//! reconcile still cannot be overtaken.
//!
//! The agent tests move an injected clock by hand and wait on the probe
//! itself, never on elapsed time, so machine load cannot change a verdict.

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{SystemTime, UNIX_EPOCH};
use storyhook::daemon::bus::{Change, ChangeBus};
use storyhook::daemon::lifecycle::CONTROL_DEADLINE;
use storyhook::daemon::verification::{
    GONE_CONFIRMATIONS, HoldRelease, HoldWatch, ReconcileWait, ReservationReason,
    VERIFICATION_HOLD_RELEASED_PREFIX, VerificationCancellation, wait_for_reconciled_candidate,
};
use storyhook::service::RelationService;
use storyhook::service::engine::{STALL_CEILING_SECS, WindowProbe};
use storyhook_test_support::load_grace;
use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE};

/// An agent pane that wrote just now: the reconcile is live, so only a
/// resubmission, a stop, or a store fact may end the hold.
pub(super) fn live_agent(
    _candidate: &VerificationCandidate,
    _lease: Option<&StoryCleanupLease>,
    _cancellation: &VerificationCancellation,
) -> WindowProbe {
    WindowProbe::Alive {
        last_output_at: unix(SystemTime::now()),
    }
}

fn unix(time: SystemTime) -> Option<i64> {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_secs()).ok())
}

/// How often a patient receive looks again at its channel and its patience.
const RECEIVE_POLL: Duration = Duration::from_millis(25);

/// Receives from `receiver` within [`CONTROL_DEADLINE`], graced by machine
/// contention (SH-806). Every wait in this harness is patience for something
/// expected to happen (a hold thread that probes, starts or ends), never proof
/// that something did not, so a bare deadline only measures the scheduler: at
/// a load average of about 190 on 10 cores, three runs in a row failed a
/// different hold test each at a bare 5 s wait (found by SH-827). Each pass
/// receives before it judges the clock, so a message that arrived while this
/// thread was starved still counts (SH-766).
fn recv_patiently<T>(receiver: &Receiver<T>) -> Result<T, RecvTimeoutError> {
    let mut patience = load_grace::Patience::new(CONTROL_DEADLINE);
    loop {
        match receiver.recv_timeout(RECEIVE_POLL) {
            Err(RecvTimeoutError::Timeout) if !patience.expired() => {}
            received => return received,
        }
    }
}

/// Changes the store with `act`, then runs the production wait for
/// `reserved` with a live agent. A hold that never ends is stopped after
/// [`CONTROL_DEADLINE`], graced by contention ([`recv_patiently`]), so it
/// fails the caller's assertion instead of hanging the suite.
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
            if matches!(recv_patiently(&finished), Err(RecvTimeoutError::Timeout)) {
                stop.store(true, Ordering::Relaxed);
            }
        });
        let result = wait_for_reconciled_candidate(
            fixture.store(),
            &subscription,
            stop,
            reserved,
            &HoldWatch::production(&live_agent),
        );
        let _ = done.send(());
        result
    })
}

/// A clock the test moves by hand, from a fixed instant.
struct TestClock(Mutex<SystemTime>);

impl TestClock {
    fn new() -> Self {
        Self(Mutex::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000)))
    }

    fn now(&self) -> SystemTime {
        *self.0.lock().unwrap()
    }

    fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }
}

enum Event {
    Probed,
    Ended,
}

/// Moves a watched hold's clock from the test's side.
struct Driver<'a> {
    clock: &'a TestClock,
    bus: &'a ChangeBus,
    events: Receiver<Event>,
    every: Duration,
    /// Every activity instant the hold published, oldest first.
    published: &'a Mutex<Vec<SystemTime>>,
}

impl Driver<'_> {
    /// How long the hold has published no activity: what status measures the
    /// Reconcile reservation's bound from (SH-770 D1, applied by SH-827).
    fn idle(&self) -> Duration {
        let last = *self
            .published
            .lock()
            .unwrap()
            .last()
            .expect("a hold publishes its start before its first probe");
        self.clock.now().duration_since(last).unwrap_or_default()
    }

    /// The status bound on [`Self::idle`], read from its owner rather than
    /// copied, so a change to the production bound changes what this proves.
    fn overdue_after(&self) -> Duration {
        ReservationReason::Reconcile
            .overdue_after()
            .expect("a reconcile reservation is bounded")
    }

    /// Moves the clock one probe interval and waits for what that made due:
    /// `true` for a probe, `false` when the hold ended instead.
    fn step(&self) -> bool {
        self.clock.advance(self.every);
        self.bus.publish(Change::Ping);
        match recv_patiently(&self.events) {
            Ok(Event::Probed) => true,
            Ok(Event::Ended) => false,
            Err(error) => panic!("the hold neither probed nor ended after one interval: {error}"),
        }
    }

    /// Steps until the hold ends, at most `limit` times; the probes it saw.
    fn until_ended(&self, limit: usize) -> usize {
        let mut probes = 0;
        while self.step() {
            probes += 1;
            assert!(
                probes <= limit,
                "the hold did not end within {limit} probes"
            );
        }
        probes
    }
}

/// Runs the production wait for `reserved` with an injected clock and a
/// probe that answers `answer(n)` on its `n`th call, while `drive` moves the
/// clock on another thread. Returns how the wait ended and the probe count.
fn watched_wait(
    fixture: &ServiceFixture,
    reserved: &VerificationCandidate,
    clock: &TestClock,
    answer: &(dyn Fn(usize) -> WindowProbe + Sync),
    drive: impl FnOnce(&Driver<'_>) + Send,
) -> (Result<ReconcileWait, AppError>, usize) {
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    let stop = AtomicBool::new(false);
    let probes = AtomicUsize::new(0);
    let (events, received) = channel();
    let probe =
        |_: &VerificationCandidate, _: Option<&StoryCleanupLease>, _: &VerificationCancellation| {
            let answered = answer(probes.fetch_add(1, Ordering::SeqCst) + 1);
            let _ = events.send(Event::Probed);
            answered
        };
    // The driver starts moving time only once the hold has read it: a clock
    // moved before the hold starts would shift every deadline by a step.
    let (started, start) = channel::<()>();
    let unread = AtomicBool::new(true);
    let now = || {
        if unread.swap(false, Ordering::SeqCst) {
            let _ = started.send(());
        }
        clock.now()
    };
    let published = Mutex::new(Vec::new());
    let publish = |at: SystemTime| published.lock().unwrap().push(at);
    let watch = HoldWatch {
        clock: &now,
        on_activity: Some(&publish),
        ..HoldWatch::production(&probe)
    };
    let (finished, finish) = channel::<()>();
    let driver = Driver {
        clock,
        bus: &bus,
        events: received,
        every: watch.probe_every,
        published: &published,
    };
    thread::scope(|scope| {
        let stop = &stop;
        scope.spawn(move || {
            // A driver that fails stops the hold at once, or the scope would
            // wait on a hold that nothing will end.
            let driven = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                recv_patiently(&start).expect("the hold reads its clock when it starts");
                drive(&driver);
            }));
            if let Err(failure) = driven {
                stop.store(true, Ordering::Relaxed);
                std::panic::resume_unwind(failure);
            }
            // A hold still running after its driver is done fails the
            // caller's assertion instead of hanging the suite.
            if matches!(recv_patiently(&finish), Err(RecvTimeoutError::Timeout)) {
                stop.store(true, Ordering::Relaxed);
            }
        });
        let result =
            wait_for_reconciled_candidate(fixture.store(), &subscription, stop, reserved, &watch);
        let _ = events.send(Event::Ended);
        let _ = finished.send(());
        (result, probes.load(Ordering::SeqCst))
    })
}

/// Probe intervals in one stall ceiling.
fn intervals_per_ceiling() -> usize {
    usize::try_from(STALL_CEILING_SECS / HoldWatch::production(&live_agent).probe_every.as_secs())
        .unwrap()
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

    /// One verifier tick whose conflict hold runs `waiter`.
    fn tick_holding(
        &self,
        actuator: &SequencedActuator,
        waiter: impl FnOnce(&VerificationCandidate) -> Result<ReconcileWait, AppError>,
    ) -> TickResult {
        let mut waiter = Some(waiter);
        tick_with_reconciliation(
            self.fixture.store(),
            self.fixture.env(),
            actuator,
            &self.activity,
            &self.inflight,
            self.fixture.project(),
            |reserved| waiter.take().expect("one conflict hold per tick")(reserved),
        )
        .unwrap()
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

/// The story's failure case: the reconciling agent dies. Its pane is gone on
/// two probes in a row, the hold releases, and the next story is verified.
#[test]
fn a_dead_agent_pane_releases_the_queue_after_two_probes() {
    let board = Board::new();
    let held = submitted(&board.fixture, "reconciling", Priority::High, PR_ONE);
    let waiting = submitted(&board.fixture, "queued behind", Priority::Medium, PR_TWO);
    let actuator = sequenced([conflict(), certified()]);
    let clock = TestClock::new();
    let gone = |_: usize| WindowProbe::Gone {
        detail: "tmux finds no pane `SH-1`".into(),
    };
    let mut probes = 0;

    let result = board.tick_holding(&actuator, |reserved| {
        let (ended, seen) = watched_wait(&board.fixture, reserved, &clock, &gone, |driver| {
            driver.until_ended(GONE_CONFIRMATIONS as usize);
        });
        probes = seen;
        ended
    });

    assert_eq!(result, TickResult::Returned);
    assert_eq!(
        probes, GONE_CONFIRMATIONS as usize,
        "one gone probe is not enough; the second releases"
    );
    let comments = release_comments(&board.fixture, &held);
    assert_eq!(comments.len(), 1, "{comments:?}");
    assert!(
        comments[0].contains("agent pane is gone"),
        "{}",
        comments[0]
    );
    assert_eq!(board.tick_without_hold(&actuator), TickResult::Completed);
    assert_eq!(
        actuator.verified.lock().unwrap().as_slice(),
        [held.as_str(), waiting.as_str()],
        "the released queue verifies the next story"
    );
    assert_eq!(story_row(&board.fixture, &held).snapshot.awaiting, None);
}

/// The stalled case: the pane lives but writes nothing, and the story has no
/// event, for longer than the engine's stall ceiling (an agent idle at its
/// prompt or behind a dialog). The hold releases then, and not before.
#[test]
fn a_silent_agent_releases_the_hold_only_past_the_stall_ceiling() {
    let board = Board::new();
    submitted(&board.fixture, "reconciling", Priority::High, PR_ONE);
    let actuator = sequenced([conflict()]);
    let clock = TestClock::new();
    let wrote_at = unix(clock.now());
    let stale = move |_: usize| WindowProbe::Alive {
        last_output_at: wrote_at,
    };
    let mut ended = None;

    board.tick_holding(&actuator, |reserved| {
        let (result, probes) = watched_wait(&board.fixture, reserved, &clock, &stale, |driver| {
            driver.until_ended(intervals_per_ceiling() + 1);
            // A hold that releases never reads overdue in status.
            assert!(
                driver.idle() <= driver.overdue_after(),
                "released {:?} after its last activity",
                driver.idle()
            );
        });
        assert_eq!(
            probes,
            intervals_per_ceiling(),
            "the hold keeps probing until the ceiling has passed"
        );
        ended = Some(result.as_ref().unwrap().clone());
        result
    });

    let Some(ReconcileWait::Released(HoldRelease::AgentSilent { silent_secs, .. })) = ended else {
        panic!("a silent agent must release the hold, got {ended:?}");
    };
    assert!(silent_secs > STALL_CEILING_SECS, "{silent_secs}");
}

/// No pane evidence at all — no lease, or tmux that does not answer — does
/// not hold forever: the store's silence alone decides at the same ceiling
/// (the engine's SH-626 rule, council decision D1).
#[test]
fn without_pane_evidence_the_store_alone_releases_at_the_ceiling() {
    let board = Board::new();
    submitted(&board.fixture, "reconciling", Priority::High, PR_ONE);
    let actuator = sequenced([conflict()]);
    let clock = TestClock::new();
    let unanswered = |_: usize| WindowProbe::Unanswered {
        detail: "no cleanup lease records the tmux server".into(),
    };
    let mut ended = None;

    board.tick_holding(&actuator, |reserved| {
        let (result, _) = watched_wait(&board.fixture, reserved, &clock, &unanswered, |driver| {
            driver.until_ended(intervals_per_ceiling() + 1);
        });
        ended = Some(result.as_ref().unwrap().clone());
        result
    });

    assert!(
        matches!(
            &ended,
            Some(ReconcileWait::Released(HoldRelease::AgentSilent { probe: Some(probe), .. }))
                if probe.contains("no cleanup lease")
        ),
        "{ended:?}"
    );
}

/// The invariant the hold exists for: a live reconcile — its pane writing —
/// keeps the queue across several stall ceilings, a higher-priority arrival
/// cannot take the slot, and the resubmission is verified next.
#[test]
fn a_live_agent_holds_past_several_ceilings_and_is_verified_first_on_resubmission() {
    let board = Board::new();
    let held = submitted(&board.fixture, "reconciling", Priority::Low, PR_ONE);
    let actuator = sequenced([conflict(), certified()]);
    let clock = TestClock::new();
    let writing = |_: usize| WindowProbe::Alive {
        last_output_at: unix(clock.now()),
    };

    let result = board.tick_holding(&actuator, |reserved| {
        let (ended, _) = watched_wait(&board.fixture, reserved, &clock, &writing, |driver| {
            submitted(&board.fixture, "urgent arrival", Priority::Critical, PR_TWO);
            for step in 0..3 * intervals_per_ceiling() {
                assert!(driver.step(), "the hold ended at step {step}");
                // Hours of live work, and status never calls it overdue.
                assert!(
                    driver.idle() <= driver.overdue_after(),
                    "step {step}: idle {:?}",
                    driver.idle()
                );
            }
            StoryService::new(&board.fixture.ctx())
                .set_state(&held, "verifying", None, Some("in-progress"), None)
                .unwrap();
            driver.bus.publish(Change::Project("fixture".into()));
        });
        ended
    });

    assert_eq!(result, TickResult::Completed);
    assert_eq!(
        actuator.verified.lock().unwrap().as_slice(),
        [held.as_str(), held.as_str()],
        "a live reconcile cannot be overtaken"
    );
    assert!(release_comments(&board.fixture, &held).is_empty());
}

/// Story events are progress on their own channel: a hold whose pane cannot
/// be probed still holds while its story keeps moving.
#[test]
fn story_events_keep_a_hold_whose_pane_cannot_be_probed() {
    let board = Board::new();
    let held = submitted(&board.fixture, "reconciling", Priority::High, PR_ONE);
    let actuator = sequenced([conflict()]);
    let clock = TestClock::new();
    let unanswered = |_: usize| WindowProbe::Unanswered {
        detail: "tmux did not answer".into(),
    };
    let mut ended = None;

    board.tick_holding(&actuator, |reserved| {
        let (result, _) = watched_wait(&board.fixture, reserved, &clock, &unanswered, |driver| {
            for step in 0..3 * intervals_per_ceiling() {
                StoryService::new(&board.fixture.ctx())
                    .comment(&held, &format!("reconcile step {step}"))
                    .unwrap();
                assert!(driver.step(), "a moving story must hold at step {step}");
            }
            StoryService::new(&board.fixture.ctx())
                .set_state(&held, "verifying", None, Some("in-progress"), None)
                .unwrap();
            driver.bus.publish(Change::Project("fixture".into()));
        });
        ended = Some(result.as_ref().unwrap().clone());
        // The resubmission is not verified in this test: end the tick here.
        Ok(ReconcileWait::Ended)
    });

    assert!(
        matches!(ended, Some(ReconcileWait::Resubmitted(_))),
        "{ended:?}"
    );
}

/// An agent that resubmits and then exits reads gone on the next probe. The
/// generation is read again before any release, so the resubmission wins.
#[test]
fn a_gone_pane_after_a_resubmission_resubmits() {
    let board = Board::new();
    let held = submitted(&board.fixture, "reconciling", Priority::High, PR_ONE);
    let actuator = sequenced([conflict()]);
    let clock = TestClock::new();
    let resubmitted_then_gone = |probe: usize| {
        if probe == GONE_CONFIRMATIONS as usize {
            StoryService::new(&board.fixture.ctx())
                .set_state(&held, "verifying", None, Some("in-progress"), None)
                .unwrap();
        }
        WindowProbe::Gone {
            detail: "the agent exited".into(),
        }
    };
    let mut ended = None;

    board.tick_holding(&actuator, |reserved| {
        let (result, _) = watched_wait(
            &board.fixture,
            reserved,
            &clock,
            &resubmitted_then_gone,
            |driver| {
                driver.until_ended(GONE_CONFIRMATIONS as usize);
            },
        );
        ended = Some(result.as_ref().unwrap().clone());
        // The resubmission is not verified in this test: end the tick here.
        Ok(ReconcileWait::Ended)
    });

    assert!(
        matches!(ended, Some(ReconcileWait::Resubmitted(_))),
        "{ended:?}"
    );
}

/// The production probe, through the actuator seam, against a real tmux
/// server on a private socket. tmux names a pane's command by its resolved
/// executable, so a real pane cannot pose as an agent here: the live verdict
/// is [`storyhook::service::engine`]'s own pane classification, covered with
/// the combination rules by the probe's unit tests. This proves the real
/// answer is read by window name and field, and that a pane running something
/// else is no evidence, a dead pane is gone, and a stopped server is gone.
#[test]
fn the_shell_probe_reads_a_real_story_window() {
    let fixture = ServiceFixture::new();
    let root = scratch_dir();
    let socket = root.path().join("tmux");
    let mut candidate = cleanup_candidate(&fixture, root.path());
    let lease = StoryCleanupLease {
        tmux: TmuxCleanupTarget {
            socket_path: socket.clone(),
        },
        ..candidate.cleanup_lease.take().unwrap()
    };
    let tmux = |args: &[&str]| {
        let mut command = Command::new("tmux");
        command
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .args(["-f", "/dev/null", "-S"])
            .arg(&socket)
            .args(args);
        let output = ChildGuard::spawn_with_output(&mut command)
            .expect("start fixture tmux client")
            .wait_with_output_within(load_grace::graced_now(STORY_COMMAND_DEADLINE), || {
                format!("fixture tmux {args:?} did not finish")
            });
        assert!(
            output.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    struct KillServer<'a>(&'a Path);
    impl Drop for KillServer<'_> {
        fn drop(&mut self) {
            let _ = Command::new("tmux")
                .env_remove("TMUX")
                .arg("-S")
                .arg(self.0)
                .arg("kill-server")
                .output();
        }
    }
    let actuator = ShellVerificationActuator::new(fixture.env().clone());
    let probe = || {
        actuator.probe_agent(
            &candidate,
            Some(&lease),
            &VerificationCancellation::default(),
        )
    };
    // A pane's answer changes as its process does. Each wait is the daemon's
    // control deadline, graced by machine contention (SH-806).
    let settles_to = |expected: &dyn Fn(&WindowProbe) -> bool| {
        let last = std::cell::RefCell::new(None);
        load_grace::wait_for(
            load_grace::Patience::new(CONTROL_DEADLINE),
            Duration::from_millis(25),
            || format!("the probe still answers {:?}", last.borrow()),
            || {
                let answer = probe();
                let settled = expected(&answer);
                *last.borrow_mut() = Some(answer);
                settled.then_some(())
            },
        );
    };

    assert!(matches!(
        actuator.probe_agent(&candidate, None, &VerificationCancellation::default()),
        WindowProbe::Unanswered { .. }
    ));
    assert!(
        matches!(probe(), WindowProbe::Gone { .. }),
        "no server has started on the lease's socket"
    );
    let window = candidate.story_id.clone();
    tmux(&[
        "new-session",
        "-d",
        "-s",
        "fixture",
        "-n",
        &window,
        "/bin/sleep 600",
    ]);
    let _server = KillServer(&socket);
    tmux(&["set-option", "-g", "remain-on-exit", "on"]);
    settles_to(
        &|answer| matches!(answer, WindowProbe::Unanswered { detail } if detail.contains("runs `sleep`")),
    );

    tmux(&[
        "respawn-pane",
        "-k",
        "-t",
        &format!("fixture:{window}"),
        "/usr/bin/true",
    ]);
    settles_to(&|answer| matches!(answer, WindowProbe::Gone { detail } if detail.contains("dead")));

    tmux(&["kill-server"]);
    settles_to(&|answer| matches!(answer, WindowProbe::Gone { .. }));
}
