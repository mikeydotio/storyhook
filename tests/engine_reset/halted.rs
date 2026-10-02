//! SH-790: cancellation never reacquires a halted run's admission slot.
use super::*;
use storyhook::service::engine::{BREAKER_TRIPPED, OPERATOR_STOPPED_NOW};
use storyhook::store::{EngineQuarantineRecord, EngineRunRecord};

fn halted(fixture: &ServiceFixture, fake: &FakeDispatcher) -> EngineRunRecord {
    let id = setup(fixture, fake, "todo");
    fixture
        .store()
        .write(|tx| {
            let mut run = tx.engine_run(&id)?.unwrap();
            run.state = EngineRunState::Halted;
            run.stop_reason = Some(BREAKER_TRIPPED.into());
            run.acknowledged_at = Some(FIXTURE_NOW.into());
            run.consecutive_hard_stops = 3;
            run.recent_quarantines = vec![EngineQuarantineRecord {
                lane_index: 0,
                story_id: Some("SH-1".into()),
                kind: "dispatch-refused".into(),
                detail: Some("original breaker evidence".into()),
                pane_id: None,
                window_name: None,
                worktree_path: None,
                observed_at: FIXTURE_NOW.into(),
            }];
            tx.update_engine_run(&run)?;
            Ok(run)
        })
        .unwrap()
}

fn sibling(fixture: &ServiceFixture, state: EngineRunState) -> EngineRunRecord {
    let ctx = fixture.ctx();
    let fake = FakeDispatcher::default();
    let mut run = EngineService::new(&ctx, &fake)
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 1,
            agent: EngineAgent::Codex,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap();
    run.state = state;
    fixture
        .store()
        .write(|tx| tx.update_engine_run(&run))
        .unwrap();
    run
}

#[test]
fn halted_cleanup_never_changes_the_live_sibling_or_breaker_evidence() {
    for state in [
        EngineRunState::Running,
        EngineRunState::Paused,
        EngineRunState::Draining,
    ] {
        let fixture = ServiceFixture::new();
        let fake = FakeDispatcher::new([DispatcherStep::Reset]);
        let old = halted(&fixture, &fake);
        let live = sibling(&fixture, state);
        let live_lanes = fixture
            .store()
            .read(|tx| tx.engine_lanes(&live.id))
            .unwrap();
        let ctx = fixture.ctx();
        let engine = EngineService::new(&ctx, &fake);
        assert_eq!(engine.resolve_run_id(None).unwrap(), live.id);
        let result = engine.stop(&old.id, true).unwrap();
        assert_eq!(result.run.state, EngineRunState::Finished);
        assert_eq!(
            result.run.stop_reason.as_deref(),
            Some(OPERATOR_STOPPED_NOW)
        );
        assert_eq!(
            result.run.consecutive_hard_stops,
            old.consecutive_hard_stops
        );
        assert_eq!(result.run.recent_quarantines, old.recent_quarantines);
        assert!(result.run.acknowledged_at.is_none());
        assert!(
            result
                .lanes
                .iter()
                .all(|l| l.state == EngineLaneState::Idle)
        );
        fixture
            .store()
            .read(|tx| {
                assert_eq!(tx.engine_run(&live.id)?, Some(live.clone()));
                assert_eq!(tx.engine_lanes(&live.id)?, live_lanes);
                assert_eq!(tx.live_engine_runs()?, vec![live.clone()]);
                assert_eq!(
                    tx.story(fixture.project(), StoryNo::new(1))?.unwrap().state,
                    "todo"
                );
                Ok(())
            })
            .unwrap();
        assert_eq!(engine.stop(&old.id, true).unwrap().run, result.run);
        assert_eq!(fake.calls().len(), 1);
    }
}

#[test]
fn halted_failure_stays_non_live_and_restart_retains_authorized_cleanup() {
    for with_sibling in [false, true] {
        let fixture = ServiceFixture::new();
        let fake = FakeDispatcher::new([
            DispatcherStep::ResetFailure("partial deletion".into()),
            DispatcherStep::ResetFailure("partial deletion".into()),
            DispatcherStep::Reset,
        ]);
        let old = halted(&fixture, &fake);
        let live = with_sibling.then(|| sibling(&fixture, EngineRunState::Running));
        let ctx = fixture.ctx();
        let engine = EngineService::new(&ctx, &fake);
        let error = engine.stop(&old.id, true).unwrap_err().to_string();
        assert!(error.contains("partial deletion"), "{error}");
        let recorded = fixture
            .store()
            .read(|tx| tx.engine_run(&old.id))
            .unwrap()
            .unwrap();
        assert_eq!(recorded.state, EngineRunState::Halted);
        let reset = fixture
            .store()
            .read(|tx| tx.engine_reset(fixture.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap();
        assert_eq!(engine.reset_target(&old.id, &reset.token).unwrap(), reset);
        assert!(engine.reset_target(&old.id, "wrong-token").is_err());
        let later = storyhook::service::Ctx::new(
            fixture.store(),
            fixture.project(),
            fixture.cwd(),
            fixture.env().clone(),
        )
        .clock(storyhook::service::Clock::Fixed(
            "2026-10-02T08:00:00Z".into(),
        ));
        assert!(
            EngineService::new(&later, &fake)
                .stop(&old.id, true)
                .is_err()
        );
        assert_eq!(
            fixture.store().read(|tx| tx.engine_run(&old.id)).unwrap(),
            Some(recorded.clone())
        );

        let reopened = storyhook::store::SqliteStore::open(fixture.env().store_path()).unwrap();
        let resumed = storyhook::service::Ctx::new(
            &reopened,
            fixture.project(),
            fixture.cwd(),
            fixture.env().clone(),
        );
        let engine = EngineService::new(&resumed, &fake);
        assert_eq!(
            engine.reconcile_after_restart(&old.id).unwrap().run_state,
            EngineRunState::Halted
        );
        assert_eq!(fake.calls().len(), 2, "startup must not run helpers");
        assert_eq!(
            engine.reset_target(&old.id, &reset.token).unwrap().token,
            reset.token
        );
        assert_eq!(
            reopened.read(|tx| tx.live_engine_runs()).unwrap(),
            live.into_iter().collect::<Vec<_>>()
        );
        assert_eq!(
            engine.reconcile(&old.id).unwrap().run_state,
            EngineRunState::Finished
        );
        assert!(engine.reset_target(&old.id, &reset.token).is_err());
        assert!(
            reopened
                .read(|tx| tx.engine_reset(fixture.project(), StoryNo::new(1)))
                .unwrap()
                .is_none()
        );
        assert_eq!(fake.calls().len(), 3);
    }
}

#[test]
fn halted_duplicate_records_intent_while_the_controller_is_busy() {
    use fs4::FileExt;
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([DispatcherStep::Reset]);
    let old = halted(&fixture, &fake);
    sibling(&fixture, EngineRunState::Running);
    let key: String = old.id.bytes().map(|b| format!("{b:02x}")).collect();
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(
            fixture
                .env()
                .store_path()
                .with_extension(format!("reset-{key}.lock")),
        )
        .unwrap();
    lock.try_lock_exclusive().unwrap();
    let ctx = fixture.ctx();
    let engine = EngineService::new(&ctx, &fake);
    let first = engine.stop(&old.id, true).unwrap();
    assert_eq!(first.run.state, EngineRunState::Halted);
    assert_eq!(first.run.stop_reason.as_deref(), Some(OPERATOR_STOPPED_NOW));
    assert_eq!(engine.stop(&old.id, true).unwrap().run, first.run);
    assert!(fake.calls().is_empty());
    FileExt::unlock(&lock).unwrap();
    assert_eq!(
        engine.reconcile(&old.id).unwrap().run_state,
        EngineRunState::Finished
    );
    assert_eq!(fake.calls().len(), 1);
}

#[test]
fn halted_leaseless_lane_is_released_with_its_claim_and_diagnostics_preserved() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let old = halted(&fixture, &fake);
    sibling(&fixture, EngineRunState::Running);
    StoryService::new(&fixture.ctx())
        .set_awaiting("SH-1", "original refusal")
        .unwrap();
    fixture
        .store()
        .write(|tx| {
            let mut lane = tx.engine_lanes(&old.id)?.remove(0);
            lane.state = EngineLaneState::Quarantined;
            lane.cleanup_lease = None;
            lane.outcome_detail = Some("original refusal".into());
            tx.put_engine_lane(&lane)
        })
        .unwrap();
    let result = EngineService::new(&fixture.ctx(), &fake)
        .stop(&old.id, true)
        .unwrap();
    assert_eq!(result.run.state, EngineRunState::Finished);
    let story = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(story.state, "in-progress");
    assert!(story.awaiting.unwrap().contains("original refusal"));
    assert!(fake.calls().is_empty());
}

#[test]
fn halted_cleanup_defers_to_a_card_reset_then_finishes_without_resetting_again() {
    use storyhook::service::story_reset::StoryResetService;
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let old = halted(&fixture, &fake);
    sibling(&fixture, EngineRunState::Running);
    let ctx = fixture.ctx();
    let mut card = StoryResetService::new(&ctx)
        .reserve("SH-1", "SH-1")
        .unwrap();
    let engine = EngineService::new(&ctx, &fake);
    let pending = engine.stop(&old.id, true).unwrap();
    assert_eq!(pending.run.state, EngineRunState::Halted);
    assert_eq!(pending.lanes[0].state, EngineLaneState::Working);
    assert!(fake.calls().is_empty());
    card.completed = true;
    fixture
        .store()
        .write(|tx| tx.put_story_reset(&card))
        .unwrap();
    StoryService::new(&ctx)
        .set_state("SH-1", "todo", None, None, None)
        .unwrap();
    assert_eq!(
        engine.reconcile(&old.id).unwrap().run_state,
        EngineRunState::Finished
    );
    assert!(fake.calls().is_empty());
}

#[test]
fn halted_reserved_lane_retries_unclaim_without_destroying_work() {
    for label in [
        storyhook::domain::LABEL_NO_AUTO,
        storyhook::domain::LABEL_HUMAN_ONLY,
    ] {
        let fixture = ServiceFixture::new();
        let fake = FakeDispatcher::new([
            DispatcherStep::UnclaimFailure("tmux unavailable".into()),
            DispatcherStep::Unclaim(DispatchOutcome::from_payload(
                serde_json::json!({"ok":true,"id":"SH-1"}),
            )),
        ]);
        let old = halted(&fixture, &fake);
        sibling(&fixture, EngineRunState::Running);
        let ctx = fixture.ctx();
        StoryService::new(&ctx)
            .set_labels("SH-1", &[label.to_string()], &[])
            .unwrap();
        let engine = EngineService::new(&ctx, &fake);
        assert!(
            engine
                .stop(&old.id, true)
                .unwrap_err()
                .to_string()
                .contains("tmux unavailable")
        );
        assert_eq!(
            fixture
                .store()
                .read(|tx| tx.engine_run(&old.id))
                .unwrap()
                .unwrap()
                .state,
            EngineRunState::Halted
        );
        assert_eq!(
            engine.reconcile(&old.id).unwrap().run_state,
            EngineRunState::Finished
        );
        assert!(
            fake.calls()
                .iter()
                .all(|call| matches!(call, DispatcherCall::Unclaim(_)))
        );
        assert!(
            fixture
                .store()
                .read(|tx| tx.engine_reset(fixture.project(), StoryNo::new(1)))
                .unwrap()
                .is_none()
        );
    }
}
