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

fn dispatched_with_pane() -> DispatcherStep {
    DispatcherStep::Dispatch(DispatchOutcome::from_payload(serde_json::json!({
        "ok": true, "pane": "%42", "window_name": "story-window", "worktree_path": "/tmp/preserved"
    })))
}

#[test]
fn deleted_lane_story_is_quarantined_once_even_when_its_pane_survives() {
    use storyhook::store::{EngineRunState, ReadOps, Store};
    for alive in [false, true] {
        for restart in [false, true] {
            let fixture = ServiceFixture::new();
            let ctx = fixture.ctx();
            let deleted = story(&ctx, "deleted during work");
            let fake = FakeDispatcher::new([
                dispatched_with_pane(),
                DispatcherStep::WindowAlive {
                    window: "%42".into(),
                    alive,
                },
                dispatched(),
            ]);
            let run = start(&ctx, &fake, EngineScope::Project, 1);
            let engine = EngineService::new(&ctx, &fake);
            engine.reconcile(&run).unwrap();
            StoryService::new(&ctx).delete(&deleted).unwrap();
            let next = story(&ctx, "next independent work");
            let report = if restart {
                engine.reconcile_after_restart(&run)
            } else {
                engine.reconcile(&run)
            }
            .unwrap();
            assert_eq!(report.quarantined.len(), 1);
            assert_eq!(report.quarantined[0].1.as_str(), "story-missing");
            let record = fixture
                .store()
                .read(|tx| tx.engine_run(&run))
                .unwrap()
                .unwrap();
            assert_eq!(record.consecutive_hard_stops, 1);
            assert_eq!(
                record.recent_quarantines[0].story_id.as_deref(),
                Some(deleted.as_str())
            );
            assert_eq!(
                record.recent_quarantines[0].worktree_path.as_deref(),
                Some("/tmp/preserved")
            );
            assert_eq!(record.state, EngineRunState::Running);
            if restart {
                assert!(report.filled.is_empty());
                assert_eq!(engine.reconcile(&run).unwrap().filled, [(0, next)]);
            } else {
                assert_eq!(report.filled, [(0, next)]);
            }
            assert_eq!(
                fixture
                    .store()
                    .read(|tx| tx.engine_run(&run))
                    .unwrap()
                    .unwrap()
                    .consecutive_hard_stops,
                1
            );
        }
    }
}

struct ChangeDuringProbe<'a> {
    ctx: &'a Ctx<'a, SqliteStore>,
    story: &'a str,
    state: &'a str,
    changed: std::sync::atomic::AtomicBool,
}

impl storyhook::service::engine::Dispatcher for ChangeDuringProbe<'_> {
    fn dispatch(
        &self,
        _: storyhook::service::engine::DispatchRequest,
    ) -> Result<DispatchOutcome, storyhook::error::AppError> {
        panic!("the occupied lane must not be dispatched again")
    }
    fn unclaim(
        &self,
        _: storyhook::service::engine::UnclaimRequest,
    ) -> Result<DispatchOutcome, storyhook::error::AppError> {
        panic!("must preserve claim")
    }
    fn kill_window(&self, _: &str) -> Result<(), storyhook::error::AppError> {
        panic!("must preserve pane")
    }
    fn window_alive(&self, _: &str) -> bool {
        if !self.changed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            StoryService::new(self.ctx)
                .set_state(self.story, self.state, None, None, None)
                .unwrap();
        }
        false
    }
}

#[test]
fn a_completion_or_verification_during_the_probe_cannot_be_quarantined() {
    use storyhook::service::QueryService;
    use storyhook::store::{ReadOps, Store};
    for state in ["done", "verifying"] {
        let fixture = ServiceFixture::new();
        let ctx = fixture.ctx();
        let id = story(&ctx, "handoff during probe");
        let fake = FakeDispatcher::new([dispatched_with_pane()]);
        let run = start(&ctx, &fake, EngineScope::Project, 1);
        EngineService::new(&ctx, &fake).reconcile(&run).unwrap();
        let raced = ChangeDuringProbe {
            ctx: &ctx,
            story: &id,
            state,
            changed: false.into(),
        };
        let engine = EngineService::new(&ctx, &raced);
        assert!(engine.reconcile(&run).unwrap().quarantined.is_empty());
        let row = fixture
            .store()
            .read(|tx| {
                Ok(
                    QueryService::new(tx, fixture.project(), storyhook_test_support::FIXTURE_NOW)
                        .show(&id)?,
                )
            })
            .unwrap();
        assert!(row.story.awaiting.is_none());
        assert_eq!(row.story.state, state);
        assert_eq!(
            fixture
                .store()
                .read(|tx| tx.engine_run(&run))
                .unwrap()
                .unwrap()
                .consecutive_hard_stops,
            0
        );
        let next = engine.reconcile(&run).unwrap();
        if state == "done" {
            assert_eq!(next.completed, [0]);
        } else {
            assert_eq!(next.verifying, [0]);
        }
    }
}

#[test]
fn an_unreadable_story_is_not_misreported_as_a_deleted_story() {
    use storyhook::store::{ReadOps, Store};
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    story(&ctx, "corrupt read");
    let fake = FakeDispatcher::new([dispatched()]);
    let run = start(&ctx, &fake, EngineScope::Project, 1);
    let engine = EngineService::new(&ctx, &fake);
    engine.reconcile(&run).unwrap();
    let conn = rusqlite::Connection::open(fixture.env().store_path()).unwrap();
    let original: String = conn
        .query_row("SELECT snapshot FROM stories", [], |row| row.get(0))
        .unwrap();
    conn.execute("UPDATE stories SET snapshot = 'invalid json'", [])
        .unwrap();
    let result = engine.reconcile(&run);
    conn.execute("UPDATE stories SET snapshot = ?1", [&original])
        .unwrap();
    assert!(result.is_err(), "a failed read must be reported");
    let record = fixture
        .store()
        .read(|tx| tx.engine_run(&run))
        .unwrap()
        .unwrap();
    assert!(record.recent_quarantines.is_empty());
    assert_eq!(record.consecutive_hard_stops, 0);
}
