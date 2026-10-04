//! Restoration changes physical bindings without reclaiming or redispatching.
use serde_json::{Value, json};
use std::os::fd::BorrowedFd;
use std::sync::atomic::{AtomicUsize, Ordering};
use storyhook::domain::{StoryCleanupLease, TmuxCleanupTarget};
use storyhook::error::AppError;
use storyhook::lane_budget::WindowCensus;
use storyhook::service::engine::{
    DispatchOutcome, DispatchRequest, Dispatcher, EngineService, StartRequest, UnclaimRequest,
    WindowProbe,
};
use storyhook::service::{Ctx, NewStoryInput, StoryService};
use storyhook::store::{
    EngineAgent, EngineLaneRecord, EngineScope, ReadOps, SqliteStore, Store, WriteOps,
};
use storyhook_test_support::{DispatcherStep, FakeDispatcher, ServiceFixture};

struct Restorer<'a> {
    ctx: &'a Ctx<'a, SqliteStore>,
    proposal: Value,
    race: u8,
    publications: AtomicUsize,
    watches: AtomicUsize,
}
impl Dispatcher for Restorer<'_> {
    fn dispatch(&self, _: DispatchRequest) -> Result<DispatchOutcome, AppError> {
        panic!("restoration must not dispatch");
    }
    fn unclaim(&self, _: UnclaimRequest) -> Result<DispatchOutcome, AppError> {
        panic!("restoration must retain claim");
    }
    fn kill_window(&self, _: &str) -> Result<(), AppError> {
        panic!("restoration must retain pane");
    }
    fn census(&self) -> WindowCensus {
        WindowCensus::Counted { windows: vec![] }
    }
    fn probe_window(&self, _: &str) -> WindowProbe {
        panic!("successful restoration already proved liveness");
    }
    fn restore_lane(
        &self,
        lane: &EngineLaneRecord,
        expected: Option<&Value>,
        workspace: Option<BorrowedFd<'_>>,
        _deadline: std::time::Instant,
    ) -> Result<Option<Value>, AppError> {
        if let Some(expected) = expected {
            assert_eq!(expected, &self.proposal);
            assert!(workspace.is_some());
            self.publications.fetch_add(1, Ordering::SeqCst);
            if self.race == 2 {
                StoryService::new(self.ctx)
                    .comment(lane.story_id.as_deref().unwrap(), "concurrent correction")
                    .unwrap();
            }
            if self.race == 3 {
                self.ctx.store().write(|tx| {
                    let mut request = tx.continuations(self.ctx.project())?[0].clone();
                    let revision = request.revision;
                    request.revision += 1;
                    request.detail = "concurrent receipt".into();
                    assert!(tx.update_continuation(&request, revision)?);
                    Ok(())
                })?;
            }
            if self.race == 1 {
                self.ctx.store().write(|tx| {
                    let mut fresh = lane.clone();
                    fresh.last_progress_at = Some("2030-01-01T00:00:00Z".into());
                    tx.put_engine_lane(&fresh)
                })?;
            }
        }
        Ok(Some(self.proposal.clone()))
    }
    fn rearm_restored_lane(
        &self,
        _: &EngineLaneRecord,
        _: &Value,
        _: BorrowedFd<'_>,
        _: std::time::Instant,
    ) -> Result<(), AppError> {
        self.watches.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn restored_lane_preserves_claim_and_progress_and_loses_a_concurrent_cas_safely() {
    for race in [0, 1, 2, 3] {
        let f = ServiceFixture::new();
        let path = f.cwd().canonicalize().unwrap();
        assert!(
            storyhook::env::git_env::command(&path)
                .args(["init", "-b", "main"])
                .output()
                .unwrap()
                .status
                .success()
        );
        f.store()
            .write(|tx| tx.set_checkout_path(f.project(), Some(&path)))
            .unwrap();
        let ctx = f.ctx();
        let id = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: "retain session".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        let lease = StoryCleanupLease {
            version: 1,
            project_slug: "fixture".into(),
            story_id: id.clone(),
            repository_path: path.clone(),
            worktree_path: path.clone(),
            branch: "worktree-SH-1".into(),
            tmux: TmuxCleanupTarget {
                socket_path: "/tmp/old".into(),
                revivify: None,
            },
        };
        let dispatch = FakeDispatcher::new([DispatcherStep::Dispatch(
            DispatchOutcome::from_payload(
                json!({"ok":true,"pane":"%1","window_name":id,"worktree_path":path,"cleanup_lease":lease}),
            ),
        )]);
        let run = EngineService::new(&ctx, &dispatch)
            .start(StartRequest {
                scope: EngineScope::Project,
                lanes: 1,
                agent: EngineAgent::Codex,
                model: None,
                effort: None,
                speed: None,
            })
            .unwrap();
        EngineService::new(&ctx, &dispatch)
            .reconcile(&run.id)
            .unwrap();
        f.store()
            .write(|tx| {
                let mut lane = tx.engine_lanes(&run.id)?[0].clone();
                lane.last_progress_seq =
                    Some(tx.stories(f.project(), &Default::default())?[0].head_global_seq);
                lane.last_progress_at = Some(storyhook_test_support::FIXTURE_NOW.into());
                tx.put_engine_lane(&lane)
            })
            .unwrap();
        let old = f
            .store()
            .read(|tx| Ok(tx.engine_lanes(&run.id)?[0].clone()))
            .unwrap();
        let stories_before = f
            .store()
            .read(|tx| tx.stories(f.project(), &Default::default()))
            .unwrap();
        let capture = json!({"provider":"codex","session_id":"conversation","transcript_path":"/transcript","lease":lease,"socket":"/tmp/old","pane":"%1","pid":7,"window":"@1","started":"old","head":"head","fingerprint":"dirty","turn_id":"turn","message_id":"message"});
        let request = storyhook::store::Continuation {
            id: "restore-request".into(),
            project_id: f.project(),
            story_no: stories_before[0].story_no,
            story_id: id.clone(),
            handoff: json!({"evidence":"retained"}),
            generation: json!({"provider":"codex","session_id":"conversation","turn_id":"turn","message_id":"message"}),
            capture,
            status: storyhook::store::ContinuationStatus::Acknowledged,
            phase: storyhook::store::ContinuationPhase::Complete,
            revision: 0,
            attempts: 2,
            created_at: storyhook_test_support::FIXTURE_NOW.into(),
            updated_at: storyhook_test_support::FIXTURE_NOW.into(),
            detail: "acknowledged".into(),
            reviewed_seq: Some(stories_before[0].head_global_seq.get()),
            reviewed_head: Some("head".into()),
        };
        f.store()
            .write(|tx| tx.insert_continuation(&request))
            .unwrap();
        let mut new = lease.clone();
        new.tmux.socket_path = "/tmp/new".into();
        let restorer = Restorer {
            ctx: &ctx,
            proposal: json!({"common":path.join(".git"),"lease_before":lease,"lease":new,"pane":"%8","window":"@9","metadata":{"provider":"codex","session_id":"conversation","transcript_path":"/transcript","socket":"/tmp/new","pane":"%8","pid":8,"window":"@9","started":"new"}}),
            race,
            publications: AtomicUsize::new(0),
            watches: AtomicUsize::new(0),
        };
        let report = EngineService::new(&ctx, &restorer)
            .reconcile(&run.id)
            .unwrap();
        assert!(report.filled.is_empty() && report.quarantined.is_empty());
        let fresh = f
            .store()
            .read(|tx| Ok(tx.engine_lanes(&run.id)?[0].clone()))
            .unwrap();
        assert_eq!(fresh.story_id, old.story_id);
        assert_eq!(fresh.dispatched_at, old.dispatched_at);
        let stories_after = f
            .store()
            .read(|tx| tx.stories(f.project(), &Default::default()))
            .unwrap();
        if race == 2 {
            assert!(stories_after[0].head_global_seq > stories_before[0].head_global_seq);
        } else {
            assert_eq!(stories_after, stories_before);
        }
        assert_eq!(fresh.last_progress_seq, old.last_progress_seq);
        assert_eq!(restorer.publications.load(Ordering::SeqCst), 1);
        let rebound = f
            .store()
            .read(|tx| Ok(tx.continuations(f.project())?[0].clone()))
            .unwrap();
        assert_eq!(rebound.generation, request.generation);
        assert_eq!(rebound.handoff, request.handoff);
        assert_eq!(rebound.status, request.status);
        assert_eq!(rebound.phase, request.phase);
        assert_eq!(rebound.attempts, request.attempts);
        for key in ["head", "fingerprint", "turn_id", "message_id"] {
            assert_eq!(rebound.capture[key], request.capture[key]);
        }
        if race == 0 {
            assert_eq!(rebound.capture["socket"], "/tmp/new");
            assert_eq!(rebound.revision, 1);
        } else {
            assert_eq!(rebound.capture["socket"], "/tmp/old");
        }
        if race == 3 {
            assert_eq!(rebound.revision, 1);
            assert_eq!(rebound.detail, "concurrent receipt");
        }
        if race != 0 {
            assert_eq!(fresh.cleanup_lease, old.cleanup_lease);
            if race == 1 {
                assert_eq!(
                    fresh.last_progress_at.as_deref(),
                    Some("2030-01-01T00:00:00Z")
                );
            } else {
                assert_eq!(fresh.last_progress_at, old.last_progress_at);
            }
            assert_eq!(restorer.watches.load(Ordering::SeqCst), 0);
        } else {
            assert_eq!(fresh.cleanup_lease, Some(new));
            assert_eq!(fresh.pane_id.as_deref(), Some("%8"));
            assert_eq!(fresh.last_progress_at, old.last_progress_at);
            assert_eq!(restorer.watches.load(Ordering::SeqCst), 1);
        }
    }
}
