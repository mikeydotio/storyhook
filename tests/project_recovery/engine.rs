//! Full Auto reconciliation must retain the durable recovery owner's lane.
use super::*;
use storyhook::service::{
    engine::{EngineService, StartRequest},
    project_recovery::RepairScope,
};
use storyhook::store::{EngineAgent, EngineLaneState, EngineScope};
use storyhook_test_support::{DispatcherStep, FakeDispatcher};

#[test]
fn sh870_quarantined_lane_cannot_assign_unproved_repair() {
    for claimed in [false, true] {
        let f = fixture();
        let candidate = submitted(&f, "quarantined assessment");
        let ctx = f.ctx();
        let recovery = ProjectRecoveryService::new(&ctx);
        let view = recovery
            .observe(&candidate, &fault(), "quarantine")
            .unwrap()
            .unwrap();
        if claimed {
            assert!(
                recovery
                    .claim_assessment(&view.record.id)
                    .unwrap()
                    .is_none()
            );
        }
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
        lane.story_id = Some("SH-1".into());
        lane.state = EngineLaneState::Quarantined;
        f.store().write(|tx| tx.put_engine_lane(&lane)).unwrap();
        if claimed {
            assert!(
                !recovery
                    .delivery_permitted(&view.record.id, None, 1, false)
                    .unwrap()
            );
        } else {
            assert!(
                recovery
                    .claim_assessment(&view.record.id)
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                recovery
                    .show(&view.record.id)
                    .unwrap()
                    .state
                    .assessment
                    .hold,
                Some(AssessmentHold::CauseUnproved)
            );
        }
    }
}

#[test]
fn sh870_restart_preserves_accepted_recovery_work_and_dependencies() {
    for phase in ["dependency", "work"] {
        let f = fixture();
        let candidate = submitted(&f, "owned recovery lane");
        let ctx = f.ctx();
        let recovery = ProjectRecoveryService::new(&ctx);
        let view = recovery
            .observe(&candidate, &fault(), "scope-attempt")
            .unwrap()
            .unwrap();
        legacy::retain(
            &f,
            view.clone(),
            if phase == "dependency" {
                RepairScope::SeparateStory
            } else {
                RepairScope::SameStory
            },
        );
        let endpoint = FakeDispatcher::new([DispatcherStep::WindowAlive {
            window: "=fixture:=SH-1".into(),
            alive: false,
        }]);
        let engine = EngineService::new(&ctx, &endpoint);
        let run = engine
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
        lane.state = EngineLaneState::Working;
        lane.story_id = Some("SH-1".into());
        lane.window_name = Some("SH-1".into());
        f.store().write(|tx| tx.put_engine_lane(&lane)).unwrap();
        let result = engine.reconcile_after_restart(&run.id).unwrap();
        assert!(
            result.quarantined.is_empty(),
            "{phase}: {:?}",
            result.quarantined
        );
        assert_eq!(
            f.store().read(|tx| tx.engine_lanes(&run.id)).unwrap()[0].state,
            EngineLaneState::Working
        );
    }
}

#[test]
fn sh870_unproved_fault_is_not_an_engine_repair_exemption() {
    for change in ["awaiting", "state", "label", "terminal", "stop", "external"] {
        let f = fixture();
        let candidate = submitted(&f, "revoked recovery lane");
        let ctx = f.ctx();
        let recovery = ProjectRecoveryService::new(&ctx);
        let view = recovery
            .observe(&candidate, &fault(), "scope-attempt")
            .unwrap()
            .unwrap();
        match change {
            "awaiting" => {
                StoryService::new(&ctx)
                    .set_awaiting("SH-1", "operator hold")
                    .unwrap();
            }
            "state" => {
                StoryService::new(&ctx)
                    .set_state("SH-1", "todo", None, None, None)
                    .unwrap();
            }
            "label" => {
                StoryService::new(&ctx)
                    .set_labels("SH-1", &["no-auto".into()], &[])
                    .unwrap();
            }
            "stop" => {
                f.store()
                    .write(|tx| tx.put_verification_enabled(f.project(), false))
                    .unwrap();
            }
            // An external prerequisite waits on a person for an unbounded
            // time; its owned hold must not keep a Full Auto lane (SH-849).
            "external" => {
                legacy::retain(&f, view.clone(), RepairScope::External);
            }
            _ => {
                assert!(
                    recovery
                        .claim_assessment(&view.record.id)
                        .unwrap()
                        .is_none()
                );
            }
        }
        let endpoint = FakeDispatcher::new([DispatcherStep::WindowAlive {
            window: "=fixture:=SH-1".into(),
            alive: false,
        }]);
        let engine = EngineService::new(&ctx, &endpoint);
        let run = engine
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
        lane.state = EngineLaneState::Working;
        lane.story_id = Some("SH-1".into());
        lane.window_name = Some("SH-1".into());
        f.store().write(|tx| tx.put_engine_lane(&lane)).unwrap();
        let report = engine.reconcile_after_restart(&run.id).unwrap();
        // A reserved label ends the lane's claim outright (SH-837): the lane
        // is released to the operator rather than quarantined, and either
        // way recovery ownership exempted nothing.
        let (quarantined, reserved) = if change == "label" {
            (0, 1)
        } else if matches!(change, "terminal" | "stop") {
            (0, 0)
        } else {
            (1, 0)
        };
        assert_eq!(report.quarantined.len(), quarantined, "{change}");
        assert_eq!(report.reserved.len(), reserved, "{change}");
    }
}
