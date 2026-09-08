//! SH-609: interactions across the engine's real queue, store, and lane lifecycle.

mod store_support;

use storyhook::service::engine::{DispatchOutcome, EngineService, StartRequest};
use storyhook::service::{Ctx, NewStoryInput, RelationService, StoryService};
use storyhook::store::{EngineAgent, EngineScope, SqliteStore};
use storyhook_test_support::{DispatcherStep, FakeDispatcher, ServiceFixture};

fn story(ctx: &Ctx<'_, SqliteStore>, title: &str) -> String {
    StoryService::new(ctx)
        .create(&NewStoryInput {
            title: title.into(),
            ..Default::default()
        })
        .unwrap()
        .id
}

fn start(
    ctx: &Ctx<'_, SqliteStore>,
    dispatcher: &FakeDispatcher,
    scope: EngineScope,
    lanes: u32,
) -> String {
    EngineService::new(ctx, dispatcher)
        .start(StartRequest {
            scope,
            lanes,
            agent: EngineAgent::Codex,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap()
        .id
}

fn dispatched() -> DispatcherStep {
    DispatcherStep::Dispatch(DispatchOutcome::from_payload(
        serde_json::json!({"ok": true}),
    ))
}

#[test]
fn ordinary_parent_is_dispatched_before_its_blocked_child() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let parent = story(&ctx, "executable parent");
    let child = story(&ctx, "dependent child");
    let relations = RelationService::new(&ctx);
    relations
        .relate(&parent, "parent-of", &child, false)
        .unwrap();
    relations.relate(&parent, "blocks", &child, false).unwrap();
    let fake = FakeDispatcher::new([dispatched(), dispatched()]);
    let run = start(&ctx, &fake, EngineScope::Project, 2);
    let engine = EngineService::new(&ctx, &fake);
    assert_eq!(
        engine.reconcile(&run).unwrap().filled,
        [(0, parent.clone())]
    );
    StoryService::new(&ctx)
        .set_state(&parent, "done", None, None, None)
        .unwrap();
    assert_eq!(engine.reconcile(&run).unwrap().filled, [(0, child)]);
}

fn other_context<'a>(
    fixture: &'a ServiceFixture,
    slug: &str,
    prefix: &str,
) -> Ctx<'a, SqliteStore> {
    let project = store_support::seed_project(fixture.store(), slug, prefix);
    Ctx::new(
        fixture.store(),
        project,
        fixture.cwd(),
        fixture.env().clone(),
    )
    .clock(storyhook::service::Clock::Fixed(
        storyhook_test_support::FIXTURE_NOW.into(),
    ))
}

#[test]
fn capacity_is_shared_and_waiters_resume_after_a_slot_is_released() {
    use storyhook::service::engine::ENGINE_LANE_BUDGET;
    use storyhook::store::{EngineLaneState, EngineRunState, ReadOps, Store, WriteOps};
    for state in [
        EngineRunState::Running,
        EngineRunState::Paused,
        EngineRunState::Draining,
        EngineRunState::Halted,
    ] {
        let fixture = ServiceFixture::new();
        let ctx = fixture.ctx();
        for _ in 0..ENGINE_LANE_BUDGET {
            story(&ctx, "occupant");
        }
        let holder = FakeDispatcher::new((0..ENGINE_LANE_BUDGET).map(|_| dispatched()));
        let run = start(
            &ctx,
            &holder,
            EngineScope::Project,
            ENGINE_LANE_BUDGET as u32,
        );
        EngineService::new(&ctx, &holder).reconcile(&run).unwrap();
        fixture
            .store()
            .write(|tx| {
                let mut record = tx.engine_run(&run)?.unwrap();
                record.state = state;
                tx.update_engine_run(&record)
            })
            .unwrap();
        let other = other_context(&fixture, "other", "OTHER");
        let waiting = story(&other, "waiting for capacity");
        let fake = FakeDispatcher::new([dispatched()]);
        let waiter = start(&other, &fake, EngineScope::Project, 1);
        let engine = EngineService::new(&other, &fake);
        for _ in 0..2 {
            let report = engine.reconcile(&waiter).unwrap();
            assert!(
                report.filled.is_empty(),
                "overfilled while holder was {state:?}"
            );
            assert_eq!(report.run_state, EngineRunState::Running);
            assert!(report.stop_reason.is_none());
        }
        fixture
            .store()
            .write(|tx| {
                let mut lane = tx.engine_lanes(&run)?.remove(0);
                // Quarantine retains evidence but releases dispatch capacity.
                lane.state = EngineLaneState::Quarantined;
                tx.put_engine_lane(&lane)
            })
            .unwrap();
        assert_eq!(engine.reconcile(&waiter).unwrap().filled, [(0, waiting)]);
    }
}

#[test]
fn concurrent_projects_cannot_both_reserve_the_last_slot() {
    use storyhook::service::engine::ENGINE_LANE_BUDGET;
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    for _ in 1..ENGINE_LANE_BUDGET {
        story(&ctx, "occupant");
    }
    let holder = FakeDispatcher::new((1..ENGINE_LANE_BUDGET).map(|_| dispatched()));
    let run = start(
        &ctx,
        &holder,
        EngineScope::Project,
        ENGINE_LANE_BUDGET as u32,
    );
    EngineService::new(&ctx, &holder).reconcile(&run).unwrap();
    let a = other_context(&fixture, "a", "A");
    let b = other_context(&fixture, "b", "B");
    story(&a, "a");
    story(&b, "b");
    let fake = FakeDispatcher::new([dispatched(), dispatched()]);
    let a_run = start(&a, &fake, EngineScope::Project, 1);
    let b_run = start(&b, &fake, EngineScope::Project, 1);
    let barrier = std::sync::Barrier::new(2);
    let filled = std::thread::scope(|scope| {
        let one = scope.spawn(|| {
            barrier.wait();
            EngineService::new(&a, &fake)
                .reconcile(&a_run)
                .unwrap()
                .filled
                .len()
        });
        let two = scope.spawn(|| {
            barrier.wait();
            EngineService::new(&b, &fake)
                .reconcile(&b_run)
                .unwrap()
                .filled
                .len()
        });
        one.join().unwrap() + two.join().unwrap()
    });
    assert_eq!(
        filled, 1,
        "the final capacity check and reservation must share one write"
    );
}
