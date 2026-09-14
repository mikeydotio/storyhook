//! Stop Now ownership, recovery and receipt contracts use the real store.
use storyhook::domain::{CLEANUP_LEASE_VERSION, StoryCleanupLease, TmuxCleanupTarget};
use storyhook::service::engine::{DispatchOutcome, EngineService, StartRequest};
use storyhook::service::{NewStoryInput, StoryService};
use storyhook::store::{
    EngineAgent, EngineLaneState, EngineRunState, EngineScope, ReadOps, Store, StoryNo, WriteOps,
};
use storyhook_test_support::{
    DispatcherCall, DispatcherStep, FIXTURE_NOW, FakeDispatcher, ServiceFixture,
};

fn setup(fixture: &ServiceFixture, fake: &FakeDispatcher, initial: &str) -> String {
    let ctx = fixture.ctx();
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Owned reset target".into(),
            state: Some(initial.into()),
            ..NewStoryInput::default()
        })
        .unwrap();
    if initial != "in-progress" {
        StoryService::new(&ctx)
            .set_state(&story.id, "in-progress", None, None, None)
            .unwrap();
    }
    let engine = EngineService::new(&ctx, fake);
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
    fixture
        .store()
        .write(|tx| {
            let mut lane = tx.engine_lanes(&run.id)?.remove(0);
            lane.state = EngineLaneState::Working;
            lane.story_id = Some(story.id.clone());
            lane.window_name = Some(story.id.clone());
            lane.dispatched_at = Some(FIXTURE_NOW.into());
            lane.cleanup_lease = Some(StoryCleanupLease {
                version: CLEANUP_LEASE_VERSION,
                project_slug: "fixture".into(),
                story_id: story.id,
                repository_path: "/owned/repo".into(),
                worktree_path: "/owned/repo/lane".into(),
                branch: "worktree-SH-1".into(),
                tmux: TmuxCleanupTarget {
                    socket_path: "/owned/socket".into(),
                },
            });
            tx.put_engine_lane(&lane)
        })
        .unwrap();
    run.id
}

#[test]
fn restart_retains_explicit_reset_then_steady_pass_finishes_it_without_quarantine() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([
        DispatcherStep::ResetFailure("partial deletion".into()),
        DispatcherStep::Reset,
    ]);
    let run = setup(&fixture, &fake, "todo");
    let ctx = fixture.ctx();
    let engine = EngineService::new(&ctx, &fake);
    assert!(engine.stop(&run, true).is_err());
    let before = fixture
        .store()
        .read(|tx| tx.engine_reset(fixture.project(), StoryNo::new(1)))
        .unwrap();
    // A new store connection reconstructs ownership from disk, not the
    // original service instance's memory.
    let reopened = storyhook::store::SqliteStore::open(fixture.env().store_path()).unwrap();
    let resumed_ctx = storyhook::service::Ctx::new(
        &reopened,
        fixture.project(),
        fixture.cwd(),
        fixture.env().clone(),
    );
    let engine = EngineService::new(&resumed_ctx, &fake);
    assert_eq!(
        engine.reconcile_after_restart(&run).unwrap().run_state,
        EngineRunState::Draining
    );
    assert_eq!(fake.calls().len(), 1);
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.engine_reset(fixture.project(), StoryNo::new(1)))
            .unwrap(),
        before
    );
    assert_eq!(
        engine.reconcile(&run).unwrap().run_state,
        EngineRunState::Finished
    );
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "todo");
    assert!(row.awaiting.is_none());
    assert!(
        !row.snapshot
            .comments
            .iter()
            .any(|c| c.text.contains("window-gone"))
    );
}

#[test]
fn reset_of_a_verifier_return_never_restores_it_to_verifying() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([DispatcherStep::Reset]);
    let run = setup(&fixture, &fake, "verifying");
    EngineService::new(&fixture.ctx(), &fake)
        .stop(&run, true)
        .unwrap();
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .state,
        "todo"
    );
}

#[test]
fn forged_or_incomplete_receipts_never_release_the_story_or_reservation() {
    for field in [
        "token",
        "lease",
        "tmux_story_windows_absent",
        "worktree_registration_absent",
        "worktree_path_absent",
        "branch_absent",
        "ok",
    ] {
        let fixture = ServiceFixture::new();
        let fake = FakeDispatcher::new([DispatcherStep::ResetFailure("obtain reservation".into())]);
        let run = setup(&fixture, &fake, "todo");
        assert!(
            EngineService::new(&fixture.ctx(), &fake)
                .stop(&run, true)
                .is_err()
        );
        let reset = fixture
            .store()
            .read(|tx| tx.engine_reset(fixture.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap();
        let mut payload = serde_json::json!({"ok":true,"token":reset.token,"lease":reset.lease,
            "postconditions":{"tmux_story_windows_absent":true,"worktree_registration_absent":true,"worktree_path_absent":true,"branch_absent":true}});
        if ["token", "lease", "ok"].contains(&field) {
            payload.as_object_mut().unwrap().remove(field);
        } else {
            payload["postconditions"][field] = serde_json::json!(false);
        }
        let forged = FakeDispatcher::new([DispatcherStep::ResetReceipt(
            DispatchOutcome::from_payload(payload),
        )]);
        assert!(
            EngineService::new(&fixture.ctx(), &forged)
                .stop(&run, true)
                .is_err(),
            "{field}"
        );
        let row = fixture
            .store()
            .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "in-progress", "{field}");
        assert_eq!(row.awaiting, None, "{field}");
        assert_eq!(
            fixture
                .store()
                .read(|tx| tx.engine_reset(fixture.project(), StoryNo::new(1)))
                .unwrap()
                .unwrap()
                .token,
            reset.token
        );
    }
}

#[test]
fn reset_check_is_read_only_and_rejects_the_reserved_story() {
    use storyhook::cli::{EngineAction, Invocation};
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([DispatcherStep::ResetFailure("hold".into())]);
    let run = setup(&fixture, &fake, "todo");
    let ctx = fixture.ctx();
    let check = || {
        storyhook::invoke::dispatch(
            &ctx,
            Invocation::Engine {
                action: EngineAction::ResetCheck {
                    story: "SH-1".into(),
                },
            },
        )
    };
    assert!(check().is_ok());
    assert!(EngineService::new(&ctx, &fake).stop(&run, true).is_err());
    assert!(
        check()
            .unwrap_err()
            .to_string()
            .contains("reset in progress")
    );
    assert!(matches!(
        fake.calls().as_slice(),
        [DispatcherCall::Reset(_)]
    ));
}

#[test]
fn cleanup_authorization_command_survives_the_real_cli_flag_gate() {
    use storyhook::cli::{EngineAction, Invocation, parse_invocation};
    let args: Vec<String> = [
        "engine",
        "reset-target",
        "--run",
        "run-1",
        "--token",
        "operation-1",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(
        parse_invocation(&args).unwrap(),
        Invocation::Engine {
            action: EngineAction::ResetTarget {
                run: "run-1".into(),
                token: "operation-1".into()
            }
        }
    );
    for invalid in [
        vec!["engine", "reset-target", "--run", "run-1"],
        vec![
            "engine",
            "reset-target",
            "--run",
            "run-1",
            "--force",
            "operation-1",
        ],
        vec!["engine", "reset-check", "SH-1", "--force"],
    ] {
        assert!(
            parse_invocation(&invalid.into_iter().map(String::from).collect::<Vec<_>>()).is_err()
        );
    }
}

#[test]
fn stale_probe_and_duplicate_stop_cannot_compete_with_the_reset_owner() {
    use std::sync::{Mutex, mpsc};
    use storyhook::error::AppError;
    use storyhook::lane_budget::WindowCensus;
    use storyhook::service::engine::{
        DISPATCH_TIMEOUT, DispatchRequest, Dispatcher, UnclaimRequest, WindowProbe,
    };
    use storyhook::store::EngineReset;

    struct Held {
        probe_entered: mpsc::Sender<()>,
        probe_release: Mutex<mpsc::Receiver<()>>,
        reset_entered: mpsc::Sender<()>,
        reset_release: Mutex<mpsc::Receiver<()>>,
    }
    impl Dispatcher for Held {
        fn dispatch(&self, _: DispatchRequest) -> Result<DispatchOutcome, AppError> {
            panic!("stop must not dispatch")
        }
        fn unclaim(&self, _: UnclaimRequest) -> Result<DispatchOutcome, AppError> {
            panic!("stop must not unclaim")
        }
        fn kill_window(&self, _: &str) -> Result<(), AppError> {
            panic!("reset actuator owns closure")
        }
        fn census(&self) -> WindowCensus {
            WindowCensus::Counted { windows: vec![] }
        }
        fn probe_window(&self, _: &str) -> WindowProbe {
            self.probe_entered.send(()).unwrap();
            self.probe_release
                .lock()
                .unwrap()
                .recv_timeout(DISPATCH_TIMEOUT)
                .unwrap();
            WindowProbe::Gone {
                detail: "old observation: agent exited".into(),
            }
        }
        fn reset(&self, request: EngineReset) -> Result<DispatchOutcome, AppError> {
            self.reset_entered.send(()).unwrap();
            self.reset_release
                .lock()
                .unwrap()
                .recv_timeout(DISPATCH_TIMEOUT)
                .unwrap();
            Ok(DispatchOutcome::from_payload(
                serde_json::json!({"ok":true,"token":request.token,"lease":request.lease,
                "postconditions":{"tmux_story_windows_absent":true,"worktree_registration_absent":true,"worktree_path_absent":true,"branch_absent":true}}),
            ))
        }
    }
    let fixture = ServiceFixture::new();
    let run = setup(&fixture, &FakeDispatcher::default(), "todo");
    let (probe_entered, probe_started) = mpsc::channel();
    let (release_probe, probe_release) = mpsc::channel();
    let (reset_entered, reset_started) = mpsc::channel();
    let (release_reset, reset_release) = mpsc::channel();
    let held = Held {
        probe_entered,
        probe_release: Mutex::new(probe_release),
        reset_entered,
        reset_release: Mutex::new(reset_release),
    };
    let ctx = fixture.ctx();
    let engine = EngineService::new(&ctx, &held);
    std::thread::scope(|scope| {
        let observer = scope.spawn(|| engine.reconcile(&run));
        probe_started.recv_timeout(DISPATCH_TIMEOUT).unwrap();
        let owner = scope.spawn(|| engine.stop(&run, true));
        reset_started.recv_timeout(DISPATCH_TIMEOUT).unwrap();
        assert!(
            engine
                .stop(&run, true)
                .unwrap_err()
                .to_string()
                .contains("already in progress")
        );
        StoryService::new(&ctx)
            .comment("SH-1", "comment during cleanup survives")
            .unwrap();
        release_probe.send(()).unwrap();
        let report = observer.join().unwrap().unwrap();
        assert!(report.quarantined.is_empty());
        assert_eq!(report.run_state, EngineRunState::Draining);
        let row = fixture
            .store()
            .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap();
        assert!(row.awaiting.is_none());
        assert_eq!(row.state, "in-progress");
        release_reset.send(()).unwrap();
        assert_eq!(
            owner.join().unwrap().unwrap().run.state,
            EngineRunState::Finished
        );
    });
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "todo");
    assert!(
        row.snapshot
            .comments
            .iter()
            .any(|comment| comment.text == "comment during cleanup survives")
    );
}

#[test]
fn reservation_identity_is_immutable_but_failure_diagnostics_can_change() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([DispatcherStep::ResetFailure("hold".into())]);
    let run = setup(&fixture, &fake, "todo");
    assert!(
        EngineService::new(&fixture.ctx(), &fake)
            .stop(&run, true)
            .is_err()
    );
    let original = fixture
        .store()
        .read(|tx| tx.engine_reset(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    for field in ["token", "run", "lane", "lease", "destination"] {
        let mut changed = original.clone();
        match field {
            "token" => changed.token.push('x'),
            "run" => changed.run_id.push('x'),
            "lane" => changed.lane_index += 1,
            "lease" => changed.lease.branch.push('x'),
            "destination" => changed.restore_to = "verifying".into(),
            _ => unreachable!(),
        }
        assert!(
            fixture
                .store()
                .write(|tx| tx.put_engine_reset(&changed))
                .is_err(),
            "{field}"
        );
        assert_eq!(
            fixture
                .store()
                .read(|tx| tx.engine_reset(fixture.project(), StoryNo::new(1)))
                .unwrap()
                .unwrap(),
            original
        );
    }
    let mut diagnostic = original;
    diagnostic.failure = Some("second cleanup failure".into());
    fixture
        .store()
        .write(|tx| tx.put_engine_reset(&diagnostic))
        .unwrap();
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.engine_reset(fixture.project(), StoryNo::new(1)))
            .unwrap(),
        Some(diagnostic)
    );
}

#[test]
fn stop_from_inside_a_target_refuses_before_scheduling_background_cleanup() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let run = setup(&fixture, &fake, "todo");
    fixture
        .store()
        .write(|tx| {
            let mut lane = tx.engine_lanes(&run)?.remove(0);
            lane.cleanup_lease.as_mut().unwrap().worktree_path = fixture.cwd().to_path_buf();
            tx.put_engine_lane(&lane)
        })
        .unwrap();
    let result = EngineService::new(&fixture.ctx(), &fake).stop(&run, true);
    assert!(result.unwrap_err().to_string().contains("calling worktree"));
    assert!(fake.calls().is_empty());
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.engine_run(&run))
            .unwrap()
            .unwrap()
            .state,
        EngineRunState::Running
    );
    assert!(
        fixture
            .store()
            .read(|tx| tx.engine_reset(fixture.project(), StoryNo::new(1)))
            .unwrap()
            .is_none()
    );
}

#[test]
fn reset_preserves_metadata_and_dependency_edges_while_clearing_awaiting() {
    use storyhook::service::RelationService;
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([DispatcherStep::Reset]);
    let run = setup(&fixture, &fake, "todo");
    let ctx = fixture.ctx();
    let stories = StoryService::new(&ctx);
    let other = stories
        .create(&NewStoryInput {
            title: "dependency".into(),
            ..NewStoryInput::default()
        })
        .unwrap();
    RelationService::new(&ctx)
        .relate("SH-1", "blocked-by", &other.id, false)
        .unwrap();
    stories.comment("SH-1", "retain the investigation").unwrap();
    stories
        .set_labels("SH-1", &["regression".into()], &[])
        .unwrap();
    stories.set_awaiting("SH-1", "previous quarantine").unwrap();
    let before = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap()
        .snapshot;
    EngineService::new(&ctx, &fake).stop(&run, true).unwrap();
    let after = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap()
        .snapshot;
    assert_eq!(after.relationships, before.relationships);
    assert_eq!(after.labels, before.labels);
    assert_eq!(after.assignee, before.assignee);
    assert!(after.comments.starts_with(&before.comments));
    assert_eq!(after.awaiting, None);
    assert_eq!(after.state, "todo");
}

#[test]
fn closed_lane_detaches_without_story_mutation_or_helper_cleanup() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let run = setup(&fixture, &fake, "todo");
    let ctx = fixture.ctx();
    StoryService::new(&ctx)
        .set_state("SH-1", "done", None, None, None)
        .unwrap();
    let before = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap();
    assert_eq!(
        EngineService::new(&ctx, &fake)
            .stop(&run, true)
            .unwrap()
            .run
            .state,
        EngineRunState::Finished
    );
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
            .unwrap(),
        before
    );
    assert!(fake.calls().is_empty());
}
