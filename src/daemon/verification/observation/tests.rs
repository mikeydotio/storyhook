//! Observation scheduling uses the real bounded bus; authority uses real stores.

use super::*;
use crate::service::{NewStoryInput, PrLinkService};
use storyhook_test_support::ServiceFixture;

struct Fixture {
    _seed: ServiceFixture,
    store: crate::store::SqliteStore,
    env: Environment,
    project: ProjectId,
}

impl Fixture {
    fn new() -> Self {
        let seed = ServiceFixture::new();
        seed.github_checkout("https://github.com/acme/widgets");
        // Test-support links another crate instance; reopen the real seed
        // with this unit-test crate's types, as the control tests do.
        let store = crate::store::SqliteStore::open(seed.store().path()).unwrap();
        let env = Environment::at(seed.cwd());
        let project = ProjectId::new(seed.project().get());
        Self {
            _seed: seed,
            store,
            env,
            project,
        }
    }

    fn ctx(&self) -> Ctx<'_, crate::store::SqliteStore> {
        Ctx::new(
            &self.store,
            self.project,
            self.env.home().to_path_buf(),
            self.env.clone(),
        )
        .no_hooks(true)
    }

    fn store(&self) -> &crate::store::SqliteStore {
        &self.store
    }
}

fn candidate(f: &Fixture) -> VerificationCandidate {
    let id = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "Withdrawn verification".into(),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id;
    PrLinkService::new(&f.ctx())
        .link(&id, "https://github.com/acme/widgets/pull/1", true)
        .unwrap();
    StoryService::new(&f.ctx())
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    VerificationQueue::new(f.store()).next().unwrap().unwrap()
}

#[test]
fn stale_authority_prevents_spawn() {
    let f = Fixture::new();
    let c = candidate(&f);
    // An operator completing the story by hand is an override and carries
    // its reason (SH-692); the bare move is refused.
    StoryService::new(&f.ctx())
        .set_state(
            &c.story_id,
            "done",
            Some("completed by hand before the attempt started"),
            None,
            None,
        )
        .unwrap();
    assert!(
        verify(
            f.store(),
            &ChangeBus::new(),
            &c,
            &Cancellation::default(),
            |_| panic!("stale attempt started")
        )
        .unwrap()
        .is_none()
    );
}

#[test]
fn human_only_removed_between_reads_still_revokes_the_old_attempt() {
    let f = Fixture::new();
    let c = candidate(&f);
    let ctx = f.ctx();
    let service = StoryService::new(&ctx);
    service
        .set_labels(&c.story_id, &["human-only".into()], &[])
        .unwrap();
    service
        .set_labels(&c.story_id, &[], &["human-only".into()])
        .unwrap();
    assert!(!current(f.store(), &c).unwrap());
    let fresh = VerificationQueue::new(f.store()).next().unwrap().unwrap();
    assert!(current(f.store(), &fresh).unwrap());
}

#[test]
fn a_block_cleared_between_observer_reads_still_withdraws_the_attempt() {
    let f = Fixture::new();
    let c = candidate(&f);
    let ctx = f.ctx();
    let svc = StoryService::new(&ctx);
    svc.set_awaiting(&c.story_id, "temporary hold").unwrap();
    svc.clear_awaiting(&c.story_id).unwrap();
    assert!(
        !current(f.store(), &c).unwrap(),
        "the pre-block attempt retained authority"
    );
    let next = VerificationQueue::new(f.store()).next().unwrap().unwrap();
    assert!(
        current(f.store(), &next).unwrap(),
        "a fresh admission after unblock is valid"
    );
}

#[test]
fn completed_outcomes_are_rechecked_even_without_a_notification() {
    for outcome in [
        VerificationOutcome::Certified {
            head: "a".repeat(40),
            tree: "tree".into(),
            detail: "certified".into(),
            gate: "gate".into(),
        },
        VerificationOutcome::TestsFailed {
            tree: "tree".into(),
            detail: "red".into(),
            gate: "gate".into(),
            log: "log".into(),
        },
        VerificationOutcome::InfrastructureFailure {
            detail: "failed".into(),
            disposition: VerificationFailureDisposition::Permanent,
        },
    ] {
        let f = Fixture::new();
        let c = candidate(&f);
        let result = verify(
            f.store(),
            &ChangeBus::new(),
            &c,
            &Cancellation::default(),
            |_| {
                // The hand completion that withdraws the attempt is an
                // override with a reason (SH-692).
                StoryService::new(&f.ctx())
                    .set_state(
                        &c.story_id,
                        "done",
                        Some("completed by hand while the attempt ran"),
                        None,
                        None,
                    )
                    .unwrap();
                outcome
            },
        )
        .unwrap();
        assert!(result.is_none(), "stale outcome was accepted");
    }
}

#[test]
fn unchanged_generation_edits_keep_the_outcome() {
    let f = Fixture::new();
    let c = candidate(&f);
    let bus = ChangeBus::new();
    let outcome = VerificationOutcome::Conflict {
        detail: "expected".into(),
    };
    let result = verify(f.store(), &bus, &c, &Cancellation::default(), |token| {
        StoryService::new(&f.ctx())
            .comment(&c.story_id, "ordinary comment")
            .unwrap();
        bus.publish(Change::Project(c.project_slug.clone()));
        bus.publish(Change::Resync);
        assert!(!token.is_cancelled());
        outcome.clone()
    })
    .unwrap();
    assert_eq!(result, Some(outcome));
}

#[test]
fn missing_events_and_unrelated_flood_cannot_extend_recovery() {
    for flood in [false, true] {
        let bus = ChangeBus::new();
        let subscription = bus.subscribe();
        let done = AtomicBool::new(false);
        let manual = Cancellation::default();
        let attempt = Cancellation::default();
        std::thread::scope(|scope| {
            let _finish = Finish(&done);
            if flood {
                scope.spawn(|| {
                    while !done.load(Ordering::Acquire) {
                        bus.publish(Change::Project("other".into()));
                        std::thread::sleep(Duration::from_millis(1));
                    }
                });
            }
            let started = Instant::now();
            assert!(
                !observe(
                    &subscription,
                    &done,
                    &manual,
                    &attempt,
                    "fixture",
                    Duration::from_millis(80),
                    || Ok(false)
                )
                .unwrap()
            );
            assert!(started.elapsed() < Duration::from_secs(2));
        });
        assert!(attempt.is_cancelled());
        assert!(!manual.is_cancelled());
    }
}

#[test]
fn overflow_rechecks_authority_before_the_recovery_deadline() {
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    bus.publish(Change::Project("fixture".into()));
    for _ in 0..64 {
        bus.publish(Change::Ping);
    }
    assert!(subscription.dropped() > 0);
    let attempt = Cancellation::default();
    let started = Instant::now();
    assert!(
        !observe(
            &subscription,
            &AtomicBool::new(false),
            &Cancellation::default(),
            &attempt,
            "fixture",
            Duration::from_secs(30),
            || Ok(false)
        )
        .unwrap()
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(attempt.is_cancelled());
}

#[test]
fn observation_error_cancels_without_poisoning_manual_control() {
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    bus.publish(Change::Resync);
    let manual = Cancellation::default();
    let attempt = Cancellation::default();
    let error = observe(
        &subscription,
        &AtomicBool::new(false),
        &manual,
        &attempt,
        "fixture",
        Duration::from_secs(30),
        || Err(AppError::Storage("read failed".into())),
    )
    .unwrap_err();
    assert!(error.to_string().contains("read failed"));
    assert!(attempt.is_cancelled());
    assert!(!manual.is_cancelled());
}

#[test]
fn manual_cancellation_is_propagated_even_without_store_events() {
    let bus = ChangeBus::new();
    let manual = Cancellation::default();
    manual.cancel();
    let attempt = Cancellation::default();
    assert!(
        observe(
            &bus.subscribe(),
            &AtomicBool::new(false),
            &manual,
            &attempt,
            "fixture",
            Duration::from_secs(30),
            || panic!("manual stop needs no store read")
        )
        .unwrap()
    );
    assert!(attempt.is_cancelled());
    assert!(manual.is_cancelled());
}

#[test]
fn panic_in_actuator_stops_and_joins_the_monitor() {
    let f = Fixture::new();
    let c = candidate(&f);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = verify(
            f.store(),
            &ChangeBus::new(),
            &c,
            &Cancellation::default(),
            |_| panic!("actuator panic"),
        );
    }));
    assert!(result.is_err());
}

#[test]
fn human_observer_cancels_owned_work_outside_verifying_and_preserves_errors() {
    for state in ["verifying", "in-progress"] {
        let f = Fixture::new();
        let c = candidate(&f);
        if state != "verifying" {
            StoryService::new(&f.ctx())
                .set_state(&c.story_id, state, Some("operator state"), None, None)
                .unwrap();
        }
        let bus = ChangeBus::new();
        let cancellation = Cancellation::default();
        let result = human_owned(f.store(), &f.env, &bus, &c, &cancellation, || {
            StoryService::new(&f.ctx())
                .set_labels(&c.story_id, &["human-only".into()], &[])
                .unwrap();
            bus.publish(Change::Project(c.project_slug.clone()));
            let deadline = Instant::now() + Duration::from_secs(2);
            while !cancellation.is_cancelled() {
                assert!(
                    Instant::now() < deadline,
                    "label observer did not cancel {state}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(AppError::Storage("owned child cleanup diagnostic".into()))
        })
        .unwrap();
        assert_eq!(result, TickResult::Returned);
        let row = f
            .store
            .read(|tx| tx.story(f.project, crate::store::StoryNo::new(1)))
            .unwrap()
            .unwrap();
        assert_eq!(row.state, state);
        assert!(
            row.snapshot
                .comments
                .iter()
                .any(|comment| comment.text.contains("owned child cleanup diagnostic"))
        );
    }
}

#[test]
fn revoked_candidates_cannot_write_verdicts_progress_or_remediation() {
    use crate::service::verification::GenerationWrite;
    for transient in [false, true] {
        let f = Fixture::new();
        let c = candidate(&f);
        let ctx = f.ctx();
        let queue = VerificationQueue::new(f.store());
        StoryService::new(&ctx)
            .set_labels(&c.story_id, &["human-only".into()], &[])
            .unwrap();
        if transient {
            StoryService::new(&ctx)
                .set_labels(&c.story_id, &[], &["human-only".into()])
                .unwrap();
        }
        assert!(matches!(
            queue
                .record_generation_completed(&ctx, &c, "verdict", None)
                .unwrap(),
            GenerationWrite::Superseded
        ));
        assert!(matches!(
            queue
                .record_generation_returned(&ctx, &c, "repair")
                .unwrap(),
            GenerationWrite::Superseded
        ));
        assert!(matches!(
            queue
                .upsert_generation_comment(&ctx, &c, "progress", "progress", None)
                .unwrap(),
            GenerationWrite::Superseded
        ));
        StoryService::new(&ctx)
            .set_state(&c.story_id, "in-progress", None, None, None)
            .unwrap();
        assert!(matches!(
            queue.set_generation_awaiting(&ctx, &c, "park").unwrap(),
            GenerationWrite::Superseded
        ));
        queue
            .comment_if_human_permitted(&ctx, &c, "stale lifecycle comment")
            .unwrap();
        let row = f
            .store
            .read(|tx| tx.story(f.project, crate::store::StoryNo::new(1)))
            .unwrap()
            .unwrap();
        assert!(row.awaiting.is_none());
        assert!(
            !row.snapshot
                .comments
                .iter()
                .any(|comment| comment.text == "stale lifecycle comment")
        );
    }
}

#[test]
fn admission_refuses_current_and_transient_human_reservations_without_ownership() {
    for transient in [false, true] {
        let f = Fixture::new();
        let c = candidate(&f);
        StoryService::new(&f.ctx())
            .set_labels(&c.story_id, &["human-only".into()], &[])
            .unwrap();
        if transient {
            StoryService::new(&f.ctx())
                .set_labels(&c.story_id, &[], &["human-only".into()])
                .unwrap();
        }
        let activity = VerificationActivity::new();
        assert!(
            activity
                .try_acquire(f.store(), &c, f.env.now())
                .unwrap()
                .is_none()
        );
        assert!(activity.active_all().is_empty());
    }
}

#[test]
fn human_withdrawal_preserves_cleanup_failure_evidence_without_a_verdict() {
    let f = Fixture::new();
    let c = candidate(&f);
    let result = verify(
        f.store(),
        &ChangeBus::new(),
        &c,
        &Cancellation::default(),
        |_| {
            StoryService::new(&f.ctx())
                .set_labels(&c.story_id, &["human-only".into()], &[])
                .unwrap();
            VerificationOutcome::CleanupFailed {
                verdict: CompletedVerification::GatePassed {
                    tree: "tree".into(),
                    log: "log".into(),
                    detail: "passed".into(),
                    gate: "gate".into(),
                },
                cleanup: VerificationCleanupFailure {
                    phase: "writer-drain".into(),
                    detail: "child still owns workspace".into(),
                    owner: Some("owner.json".into()),
                    worktree: Some("retained-worktree".into()),
                    disposition: VerificationFailureDisposition::Permanent,
                },
            }
        },
    )
    .unwrap();
    assert!(result.is_none());
    let row = f
        .store
        .read(|tx| tx.story(f.project, crate::store::StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "verifying");
    assert!(
        row.snapshot
            .comments
            .iter()
            .any(|comment| comment.text.contains("child still owns workspace"))
    );
    assert!(
        !row.snapshot
            .comments
            .iter()
            .any(|comment| comment.text.starts_with(VERIFICATION_GREEN_PREFIX))
    );
}

#[test]
fn landing_completion_requires_the_admitted_human_revision() {
    use crate::service::landing::{LandingAdmission, VerifiedSubmission};
    let f = Fixture::new();
    let c = candidate(&f);
    let ctx = f.ctx();
    let queue = VerificationQueue::new(f.store());
    let certificate = VerifiedSubmission {
        head: "a".repeat(40),
        tree: "b".repeat(40),
        gate: "gate".into(),
    };
    let LandingAdmission::Admitted(intent) = queue.begin_landing(&ctx, &c, &certificate).unwrap()
    else {
        panic!("expected admission")
    };
    StoryService::new(&ctx)
        .set_labels(&c.story_id, &["human-only".into()], &[])
        .unwrap();
    StoryService::new(&ctx)
        .set_labels(&c.story_id, &[], &["human-only".into()])
        .unwrap();
    assert!(
        !queue
            .complete_landing_for(&ctx, &c, &intent, "old merge result")
            .unwrap()
    );
    assert_eq!(
        f.store.read(|tx| tx.landing_intents()).unwrap(),
        vec![intent.clone()]
    );
    let fresh = queue.ordered_for(f.project).unwrap().remove(0);
    assert!(
        queue
            .complete_landing_for(&ctx, &fresh, &intent, "reconciled merge")
            .unwrap()
    );
}

/// SH-772: the daemon's landing door records the Resume of every active story
/// it unblocks, exactly as every other story mutation does.
#[test]
fn landing_completion_resumes_the_dependents_it_unblocks() {
    use crate::service::RelationService;
    use crate::service::landing::{LandingAdmission, VerifiedSubmission};
    use crate::store::BlockAction;
    let f = Fixture::new();
    let c = candidate(&f);
    let ctx = f.ctx();
    let dependent = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Waiting on the submission".into(),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id;
    StoryService::new(&ctx)
        .set_state(&dependent, "in-progress", None, None, None)
        .unwrap();
    RelationService::new(&ctx)
        .relate(&dependent, "blocked-by", &c.story_id, false)
        .unwrap();
    let queue = VerificationQueue::new(f.store());
    let fresh = queue.ordered_for(f.project).unwrap().remove(0);
    let certificate = VerifiedSubmission {
        head: "a".repeat(40),
        tree: "b".repeat(40),
        gate: "gate".into(),
    };
    let LandingAdmission::Admitted(intent) =
        queue.begin_landing(&ctx, &fresh, &certificate).unwrap()
    else {
        panic!("expected admission")
    };
    assert!(
        queue
            .complete_landing_for(&ctx, &fresh, &intent, "merged")
            .unwrap()
    );
    let story = crate::store::StoryNo::parse_id("SH", &dependent).unwrap();
    let actions: Vec<BlockAction> = f
        .store
        .read(|tx| tx.block_deliveries(f.project))
        .unwrap()
        .into_iter()
        .filter(|delivery| delivery.story == story)
        .map(|delivery| delivery.action)
        .collect();
    assert_eq!(actions, [BlockAction::Interrupt, BlockAction::Resume]);
}
