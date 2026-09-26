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
    let repo = fixture.cwd().canonicalize().unwrap();
    let init = storyhook::env::git_env::command(&repo)
        .args(["init", "--initial-branch=main"])
        .output()
        .unwrap();
    assert!(init.status.success(), "{init:?}");
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), Some(&repo)))
        .unwrap();
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
                repository_path: repo.clone(),
                worktree_path: repo.join("lane"),
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
    let completion = row.snapshot.comments.last().unwrap();
    assert!(completion.text.contains(&run));
    assert!(completion.text.contains(&before.unwrap().token));
    assert!(completion.text.contains("lane 0"));
    assert!(completion.text.contains("`todo`"));
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
        resets: std::sync::atomic::AtomicUsize,
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
        fn reset(
            &self,
            request: EngineReset,
            _workspace: std::os::fd::BorrowedFd<'_>,
        ) -> Result<DispatchOutcome, AppError> {
            self.resets
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
        resets: std::sync::atomic::AtomicUsize::new(0),
    };
    let ctx = fixture.ctx();
    let engine = EngineService::new(&ctx, &held);
    std::thread::scope(|scope| {
        let observer = scope.spawn(|| engine.reconcile(&run));
        probe_started.recv_timeout(DISPATCH_TIMEOUT).unwrap();
        let owner = scope.spawn(|| engine.stop(&run, true));
        reset_started.recv_timeout(DISPATCH_TIMEOUT).unwrap();
        // SH-774: a duplicate cannot compete, and it is not a failure either:
        // the owner already holds the durable intent it would record.
        let duplicate = engine.stop(&run, true).unwrap();
        assert_eq!(duplicate.run.state, EngineRunState::Draining);
        assert_eq!(
            duplicate.run.stop_reason.as_deref(),
            Some("operator-stopped-now")
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
    assert_eq!(
        held.resets.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the duplicate Stop Now must not run a second cleanup"
    );
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

/// SH-774: every failed Stop Now retry rewrote the run's `updated_at`. The
/// daemon's change watcher compares whole run records, so each retry
/// published a project change that woke the next retry at once.
#[test]
fn a_repeated_failing_stop_now_does_not_rewrite_the_run_record() {
    use storyhook::service::{Clock, Ctx};
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([
        DispatcherStep::ResetFailure("first cleanup failure".into()),
        DispatcherStep::ResetFailure("second cleanup failure".into()),
    ]);
    let run = setup(&fixture, &fake, "todo");
    let at = |instant: &str| {
        Ctx::new(
            fixture.store(),
            fixture.project(),
            fixture.cwd(),
            fixture.env().clone(),
        )
        .clock(Clock::Fixed(instant.into()))
    };
    let first = at("2026-09-25T23:10:37Z");
    assert!(EngineService::new(&first, &fake).stop(&run, true).is_err());
    let recorded = fixture
        .store()
        .read(|tx| tx.engine_run(&run))
        .unwrap()
        .unwrap();
    assert_eq!(recorded.updated_at, "2026-09-25T23:10:37Z");

    let later = at("2026-09-25T23:10:38Z");
    assert!(EngineService::new(&later, &fake).stop(&run, true).is_err());
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.engine_run(&run))
            .unwrap()
            .unwrap(),
        recorded,
        "a retry that changes no run fact must not write the run record"
    );
    assert_eq!(fake.calls().len(), 2, "both attempts ran the helper");
}

/// SH-774: a graceful drain that finished first made a later Stop Now (the
/// dashboard's Abandon run) fail with "cannot `stop --now`", although a
/// finished run has idle lanes and nothing left to discard.
#[test]
fn stop_now_on_a_finished_run_returns_it_unchanged() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let engine = EngineService::new(&ctx, &fake);
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
    let drained = engine.stop(&run.id, false).unwrap();
    assert_eq!(drained.run.state, EngineRunState::Finished);
    let record = fixture.store().read(|tx| tx.engine_run(&run.id)).unwrap();

    let stopped = engine.stop(&run.id, true).unwrap();

    assert_eq!(stopped.run.state, EngineRunState::Finished);
    assert_eq!(stopped.run.stop_reason.as_deref(), Some("operator-stopped"));
    assert_eq!(
        fixture.store().read(|tx| tx.engine_run(&run.id)).unwrap(),
        record,
        "an idempotent Stop Now must not rewrite a finished run"
    );
    assert!(fake.calls().is_empty());
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

#[test]
fn stop_now_retries_after_workspace_delivery_and_retires_old_pending_authority() {
    use fs4::FileExt;
    use storyhook::store::DeliveryStatus;
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::new([DispatcherStep::Reset]);
    let run = setup(&fixture, &fake, "todo");
    let ctx = fixture.ctx();
    StoryService::new(&ctx)
        .set_awaiting("SH-1", "Current delivery owns the workspace")
        .unwrap();
    let directory = fixture.cwd().join(".git/storyhook/workspace-locks");
    std::fs::create_dir_all(&directory).unwrap();
    let owner = std::fs::File::create(directory.join("SH-1.lock")).unwrap();
    owner.lock_exclusive().unwrap();
    let engine = EngineService::new(&ctx, &fake);
    let error = engine.stop(&run, true).unwrap_err();
    assert!(error.to_string().contains("workspace is busy"), "{error}");
    assert!(
        fake.calls().is_empty(),
        "a contended reset must not call its actuator"
    );
    assert!(
        fixture
            .store()
            .read(|tx| tx.engine_reset(fixture.project(), StoryNo::new(1)))
            .unwrap()
            .is_some()
    );
    drop(owner);
    engine.stop(&run, true).unwrap();
    assert!(
        fixture
            .store()
            .read(|tx| tx.block_deliveries(fixture.project()))
            .unwrap()
            .iter()
            .all(|d| d.status == DeliveryStatus::Superseded)
    );
}

#[path = "engine_reset/quiescent.rs"]
mod quiescent;

/// SH-774 incident replay (run 22e4276a, 2026-09-25). Stop Now arrives while
/// the engine is still dispatching and waits for that dispatch. The operator
/// presses Abandon run meanwhile, which sends a duplicate Stop Now. The
/// dispatch is then refused: the lane is quarantined with its story claimed
/// and no cleanup lease. The duplicate must succeed, and the first Stop Now
/// must finish the run without inventing cleanup for the refused story.
#[test]
fn stop_now_during_a_dispatch_that_is_then_refused_finishes_the_run() {
    use std::sync::{Mutex, mpsc};
    use std::time::{Duration, Instant};
    use storyhook::error::AppError;
    use storyhook::lane_budget::WindowCensus;
    use storyhook::service::engine::{
        DISPATCH_TIMEOUT, DispatchRequest, Dispatcher, UnclaimRequest, WindowProbe,
    };
    use storyhook::store::EngineReset;

    const REFUSAL: &str = "could not confirm Codex is running in window `SH-1`";
    struct Refusing {
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl Dispatcher for Refusing {
        fn dispatch(&self, _: DispatchRequest) -> Result<DispatchOutcome, AppError> {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(DISPATCH_TIMEOUT)
                .unwrap();
            Ok(DispatchOutcome::from_payload(
                serde_json::json!({"ok": false, "display": REFUSAL}),
            ))
        }
        fn unclaim(&self, _: UnclaimRequest) -> Result<DispatchOutcome, AppError> {
            panic!("Stop Now must not unclaim")
        }
        fn kill_window(&self, _: &str) -> Result<(), AppError> {
            panic!("no window is proven owned")
        }
        fn census(&self) -> WindowCensus {
            WindowCensus::Counted { windows: vec![] }
        }
        fn probe_window(&self, _: &str) -> WindowProbe {
            panic!("the only lane is dispatching, so nothing is probed")
        }
        fn reset(
            &self,
            _: EngineReset,
            _: std::os::fd::BorrowedFd<'_>,
        ) -> Result<DispatchOutcome, AppError> {
            panic!("a refused dispatch has no lease, so nothing may be reset")
        }
    }

    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Refused at dispatch".into(),
            ..NewStoryInput::default()
        })
        .unwrap();
    let (entered, dispatching) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let refusing = Refusing {
        entered,
        release: Mutex::new(released),
    };
    let engine = EngineService::new(&ctx, &refusing);
    let run = engine
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 1,
            agent: EngineAgent::Codex,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap()
        .id;
    std::thread::scope(|scope| {
        let filler = scope.spawn(|| engine.reconcile(&run));
        dispatching.recv_timeout(DISPATCH_TIMEOUT).unwrap();
        let owner = scope.spawn(|| engine.stop(&run, true));
        let deadline = Instant::now() + Duration::from_secs(30);
        while engine.status(Some(&run)).unwrap().pop().unwrap().run.state
            != EngineRunState::Draining
        {
            assert!(Instant::now() < deadline, "Stop Now never recorded");
            std::thread::sleep(Duration::from_millis(10));
        }

        let duplicate = engine.stop(&run, true).unwrap();
        assert_eq!(duplicate.run.state, EngineRunState::Draining);

        release.send(()).unwrap();
        filler.join().unwrap().unwrap();
        let stopped = owner.join().unwrap().unwrap();
        assert_eq!(stopped.run.state, EngineRunState::Finished);
        assert_eq!(stopped.lanes[0].state, EngineLaneState::Idle);
    });
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(
        row.state, "in-progress",
        "the refused story keeps its claim"
    );
    assert!(
        row.awaiting.as_deref().unwrap().contains(REFUSAL),
        "the dispatch refusal stays the diagnosis: {:?}",
        row.awaiting
    );
    assert!(
        row.snapshot
            .comments
            .last()
            .unwrap()
            .text
            .contains("no cleanup lease")
    );
}
