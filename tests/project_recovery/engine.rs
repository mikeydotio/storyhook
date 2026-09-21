//! Full Auto reconciliation must retain the durable recovery owner's lane.
use super::*;
use storyhook::service::{
    engine::{EngineService, StartRequest},
    project_recovery::RepairScope,
};
use storyhook::store::{EngineAgent, EngineLaneState, EngineScope};
use storyhook_test_support::{DispatcherStep, FakeDispatcher};

#[test]
fn restart_does_not_quarantine_pending_recovery_or_owned_repair_dependency() {
    for phase in ["pending", "in-flight", "delivered", "dependency", "work"] {
        let f = fixture();
        let candidate = submitted(&f, "owned recovery lane");
        let ctx = f.ctx();
        let recovery = ProjectRecoveryService::new(&ctx);
        let mut view = recovery
            .observe(&candidate, &fault(), "scope-attempt")
            .unwrap()
            .unwrap();
        if phase != "pending" {
            view = recovery.claim_assessment(&view.record.id).unwrap().unwrap();
            if phase != "in-flight" {
                view = recovery
                    .settle_assessment(
                        &view.record.id,
                        &view.state.assessment.dispatch_identity,
                        view.state.assessment.epoch,
                        AssessmentDelivery::Delivered,
                    )
                    .unwrap();
            }
        }
        if matches!(phase, "dependency" | "work") {
            let scope = if phase == "dependency" {
                RepairScope::SeparateStory
            } else {
                RepairScope::SameStory
            };
            recovery
                .decide(&view.record.id, &decision::input(&view, scope))
                .unwrap();
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
fn operator_replaced_hold_and_terminal_recovery_are_not_engine_exemptions() {
    for change in ["awaiting", "state", "label", "terminal", "stop"] {
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
            _ => {
                let claimed = recovery.claim_assessment(&view.record.id).unwrap().unwrap();
                recovery
                    .settle_assessment(
                        &view.record.id,
                        &claimed.state.assessment.dispatch_identity,
                        1,
                        AssessmentDelivery::Uncertain("provider ownership unknown".into()),
                    )
                    .unwrap();
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
        assert_eq!(
            engine
                .reconcile_after_restart(&run.id)
                .unwrap()
                .quarantined
                .len(),
            1,
            "{change}"
        );
    }
}
