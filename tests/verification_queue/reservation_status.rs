//! A verifier held for a story that its own write took out of the queue reads
//! as activity, never as missing evidence (SH-768).

use super::*;
use storyhook::daemon::verification::status::VerifierStatus;
use storyhook::daemon::verification::{ReservationReason, VerifierReservation};

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

#[test]
fn sh870_conflict_hold_reports_evidence_without_a_live_repair_reservation() {
    let f = ServiceFixture::new();
    f.github_checkout("https://github.com/acme/widgets");
    let held = submitted(&f, "conflict", Priority::Low, PR_ONE);
    let activity = VerificationActivity::new();
    let inflight = InFlight::new(f.env().clone());
    let actuator = FakeActuator::new(VerificationOutcome::Conflict {
        detail: "base moved".into(),
    });
    assert_eq!(
        tick_with_reconciliation(
            f.store(),
            f.env(),
            &actuator,
            &activity,
            &inflight,
            f.project(),
            |_| panic!("no unproved repair wait")
        )
        .unwrap(),
        TickResult::Returned
    );
    let after = status_at(&f, &activity, &f.env().now());
    assert!(after.active.is_none());
    assert!(after.reservation.is_none());
    assert_eq!(after.attribution_holds.len(), 1);
    assert_eq!(
        after.attribution_holds[0].cause,
        storyhook::service::attribution::FailureCause::Integration
    );
    assert_eq!(after.attribution_holds[0].story_id, held);
    assert!(after.warning.is_none());
    assert!(after.evidence_error.is_none());
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
    submissions:
        Mutex<VecDeque<Result<storyhook::domain::landing::SubmissionOutcome, SubmissionFailure>>>,
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
    ) -> Result<storyhook::domain::landing::SubmissionOutcome, SubmissionFailure> {
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

#[test]
fn unproved_failures_hold_attribution_without_repair_delivery() {
    let cases = [
        (
            "red",
            VerificationOutcome::TestsFailed {
                tree: "abc123".into(),
                log: "/tmp/red.log".into(),
                detail: "red".into(),
                gate: GateCommand::DEFAULT.into(),
            },
        ),
        (
            "invalid submission",
            VerificationOutcome::InvalidSubmission {
                detail: "invalid".into(),
            },
        ),
        (
            "conflict",
            VerificationOutcome::Conflict {
                detail: "conflict".into(),
            },
        ),
    ];
    for (case, outcome) in cases {
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
        assert!(
            probes.is_empty(),
            "{case}: unproved evidence cannot assign repair"
        );
        assert_eq!(story_row(&fixture, &id).state, "verifying", "{case}");
        assert!(
            VerificationQueue::new(fixture.store())
                .next()
                .unwrap()
                .is_none()
        );
        let status = activity.status(&fixture.ctx()).unwrap();
        assert_eq!(status.evidence_error, None, "{case}: {status:?}");
        assert_eq!(status.warning, None, "{case}: {status:?}");
        assert!(status.reservation.is_none(), "{case}: the slot is released");
        assert_eq!(status.attribution_holds.len(), 1, "{case}");
        assert_eq!(status.attribution_holds[0].story_id, id, "{case}");
        let evidence = fixture
            .store()
            .read(|tx| tx.attributions(fixture.project()))
            .unwrap();
        assert_eq!(evidence.len(), 1, "{case}");
        assert!(evidence[0].held, "{case}");
        assert!(
            evidence[0].probes.is_empty(),
            "{case}: no causal probe was available"
        );
        assert!(activity.active_for(fixture.project()).is_none(), "{case}");
    }
}

#[test]
fn a_refused_submission_releases_ownership_without_a_repair_delivery() {
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
    assert!(probes.is_empty());
    assert_eq!(story_row(&fixture, &id).state, "verifying");
    assert!(activity.active_for(fixture.project()).is_none());
}

/// An administrative refusal cannot invoke a delivery callback that changes the story.
#[test]
fn an_unproved_refusal_never_enters_the_delivery_resubmission_callback() {
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
    assert!(
        verified.is_empty(),
        "no verification after a held submission refusal"
    );
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.attributions(fixture.project()))
            .unwrap()[0]
            .submission
            .generation,
        returned
    );
    let probes = actuator.probes();
    assert!(
        probes.is_empty(),
        "no repair delivery or resubmission callback"
    );
    assert_eq!(story_row(&fixture, &id).state, "verifying");
    assert!(activity.active_for(fixture.project()).is_none());
}

#[test]
fn a_certified_landing_releases_verifier_ownership_to_durable_cleanup() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    let id = submitted(&fixture, "lands", Priority::Low, PR_ONE);
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
    assert!(actuator.probes().is_empty());
    assert!(activity.active_for(fixture.project()).is_none());
    assert_cleanup_pending(&fixture, &id);
}

#[test]
fn a_recovered_landing_releases_verifier_ownership_to_durable_cleanup() {
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
    assert!(actuator.probes().is_empty());
    assert!(activity.active_for(fixture.project()).is_none());
    assert_cleanup_pending(&fixture, &id);
}

#[test]
fn a_cleanup_retry_never_reacquires_verifier_ownership() {
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

    assert_eq!(result, TickResult::Idle);
    assert!(actuator.probes().is_empty());
    assert!(activity.active_for(fixture.project()).is_none());
    assert_cleanup_pending(&fixture, &id);
}

fn assert_cleanup_pending(fixture: &ServiceFixture, id: &str) {
    let no = StoryNo::parse_id("SH", id).unwrap();
    let request = fixture
        .store()
        .read(|tx| tx.closure_cleanup(fixture.project(), no))
        .unwrap()
        .unwrap();
    assert!(!request.completed);
}
