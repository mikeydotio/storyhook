//! A verifier held for a story that its own write took out of the queue reads
//! as activity, never as missing evidence (SH-768).

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use storyhook::daemon::bus::ChangeBus;
use storyhook::daemon::verification::status::VerifierStatus;
use storyhook::daemon::verification::{
    HoldWatch, ReservationReason, VerifierReservation, wait_for_reconciled_candidate,
};

/// A status read at `now`, as `story verifier status` takes it.
fn status_at(
    fixture: &ServiceFixture,
    activity: &VerificationActivity,
    now: &str,
) -> VerifierStatus {
    activity
        .status(&fixture.ctx().clock(Clock::Fixed(now.into())))
        .unwrap()
}

fn seconds_after(at: &str, seconds: i64) -> String {
    (chrono::DateTime::parse_from_rfc3339(at).unwrap() + chrono::Duration::seconds(seconds))
        .to_rfc3339()
}

/// Ends the real waiter when its companion thread panics, so a failed
/// assertion fails the test instead of leaving the wait to run for ever.
struct StopOnPanic<'a>(&'a AtomicBool);

impl Drop for StopOnPanic<'_> {
    fn drop(&mut self) {
        if thread::panicking() {
            self.0.store(true, Ordering::Relaxed);
        }
    }
}

#[test]
fn a_conflict_reservation_reads_as_activity_for_the_whole_wait() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    let held = submitted(&fixture, "reconciling", Priority::Low, PR_ONE);
    let returned_generation = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap()
        .verifying_generation;
    let activity = VerificationActivity::new();
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(fixture.env().clone());
    let actuator = SequencedActuator {
        outcomes: Mutex::new(VecDeque::from([
            VerificationOutcome::Conflict {
                detail: "both modified src/lib.rs".into(),
            },
            VerificationOutcome::Certified {
                head: "a".repeat(40),
                tree: "b".repeat(40),
                detail: "landed after reconciliation".into(),
                gate: GateCommand::DEFAULT.into(),
            },
        ])),
        verified: Mutex::new(Vec::new()),
        notified: Mutex::new(Vec::new()),
        reaped: Mutex::new(Vec::new()),
    };
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    let stop = AtomicBool::new(false);

    let result = tick_with_reconciliation(
        fixture.store(),
        fixture.env(),
        &actuator,
        &activity,
        &inflight,
        fixture.project(),
        |reserved| {
            let queued = submitted(&fixture, "queued behind", Priority::Critical, PR_TWO);
            let status = status_at(&fixture, &activity, &fixture.env().now());
            let reservation = status
                .reservation
                .clone()
                .unwrap_or_else(|| panic!("the reconcile is not reported: {status:?}"));
            assert_eq!(reservation.story_id, held);
            assert_eq!(reservation.generation, returned_generation);
            assert_eq!(reservation.reason, ReservationReason::Reconcile);
            assert_eq!(reservation.queued_behind, 1);
            assert_eq!(status.verifying, [queued]);
            assert_eq!(
                status.active.as_ref().map(|active| active.generation),
                Some(returned_generation)
            );
            assert_eq!(status.evidence_error, None, "{status:?}");
            assert_eq!(status.warning, None, "{status:?}");

            // Past the publisher interval a reconcile is still activity.
            let later = status_at(
                &fixture,
                &activity,
                &seconds_after(&reservation.reserved_at, 121),
            );
            assert_eq!(
                later.reservation.as_ref().and_then(|r| r.age_seconds),
                Some(121)
            );
            assert_eq!(later.evidence_error, None, "{later:?}");
            assert_eq!(later.warning, None, "{later:?}");
            assert_eq!(later.silence_seconds, None);
            let text = later.render_human();
            assert!(
                text.contains(&format!("{held} reserved for merge-conflict reconcile")),
                "{text}"
            );
            assert!(!text.contains("gate on"), "{text}");

            // The production waiter runs while another reader looks and the
            // agent resubmits.
            thread::scope(|scope| {
                scope.spawn(|| {
                    let _stop = StopOnPanic(&stop);
                    let during = status_at(&fixture, &activity, &fixture.env().now());
                    assert_eq!(
                        during.reservation.map(|r| r.reason),
                        Some(ReservationReason::Reconcile)
                    );
                    assert_eq!(during.evidence_error, None);
                    assert_eq!(during.warning, None);
                    StoryService::new(&fixture.ctx())
                        .set_state(&held, "verifying", None, Some("in-progress"), None)
                        .unwrap();
                });
                // SH-770 gave the wait an agent watch; a live agent keeps the
                // hold until the resubmission, which is what this test reads.
                wait_for_reconciled_candidate(
                    fixture.store(),
                    &subscription,
                    &stop,
                    reserved,
                    &HoldWatch::production(&super::reconcile_hold::live_agent),
                )
            })
        },
    )
    .unwrap();

    assert_eq!(result, TickResult::Completed);
    assert_eq!(
        actuator.verified.lock().unwrap().as_slice(),
        [held.as_str(), held.as_str()]
    );
    let after = status_at(&fixture, &activity, &fixture.env().now());
    assert!(after.active.is_none());
    assert!(after.reservation.is_none());
}

#[test]
fn a_reservation_crosses_the_wire_and_its_absence_still_decodes() {
    let fixture = ServiceFixture::new();
    let mut status = VerificationActivity::new().status(&fixture.ctx()).unwrap();
    let absent = serde_json::to_value(&status).unwrap();
    assert!(
        absent.get("reservation").is_none(),
        "an absent reservation is omitted, as older payloads were: {absent}"
    );
    let decoded: VerifierStatus = serde_json::from_value(absent).unwrap();
    assert!(decoded.reservation.is_none());

    status.reservation = Some(VerifierReservation {
        story_id: "SH-1".into(),
        generation: Some(GlobalSeq::new(7)),
        reason: ReservationReason::Reconcile,
        reserved_at: FIXTURE_NOW.into(),
        age_seconds: Some(5),
        queued_behind: 2,
    });
    let present = serde_json::to_value(&status).unwrap();
    assert_eq!(present["reservation"]["reason"], "reconcile");
    assert_eq!(present["reservation"]["reserved_at"], FIXTURE_NOW);
    let decoded: VerifierStatus = serde_json::from_value(present).unwrap();
    assert_eq!(decoded.reservation, status.reservation);
}

#[test]
fn the_verifier_help_topic_names_the_reservation() {
    let topic = storyhook::help_topics::get_help_topic("verifier").unwrap();
    assert!(topic.contains("reservation"), "{topic}");
}

/// A fake verifier that reads status from inside its own blocking calls,
/// where the tick still owns the project's slot.
struct StatusProbe<'a> {
    fixture: &'a ServiceFixture,
    activity: &'a VerificationActivity,
    outcomes: Mutex<VecDeque<VerificationOutcome>>,
    /// Successive submission answers; exhausted means adopt the linked PR.
    submissions: Mutex<VecDeque<Result<SubmittedPullRequest, SubmissionFailure>>>,
    /// When set, the first `notify` resubmits this story and then fails.
    resubmit_on_first_notify: Option<String>,
    notifications: Mutex<usize>,
    verified: Mutex<Vec<Option<GlobalSeq>>>,
    probes: Mutex<Vec<(&'static str, VerifierStatus)>>,
}

impl<'a> StatusProbe<'a> {
    fn new(
        fixture: &'a ServiceFixture,
        activity: &'a VerificationActivity,
        outcomes: impl IntoIterator<Item = VerificationOutcome>,
    ) -> Self {
        Self {
            fixture,
            activity,
            outcomes: Mutex::new(outcomes.into_iter().collect()),
            submissions: Mutex::new(VecDeque::new()),
            resubmit_on_first_notify: None,
            notifications: Mutex::new(0),
            verified: Mutex::new(Vec::new()),
            probes: Mutex::new(Vec::new()),
        }
    }

    fn probe(&self, call: &'static str) {
        let status = status_at(self.fixture, self.activity, &self.fixture.env().now());
        self.probes.lock().unwrap().push((call, status));
    }

    fn probes(&self) -> Vec<(&'static str, VerifierStatus)> {
        self.probes.lock().unwrap().clone()
    }
}

impl VerificationActuator for StatusProbe<'_> {
    fn submit(
        &self,
        candidate: &VerificationCandidate,
    ) -> Result<SubmittedPullRequest, SubmissionFailure> {
        self.submissions
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| adopt_linked(candidate))
    }

    fn verify(
        &self,
        candidate: &VerificationCandidate,
        _pull_request: &PrLink,
    ) -> VerificationOutcome {
        self.verified
            .lock()
            .unwrap()
            .push(candidate.verifying_generation);
        self.outcomes
            .lock()
            .unwrap()
            .pop_front()
            .expect("every verification attempt must have a fixture outcome")
    }

    fn land(
        &self,
        _candidate: &VerificationCandidate,
        _intent: &storyhook::store::LandingIntent,
    ) -> storyhook::daemon::verification::LandingOutcome {
        storyhook::daemon::verification::LandingOutcome::Merged {
            detail: "test merge confirmed".into(),
        }
    }

    fn recover_landing(
        &self,
        _candidate: &VerificationCandidate,
        _intent: &storyhook::store::LandingIntent,
    ) -> storyhook::daemon::verification::LandingOutcome {
        storyhook::daemon::verification::LandingOutcome::Merged {
            detail: "test merge recovered".into(),
        }
    }

    fn notify(
        &self,
        _candidate: &VerificationCandidate,
        _message: &str,
    ) -> Result<NotifyDelivery, AppError> {
        self.probe("notify");
        let first = {
            let mut notifications = self.notifications.lock().unwrap();
            *notifications += 1;
            *notifications == 1
        };
        if first && let Some(story) = &self.resubmit_on_first_notify {
            StoryService::new(&self.fixture.ctx())
                .set_state(story, "verifying", None, Some("in-progress"), None)
                .unwrap();
            return Err(AppError::Storage("pane query failed".into()));
        }
        Ok(NotifyDelivery::Delivered)
    }

    fn redispatch(
        &self,
        _candidate: &VerificationCandidate,
        _plan: &ResumePlan,
    ) -> Result<(), AppError> {
        panic!("a delivered or failed notification never re-dispatches here")
    }

    fn reap(&self, _candidate: &VerificationCandidate) -> Result<(), AppError> {
        self.probe("reap");
        Ok(())
    }
}

/// Asserts that `status` shows `story` held for `reason` as ordinary work.
fn assert_reserved(status: &VerifierStatus, story: &str, reason: ReservationReason, case: &str) {
    let reservation = status
        .reservation
        .as_ref()
        .unwrap_or_else(|| panic!("{case}: no reservation in {status:?}"));
    assert_eq!(reservation.story_id, story, "{case}");
    assert_eq!(reservation.reason, reason, "{case}");
    assert_eq!(status.evidence_error, None, "{case}: {status:?}");
    assert_eq!(status.warning, None, "{case}: {status:?}");
}

#[test]
fn every_return_reserves_the_verifier_while_its_diagnosis_is_delivered() {
    let cases = [
        (
            "red",
            VerificationOutcome::TestsFailed {
                tree: "abc123".into(),
                log: "/tmp/red.log".into(),
                detail: "red".into(),
                gate: GateCommand::DEFAULT.into(),
            },
            ReservationReason::Remediation,
        ),
        (
            "invalid submission",
            VerificationOutcome::InvalidSubmission {
                detail: "invalid".into(),
            },
            ReservationReason::Remediation,
        ),
        (
            "conflict",
            VerificationOutcome::Conflict {
                detail: "conflict".into(),
            },
            ReservationReason::Reconcile,
        ),
    ];
    for (case, outcome, reason) in cases {
        let fixture = ServiceFixture::new();
        fixture.github_checkout("https://github.com/acme/widgets");
        let id = submitted(&fixture, case, Priority::Low, PR_ONE);
        let activity = VerificationActivity::new();
        std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
        let inflight = InFlight::new(fixture.env().clone());
        let actuator = StatusProbe::new(&fixture, &activity, [outcome]);

        let result = tick_with_activity(
            fixture.store(),
            fixture.env(),
            &actuator,
            &activity,
            &inflight,
            fixture.project(),
        )
        .unwrap();

        assert_eq!(result, TickResult::Returned, "{case}");
        let probes = actuator.probes();
        assert_eq!(probes.len(), 1, "{case}");
        assert_eq!(probes[0].0, "notify", "{case}");
        assert_reserved(&probes[0].1, &id, reason, case);
        assert!(activity.active_for(fixture.project()).is_none(), "{case}");
    }
}

#[test]
fn a_refused_submission_reserves_the_verifier_while_its_diagnosis_is_delivered() {
    let fixture = ServiceFixture::new();
    let root = scratch_dir();
    let (id, _) = leased_submission(&fixture, root.path(), "dirty", None);
    let env = Environment::at(root.path());
    std::fs::create_dir_all(env.daemon_state_dir()).unwrap();
    let inflight = InFlight::new(env.clone());
    let activity = VerificationActivity::new();
    let actuator = StatusProbe::new(&fixture, &activity, []);
    actuator
        .submissions
        .lock()
        .unwrap()
        .push_back(Err(SubmissionFailure::Refused {
            reason: "dirty-worktree".into(),
            display: "story.sh submit: the worktree has uncommitted changes.".into(),
        }));

    let result = tick_with_activity(
        fixture.store(),
        &env,
        &actuator,
        &activity,
        &inflight,
        fixture.project(),
    )
    .unwrap();

    assert_eq!(result, TickResult::Returned);
    let probes = actuator.probes();
    assert_eq!(probes.len(), 1);
    assert_reserved(&probes[0].1, &id, ReservationReason::Remediation, "refused");
}

/// The reservation is kept once the return commits, even when the delivery
/// that follows fails and the story has already come back: the tick then
/// continues, and the new generation must replace the reservation before any
/// refresh finds it current (a debug assertion pins that order).
#[test]
fn a_failed_delivery_after_a_resubmission_continues_with_the_new_generation() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    let root = scratch_dir();
    let (id, _) = leased_submission(&fixture, root.path(), "comes back", Some(PR_ONE));
    let returned = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap()
        .verifying_generation;
    let env = Environment::at(root.path());
    std::fs::create_dir_all(env.daemon_state_dir()).unwrap();
    let inflight = InFlight::new(env.clone());
    let activity = VerificationActivity::new();
    let mut actuator = StatusProbe::new(
        &fixture,
        &activity,
        [VerificationOutcome::TestsFailed {
            tree: "abc123".into(),
            log: "/tmp/red.log".into(),
            detail: "red".into(),
            gate: GateCommand::DEFAULT.into(),
        }],
    );
    actuator.resubmit_on_first_notify = Some(id.clone());
    actuator
        .submissions
        .lock()
        .unwrap()
        .push_back(Err(SubmissionFailure::Refused {
            reason: "dirty-worktree".into(),
            display: "story.sh submit: the worktree has uncommitted changes.".into(),
        }));

    let result = tick_with_activity(
        fixture.store(),
        &env,
        &actuator,
        &activity,
        &inflight,
        fixture.project(),
    )
    .unwrap();

    assert_eq!(result, TickResult::Returned);
    let verified = actuator.verified.lock().unwrap().clone();
    assert_eq!(verified.len(), 1, "only the resubmission is verified");
    assert_ne!(verified[0], returned);
    let probes = actuator.probes();
    assert_eq!(probes.len(), 2);
    for (_, status) in &probes {
        assert_reserved(status, &id, ReservationReason::Remediation, "delivery");
    }
    assert!(activity.active_for(fixture.project()).is_none());
}

#[test]
fn a_certified_landing_reserves_the_verifier_while_it_reaps() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    let id = submitted(&fixture, "lands", Priority::Low, PR_ONE);
    let landed = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap()
        .verifying_generation;
    let activity = VerificationActivity::new();
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(fixture.env().clone());
    let actuator = StatusProbe::new(
        &fixture,
        &activity,
        [VerificationOutcome::Certified {
            head: "a".repeat(40),
            tree: "b".repeat(40),
            detail: "landed".into(),
            gate: GateCommand::DEFAULT.into(),
        }],
    );

    let result = tick_with_activity(
        fixture.store(),
        fixture.env(),
        &actuator,
        &activity,
        &inflight,
        fixture.project(),
    )
    .unwrap();

    assert_eq!(result, TickResult::Completed);
    let probes = actuator.probes();
    assert_eq!(probes.len(), 1);
    assert_eq!(probes[0].0, "reap");
    assert_reserved(&probes[0].1, &id, ReservationReason::Cleanup, "certified");
    assert_eq!(probes[0].1.reservation.as_ref().unwrap().generation, landed);
    assert!(activity.active_for(fixture.project()).is_none());
}

#[test]
fn a_recovered_landing_reserves_the_verifier_while_it_reaps() {
    use storyhook::service::landing::{LandingAdmission, VerifiedSubmission};
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    let id = submitted(&fixture, "landed before a restart", Priority::Low, PR_ONE);
    let queue = VerificationQueue::new(fixture.store());
    assert!(matches!(
        queue
            .begin_landing(
                &fixture.ctx(),
                &queue.next().unwrap().unwrap(),
                &VerifiedSubmission {
                    head: "a".repeat(40),
                    tree: "b".repeat(40),
                    gate: GateCommand::DEFAULT.into(),
                },
            )
            .unwrap(),
        LandingAdmission::Admitted(_)
    ));
    let activity = VerificationActivity::new();
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(fixture.env().clone());
    let actuator = StatusProbe::new(&fixture, &activity, []);

    let result = tick_with_activity(
        fixture.store(),
        fixture.env(),
        &actuator,
        &activity,
        &inflight,
        fixture.project(),
    )
    .unwrap();

    assert_eq!(result, TickResult::Completed);
    let probes = actuator.probes();
    assert_eq!(probes.len(), 1);
    assert_eq!(probes[0].0, "reap");
    assert_reserved(&probes[0].1, &id, ReservationReason::Cleanup, "recovered");
}

#[test]
fn a_cleanup_retry_is_admitted_already_reserved() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    let id = submitted(&fixture, "landed before a crash", Priority::High, PR_ONE);
    let root = scratch_dir();
    fixture.append_cleanup_lease(&id, lease_for(root.path(), &id));
    let ctx = fixture.ctx();
    StoryService::new(&ctx)
        .comment(
            &id,
            &format!(
                "{VERIFICATION_GREEN_PREFIX} merge tree `abc123` passed `make test` and pull request {PR_ONE} landed."
            ),
        )
        .unwrap();
    VerificationQueue::new(fixture.store())
        .record_merged(&ctx, &id, PR_ONE)
        .unwrap();
    let env = Environment::at(root.path());
    std::fs::create_dir_all(env.daemon_state_dir()).unwrap();
    let inflight = InFlight::new(env.clone());
    let activity = VerificationActivity::new();
    let actuator = StatusProbe::new(&fixture, &activity, []);

    let result = tick_with_activity(
        fixture.store(),
        &env,
        &actuator,
        &activity,
        &inflight,
        fixture.project(),
    )
    .unwrap();

    assert_eq!(result, TickResult::Completed);
    let probes = actuator.probes();
    assert_eq!(probes.len(), 1);
    assert_eq!(probes[0].0, "reap");
    assert_reserved(
        &probes[0].1,
        &id,
        ReservationReason::Cleanup,
        "cleanup retry",
    );
    assert!(probes[0].1.verifying.is_empty());
}
