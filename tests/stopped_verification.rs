//! SH-882: stopped gate policy retains the production submission lifecycle.

use std::sync::Mutex;
use storyhook::daemon::verification::{
    LandingOutcome, NotifyDelivery, ResumePlan, SubmissionFailure, TickResult,
    VerificationActuator, VerificationOutcome, tick_with,
};
use storyhook::domain::landing::SubmissionOutcome;
use storyhook::error::AppError;
use storyhook::service::{
    NewStoryInput, PrLinkService, StoryService, VerificationCandidate, VerificationQueue,
};
use storyhook::store::{LandingIntent, PrLink, ReadOps, Store, WriteOps};
use storyhook_test_support::ServiceFixture;

const PR: &str = "https://github.com/acme/widgets/pull/1";

#[derive(Default)]
struct Landing<'a> {
    preparation: Option<VerificationOutcome>,
    prepare_hook: Option<Box<dyn Fn() + Send + Sync + 'a>>,
    preparations: Mutex<usize>,
    recoveries: Mutex<usize>,
    uncertain: bool,
    publish: bool,
    gated: bool,
    gates: Mutex<usize>,
    submissions: Mutex<usize>,
    landed: Mutex<Vec<LandingIntent>>,
}

impl VerificationActuator for Landing<'_> {
    fn submit(&self, _: &VerificationCandidate) -> Result<SubmissionOutcome, SubmissionFailure> {
        assert!(self.publish, "this linked fixture has no publication lease");
        *self.submissions.lock().unwrap() += 1;
        Ok(storyhook::domain::SubmittedPullRequest {
            url: PR.into(),
            number: 1,
            base: "main".into(),
            head_oid: "a".repeat(40),
            adopted: false,
        }
        .into())
    }
    fn verify(&self, _: &VerificationCandidate, _: &PrLink) -> VerificationOutcome {
        assert!(self.gated, "stopped verification must never execute a gate");
        *self.gates.lock().unwrap() += 1;
        if let Some(hook) = &self.prepare_hook {
            hook();
        }
        VerificationOutcome::Certified {
            head: "a".repeat(40),
            tree: "b".repeat(40),
            gate: "fixture gate".into(),
            detail: "certified exact input".into(),
        }
    }
    fn prepare_without_verification(
        &self,
        _: &VerificationCandidate,
        _: &PrLink,
        _: &storyhook::daemon::verification::VerificationCancellation,
    ) -> VerificationOutcome {
        *self.preparations.lock().unwrap() += 1;
        if let Some(hook) = &self.prepare_hook {
            hook();
        }
        self.preparation
            .clone()
            .unwrap_or(VerificationOutcome::Prepared {
                head: "a".repeat(40),
                tree: "b".repeat(40),
                detail: "prepared exact input".into(),
            })
    }
    fn land(&self, _: &VerificationCandidate, intent: &LandingIntent) -> LandingOutcome {
        self.landed.lock().unwrap().push(intent.clone());
        if self.uncertain {
            LandingOutcome::Uncertain {
                detail: "merge request sent; response lost".into(),
            }
        } else {
            LandingOutcome::Merged {
                detail: "confirmed exact merge".into(),
            }
        }
    }
    fn recover_landing(&self, _: &VerificationCandidate, _: &LandingIntent) -> LandingOutcome {
        *self.recoveries.lock().unwrap() += 1;
        LandingOutcome::Merged {
            detail: "observed earlier exact merge".into(),
        }
    }
    fn notify(&self, _: &VerificationCandidate, message: &str) -> Result<NotifyDelivery, AppError> {
        panic!("a skipped gate must not return work for repair: {message}")
    }
    fn redispatch(&self, _: &VerificationCandidate, _: &ResumePlan) -> Result<(), AppError> {
        panic!("a skipped gate must not redispatch")
    }
    fn reap(&self, _: &VerificationCandidate) -> Result<(), AppError> {
        Ok(())
    }
}

fn submission(f: &ServiceFixture) -> String {
    f.github_checkout("https://github.com/acme/widgets");
    let id = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "stopped-mode submission".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    PrLinkService::new(&f.ctx()).link(&id, PR, true).unwrap();
    StoryService::new(&f.ctx())
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    f.store()
        .write(|tx| tx.put_verification_enabled(f.project(), false))
        .unwrap();
    id
}

#[test]
fn stopped_verification_lands_and_completes_without_calling_the_gate() {
    let f = ServiceFixture::new();
    let id = submission(&f);
    let actuator = Landing::default();
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::Completed
    );
    assert!(
        VerificationQueue::new(f.store())
            .ordered()
            .unwrap()
            .is_empty()
    );
    let intents = actuator.landed.lock().unwrap();
    assert_eq!(intents.len(), 1);
    assert_eq!(intents[0].story_id, id);
    let evidence = serde_json::to_value(&intents[0]).unwrap();
    assert_eq!(evidence["certification"]["mode"], "verification-skipped");
    f.store()
        .read(|tx| {
            assert!(tx.landing_intents()?.is_empty());
            let row = tx.story(f.project(), intents[0].story)?.unwrap();
            assert_eq!(row.state, "done");
            assert!(
                row.snapshot
                    .comments
                    .iter()
                    .any(|c| c.text.starts_with("CENTRAL VERIFICATION SKIPPED —"))
            );
            assert!(
                !row.snapshot
                    .comments
                    .iter()
                    .any(|c| c.text.starts_with("CENTRAL VERIFICATION GREEN —"))
            );
            Ok(())
        })
        .unwrap();
    drop(intents);
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::Idle
    );
    assert_eq!(actuator.landed.lock().unwrap().len(), 1);
}

#[test]
fn uncertain_skipped_landing_survives_restart_and_start_without_repeating_the_merge() {
    let f = ServiceFixture::new();
    submission(&f);
    let actuator = Landing {
        uncertain: true,
        ..Default::default()
    };
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::RetryLater
    );
    let pending = f.store().read(|tx| tx.landing_intents()).unwrap();
    assert_eq!(pending.len(), 1);
    let reopened = storyhook::store::SqliteStore::open(f.store().path()).unwrap();
    assert_eq!(reopened.read(|tx| tx.landing_intents()).unwrap(), pending);
    reopened
        .write(|tx| tx.put_verification_enabled(f.project(), true))
        .unwrap();
    assert_eq!(
        tick_with(&reopened, f.env(), &actuator, f.project()).unwrap(),
        TickResult::Completed
    );
    assert!(reopened.read(|tx| tx.landing_intents()).unwrap().is_empty());
    assert_eq!(*actuator.preparations.lock().unwrap(), 1);
    assert_eq!(*actuator.recoveries.lock().unwrap(), 1);
    let attempts = reopened.read(|tx| tx.gate_attempts(f.project())).unwrap();
    assert!(
        attempts.iter().all(|attempt| attempt.mode
            == storyhook::domain::landing::VerificationMode::VerificationSkipped)
    );
    assert_eq!(actuator.landed.lock().unwrap().len(), 1);
}

#[test]
fn starting_during_stopped_preparation_keeps_the_admitted_mode() {
    use storyhook::daemon::verification::{VerificationActivity, tick_with_activity};
    use storyhook::service::verification_control::VerificationAction;
    let f = ServiceFixture::new();
    submission(&f);
    let activity = VerificationActivity::new();
    let inflight = storyhook::daemon::lifecycle::InFlight::new(f.env().clone());
    let actuator = Landing {
        prepare_hook: Some(Box::new(|| {
            assert_eq!(
                activity.active_for(f.project()).unwrap().mode,
                storyhook::domain::landing::VerificationMode::VerificationSkipped
            );
            activity
                .control(f.store(), f.project(), VerificationAction::Start)
                .unwrap();
        })),
        ..Default::default()
    };
    assert_eq!(
        tick_with_activity(
            f.store(),
            f.env(),
            &actuator,
            &activity,
            &inflight,
            f.project()
        )
        .unwrap(),
        TickResult::Completed
    );
    assert!(
        f.store()
            .read(|tx| tx.verification_enabled(f.project()))
            .unwrap()
    );
    assert!(
        actuator.landed.lock().unwrap()[0]
            .certification
            .certified()
            .is_none()
    );
    let attempts = f.store().read(|tx| tx.gate_attempts(f.project())).unwrap();
    assert_eq!(
        attempts[0].mode,
        storyhook::domain::landing::VerificationMode::VerificationSkipped
    );
    assert_eq!(attempts[0].verdict.as_deref(), Some("verification-skipped"));
    assert!(
        attempts[0].executions.is_empty(),
        "preparation is not a physical gate"
    );
}

#[test]
fn human_reservation_holds_stopped_work_and_release_allows_it_to_finish() {
    let f = ServiceFixture::new();
    let id = submission(&f);
    StoryService::new(&f.ctx())
        .set_labels(&id, &["human-only".into()], &[])
        .unwrap();
    let actuator = Landing::default();
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::Idle
    );
    assert_eq!(*actuator.preparations.lock().unwrap(), 0);
    StoryService::new(&f.ctx())
        .set_labels(&id, &[], &["human-only".into()])
        .unwrap();
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::Completed
    );
}

#[test]
fn human_reservation_during_preparation_prevents_skipped_landing() {
    let f = ServiceFixture::new();
    let id = submission(&f);
    let actuator = Landing {
        prepare_hook: Some(Box::new(|| {
            StoryService::new(&f.ctx())
                .set_labels(&id, &["human-only".into()], &[])
                .unwrap();
        })),
        ..Default::default()
    };
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::Returned
    );
    assert!(actuator.landed.lock().unwrap().is_empty());
    assert!(
        f.store()
            .read(|tx| tx.landing_intents())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn skipped_mode_refuses_a_fake_certificate_before_landing() {
    let f = ServiceFixture::new();
    submission(&f);
    let actuator = Landing {
        preparation: Some(VerificationOutcome::Certified {
            head: "a".repeat(40),
            tree: "b".repeat(40),
            gate: "fake".into(),
            detail: "wrong mode".into(),
        }),
        ..Default::default()
    };
    let error = tick_with(f.store(), f.env(), &actuator, f.project()).unwrap_err();
    assert!(
        error.to_string().contains("claimed certification"),
        "{error}"
    );
    assert!(actuator.landed.lock().unwrap().is_empty());
    assert!(
        f.store()
            .read(|tx| tx.landing_intents())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn multiple_stopped_submissions_complete_once_each() {
    let f = ServiceFixture::new();
    submission(&f);
    submission(&f);
    let actuator = Landing::default();
    for expected in [
        TickResult::Completed,
        TickResult::Completed,
        TickResult::Idle,
    ] {
        assert_eq!(
            tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
            expected
        );
    }
    let intents = actuator.landed.lock().unwrap();
    assert_eq!(intents.len(), 2);
    assert_ne!(intents[0].story_id, intents[1].story_id);
}

#[test]
fn stop_during_preparation_settles_before_a_new_skipped_attempt() {
    use storyhook::daemon::verification::{VerificationActivity, tick_with_activity};
    use storyhook::service::verification_control::VerificationAction;
    let f = ServiceFixture::new();
    submission(&f);
    let activity = VerificationActivity::new();
    let inflight = storyhook::daemon::lifecycle::InFlight::new(f.env().clone());
    let cancelled = Landing {
        prepare_hook: Some(Box::new(|| {
            activity
                .control(f.store(), f.project(), VerificationAction::Stop)
                .unwrap();
            assert!(
                activity
                    .control(f.store(), f.project(), VerificationAction::Start)
                    .is_err()
            );
        })),
        ..Default::default()
    };
    assert_eq!(
        tick_with_activity(
            f.store(),
            f.env(),
            &cancelled,
            &activity,
            &inflight,
            f.project()
        )
        .unwrap(),
        TickResult::Stopped
    );
    assert!(activity.active_for(f.project()).is_none());
    assert!(cancelled.landed.lock().unwrap().is_empty());
    let next = Landing::default();
    assert_eq!(
        tick_with_activity(f.store(), f.env(), &next, &activity, &inflight, f.project()).unwrap(),
        TickResult::Completed
    );
}

#[test]
fn an_infrastructure_halt_still_holds_stopped_submissions() {
    use storyhook::store::{StoryNo, VerificationFailureDisposition, VerificationIncident};
    let f = ServiceFixture::new();
    submission(&f);
    let candidate = VerificationQueue::new(f.store()).next().unwrap().unwrap();
    f.store()
        .write(|tx| {
            tx.put_verification_incident(&VerificationIncident {
                incident_id: "stopped-halt".into(),
                project: f.project(),
                story: StoryNo::new(1),
                generation: candidate.verifying_generation.unwrap(),
                disposition: VerificationFailureDisposition::Permanent,
                halted: true,
                attempts: 1,
                detail: "remote unavailable".into(),
                first_failed_at: f.env().now(),
                last_failed_at: f.env().now(),
            })
        })
        .unwrap();
    let actuator = Landing::default();
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::Halted
    );
    assert_eq!(*actuator.preparations.lock().unwrap(), 0);
}

#[test]
fn dependencies_hold_stopped_work_until_the_blocker_completes() {
    let f = ServiceFixture::new();
    let blocked = submission(&f);
    let blocker = submission(&f);
    storyhook::service::RelationService::new(&f.ctx())
        .block_on(&blocked, std::slice::from_ref(&blocker), None)
        .unwrap();
    let actuator = Landing::default();
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::Completed
    );
    assert_eq!(actuator.landed.lock().unwrap()[0].story_id, blocker);
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::Completed
    );
    assert_eq!(actuator.landed.lock().unwrap()[1].story_id, blocked);
}

#[test]
fn stopped_submission_publishes_and_leaves_one_durable_cleanup_candidate() {
    use storyhook::domain::{CLEANUP_LEASE_VERSION, StoryCleanupLease, TmuxCleanupTarget};
    let f = ServiceFixture::new();
    let root = f.github_checkout("https://github.com/acme/widgets");
    let id = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "publish stopped".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    StoryService::new(&f.ctx())
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    f.append_cleanup_lease(
        &id,
        StoryCleanupLease {
            version: CLEANUP_LEASE_VERSION,
            project_slug: "fixture".into(),
            story_id: id.clone(),
            repository_path: root.clone(),
            worktree_path: root.join("lane"),
            branch: "worktree-SH-1".into(),
            tmux: TmuxCleanupTarget {
                revivify: None,
                socket_path: f.cwd().join("tmux.sock"),
            },
        },
    );
    f.store()
        .write(|tx| tx.put_verification_enabled(f.project(), false))
        .unwrap();
    let actuator = Landing {
        publish: true,
        ..Default::default()
    };
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::Completed
    );
    let queue = VerificationQueue::new(f.store());
    let cleanup = queue.next_cleanup_for(f.project()).unwrap().unwrap();
    assert_eq!(cleanup.story_id, id);
    assert!(cleanup.cleanup_lease.is_some());
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::Idle
    );
    assert_eq!(*actuator.submissions.lock().unwrap(), 1);
    assert_eq!(actuator.landed.lock().unwrap().len(), 1);
}

#[test]
fn running_and_draining_attempts_keep_certification_and_only_later_work_skips() {
    use storyhook::daemon::verification::{VerificationActivity, tick_with_activity};
    use storyhook::service::verification_control::VerificationAction;
    for drain in [false, true] {
        let f = ServiceFixture::new();
        submission(&f);
        f.store()
            .write(|tx| tx.put_verification_enabled(f.project(), true))
            .unwrap();
        let activity = VerificationActivity::new();
        let inflight = storyhook::daemon::lifecycle::InFlight::new(f.env().clone());
        let actuator = Landing {
            gated: true,
            prepare_hook: Some(Box::new(|| {
                if drain {
                    activity
                        .control(f.store(), f.project(), VerificationAction::Drain)
                        .unwrap();
                }
            })),
            ..Default::default()
        };
        assert_eq!(
            tick_with_activity(
                f.store(),
                f.env(),
                &actuator,
                &activity,
                &inflight,
                f.project()
            )
            .unwrap(),
            TickResult::Completed
        );
        assert_eq!(*actuator.gates.lock().unwrap(), 1);
        assert_eq!(*actuator.preparations.lock().unwrap(), 0);
        assert!(
            actuator.landed.lock().unwrap()[0]
                .certification
                .certified()
                .is_some()
        );
        if drain {
            assert!(
                !f.store()
                    .read(|tx| tx.verification_enabled(f.project()))
                    .unwrap()
            );
            submission(&f);
            let skipped = Landing::default();
            assert_eq!(
                tick_with_activity(
                    f.store(),
                    f.env(),
                    &skipped,
                    &activity,
                    &inflight,
                    f.project()
                )
                .unwrap(),
                TickResult::Completed
            );
            assert!(
                skipped.landed.lock().unwrap()[0]
                    .certification
                    .certified()
                    .is_none()
            );
        }
    }
}

#[test]
fn withdrawal_during_stopped_preparation_invalidates_landing() {
    let f = ServiceFixture::new();
    let id = submission(&f);
    let actuator = Landing {
        prepare_hook: Some(Box::new(|| {
            StoryService::new(&f.ctx())
                .set_state(&id, "in-progress", None, None, None)
                .unwrap();
        })),
        ..Default::default()
    };
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::Returned
    );
    assert!(actuator.landed.lock().unwrap().is_empty());
    assert!(
        f.store()
            .read(|tx| tx.landing_intents())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn reset_fences_stopped_submissions() {
    let f = ServiceFixture::new();
    let id = submission(&f);
    storyhook::service::story_reset::StoryResetService::new(&f.ctx())
        .reserve(&id, &id)
        .unwrap();
    let actuator = Landing::default();
    assert_eq!(
        tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
        TickResult::Idle
    );
    assert_eq!(*actuator.preparations.lock().unwrap(), 0);
    assert!(actuator.landed.lock().unwrap().is_empty());
}
