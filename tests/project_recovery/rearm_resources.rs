//! Resource owners must fence both reconciliation and later delivery authority.
use super::*;
use storyhook::service::engine::{EngineService, StartRequest};
use storyhook::store::{EngineAgent, EngineLaneState, EngineScope};
use storyhook_test_support::FakeDispatcher;

#[test]
fn absent_agent_without_managed_lease_remains_uncertain_after_rearm() {
    let f = fixture();
    let initial = rearm::reserved(&f, &["no-auto"], false);
    rearm::release(&f);
    let actuator = worker::helper(
        &f,
        r#"{"ok":false,"reason":"pane-unavailable","display":"agent absent"}"#,
    );
    let activity = storyhook::daemon::verification::VerificationActivity::new();
    let stop = std::sync::atomic::AtomicBool::new(false);
    assert!(
        storyhook::daemon::project_recovery::process_one(
            f.store(),
            f.env(),
            &actuator,
            &activity,
            &stop
        )
        .unwrap()
    );
    let held = ProjectRecoveryService::new(&f.ctx())
        .show(&initial.record.id)
        .unwrap();
    assert_eq!(
        held.state.assessment.hold,
        Some(AssessmentHold::OwnershipUncertain)
    );
    assert_eq!(held.observations, initial.observations);
    assert!(held.state.assessment.detail.contains("managed lease"));
    assert!(
        !storyhook::daemon::project_recovery::process_one(
            f.store(),
            f.env(),
            &actuator,
            &activity,
            &stop
        )
        .unwrap()
    );
}

fn quarantine(f: &ServiceFixture) {
    let ctx = f.ctx();
    let endpoint = FakeDispatcher::new([]);
    let run = EngineService::new(&ctx, &endpoint)
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 1,
            agent: EngineAgent::Codex,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap();
    let mut lane = f
        .store()
        .read(|tx| tx.engine_lanes(&run.id))
        .unwrap()
        .remove(0);
    lane.state = EngineLaneState::Quarantined;
    lane.story_id = Some("SH-1".into());
    lane.window_name = Some("SH-1".into());
    f.store().write(|tx| tx.put_engine_lane(&lane)).unwrap();
}

#[test]
fn reset_landing_and_quarantine_prevent_policy_rearm() {
    for owner in ["reset", "landing", "quarantine"] {
        let f = fixture();
        let initial = rearm::reserved(&f, &["no-auto"], false);
        rearm::release(&f);
        let ctx = f.ctx();
        match owner {
            "reset" => {
                storyhook::service::story_reset::StoryResetService::new(&ctx)
                    .reserve("SH-1", "SH-1")
                    .unwrap();
            }
            "landing" => {
                let candidate = &initial.state.subjects[0].candidate;
                f.store()
                    .write(|tx| {
                        tx.insert_landing_intent(&storyhook::store::LandingIntent {
                            id: "unsettled-landing".into(),
                            project: f.project(),
                            story: StoryNo::new(1),
                            story_id: "SH-1".into(),
                            project_slug: candidate.project_slug.clone(),
                            generation: candidate.verifying_generation.unwrap(),
                            pull_request: "https://github.com/acme/widgets/pull/1".into(),
                            checkout: candidate.checkout.clone(),
                            certification: storyhook::service::landing::VerifiedSubmission {
                                head: "a".repeat(40),
                                tree: "b".repeat(40),
                                gate: "fixture gate".into(),
                            }
                            .into(),
                            created_at: ctx.now(),
                            batch: None,
                        })
                    })
                    .unwrap();
            }
            "quarantine" => quarantine(&f),
            _ => unreachable!(),
        }
        let service = ProjectRecoveryService::new(&ctx);
        assert!(
            !service
                .policy_rearm_ready(&initial.record.id, None)
                .unwrap(),
            "{owner}"
        );
        assert!(
            !service.rearm_policy_hold(&initial.record.id, None).unwrap(),
            "{owner}"
        );
        assert_eq!(service.show(&initial.record.id).unwrap(), initial);
    }
}

#[test]
fn quarantine_after_policy_release_revokes_claim_and_delivery() {
    for claimed in [false, true] {
        let f = fixture();
        let initial = rearm::reserved(&f, &["no-auto"], false);
        rearm::release(&f);
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        assert!(service.rearm_policy_hold(&initial.record.id, None).unwrap());
        if claimed {
            service
                .claim_assessment(&initial.record.id)
                .unwrap()
                .unwrap();
        }
        quarantine(&f);
        if claimed {
            assert!(
                !service
                    .delivery_permitted(&initial.record.id, None, 1, false)
                    .unwrap()
            );
        } else {
            assert!(
                service
                    .claim_assessment(&initial.record.id)
                    .unwrap()
                    .is_none()
            );
        }
    }
}

#[test]
fn concurrent_reconciliation_and_claim_only_acquire_once() {
    let f = fixture();
    let initial = rearm::reserved(&f, &["no-auto"], false);
    rearm::release(&f);
    let first = storyhook::store::SqliteStore::open(f.store().path()).unwrap();
    let second = storyhook::store::SqliteStore::open(f.store().path()).unwrap();
    let one =
        storyhook::service::Ctx::new(&first, f.project(), f.cwd().to_path_buf(), f.env().clone());
    let two =
        storyhook::service::Ctx::new(&second, f.project(), f.cwd().to_path_buf(), f.env().clone());
    let services = [
        ProjectRecoveryService::new(&one),
        ProjectRecoveryService::new(&two),
    ];
    let results = std::thread::scope(|scope| {
        let jobs = services
            .iter()
            .map(|service| {
                scope.spawn(|| service.rearm_policy_hold(&initial.record.id, None).unwrap())
            })
            .collect::<Vec<_>>();
        jobs.into_iter()
            .map(|job| job.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|result| **result).count(), 1);
    let claims = std::thread::scope(|scope| {
        let jobs = services
            .iter()
            .map(|service| {
                scope.spawn(|| {
                    service
                        .claim_assessment(&initial.record.id)
                        .unwrap()
                        .is_some()
                })
            })
            .collect::<Vec<_>>();
        jobs.into_iter()
            .map(|job| job.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(claims.iter().filter(|result| **result).count(), 1);
    let claimed = services[0].show(&initial.record.id).unwrap();
    assert_eq!(claimed.state.assessment.epoch, 1);
    assert_eq!(claimed.observations, initial.observations);
}
