//! SH-656: durable authority spans the gap between local admission and GitHub.

use storyhook::service::landing::{LandingAdmission, VerifiedSubmission};
use storyhook::service::{
    NewStoryInput, PrLinkService, RelationService, StoryService, VerificationQueue,
};
use storyhook::store::{ReadOps, Store};
use storyhook_test_support::ServiceFixture;

const PR: &str = "https://github.com/acme/widgets/pull/1";

#[test]
fn external_landing_evidence_cannot_prevent_recording_the_outcome() {
    struct EvidenceActuator(LandingOutcome);
    impl VerificationActuator for EvidenceActuator {
        fn submit(
            &self,
            _: &VerificationCandidate,
        ) -> Result<
            storyhook::domain::SubmittedPullRequest,
            storyhook::daemon::verification::SubmissionFailure,
        > {
            panic!("linked fixture does not submit")
        }
        fn verify(&self, _: &VerificationCandidate, _: &PrLink) -> VerificationOutcome {
            let certificate = certification();
            VerificationOutcome::Certified {
                head: certificate.head,
                tree: certificate.tree,
                gate: certificate.gate,
                detail: "Don't utilize this gate output as authored prose.".into(),
            }
        }
        fn land(&self, _: &VerificationCandidate, _: &LandingIntent) -> LandingOutcome {
            self.0.clone()
        }
        fn recover_landing(&self, _: &VerificationCandidate, _: &LandingIntent) -> LandingOutcome {
            self.0.clone()
        }
        fn notify(&self, _: &VerificationCandidate, _: &str) -> Result<NotifyDelivery, AppError> {
            panic!("landing evidence must not return work to an agent")
        }
        fn redispatch(&self, _: &VerificationCandidate, _: &ResumePlan) -> Result<(), AppError> {
            panic!("landing evidence must not redispatch work")
        }
        fn reap(&self, _: &VerificationCandidate) -> Result<(), AppError> {
            Ok(())
        }
    }

    let evidence = "Don't utilize this diagnostic as authored prose.\n```raw\nIt's external.\n```";
    for outcome in [
        LandingOutcome::Merged {
            detail: evidence.into(),
        },
        LandingOutcome::NotAttempted {
            detail: evidence.into(),
        },
        LandingOutcome::Uncertain {
            detail: evidence.into(),
        },
    ] {
        let f = ServiceFixture::new();
        let id = submitted(&f);
        let pending = matches!(outcome, LandingOutcome::Uncertain { .. });
        let merged = matches!(outcome, LandingOutcome::Merged { .. });
        let actuator = EvidenceActuator(outcome);
        assert_eq!(
            tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
            if merged {
                TickResult::Completed
            } else {
                TickResult::RetryLater
            }
        );
        let row = f
            .store()
            .read(|tx| tx.story(f.project(), storyhook::store::StoryNo::parse_id("SH", &id)?))
            .unwrap()
            .unwrap();
        assert_eq!(
            row.snapshot.state,
            if merged { "done" } else { "verifying" }
        );
        assert!(
            row.snapshot.comments.iter().any(|c| c
                .text
                .lines()
                .map(|line| line.strip_prefix("> ").unwrap_or(line))
                .collect::<Vec<_>>()
                .join("\n")
                .contains(evidence)),
            "quoted external evidence must preserve its line content"
        );
        assert_eq!(
            f.store().read(|tx| tx.landing_intents()).unwrap().len(),
            usize::from(pending)
        );
        if pending {
            let recovery = EvidenceActuator(LandingOutcome::Merged {
                detail: evidence.into(),
            });
            assert_eq!(
                tick_with(f.store(), f.env(), &recovery, f.project()).unwrap(),
                TickResult::Completed
            );
            assert!(
                f.store()
                    .read(|tx| tx.landing_intents())
                    .unwrap()
                    .is_empty()
            );
        }
    }
}

#[test]
fn pending_landing_recovery_respects_project_and_stop_permission() {
    use storyhook::daemon::verification::VerificationActivity;
    use storyhook::service::verification_control::VerificationAction;
    let f = ServiceFixture::new();
    submitted(&f);
    let q = VerificationQueue::new(f.store());
    let candidate = q.next().unwrap().unwrap();
    let LandingAdmission::Admitted(intent) = q
        .begin_landing(&f.ctx(), &candidate, &certification())
        .unwrap()
    else {
        panic!("expected admission");
    };
    let other = f.add_project("gadgets", "GD");
    let actuator = RacingActuator {
        fixture: &f,
        blocker: None,
        replacement: false,
        uncertain: false,
        recovered: true,
        landed: Mutex::new(Vec::new()),
    };
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, other).unwrap(),
        TickResult::Idle
    );
    assert_eq!(
        f.store().read(|tx| tx.landing_intents()).unwrap(),
        std::slice::from_ref(&intent)
    );
    let activity = VerificationActivity::new();
    activity
        .control(f.store(), f.project(), VerificationAction::Stop)
        .unwrap();
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::Stopped
    );
    assert_eq!(f.store().read(|tx| tx.landing_intents()).unwrap(), [intent]);
    activity
        .control(f.store(), f.project(), VerificationAction::Start)
        .unwrap();
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::Completed
    );
    assert!(
        f.store()
            .read(|tx| tx.landing_intents())
            .unwrap()
            .is_empty()
    );
    assert!(
        actuator.landed.lock().unwrap().is_empty(),
        "recovery must not send another merge"
    );
}

fn submitted(f: &ServiceFixture) -> String {
    f.link_origin("https://github.com/acme/widgets");
    let ctx = f.ctx();
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "submission".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    PrLinkService::new(&ctx).link(&id, PR, true).unwrap();
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    id
}

fn certification() -> VerifiedSubmission {
    VerifiedSubmission {
        head: "a".repeat(40),
        tree: "b".repeat(40),
        gate: "make test".into(),
    }
}

#[test]
fn open_blocker_holds_a_visible_submission_without_admitting_landing() {
    let f = ServiceFixture::new();
    let id = submitted(&f);
    let blocker = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "blocker".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    let queue = VerificationQueue::new(f.store());
    let candidate = queue.next().unwrap().unwrap();
    RelationService::new(&f.ctx())
        .relate(&id, "blocked-by", &blocker, false)
        .unwrap();
    assert!(queue.next().unwrap().is_none());
    assert_eq!(
        queue.ordered().unwrap()[0].blocked_by,
        std::slice::from_ref(&blocker)
    );
    assert!(matches!(
        queue
            .begin_landing(&f.ctx(), &candidate, &certification())
            .unwrap(),
        LandingAdmission::Held(_)
    ));
    StoryService::new(&f.ctx())
        .set_state(&blocker, "done", None, None, None)
        .unwrap();
    assert_eq!(
        queue.next().unwrap().unwrap().verifying_generation,
        candidate.verifying_generation
    );
}

#[test]
fn committed_intent_refuses_new_blockers_and_submission_changes_but_allows_comments() {
    let f = ServiceFixture::new();
    let id = submitted(&f);
    let blocker = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "blocker".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    let queue = VerificationQueue::new(f.store());
    let candidate = queue.next().unwrap().unwrap();
    let LandingAdmission::Admitted(intent) = queue
        .begin_landing(&f.ctx(), &candidate, &certification())
        .unwrap()
    else {
        panic!("expected landing authority");
    };
    let error = RelationService::new(&f.ctx())
        .relate(&id, "blocked-by", &blocker, false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("landing") && error.contains(&id), "{error}");
    assert!(
        StoryService::new(&f.ctx())
            .set_state(&id, "in-progress", None, None, None)
            .is_err()
    );
    StoryService::new(&f.ctx())
        .comment(&id, "still observable")
        .unwrap();
    assert_eq!(f.store().read(|tx| tx.landing_intents()).unwrap(), [intent]);
    assert!(queue.next().unwrap().is_none());
}

#[test]
fn reset_and_landing_reservations_exclude_each_other_in_both_orders() {
    use storyhook::store::WriteOps;
    for reset_first in [false, true] {
        let f = ServiceFixture::new();
        submitted(&f);
        let intent = admit(&f);
        if reset_first {
            f.store()
                .write(|tx| tx.remove_landing_intent(&intent))
                .unwrap();
            f.store()
                .write(|tx| tx.put_story_reset(f.project(), intent.story, Some("{}")))
                .unwrap();
            let error = f
                .store()
                .write(|tx| tx.insert_landing_intent(&intent))
                .unwrap_err()
                .to_string();
            assert!(error.contains("reset"), "{error}");
            assert!(
                f.store()
                    .read(|tx| tx.landing_intents())
                    .unwrap()
                    .is_empty()
            );
            assert!(VerificationQueue::new(f.store()).next().unwrap().is_none());
        } else {
            let error = f
                .store()
                .write(|tx| tx.put_story_reset(f.project(), intent.story, Some("{}")))
                .unwrap_err()
                .to_string();
            assert!(error.contains("landing"), "{error}");
            assert!(
                f.store()
                    .read(|tx| tx.story_resets(f.project()))
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(f.store().read(|tx| tx.landing_intents()).unwrap(), [intent]);
        }
    }
}

#[test]
fn intent_survives_a_second_connection_and_cannot_be_replaced() {
    let f = ServiceFixture::new();
    submitted(&f);
    let queue = VerificationQueue::new(f.store());
    let candidate = queue.next().unwrap().unwrap();
    let LandingAdmission::Admitted(intent) = queue
        .begin_landing(&f.ctx(), &candidate, &certification())
        .unwrap()
    else {
        panic!("expected admission");
    };
    assert!(matches!(
        queue
            .begin_landing(&f.ctx(), &candidate, &certification())
            .unwrap(),
        LandingAdmission::Pending(_)
    ));
    let reopened = storyhook::store::SqliteStore::open(f.store().path()).unwrap();
    assert_eq!(reopened.read(|tx| tx.landing_intents()).unwrap(), [intent]);
}

fn admit(f: &ServiceFixture) -> storyhook::store::LandingIntent {
    let q = VerificationQueue::new(f.store());
    let candidate = q.next().unwrap().unwrap();
    let LandingAdmission::Admitted(intent) = q
        .begin_landing(&f.ctx(), &candidate, &certification())
        .unwrap()
    else {
        panic!("admission refused")
    };
    intent
}

fn historical_edge(f: &ServiceFixture, id: &str, target: &str) {
    use storyhook::domain::{StoryEvent, fold_story};
    use storyhook::store::{ExpectedSeq, StoryNo, WriteOps, partition_known};
    f.store()
        .write(|tx| {
            for (source, target, relation) in [(id, target, "blocked-by"), (target, id, "blocks")] {
                let no = StoryNo::parse_id("SH", source)?;
                let head = tx.append_events(
                    f.project(),
                    no,
                    ExpectedSeq::Any,
                    &[StoryEvent::StoryRelationshipAdded {
                        at: storyhook_test_support::FIXTURE_NOW.into(),
                        relation: relation.into(),
                        other_id: target.into(),
                    }],
                    &storyhook::domain::provenance::Provenance::unrecorded(),
                )?;
                let (known, _) = partition_known(no, &tx.events_for(f.project(), no)?);
                let snapshot = fold_story(source, &known, &tx.state_map(f.project())?)?;
                tx.put_story(f.project(), &snapshot, head)?;
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn reopening_a_historical_closed_dependency_is_fenced_on_another_connection() {
    let f = ServiceFixture::new();
    let id = submitted(&f);
    let blocker = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "closed dependency".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    StoryService::new(&f.ctx())
        .set_state(&blocker, "dropped", None, None, None)
        .unwrap();
    historical_edge(&f, &id, &blocker);
    let intent = admit(&f);
    let other = storyhook::store::SqliteStore::open(f.store().path()).unwrap();
    let ctx = storyhook::service::Ctx::new(
        &other,
        f.project(),
        intent.checkout.clone(),
        f.env().clone(),
    )
    .no_hooks(true);
    let error = StoryService::new(&ctx)
        .reopen(&blocker)
        .unwrap_err()
        .to_string();
    assert!(error.contains("open blockers introduced"), "{error}");
    assert_eq!(other.read(|tx| tx.landing_intents()).unwrap(), [intent]);
}

#[test]
fn materializing_a_missing_dependency_rolls_back_and_unrelated_creation_proceeds() {
    use storyhook::store::{StoryNo, WriteOps};
    let f = ServiceFixture::new();
    let id = submitted(&f);
    let target = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "missing row".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    historical_edge(&f, &id, &target);
    let no = StoryNo::parse_id("SH", &target).unwrap();
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), no))
        .unwrap()
        .unwrap();
    // Old/damaged read models can have missing rows even though live relation
    // writes reject dangling targets. Exercise repair materialization directly.
    rusqlite::Connection::open(f.store().path())
        .unwrap()
        .execute(
            "DELETE FROM stories WHERE project_id = ?1 AND story_no = ?2",
            rusqlite::params![f.project().get(), no.get()],
        )
        .unwrap();
    let intent = admit(&f);
    let error = f
        .store()
        .write(|tx| tx.put_story(f.project(), &row.snapshot, row.head_seq))
        .unwrap_err()
        .to_string();
    assert!(error.contains("open blockers introduced"), "{error}");
    StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "unrelated work".into(),
            ..Default::default()
        })
        .unwrap();
    // Restore the deliberately damaged fixture only after ending its test intent.
    f.store()
        .write(|tx| {
            tx.remove_landing_intent(&intent)?;
            tx.put_story(f.project(), &row.snapshot, row.head_seq)
        })
        .unwrap();
}

#[test]
fn stale_identity_cannot_resolve_an_intent_and_admin_authority_changes_roll_back() {
    use storyhook::store::WriteOps;
    let f = ServiceFixture::new();
    let id = submitted(&f);
    let intent = admit(&f);
    let mut stale = intent.clone();
    stale.certification.head = "c".repeat(40);
    assert!(
        !VerificationQueue::new(f.store())
            .complete_landing(&f.ctx(), &stale, "stale")
            .unwrap()
    );
    assert!(PrLinkService::new(&f.ctx()).unlink(&id, PR).is_err());
    assert!(
        f.store()
            .write(|tx| tx.set_checkout_path(f.project(), Some(std::path::Path::new("/different"))))
            .is_err()
    );
    assert!(
        f.store()
            .write(|tx| {
                let mut states = tx.states(f.project())?;
                states.retain(|s| s.slug != "done");
                tx.put_states(f.project(), &states)
            })
            .is_err()
    );
    StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "unrelated work proceeds".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(f.store().read(|tx| tx.landing_intents()).unwrap(), [intent]);
}

use std::sync::Mutex;
use storyhook::daemon::verification::{
    LandingOutcome, NotifyDelivery, ResumePlan, TickResult, VerificationActuator,
    VerificationOutcome, tick_with,
};
use storyhook::error::AppError;
use storyhook::service::VerificationCandidate;
use storyhook::store::{LandingIntent, PrLink};

struct RacingActuator<'a> {
    fixture: &'a ServiceFixture,
    blocker: Option<String>,
    replacement: bool,
    uncertain: bool,
    recovered: bool,
    landed: Mutex<Vec<String>>,
}
impl VerificationActuator for RacingActuator<'_> {
    fn submit(
        &self,
        _: &VerificationCandidate,
    ) -> Result<
        storyhook::domain::SubmittedPullRequest,
        storyhook::daemon::verification::SubmissionFailure,
    > {
        panic!("unleased fixture must not submit")
    }
    fn verify(&self, candidate: &VerificationCandidate, _: &PrLink) -> VerificationOutcome {
        if let Some(blocker) = &self.blocker {
            RelationService::new(&self.fixture.ctx())
                .relate(&candidate.story_id, "blocked-by", blocker, false)
                .unwrap();
        }
        if self.replacement && candidate.pull_request.as_ref().unwrap().url == PR {
            let ctx = self.fixture.ctx();
            let service = PrLinkService::new(&ctx);
            service.unlink(&candidate.story_id, PR).unwrap();
            service
                .link(
                    &candidate.story_id,
                    "https://github.com/acme/widgets/pull/3",
                    true,
                )
                .unwrap();
        }
        let certificate = certification();
        VerificationOutcome::Certified {
            head: certificate.head,
            tree: certificate.tree,
            gate: certificate.gate,
            detail: "tested".into(),
        }
    }
    fn land(&self, candidate: &VerificationCandidate, intent: &LandingIntent) -> LandingOutcome {
        assert!(
            self.fixture
                .store()
                .read(|tx| tx.landing_intents())
                .unwrap()
                .contains(intent)
        );
        self.landed.lock().unwrap().push(candidate.story_id.clone());
        if self.uncertain {
            LandingOutcome::Uncertain {
                detail: "response lost after sending merge".into(),
            }
        } else {
            LandingOutcome::Merged {
                detail: "confirmed".into(),
            }
        }
    }
    fn recover_landing(&self, _: &VerificationCandidate, _: &LandingIntent) -> LandingOutcome {
        if self.recovered {
            LandingOutcome::Merged {
                detail: "confirmed after restart".into(),
            }
        } else {
            LandingOutcome::Uncertain {
                detail: "OPEN is inconclusive".into(),
            }
        }
    }
    fn notify(&self, _: &VerificationCandidate, _: &str) -> Result<NotifyDelivery, AppError> {
        panic!("held work must never notify or redispatch")
    }
    fn redispatch(&self, _: &VerificationCandidate, _: &ResumePlan) -> Result<(), AppError> {
        panic!("held work must never redispatch")
    }
    fn reap(&self, _: &VerificationCandidate) -> Result<(), AppError> {
        Ok(())
    }
}

#[test]
fn blocker_added_inside_verification_prevents_the_merge_actuator() {
    let f = ServiceFixture::new();
    let id = submitted(&f);
    let blocker = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "racing dependency".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    let actuator = RacingActuator {
        fixture: &f,
        blocker: Some(blocker),
        replacement: false,
        uncertain: false,
        recovered: false,
        landed: Mutex::new(Vec::new()),
    };
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::RetryLater
    );
    assert!(actuator.landed.lock().unwrap().is_empty());
    let q = VerificationQueue::new(f.store());
    assert_eq!(q.ordered().unwrap()[0].story_id, id);
    assert!(q.next().unwrap().is_none());
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::Idle
    );
}

#[test]
fn uncertain_restart_preserves_fence_without_starving_other_work_then_completes_once() {
    let f = ServiceFixture::new();
    let first = submitted(&f);
    let actuator = RacingActuator {
        fixture: &f,
        blocker: None,
        replacement: false,
        uncertain: true,
        recovered: false,
        landed: Mutex::new(Vec::new()),
    };
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::RetryLater
    );
    let second = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "unrelated submission".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    PrLinkService::new(&f.ctx())
        .link(&second, "https://github.com/acme/widgets/pull/2", true)
        .unwrap();
    StoryService::new(&f.ctx())
        .set_state(&second, "verifying", None, None, None)
        .unwrap();
    let reopened = storyhook::store::SqliteStore::open(f.store().path()).unwrap();
    let other = RacingActuator {
        fixture: &f,
        blocker: None,
        replacement: false,
        uncertain: false,
        recovered: false,
        landed: Mutex::new(Vec::new()),
    };
    // The pending first story is observed, never retried; the next story lands.
    assert_eq!(
        tick_with(&reopened, f.env(), &other, f.project()).unwrap(),
        TickResult::Completed
    );
    assert_eq!(*other.landed.lock().unwrap(), [second]);
    assert_eq!(
        reopened.read(|tx| tx.landing_intents()).unwrap()[0].story_id,
        first
    );
    let recovered = RacingActuator {
        recovered: true,
        ..other
    };
    assert_eq!(
        tick_with(&reopened, f.env(), &recovered, f.project()).unwrap(),
        TickResult::Completed
    );
    assert!(reopened.read(|tx| tx.landing_intents()).unwrap().is_empty());
}

#[test]
fn held_and_pending_statuses_remain_visible_without_consuming_queue_positions() {
    use storyhook::daemon::verification_progress::{VerificationStatus, status_snapshot};
    let f = ServiceFixture::new();
    submitted(&f);
    let mut candidate = VerificationQueue::new(f.store()).next().unwrap().unwrap();
    let mut held = candidate.clone();
    held.story_id = "SH-2".into();
    held.blocked_by = vec!["SH-3".into()];
    let mut pending = candidate.clone();
    pending.story_id = "SH-4".into();
    pending.landing_pending = true;
    candidate.story_id = "SH-5".into();
    let statuses = status_snapshot(
        &[held, pending, candidate],
        None,
        f.env(),
        storyhook_test_support::FIXTURE_NOW,
    );
    assert!(
        matches!(&statuses[0].2, VerificationStatus::Held { blockers } if blockers == &["SH-3"])
    );
    assert!(matches!(&statuses[1].2, VerificationStatus::LandingPending));
    assert!(matches!(
        &statuses[2].2,
        VerificationStatus::Queued { position: 1, .. }
    ));
}

#[test]
fn completion_failure_preserves_the_intent_and_rolls_back_every_event() {
    use storyhook::store::fault::{FaultAction, FaultPoint, arm};
    let f = ServiceFixture::new();
    submitted(&f);
    let intent = admit(&f);
    let before = f
        .store()
        .read(|tx| tx.events_for(f.project(), intent.story))
        .unwrap();
    let guard = arm(
        FaultPoint::BeforeCommit,
        FaultAction::Fail("completion fault".into()),
    );
    let q = VerificationQueue::new(f.store());
    assert!(q.complete_landing(&f.ctx(), &intent, "confirmed").is_err());
    drop(guard);
    assert_eq!(
        f.store()
            .read(|tx| tx.events_for(f.project(), intent.story))
            .unwrap(),
        before
    );
    assert_eq!(
        f.store().read(|tx| tx.landing_intents()).unwrap(),
        std::slice::from_ref(&intent)
    );
    assert!(
        q.complete_landing(&f.ctx(), &intent, "confirmed retry")
            .unwrap()
    );
    assert!(!q.complete_landing(&f.ctx(), &intent, "duplicate").unwrap());
}

#[test]
fn concurrent_admission_and_blocker_insertion_have_exactly_one_winner() {
    use std::sync::Barrier;
    let f = ServiceFixture::new();
    let id = submitted(&f);
    let blocker = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "concurrent blocker".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    let candidate = VerificationQueue::new(f.store()).next().unwrap().unwrap();
    let other = storyhook::store::SqliteStore::open(f.store().path()).unwrap();
    let barrier = Barrier::new(2);
    std::thread::scope(|scope| {
        let writer = scope.spawn(|| {
            let ctx = storyhook::service::Ctx::new(
                &other,
                f.project(),
                candidate.checkout.clone(),
                f.env().clone(),
            )
            .no_hooks(true);
            barrier.wait();
            RelationService::new(&ctx).relate(&id, "blocked-by", &blocker, false)
        });
        barrier.wait();
        let admission = VerificationQueue::new(f.store())
            .begin_landing(&f.ctx(), &candidate, &certification())
            .unwrap();
        let insertion = writer.join().unwrap();
        match admission {
            LandingAdmission::Admitted(_) => assert!(insertion.is_err()),
            LandingAdmission::Held(_) => assert!(insertion.is_ok()),
            other => panic!("unexpected race result: {other:?}"),
        }
    });
}

#[test]
fn a_replaced_pr_in_the_same_generation_is_reverified_before_landing() {
    let f = ServiceFixture::new();
    let id = submitted(&f);
    let candidate = VerificationQueue::new(f.store()).next().unwrap().unwrap();
    let actuator = RacingActuator {
        fixture: &f,
        blocker: None,
        replacement: true,
        uncertain: false,
        recovered: false,
        landed: Mutex::new(Vec::new()),
    };
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::Completed
    );
    assert_eq!(*actuator.landed.lock().unwrap(), [id]);
    let events = f
        .store()
        .read(|tx| {
            tx.events_for(
                f.project(),
                storyhook::store::StoryNo::parse_id("SH", &candidate.story_id)?,
            )
        })
        .unwrap();
    assert!(events.iter().any(|event| matches!(event.known(), Some(storyhook::domain::StoryEvent::StoryPrMerged { url, .. }) if url.ends_with("/pull/3"))));
}
