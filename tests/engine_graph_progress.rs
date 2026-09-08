//! SH-609: prove progress through changing dependency graphs, not just queue snapshots.

use storyhook::service::engine::{DispatchOutcome, EngineService, StartRequest};
use storyhook::service::{ConfigService, Ctx, NewStoryInput, RelationService, StoryService};
use storyhook::store::{EngineAgent, EngineRunState, EngineScope, SqliteStore};
use storyhook_test_support::{DispatcherStep, FakeDispatcher, ServiceFixture};

fn create(ctx: &Ctx<'_, SqliteStore>, title: &str, priority: &str, epic: bool) -> String {
    StoryService::new(ctx)
        .create(&NewStoryInput {
            title: title.into(),
            priority: Some(priority.into()),
            story_type: epic.then(|| "epic".into()),
            ..Default::default()
        })
        .unwrap()
        .id
}

fn start(ctx: &Ctx<'_, SqliteStore>, fake: &FakeDispatcher, scope: EngineScope) -> String {
    EngineService::new(ctx, fake)
        .start(StartRequest {
            scope,
            lanes: 1,
            agent: EngineAgent::Codex,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap()
        .id
}

fn dispatches(count: usize) -> FakeDispatcher {
    FakeDispatcher::new((0..count).map(|_| {
        DispatcherStep::Dispatch(DispatchOutcome::from_payload(
            serde_json::json!({"ok": true}),
        ))
    }))
}

/// The fake substitutes only external dispatch; claims, graph projection,
/// completion events, and each fresh scheduling decision use real services.
fn complete_in_order(
    ctx: &Ctx<'_, SqliteStore>,
    fake: &FakeDispatcher,
    run: &String,
    expected: &[String],
) {
    let engine = EngineService::new(ctx, fake);
    for id in expected {
        assert_eq!(engine.reconcile(run).unwrap().filled, [(0, id.clone())]);
        StoryService::new(ctx)
            .set_state(id, "done", None, None, None)
            .unwrap();
    }
}

#[test]
fn nested_scope_and_project_runs_reconsider_priority_as_a_diamond_unblocks() {
    for scoped in [false, true] {
        let fixture = ServiceFixture::new();
        let ctx = fixture.ctx();
        ConfigService::new(&ctx)
            .add_type("epic", None, None)
            .unwrap();
        let outer = create(&ctx, "outer", "low", true);
        let inner = create(&ctx, "inner", "high", true);
        let root = create(&ctx, "root", "low", false);
        let left = create(&ctx, "left", "high", false);
        let right = create(&ctx, "right", "medium", false);
        let tip = create(&ctx, "tip", "critical", false);
        let ready = create(&ctx, "already ready", "high", false);
        let tail = create(&ctx, "tail", "low", false);
        let outside = create(&ctx, "outside", "critical", false);
        let relations = RelationService::new(&ctx);
        relations
            .relate(&outer, "parent-of", &inner, false)
            .unwrap();
        for id in [&root, &left, &right, &tip, &ready, &tail] {
            relations.relate(&inner, "parent-of", id, false).unwrap();
        }
        for (from, to) in [
            (&root, &left),
            (&root, &right),
            (&left, &tip),
            (&right, &tip),
        ] {
            relations.relate(from, "blocks", to, false).unwrap();
        }
        let mut expected = vec![ready, root, left, right, tip, tail];
        if !scoped {
            expected.insert(0, outside);
        }
        let fake = dispatches(expected.len());
        let scope = if scoped {
            EngineScope::Epic(outer)
        } else {
            EngineScope::Project
        };
        let run = start(&ctx, &fake, scope);
        complete_in_order(&ctx, &fake, &run, &expected);
        assert_eq!(
            EngineService::new(&ctx, &fake)
                .reconcile(&run)
                .unwrap()
                .run_state,
            EngineRunState::Finished
        );
    }
}

#[test]
fn epic_priority_and_multiple_parents_break_ties_before_story_age() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    ConfigService::new(&ctx)
        .add_type("epic", None, None)
        .unwrap();
    let low = create(&ctx, "low", "low", true);
    let high = create(&ctx, "high", "high", true);
    let critical = create(&ctx, "critical", "critical", true);
    let oldest = create(&ctx, "oldest", "medium", false);
    let middle = create(&ctx, "middle", "medium", false);
    let newest = create(&ctx, "newest", "medium", false);
    let same_epic = create(&ctx, "same epic, later", "medium", false);
    let relations = RelationService::new(&ctx);
    for (parent, child) in [
        (&low, &oldest),
        (&high, &middle),
        (&low, &newest),
        (&critical, &newest),
        (&high, &same_epic),
    ] {
        relations.relate(parent, "parent-of", child, false).unwrap();
    }
    let fake = dispatches(4);
    let run = start(&ctx, &fake, EngineScope::Project);
    complete_in_order(&ctx, &fake, &run, &[newest, middle, same_epic, oldest]);
    assert_eq!(
        EngineService::new(&ctx, &fake)
            .reconcile(&run)
            .unwrap()
            .run_state,
        EngineRunState::Finished
    );
}

#[test]
fn cycles_and_outside_blockers_do_not_strand_independent_scoped_work() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    ConfigService::new(&ctx)
        .add_type("epic", None, None)
        .unwrap();
    let scope = create(&ctx, "scope", "high", true);
    let a = create(&ctx, "cycle a", "critical", false);
    let b = create(&ctx, "cycle b", "critical", false);
    let blocked = create(&ctx, "external dependent", "critical", false);
    let outside = create(&ctx, "outside blocker", "critical", false);
    let free = create(&ctx, "independent", "low", false);
    let relations = RelationService::new(&ctx);
    for id in [&a, &b, &blocked, &free] {
        relations.relate(&scope, "parent-of", id, false).unwrap();
    }
    for (from, to) in [(&a, &b), (&b, &a)] {
        relations.relate(from, "blocked-by", to, false).unwrap();
    }
    relations
        .relate(&blocked, "blocked-by", &outside, false)
        .unwrap();
    let fake = dispatches(2);
    let run = start(&ctx, &fake, EngineScope::Epic(scope));
    complete_in_order(&ctx, &fake, &run, &[free]);
    StoryService::new(&ctx)
        .set_state(&outside, "done", None, None, None)
        .unwrap();
    complete_in_order(&ctx, &fake, &run, &[blocked]);
    assert_eq!(
        EngineService::new(&ctx, &fake)
            .reconcile(&run)
            .unwrap()
            .run_state,
        EngineRunState::Finished
    );
}

#[test]
fn excluded_high_priority_work_never_preempts_an_eligible_story() {
    for exclusion in [
        "draft",
        "in-progress",
        "verifying",
        "blocked",
        "awaiting",
        "obviated",
        "done",
        "no-auto",
        "human-only",
    ] {
        let fixture = ServiceFixture::new();
        let ctx = fixture.ctx();
        let excluded = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: exclusion.into(),
                priority: Some("critical".into()),
                draft: exclusion == "draft",
                ..Default::default()
            })
            .unwrap()
            .id;
        let free = create(&ctx, "eligible", "low", false);
        let service = StoryService::new(&ctx);
        match exclusion {
            "draft" => {}
            "awaiting" => {
                service
                    .set_awaiting(&excluded, "operator dependency")
                    .unwrap();
            }
            "no-auto" | "human-only" => {
                service
                    .set_labels(&excluded, &[exclusion.into()], &[])
                    .unwrap();
            }
            "obviated" => {
                RelationService::new(&ctx)
                    .relate(&free, "obviates", &excluded, false)
                    .unwrap();
            }
            state => {
                service
                    .set_state(&excluded, state, None, None, None)
                    .unwrap();
            }
        }
        let fake = dispatches(1);
        let run = start(&ctx, &fake, EngineScope::Project);
        assert_eq!(
            EngineService::new(&ctx, &fake)
                .reconcile(&run)
                .unwrap()
                .filled,
            [(0, free)],
            "exclusion {exclusion}"
        );
    }
}
