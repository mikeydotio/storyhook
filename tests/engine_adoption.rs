//! SH-700: adoption consumes existing capacity without redispatching work.
use std::path::Path;
use storyhook::domain::{StoryCleanupLease, TmuxCleanupTarget};
use storyhook::error::AppError;
use storyhook::service::engine::adoption::{AdoptedIdentity, DispatchInspector, InspectedDispatch};
use storyhook::service::engine::{EngineService, StartRequest};
use storyhook::service::{NewStoryInput, StoryService};
use storyhook::store::{EngineAgent, EngineScope, ReadOps, Store};
use storyhook_test_support::{FakeDispatcher, ServiceFixture};

struct Inspector;
impl DispatchInspector for Inspector {
    fn inspect(&self, _: &Path, project: &str, story: &str) -> Result<InspectedDispatch, AppError> {
        Ok(InspectedDispatch {
            lease: StoryCleanupLease {
                version: 1,
                project_slug: project.into(),
                story_id: story.into(),
                repository_path: "/checkouts/fixture".into(),
                worktree_path: format!("/worktrees/{story}").into(),
                branch: format!("worktree-{story}"),
                tmux: TmuxCleanupTarget {
                    socket_path: "/tmp/test-socket".into(),
                },
            },
            pane_id: format!("%{}", story.split('-').next_back().unwrap()),
            window_name: story.into(),
            identity: AdoptedIdentity {
                provider: EngineAgent::Codex,
                pane_pid: 123,
                window_id: format!("@{story}"),
            },
        })
    }
}

fn start(
    service: &EngineService<'_, storyhook::store::SqliteStore, FakeDispatcher>,
    lanes: u32,
) -> String {
    service
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes,
            agent: EngineAgent::Claude,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap()
        .id
}

fn claimed(fixture: &ServiceFixture) -> String {
    let ctx = fixture.ctx();
    let stories = StoryService::new(&ctx);
    let story = stories
        .create(&NewStoryInput {
            title: "manual".into(),
            ..Default::default()
        })
        .unwrap();
    stories.claim_story(&story.id, None).unwrap();
    story.id
}

#[test]
fn atomic_adoption_and_retry_do_not_dispatch_or_change_the_claim() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);
    let run = start(&service, 2);
    let ids = vec![claimed(&fixture), claimed(&fixture)];
    let before = fixture
        .store()
        .read(|tx| tx.max_global_seq(ctx.project()))
        .unwrap();
    let view = service.adopt(&run, &ids, &Inspector).unwrap();
    assert_eq!(
        view.lanes
            .iter()
            .filter(|lane| lane.adopted_identity.is_some())
            .count(),
        2
    );
    assert_eq!(service.adopt(&run, &ids, &Inspector).unwrap(), view);
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.max_global_seq(ctx.project()))
            .unwrap(),
        before
    );
}

#[test]
fn capacity_failure_and_duplicate_ids_leave_every_lane_untouched() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);
    let run = start(&service, 1);
    let ids = vec![claimed(&fixture), claimed(&fixture)];
    let before = service.status(Some(&run)).unwrap();
    assert!(
        service
            .adopt(&run, &ids, &Inspector)
            .unwrap_err()
            .to_string()
            .contains("capacity")
    );
    assert!(
        service
            .adopt(&run, &[ids[0].clone(), ids[0].clone()], &Inspector)
            .is_err()
    );
    assert_eq!(service.status(Some(&run)).unwrap(), before);
}

#[test]
fn non_claimed_and_blocked_work_is_refused_before_inspection() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);
    let run = start(&service, 2);
    let stories = StoryService::new(&ctx);
    let todo = stories
        .create(&NewStoryInput {
            title: "todo".into(),
            ..Default::default()
        })
        .unwrap();
    assert!(service.adopt(&run, &[todo.id], &Inspector).is_err());
    let id = claimed(&fixture);
    stories
        .set_state(&id, "blocked", None, None, Some("held"))
        .unwrap();
    assert!(service.adopt(&run, &[id], &Inspector).is_err());
    assert!(service.adopt(&run, &[], &Inspector).is_err());
}

#[test]
fn an_inspection_failure_rolls_back_the_entire_batch() {
    struct Failing;
    impl DispatchInspector for Failing {
        fn inspect(
            &self,
            checkout: &Path,
            project: &str,
            story: &str,
        ) -> Result<InspectedDispatch, AppError> {
            if story.ends_with("-2") {
                Err(AppError::Validation("lease unreadable".into()))
            } else {
                Inspector.inspect(checkout, project, story)
            }
        }
    }
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);
    let run = start(&service, 2);
    let ids = vec![claimed(&fixture), claimed(&fixture)];
    let before = service.status(Some(&run)).unwrap();
    assert!(
        service
            .adopt(&run, &ids, &Failing)
            .unwrap_err()
            .to_string()
            .contains("lease unreadable")
    );
    assert_eq!(service.status(Some(&run)).unwrap(), before);
}

#[test]
fn adopted_handoff_and_unclaim_release_only_the_binding() {
    for destination in ["verifying", "todo", "done"] {
        let fixture = ServiceFixture::new();
        let fake = FakeDispatcher::new([storyhook_test_support::DispatcherStep::WindowAlive {
            window: "%1".into(),
            alive: false,
        }]);
        let ctx = fixture.ctx();
        let service = EngineService::new(&ctx, &fake);
        let run = start(&service, 1);
        let id = claimed(&fixture);
        service
            .adopt(&run, std::slice::from_ref(&id), &Inspector)
            .unwrap();
        service.pause(&run).unwrap();
        StoryService::new(&ctx)
            .set_state(&id, destination, None, None, None)
            .unwrap();
        service.reconcile(&run).unwrap();
        let view = service.status(Some(&run)).unwrap().remove(0);
        assert!(view.lanes[0].story_id.is_none(), "{destination}: {view:?}");
        assert!(view.lanes[0].adopted_identity.is_none());
    }
}

#[test]
fn concurrent_adopters_cannot_overfill_one_lane() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);
    let run = start(&service, 1);
    let ids = [claimed(&fixture), claimed(&fixture)];
    let barrier = std::sync::Barrier::new(2);
    struct Racing<'a>(&'a std::sync::Barrier, std::sync::atomic::AtomicUsize);
    impl DispatchInspector for Racing<'_> {
        fn inspect(&self, p: &Path, slug: &str, id: &str) -> Result<InspectedDispatch, AppError> {
            if self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
                self.0.wait();
            }
            Inspector.inspect(p, slug, id)
        }
    }
    let results = std::thread::scope(|scope| {
        let handles = ids
            .iter()
            .map(|id| {
                let service = &service;
                let run = &run;
                let barrier = &barrier;
                scope.spawn(move || {
                    service.adopt(
                        run,
                        std::slice::from_ref(id),
                        &Racing(barrier, std::sync::atomic::AtomicUsize::new(0)),
                    )
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap().is_ok())
            .collect::<Vec<_>>()
    });
    assert_eq!(results.iter().filter(|ok| **ok).count(), 1);
    assert_eq!(
        service.status(Some(&run)).unwrap()[0]
            .lanes
            .iter()
            .filter(|lane| lane.story_id.is_some())
            .count(),
        1
    );
}

#[test]
fn changed_story_or_capacity_during_inspection_cannot_commit_stale_ownership() {
    for shrink in [false, true] {
        let fixture = ServiceFixture::new();
        let fake = FakeDispatcher::default();
        let ctx = fixture.ctx();
        let service = EngineService::new(&ctx, &fake);
        let run = start(&service, 2);
        let ids = vec![claimed(&fixture), claimed(&fixture)];
        struct Changing<'a> {
            fixture: &'a ServiceFixture,
            run: &'a str,
            shrink: bool,
            changed: std::sync::atomic::AtomicBool,
        }
        impl DispatchInspector for Changing<'_> {
            fn inspect(
                &self,
                p: &Path,
                slug: &str,
                id: &str,
            ) -> Result<InspectedDispatch, AppError> {
                if !self.changed.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    let ctx = self.fixture.ctx();
                    if self.shrink {
                        EngineService::new(&ctx, &FakeDispatcher::default())
                            .configure_patch(
                                &self.run.to_string(),
                                storyhook::service::engine::ConfigurePatch {
                                    lanes: Some(1),
                                    ..Default::default()
                                },
                            )
                            .unwrap();
                    } else {
                        StoryService::new(&ctx)
                            .set_state(id, "todo", None, None, None)
                            .unwrap();
                    }
                }
                Inspector.inspect(p, slug, id)
            }
        }
        assert!(
            service
                .adopt(
                    &run,
                    &ids,
                    &Changing {
                        fixture: &fixture,
                        run: &run,
                        shrink,
                        changed: false.into()
                    }
                )
                .is_err()
        );
        assert!(
            service.status(Some(&run)).unwrap()[0]
                .lanes
                .iter()
                .all(|lane| lane.story_id.is_none())
        );
    }
}

#[test]
fn manual_no_auto_is_explicitly_eligible_but_human_only_is_not() {
    for (label, allowed) in [("no-auto", true), ("human-only", false)] {
        let fixture = ServiceFixture::new();
        let fake = FakeDispatcher::default();
        let ctx = fixture.ctx();
        let service = EngineService::new(&ctx, &fake);
        let run = start(&service, 1);
        let stories = StoryService::new(&ctx);
        let story = stories
            .create(&NewStoryInput {
                title: "manual".into(),
                labels: Some(vec![label.into()]),
                ..Default::default()
            })
            .unwrap();
        stories
            .set_state(&story.id, "in-progress", None, None, None)
            .unwrap();
        assert_eq!(
            service.adopt(&run, &[story.id], &Inspector).is_ok(),
            allowed
        );
    }
}

#[test]
fn identical_retry_remains_idempotent_after_capacity_is_lowered() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);
    let run = start(&service, 2);
    let ids = vec![claimed(&fixture), claimed(&fixture)];
    service.adopt(&run, &ids, &Inspector).unwrap();
    let contracted = service
        .configure_patch(
            &run,
            storyhook::service::engine::ConfigurePatch {
                lanes: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(service.adopt(&run, &ids, &Inspector).unwrap(), contracted);
}

#[test]
fn epic_scope_and_dependencies_are_enforced() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    storyhook::service::ConfigService::new(&ctx)
        .add_type("epic", None, None)
        .unwrap();
    let stories = StoryService::new(&ctx);
    let epic = stories
        .create(&NewStoryInput {
            title: "scope".into(),
            story_type: Some("epic".into()),
            ..Default::default()
        })
        .unwrap();
    let id = claimed(&fixture);
    let service = EngineService::new(&ctx, &fake);
    let run = service
        .start(StartRequest {
            scope: EngineScope::Epic(epic.id.clone()),
            lanes: 1,
            agent: EngineAgent::Codex,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap()
        .id;
    assert!(
        service
            .adopt(&run, std::slice::from_ref(&id), &Inspector)
            .is_err()
    );
    storyhook::service::RelationService::new(&ctx)
        .relate(&epic.id, "parent-of", &id, false)
        .unwrap();
    let blocker = stories
        .create(&NewStoryInput {
            title: "dependency".into(),
            ..Default::default()
        })
        .unwrap();
    storyhook::service::RelationService::new(&ctx)
        .relate(&id, "blocked-by", &blocker.id, false)
        .unwrap();
    assert!(
        service
            .adopt(&run, std::slice::from_ref(&id), &Inspector)
            .is_err()
    );
    stories
        .set_state(&blocker.id, "done", None, None, None)
        .unwrap();
    assert!(service.adopt(&run, &[id], &Inspector).is_ok());
}

#[test]
fn blocked_adopted_work_is_quarantined_and_restart_preserves_ownership() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([
        storyhook_test_support::DispatcherStep::WindowAlive {
            window: "%1".into(),
            alive: true,
        },
        storyhook_test_support::DispatcherStep::WindowAlive {
            window: "%1".into(),
            alive: true,
        },
    ]);
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);
    let run = start(&service, 1);
    let id = claimed(&fixture);
    let adopted = service
        .adopt(&run, std::slice::from_ref(&id), &Inspector)
        .unwrap();
    service.pause(&run).unwrap();
    service.reconcile_after_restart(&run).unwrap();
    let current = service.status(Some(&run)).unwrap().remove(0);
    assert_eq!(
        current.lanes[0].adopted_identity,
        adopted.lanes[0].adopted_identity
    );
    StoryService::new(&ctx)
        .set_state(&id, "blocked", None, None, Some("manual hold"))
        .unwrap();
    service.reconcile(&run).unwrap();
    let current = service.status(Some(&run)).unwrap().remove(0);
    assert_eq!(
        current.lanes[0].state,
        storyhook::store::EngineLaneState::Quarantined
    );
    assert_eq!(
        current.lanes[0].cleanup_lease,
        adopted.lanes[0].cleanup_lease
    );
}

#[test]
fn adoption_cli_and_wire_preserve_ids_and_refuse_missing_operands() {
    use storyhook::cli::{EngineAction, Invocation, parse_invocation};
    let invocation =
        parse_invocation(&["engine", "adopt", "1", "SH-2", "--run", "opaque"].map(str::to_owned))
            .unwrap();
    let restored: Invocation =
        serde_json::from_str(&serde_json::to_string(&invocation).unwrap()).unwrap();
    assert_eq!(restored, invocation);
    assert!(
        matches!(invocation, Invocation::Engine { action: EngineAction::Adopt { ids, run } } if ids == ["1", "SH-2"] && run.as_deref() == Some("opaque"))
    );
    for args in [
        vec!["engine", "adopt"],
        vec!["engine", "adopt", "--run", "opaque"],
        vec!["engine", "adopt", "SH-1", "--run"],
        vec!["engine", "adopt", "SH-1", "--lanes", "2"],
    ] {
        assert!(
            parse_invocation(&args.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>()).is_err()
        );
    }
}
