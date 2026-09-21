//! Human reservations across admission, external operations, and recovery.

use super::*;
use storyhook::daemon::bus::ChangeBus;
use storyhook::daemon::verification::{LandingOutcome, wait_for_reconciled_candidate};
use storyhook::service::landing::{LandingAdmission, VerifiedSubmission};
use storyhook::store::LandingIntent;

fn reserve(f: &ServiceFixture, id: &str, transient: bool) {
    let number = StoryNo::parse_id("SH", id).unwrap();
    if f.store()
        .read(|tx| tx.story(f.project(), number))
        .unwrap()
        .unwrap()
        .superstate
        == SuperState::Closed
    {
        // Closed stories reject ordinary edits. Imported history can still
        // contain labels; exercise the real append/fold path for that data.
        f.store()
            .write(|tx| {
                let mut changes = vec![StoryEvent::StoryLabelsSet {
                    at: FIXTURE_NOW.into(),
                    labels: vec!["human-only".into()],
                }];
                if transient {
                    changes.push(StoryEvent::StoryLabelsSet {
                        at: FIXTURE_NOW.into(),
                        labels: vec![],
                    });
                }
                let head = tx.append_events(
                    f.project(),
                    number,
                    storyhook::store::ExpectedSeq::Any,
                    &changes,
                    &storyhook::domain::provenance::Provenance::unrecorded(),
                )?;
                let stored = tx.events_for(f.project(), number)?;
                let (known, _) = storyhook::store::partition_known(number, &stored);
                let snapshot =
                    storyhook::domain::fold_story(id, &known, &tx.state_map(f.project())?)?;
                tx.put_story(f.project(), &snapshot, head)
            })
            .unwrap();
        return;
    }
    StoryService::new(&f.ctx())
        .set_labels(id, &["human-only".into()], &[])
        .unwrap();
    if transient {
        StoryService::new(&f.ctx())
            .set_labels(id, &[], &["human-only".into()])
            .unwrap();
    }
}

fn certificate() -> VerifiedSubmission {
    VerifiedSubmission {
        head: "a".repeat(40),
        tree: "b".repeat(40),
        gate: "test gate".into(),
    }
}

fn certified() -> VerificationOutcome {
    let c = certificate();
    VerificationOutcome::Certified {
        head: c.head,
        tree: c.tree,
        gate: c.gate,
        detail: "external gate passed".into(),
    }
}

struct ReservingActuator<'a> {
    fixture: &'a ServiceFixture,
    phase: &'static str,
    transient: bool,
    inner: FakeActuator,
}

impl ReservingActuator<'_> {
    fn enter(&self, phase: &str, candidate: &VerificationCandidate) {
        if self.phase == phase {
            reserve(self.fixture, &candidate.story_id, self.transient);
        }
    }
}

impl VerificationActuator for ReservingActuator<'_> {
    fn submit(&self, c: &VerificationCandidate) -> Result<SubmittedPullRequest, SubmissionFailure> {
        self.enter("submit", c);
        self.inner.submit(c)
    }
    fn verify(&self, c: &VerificationCandidate, pr: &PrLink) -> VerificationOutcome {
        self.enter("verify", c);
        self.inner.verify(c, pr)
    }
    fn land(&self, c: &VerificationCandidate, intent: &LandingIntent) -> LandingOutcome {
        self.enter("land", c);
        self.inner.land(c, intent)
    }
    fn recover_landing(&self, c: &VerificationCandidate, _: &LandingIntent) -> LandingOutcome {
        self.enter("recover", c);
        LandingOutcome::Merged {
            detail: "remote merge confirmed".into(),
        }
    }
    fn notify(&self, c: &VerificationCandidate, message: &str) -> Result<NotifyDelivery, AppError> {
        self.enter("notify", c);
        self.inner.notify(c, message)
    }
    fn redispatch(&self, _: &VerificationCandidate, _: &ResumePlan) -> Result<(), AppError> {
        panic!("human reservation must not dispatch an agent")
    }
    fn reap(&self, c: &VerificationCandidate) -> Result<(), AppError> {
        self.enter("reap", c);
        self.inner.reap(c)
    }
}

#[test]
fn every_owned_phase_releases_a_new_human_reservation() {
    for phase in ["submit", "verify", "notify", "land", "recover", "reap"] {
        for transient in [false, true] {
            let f = ServiceFixture::new();
            f.github_checkout("https://github.com/acme/widgets");
            let (id, _) = leased_submission(&f, f.cwd(), phase, Some(PR_ONE));
            let queue = VerificationQueue::new(f.store());
            if phase == "recover" {
                assert!(matches!(
                    queue
                        .begin_landing(&f.ctx(), &queue.next().unwrap().unwrap(), &certificate())
                        .unwrap(),
                    LandingAdmission::Admitted(_)
                ));
            }
            let actuator = ReservingActuator {
                fixture: &f,
                phase,
                transient,
                inner: FakeActuator::new(if phase == "notify" {
                    VerificationOutcome::Conflict {
                        detail: "external conflict".into(),
                    }
                } else {
                    certified()
                }),
            };
            let activity = VerificationActivity::new();
            let inflight = InFlight::new(f.env().clone());
            let outcome = tick_with_activity(
                f.store(),
                f.env(),
                &actuator,
                &activity,
                &inflight,
                f.project(),
            )
            .unwrap();
            assert_eq!(
                outcome,
                TickResult::Returned,
                "{phase}, transient={transient}"
            );
            assert!(activity.active_all().is_empty());
            assert!(
                f.store()
                    .read(|tx| tx.verification_enabled(f.project()))
                    .unwrap()
            );
            let row = f
                .store()
                .read(|tx| tx.story(f.project(), StoryNo::parse_id("SH", &id).unwrap()))
                .unwrap()
                .unwrap();
            let expected = match phase {
                "notify" => "in-progress",
                "reap" => "done",
                _ => "verifying",
            };
            assert_eq!(row.state, expected, "{phase}");
            assert!(row.awaiting.is_none());
            assert!(
                row.snapshot
                    .comments
                    .iter()
                    .any(|c| c.text.starts_with(VERIFICATION_WITHDRAWN_PREFIX)
                        && c.text.contains("human-only")),
                "{phase}"
            );
            assert!(
                !row.snapshot
                    .comments
                    .iter()
                    .any(|c| c.text.starts_with(VERIFICATION_CLEANUP_COMPLETE_PREFIX)),
                "{phase}"
            );
            if phase == "land" || phase == "recover" {
                assert_eq!(f.store().read(|tx| tx.landing_intents()).unwrap().len(), 1);
                assert!(actuator.inner.reaped.lock().unwrap().is_empty());
            }
        }
    }
}

#[test]
fn human_reservation_releases_reconciliation_without_a_resubmission() {
    let f = ServiceFixture::new();
    f.github_checkout("https://github.com/acme/widgets");
    let id = submitted(&f, "repair", Priority::High, PR_ONE);
    let activity = VerificationActivity::new();
    let inflight = InFlight::new(f.env().clone());
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    let stop = std::sync::atomic::AtomicBool::new(false);
    let actuator = FakeActuator::new(VerificationOutcome::Conflict {
        detail: "conflict".into(),
    });
    let result = tick_with_reconciliation(
        f.store(),
        f.env(),
        &actuator,
        &activity,
        &inflight,
        f.project(),
        |reserved| {
            reserve(&f, &id, true);
            wait_for_reconciled_candidate(f.store(), &subscription, &stop, reserved)
        },
    )
    .unwrap();
    assert_eq!(result, TickResult::Returned);
    assert!(activity.active_all().is_empty());
}

#[test]
fn labeled_cleanup_and_malformed_submissions_never_run_after_restart() {
    let f = ServiceFixture::new();
    f.github_checkout("https://github.com/acme/widgets");
    let id = submitted(&f, "landed", Priority::High, PR_ONE);
    StoryService::new(&f.ctx())
        .comment(&id, &format!("{VERIFICATION_GREEN_PREFIX} confirmed"))
        .unwrap();
    VerificationQueue::new(f.store())
        .record_merged(&f.ctx(), &id, PR_ONE)
        .unwrap();
    reserve(&f, &id, false);
    let malformed = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "no PR".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    StoryService::new(&f.ctx())
        .set_state(&malformed, "verifying", None, None, None)
        .unwrap();
    reserve(&f, &malformed, false);
    let reopened = SqliteStore::open(f.store().path()).unwrap();
    let queue = VerificationQueue::new(&reopened);
    assert!(queue.ordered().unwrap().is_empty());
    assert!(queue.next_cleanup().unwrap().is_none());
    assert!(queue.next_cleanup_for(f.project()).unwrap().is_none());
    let actuator = FakeActuator::new(certified());
    assert_eq!(
        tick_with(&reopened, f.env(), &actuator, f.project()).unwrap(),
        TickResult::Idle
    );
    assert!(actuator.reaped.lock().unwrap().is_empty());
    assert!(actuator.notified.lock().unwrap().is_empty());
}

#[test]
fn ordinary_and_no_auto_labels_remain_verifier_eligible() {
    for labels in [
        vec!["no-auto".into()],
        vec!["ordinary".into()],
        vec!["no-auto".into(), "human-only".into()],
    ] {
        let f = ServiceFixture::new();
        f.github_checkout("https://github.com/acme/widgets");
        let id = submitted(&f, "label control", Priority::High, PR_ONE);
        StoryService::new(&f.ctx())
            .set_labels(&id, &labels, &[])
            .unwrap();
        assert_eq!(
            VerificationQueue::new(f.store()).next().unwrap().is_none(),
            labels.iter().any(|l| l == "human-only")
        );
    }
}
