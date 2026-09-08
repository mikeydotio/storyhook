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
