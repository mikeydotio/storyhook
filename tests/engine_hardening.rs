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
fn another_runs_occupied_lanes_do_not_reduce_this_runs_capacity() {
    use storyhook::store::{EngineRunState, ReadOps, Store, WriteOps};
    for state in [
        EngineRunState::Running,
        EngineRunState::Paused,
        EngineRunState::Draining,
        EngineRunState::Halted,
    ] {
        let fixture = ServiceFixture::new();
        let ctx = fixture.ctx();
        for _ in 0..6 {
            story(&ctx, "occupant");
        }
        let holder = FakeDispatcher::new((0..6).map(|_| dispatched()));
        let run = start(&ctx, &holder, EngineScope::Project, 6);
        assert_eq!(
            EngineService::new(&ctx, &holder)
                .reconcile(&run)
                .unwrap()
                .filled
                .len(),
            6
        );
        fixture
            .store()
            .write(|tx| {
                let mut record = tx.engine_run(&run)?.unwrap();
                record.state = state;
                tx.update_engine_run(&record)
            })
            .unwrap();

        let other = other_context(&fixture, "other", "OTHER");
        for _ in 0..3 {
            story(&other, "independent work");
        }
        let fake = FakeDispatcher::new((0..2).map(|_| dispatched()));
        let independent = start(&other, &fake, EngineScope::Project, 2);
        let report = EngineService::new(&other, &fake)
            .reconcile(&independent)
            .unwrap();
        assert_eq!(report.filled.len(), 2, "another run is {state:?}");
    }
}

#[test]
fn concurrent_projects_each_reserve_their_own_configured_capacity() {
    let fixture = ServiceFixture::new();
    let a = other_context(&fixture, "a", "A");
    let b = other_context(&fixture, "b", "B");
    for _ in 0..7 {
        story(&a, "a");
        story(&b, "b");
    }
    let fake = FakeDispatcher::new((0..11).map(|_| dispatched()));
    let a_run = start(&a, &fake, EngineScope::Project, 6);
    let b_run = start(&b, &fake, EngineScope::Project, 5);
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
        (one.join().unwrap(), two.join().unwrap())
    });
    assert_eq!(
        filled,
        (6, 5),
        "each run must honor its own configured capacity"
    );
}

/// Both passes observe the idle lane before either can reserve it.
struct CensusBarrierDispatcher {
    inner: FakeDispatcher,
    barrier: std::sync::Barrier,
}

impl storyhook::service::engine::Dispatcher for CensusBarrierDispatcher {
    fn dispatch(
        &self,
        request: storyhook::service::engine::DispatchRequest,
    ) -> Result<DispatchOutcome, storyhook::error::AppError> {
        self.inner.dispatch(request)
    }

    fn unclaim(
        &self,
        request: storyhook::service::engine::UnclaimRequest,
    ) -> Result<DispatchOutcome, storyhook::error::AppError> {
        self.inner.unclaim(request)
    }

    fn probe_window(&self, window: &str) -> storyhook::service::engine::WindowProbe {
        self.inner.probe_window(window)
    }

    fn kill_window(&self, window: &str) -> Result<(), storyhook::error::AppError> {
        self.inner.kill_window(window)
    }

    fn census(&self) -> storyhook::lane_budget::WindowCensus {
        self.barrier.wait();
        self.inner.census()
    }
}

#[test]
fn concurrent_passes_cannot_reserve_the_same_runs_last_lane_twice() {
    use storyhook::store::{EngineLaneState, ReadOps, Store};
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    story(&ctx, "first candidate");
    story(&ctx, "second candidate");
    let fake = FakeDispatcher::new([dispatched()]);
    let run = start(&ctx, &fake, EngineScope::Project, 1);
    let dispatcher = CensusBarrierDispatcher {
        inner: fake.clone(),
        barrier: std::sync::Barrier::new(2),
    };
    let filled = std::thread::scope(|scope| {
        let one = scope.spawn(|| {
            EngineService::new(&ctx, &dispatcher)
                .reconcile(&run)
                .unwrap()
                .filled
                .len()
        });
        let two = scope.spawn(|| {
            EngineService::new(&ctx, &dispatcher)
                .reconcile(&run)
                .unwrap()
                .filled
                .len()
        });
        one.join().unwrap() + two.join().unwrap()
    });
    assert_eq!(filled, 1);
    assert_eq!(fake.calls().len(), 1, "the losing pass must not dispatch");
    let lanes = fixture.store().read(|tx| tx.engine_lanes(&run)).unwrap();
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0].state, EngineLaneState::Working);
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
    fn census(&self) -> storyhook::lane_budget::WindowCensus {
        storyhook::lane_budget::WindowCensus::Counted {
            windows: Vec::new(),
        }
    }
    fn probe_window(&self, window: &str) -> storyhook::service::engine::WindowProbe {
        if !self.changed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            StoryService::new(self.ctx)
                .set_state(self.story, self.state, None, None, None)
                .unwrap();
        }
        storyhook::service::engine::WindowProbe::Gone {
            detail: format!("scripted: `{window}` closed under the observation"),
        }
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

fn epic(ctx: &Ctx<'_, SqliteStore>) -> String {
    storyhook::service::ConfigService::new(ctx)
        .add_type("epic", None, None)
        .unwrap();
    StoryService::new(ctx)
        .create(&NewStoryInput {
            title: "scope".into(),
            story_type: Some("epic".into()),
            ..Default::default()
        })
        .unwrap()
        .id
}

#[test]
fn missing_or_retyped_scope_halts_without_losing_status_or_recovery_controls() {
    use storyhook::service::FieldEdits;
    use storyhook::store::{EngineRunState, ReadOps, Store};
    for delete in [false, true] {
        for occupied in [false, true] {
            let fixture = ServiceFixture::new();
            fixture.write_hooks_toml(
                "on_engine_run_halted = { command = \"cat >> halt.jsonl; echo >> halt.jsonl\" }\n",
            );
            let ctx = fixture.ctx();
            let scope = epic(&ctx);
            let child = story(&ctx, "scoped work");
            RelationService::new(&ctx)
                .relate(&scope, "parent-of", &child, false)
                .unwrap();
            let fake = FakeDispatcher::new([dispatched_with_pane()]);
            let run = start(&ctx, &fake, EngineScope::Epic(scope.clone()), 1);
            let engine = EngineService::new(&ctx, &fake);
            if occupied {
                engine.reconcile(&run).unwrap();
            }
            if delete {
                StoryService::new(&ctx).delete(&scope).unwrap();
            } else {
                StoryService::new(&ctx)
                    .set_fields(
                        &scope,
                        &FieldEdits {
                            story_type: Some("feature".into()),
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }
            story(&ctx, "outside work must not be taken");
            assert!(
                engine.status(Some(&run)).is_ok(),
                "status must survive scope loss before the next tick"
            );
            let before = fixture.store().read(|tx| tx.engine_lanes(&run)).unwrap();
            for _ in 0..2 {
                let report = engine.reconcile(&run).unwrap();
                assert_eq!(report.run_state, EngineRunState::Halted);
                assert_eq!(report.stop_reason.as_deref(), Some("scope-unavailable"));
                assert!(report.filled.is_empty());
                assert!(report.quarantined.is_empty());
            }
            assert_eq!(
                fixture.store().read(|tx| tx.engine_lanes(&run)).unwrap(),
                before
            );
            let notifications = std::fs::read_to_string(fixture.cwd().join("halt.jsonl")).unwrap();
            let notifications: Vec<serde_json::Value> = notifications
                .lines()
                .filter(|line| !line.is_empty())
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(notifications.len(), 1, "scope loss must notify only once");
            assert_eq!(notifications[0]["stop_reason"], "scope-unavailable");
            assert_eq!(notifications[0]["epic_id"], scope);
            assert!(
                engine
                    .acknowledge(&run)
                    .unwrap()
                    .run
                    .acknowledged_at
                    .is_some()
            );
            // Empty halted runs can be stopped immediately without any cleanup
            // authority. Occupied runs retain the existing lease requirement.
            if !occupied {
                assert_eq!(
                    engine.stop(&run, true).unwrap().run.state,
                    EngineRunState::Finished
                );
            }
        }
    }
}

#[test]
fn graceful_stop_can_finish_after_its_epic_is_deleted() {
    use storyhook::store::EngineRunState;
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let scope = epic(&ctx);
    let child = story(&ctx, "scoped work");
    RelationService::new(&ctx)
        .relate(&scope, "parent-of", &child, false)
        .unwrap();
    let fake = FakeDispatcher::new([dispatched()]);
    let run = start(&ctx, &fake, EngineScope::Epic(scope.clone()), 1);
    let engine = EngineService::new(&ctx, &fake);
    engine.reconcile(&run).unwrap();
    engine.stop(&run, false).unwrap();
    StoryService::new(&ctx).delete(&scope).unwrap();
    StoryService::new(&ctx)
        .set_state(&child, "done", None, None, None)
        .unwrap();
    let report = engine.reconcile(&run).unwrap();
    assert_eq!(report.run_state, EngineRunState::Finished);
    assert_eq!(report.stop_reason.as_deref(), Some("operator-stopped"));
}

#[test]
fn halted_scope_can_release_its_occupied_lane_with_the_original_lease() {
    use storyhook::domain::{CLEANUP_LEASE_VERSION, StoryCleanupLease, TmuxCleanupTarget};
    use storyhook::store::EngineRunState;
    use storyhook_test_support::DispatcherCall;
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let scope = epic(&ctx);
    let child = story(&ctx, "scoped work");
    RelationService::new(&ctx)
        .relate(&scope, "parent-of", &child, false)
        .unwrap();
    let lease = StoryCleanupLease {
        version: CLEANUP_LEASE_VERSION,
        project_slug: "fixture".into(),
        story_id: child.clone(),
        repository_path: "/repos/original".into(),
        worktree_path: "/tmp/preserved".into(),
        branch: format!("worktree-{child}"),
        tmux: TmuxCleanupTarget {
            socket_path: "/tmp/tmux-original/default".into(),
        },
    };
    let fake = FakeDispatcher::new([
        DispatcherStep::Dispatch(DispatchOutcome::from_payload(
            serde_json::json!({"ok": true, "cleanup_lease": lease}),
        )),
        DispatcherStep::Unclaim(DispatchOutcome::from_payload(
            serde_json::json!({"ok": true}),
        )),
    ]);
    let run = start(&ctx, &fake, EngineScope::Epic(scope.clone()), 1);
    let engine = EngineService::new(&ctx, &fake);
    engine.reconcile(&run).unwrap();
    engine.pause(&run).unwrap();
    StoryService::new(&ctx).delete(&scope).unwrap();
    assert_eq!(
        engine.reconcile_after_restart(&run).unwrap().run_state,
        EngineRunState::Halted
    );
    assert_eq!(
        engine.stop(&run, true).unwrap().run.state,
        EngineRunState::Finished
    );
    let calls = fake.calls();
    assert!(
        matches!(&calls[1], DispatcherCall::Unclaim(request) if request.cleanup_lease == lease)
    );
    assert_eq!(calls.len(), 2);
}
