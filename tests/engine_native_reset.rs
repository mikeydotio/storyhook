//! SH-890: native Stop Now shares real teardown and retains restart evidence.
#[allow(dead_code)]
#[path = "story_reset/workspace.rs"]
mod workspace;
use storyhook::domain::{CLEANUP_LEASE_VERSION, StoryCleanupLease, TmuxCleanupTarget};
use storyhook::error::AppError;
use storyhook::lane_budget::WindowCensus;
use storyhook::service::engine::{
    DispatchOutcome, DispatchRequest, Dispatcher, EngineService, StartRequest, UnclaimRequest,
    WindowProbe,
};
use storyhook::service::{Ctx, StoryService};
use storyhook::store::{
    EngineAgent, EngineLaneState, EngineRunState, EngineScope, ReadOps, Store, StoryNo, WriteOps,
};
use workspace::{Workspace, commit, git};

struct Native;
impl Dispatcher for Native {
    fn dispatch(&self, _: DispatchRequest) -> Result<DispatchOutcome, AppError> {
        panic!("Stop Now must not dispatch")
    }
    fn unclaim(&self, _: UnclaimRequest) -> Result<DispatchOutcome, AppError> {
        panic!("ordinary lane must not unclaim")
    }
    fn probe_window(&self, _: &str) -> WindowProbe {
        panic!("stopping must not probe ordinary liveness")
    }
    fn kill_window(&self, _: &str) -> Result<(), AppError> {
        panic!("shared native cleanup owns window removal")
    }
    fn census(&self) -> WindowCensus {
        WindowCensus::Counted { windows: vec![] }
    }
}

fn setup(w: &Workspace) -> String {
    let ctx = w.fixture.ctx().no_hooks(true);
    let engine = EngineService::new(&ctx, &Native);
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
    let lease = StoryCleanupLease {
        version: CLEANUP_LEASE_VERSION,
        project_slug: "fixture".into(),
        story_id: w.id.clone(),
        repository_path: w.repo.clone(),
        worktree_path: w.worktree.clone(),
        branch: "worktree-SH-1".into(),
        tmux: TmuxCleanupTarget {
            revivify: None,
            socket_path: w.repo.join("absent-tmux-socket"),
        },
    };
    let private = git(&w.worktree, &["rev-parse", "--absolute-git-dir"]);
    std::fs::write(
        std::path::Path::new(private.trim()).join(storyhook::domain::CLEANUP_LEASE_MARKER),
        serde_json::to_vec(&lease).unwrap(),
    )
    .unwrap();
    w.fixture
        .store()
        .write(|tx| {
            let mut lane = tx.engine_lanes(&run.id)?.remove(0);
            lane.state = EngineLaneState::Working;
            lane.story_id = Some(w.id.clone());
            lane.window_name = Some(w.id.clone());
            lane.cleanup_lease = Some(lease);
            tx.put_engine_lane(&lane)
        })
        .unwrap();
    run.id
}

fn receipt(w: &Workspace, run: &str) -> serde_json::Value {
    let lanes = w.fixture.store().read(|tx| tx.engine_lanes(run)).unwrap();
    serde_json::from_str(lanes[0].outcome_detail.as_deref().unwrap()).unwrap()
}

#[test]
fn sh890_native_stop_removes_dirty_work_and_keeps_recovery_without_a_shell_helper() {
    let w = Workspace::new(true);
    std::fs::write(w.worktree.join("tracked"), "committed").unwrap();
    git(&w.worktree, &["add", "tracked"]);
    commit(&w.worktree, "private story work");
    let tip = git(&w.worktree, &["rev-parse", "HEAD"]);
    std::fs::write(w.worktree.join("tracked"), "dirty").unwrap();
    std::fs::write(w.worktree.join("untracked"), "discard").unwrap();
    let run = setup(&w);
    let ctx = w.fixture.ctx().no_hooks(true);
    StoryService::new(&ctx)
        .set_awaiting(&w.id, "old hold")
        .unwrap();
    let done = EngineService::new(&ctx, &Native).stop(&run, true).unwrap();
    assert_eq!(done.run.state, EngineRunState::Finished);
    assert!(!w.worktree.exists());
    assert!(!w.branch_exists("worktree-SH-1"));
    assert_eq!(w.story().state, "todo");
    assert!(w.story().awaiting.is_none());
    let result = receipt(&w, &run);
    assert_eq!(result["native_reset"], 1);
    assert_eq!(result["cleanup"]["recovery"]["tip"], tip.trim());
    assert_eq!(result["cleanup"]["recovery"]["dirty"], 1);
    assert_eq!(result["cleanup"]["recovery"]["untracked"], 1);
    assert_eq!(result["cleanup"]["recovery"]["unpushed"], 1);
    assert_eq!(
        result["cleanup"]["recovery"]["cleared_awaiting"],
        "old hold"
    );
    assert!(w.last_comment().contains(tip.trim()));
    assert!(
        w.fixture
            .store()
            .read(|tx| tx.story_reset(w.fixture.project(), StoryNo::new(1)))
            .unwrap()
            .is_none()
    );
}

struct LostAck;
impl Dispatcher for LostAck {
    fn dispatch(&self, r: DispatchRequest) -> Result<DispatchOutcome, AppError> {
        Native.dispatch(r)
    }
    fn unclaim(&self, r: UnclaimRequest) -> Result<DispatchOutcome, AppError> {
        Native.unclaim(r)
    }
    fn probe_window(&self, w: &str) -> WindowProbe {
        Native.probe_window(w)
    }
    fn kill_window(&self, w: &str) -> Result<(), AppError> {
        Native.kill_window(w)
    }
    fn census(&self) -> WindowCensus {
        Native.census()
    }
    fn reset(
        &self,
        _: storyhook::store::EngineReset,
        _: std::os::fd::BorrowedFd<'_>,
        native: &mut dyn FnMut() -> Result<DispatchOutcome, AppError>,
    ) -> Result<DispatchOutcome, AppError> {
        native()?;
        Err(AppError::Storage(
            "lost acknowledgement after native removal".into(),
        ))
    }
}

#[test]
fn sh890_retry_preserves_recovery_and_holds_replacement_without_replaying_removal() {
    let w = Workspace::new(true);
    commit(&w.worktree, "recover this commit");
    let tip = git(&w.worktree, &["rev-parse", "HEAD"]);
    let run = setup(&w);
    let ctx = w.fixture.ctx().no_hooks(true);
    assert!(EngineService::new(&ctx, &LostAck).stop(&run, true).is_err());
    let persisted = w
        .fixture
        .store()
        .read(|tx| tx.engine_reset(w.fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert!(persisted.cleanup.as_ref().unwrap().completed);
    assert!(!w.worktree.exists());
    git(
        &w.repo,
        &[
            "worktree",
            "add",
            "-b",
            "worktree-SH-1",
            w.worktree.to_str().unwrap(),
            "main",
        ],
    );
    std::fs::write(w.worktree.join("replacement"), "preserve me").unwrap();
    let reopened = storyhook::store::SqliteStore::open(w.fixture.store().path()).unwrap();
    let retry_ctx = Ctx::new(
        &reopened,
        w.fixture.project(),
        w.fixture.cwd(),
        w.fixture.env().clone(),
    )
    .no_hooks(true);
    let done = EngineService::new(&retry_ctx, &Native)
        .stop(&run, true)
        .unwrap();
    assert_eq!(done.run.state, EngineRunState::Finished);
    assert_eq!(
        std::fs::read_to_string(w.worktree.join("replacement")).unwrap(),
        "preserve me"
    );
    assert!(w.branch_exists("worktree-SH-1"));
    assert!(w.story().awaiting.is_some());
    let result = receipt(&w, &run);
    assert_eq!(result["cleanup"]["recovery"]["tip"], tip.trim());
    assert_eq!(result["token"], persisted.token);
    assert!(!result["cleanup"]["residue"].as_array().unwrap().is_empty());
}

#[test]
fn sh890_accepted_origin_survives_reopen_before_any_lane_reservation() {
    use fs4::FileExt;
    let w = Workspace::new(true);
    let run = setup(&w);
    let key: String = run.bytes().map(|b| format!("{b:02x}")).collect();
    let path = w
        .fixture
        .env()
        .store_path()
        .with_extension(format!("reset-{key}.lock"));
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    lock.try_lock_exclusive().unwrap();
    let caller = storyhook::service::reset::ResetCaller {
        pane: Some("%original".into()),
        socket: Some(w.repo.join("original-socket")),
    };
    let ctx = w.fixture.ctx().no_hooks(true);
    let result = EngineService::new(&ctx, &Native)
        .with_reset_caller(caller.clone())
        .stop(&run, true)
        .unwrap();
    assert_eq!(result.run.state, EngineRunState::Draining);
    assert!(
        w.fixture
            .store()
            .read(|tx| tx.engine_reset(w.fixture.project(), StoryNo::new(1)))
            .unwrap()
            .is_none()
    );
    drop(lock);
    let reopened = storyhook::store::SqliteStore::open(w.fixture.store().path()).unwrap();
    let saved = reopened
        .read(|tx| tx.engine_run(&run))
        .unwrap()
        .unwrap()
        .stop_origin
        .unwrap();
    assert_eq!(saved.caller, caller);
    assert_eq!(saved.cwd, Some(w.fixture.cwd().canonicalize().unwrap()));
    // A background context must not replace the accepted caller protections.
    let retry_ctx = Ctx::new(
        &reopened,
        w.fixture.project(),
        &w.worktree,
        w.fixture.env().clone(),
    )
    .no_hooks(true);
    let done = EngineService::new(&retry_ctx, &Native)
        .reconcile(&run)
        .unwrap();
    assert_eq!(done.run_state, EngineRunState::Finished);
    let result = receipt(&w, &run);
    assert_eq!(result["cleanup"]["origin"]["caller"]["pane"], "%original");
}

#[test]
fn sh890_legacy_stop_intent_never_adopts_background_caller_authority() {
    let w = Workspace::new(true);
    std::fs::write(w.worktree.join("keep"), "retained dirty work").unwrap();
    let run = setup(&w);
    // Model a pre-upgrade accepted intent: its durable run has no origin.
    // Background reconciliation must not invent authority from its own cwd.
    w.fixture
        .store()
        .write(|tx| {
            let mut row = tx.engine_run(&run)?.unwrap();
            row.state = EngineRunState::Draining;
            row.stop_reason = Some("operator-stopped-now".into());
            row.stop_origin = None;
            tx.update_engine_run(&row)
        })
        .unwrap();
    let ctx = w.fixture.ctx().no_hooks(true);
    let done = EngineService::new(&ctx, &Native).reconcile(&run).unwrap();
    assert_eq!(done.run_state, EngineRunState::Finished);
    assert!(w.worktree.exists());
    assert!(w.branch_exists("worktree-SH-1"));
    assert!(w.story().awaiting.is_some());
    assert_eq!(
        std::fs::read_to_string(w.worktree.join("keep")).unwrap(),
        "retained dirty work"
    );
    assert!(
        w.last_comment()
            .contains("Recorded before reset: 0 changed and 1 untracked paths")
    );
    assert!(!w.last_comment().contains("Discarded"));
    let result = receipt(&w, &run);
    assert!(result["cleanup"]["origin"].is_null());
    assert!(
        result["cleanup"]["residue"]
            .to_string()
            .contains("predates durable caller identity")
    );
}

struct Supersede<'a>(&'a Workspace);
impl Dispatcher for Supersede<'_> {
    fn dispatch(&self, r: DispatchRequest) -> Result<DispatchOutcome, AppError> {
        Native.dispatch(r)
    }
    fn unclaim(&self, r: UnclaimRequest) -> Result<DispatchOutcome, AppError> {
        Native.unclaim(r)
    }
    fn probe_window(&self, w: &str) -> WindowProbe {
        Native.probe_window(w)
    }
    fn kill_window(&self, w: &str) -> Result<(), AppError> {
        Native.kill_window(w)
    }
    fn census(&self) -> WindowCensus {
        Native.census()
    }
    fn reset(
        &self,
        _: storyhook::store::EngineReset,
        _: std::os::fd::BorrowedFd<'_>,
        native: &mut dyn FnMut() -> Result<DispatchOutcome, AppError>,
    ) -> Result<DispatchOutcome, AppError> {
        let ctx = self.0.fixture.ctx().no_hooks(true);
        storyhook::service::story_reset::StoryResetService::new(&ctx)
            .reserve(&self.0.id, &self.0.id)
            .unwrap();
        native()
    }
}

#[test]
fn sh890_story_reset_supersedes_native_teardown_without_recreating_its_owner() {
    let w = Workspace::new(true);
    let run = setup(&w);
    let ctx = w.fixture.ctx().no_hooks(true);
    let dispatcher = Supersede(&w);
    let result = EngineService::new(&ctx, &dispatcher)
        .stop(&run, true)
        .unwrap();
    assert_eq!(result.run.state, EngineRunState::Draining);
    assert!(w.worktree.exists());
    assert!(w.branch_exists("worktree-SH-1"));
    assert!(
        w.fixture
            .store()
            .read(|tx| tx.engine_reset(w.fixture.project(), StoryNo::new(1)))
            .unwrap()
            .is_none()
    );
    let owner = w
        .fixture
        .store()
        .read(|tx| tx.story_reset(w.fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert!(!owner.completed);
    assert_eq!(w.story().state, "in-progress");
    assert!(
        !w.story()
            .snapshot
            .comments
            .iter()
            .any(|c| c.text.contains("Stop Now discarded"))
    );
}

#[test]
fn sh890_native_multiple_lanes_preserve_verifying_and_closed_story_resources() {
    let w = Workspace::new(true);
    let run = setup(&w);
    let ctx = w.fixture.ctx().no_hooks(true);
    let mut excluded = Vec::new();
    for state in ["verifying", "done"] {
        let story = StoryService::new(&ctx)
            .create(&storyhook::service::NewStoryInput {
                title: format!("keep {state}"),
                state: Some("todo".into()),
                ..Default::default()
            })
            .unwrap();
        let story = StoryService::new(&ctx)
            .set_state(&story.id, state, None, Some("todo"), None)
            .unwrap();
        let branch = format!("worktree-{}", story.id);
        let path = w.repo.join(".codex/worktrees").join(&story.id);
        git(
            &w.repo,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                path.to_str().unwrap(),
                "main",
            ],
        );
        std::fs::write(path.join("keep"), state).unwrap();
        excluded.push((story, branch, path));
    }
    w.fixture
        .store()
        .write(|tx| {
            let mut record = tx.engine_run(&run)?.unwrap();
            record.lanes = 3;
            tx.update_engine_run(&record)?;
            let template = tx.engine_lanes(&run)?.remove(0);
            for (index, (story, branch, path)) in excluded.iter().enumerate() {
                let mut lane = template.clone();
                lane.lane_index = u32::try_from(index + 1).unwrap();
                lane.story_id = Some(story.id.clone());
                lane.window_name = Some(story.id.clone());
                let lease = lane.cleanup_lease.as_mut().unwrap();
                lease.story_id = story.id.clone();
                lease.branch = branch.clone();
                lease.worktree_path = path.clone();
                tx.put_engine_lane(&lane)?;
            }
            Ok(())
        })
        .unwrap();
    let result = EngineService::new(&ctx, &Native).stop(&run, true).unwrap();
    assert_eq!(result.run.state, EngineRunState::Finished);
    assert!(
        result
            .lanes
            .iter()
            .all(|lane| lane.state == EngineLaneState::Idle)
    );
    assert!(!w.worktree.exists());
    for (story, branch, path) in excluded {
        assert!(w.branch_exists(&branch));
        assert_eq!(
            std::fs::read_to_string(path.join("keep")).unwrap(),
            story.state
        );
        let number = StoryNo::parse_id("SH", &story.id).unwrap();
        assert_eq!(
            w.fixture
                .store()
                .read(|tx| tx.story(w.fixture.project(), number))
                .unwrap()
                .unwrap()
                .state,
            story.state
        );
    }
}

#[test]
fn sh890_legacy_reset_json_without_progress_is_compatible() {
    let w = Workspace::new(true);
    let run = setup(&w);
    let ctx = w.fixture.ctx().no_hooks(true);
    assert!(EngineService::new(&ctx, &LostAck).stop(&run, true).is_err());
    let reset = w
        .fixture
        .store()
        .read(|tx| tx.engine_reset(w.fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    let mut legacy = serde_json::to_value(&reset).unwrap();
    legacy.as_object_mut().unwrap().remove("cleanup");
    let decoded: storyhook::store::EngineReset = serde_json::from_value(legacy.clone()).unwrap();
    assert!(decoded.cleanup.is_none());
    assert_eq!(decoded.token, reset.token);
    assert_eq!(decoded.lease, reset.lease);
    assert_eq!(serde_json::to_value(decoded).unwrap(), legacy);
}

#[test]
fn sh890_missing_original_marker_preserves_resources_as_residue() {
    let w = Workspace::new(true);
    let run = setup(&w);
    let private = git(&w.worktree, &["rev-parse", "--absolute-git-dir"]);
    std::fs::remove_file(
        std::path::Path::new(private.trim()).join(storyhook::domain::CLEANUP_LEASE_MARKER),
    )
    .unwrap();
    let ctx = w.fixture.ctx().no_hooks(true);
    let done = EngineService::new(&ctx, &Native).stop(&run, true).unwrap();
    assert_eq!(done.run.state, EngineRunState::Finished);
    assert!(w.worktree.exists());
    assert!(w.branch_exists("worktree-SH-1"));
    assert!(w.story().awaiting.is_some());
    assert!(
        receipt(&w, &run)["cleanup"]["residue"]
            .to_string()
            .contains("marker")
    );
}

#[test]
fn sh890_schema57_stop_intent_upgrades_without_inventing_caller_authority() {
    let root = storyhook_test_support::scratch_dir();
    let store = storyhook::store::SqliteStore::open(root.path().join("old.db")).unwrap();
    store
        .migrate_with(&storyhook::store::MIGRATIONS[..57])
        .unwrap();
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    conn.execute("INSERT INTO engine_runs(id,project_slug,scope_kind,lanes,agent,state,stop_reason,created_at,updated_at) VALUES ('old-run','old-project','project',1,'codex','draining','operator-stopped-now','then','then')", []).unwrap();
    drop(conn);
    let migrated = store.migrate().unwrap();
    assert_eq!(migrated.from_version, 57);
    let reopened = storyhook::store::SqliteStore::open(store.path()).unwrap();
    let run = reopened
        .read(|tx| tx.engine_run("old-run"))
        .unwrap()
        .unwrap();
    assert!(run.is_stopping());
    assert!(run.stop_origin.is_none());
    assert_eq!(run.created_at, "then");
    assert!(reopened.migrate().unwrap().applied.is_empty());
}

#[test]
fn sh890_legacy_stop_wire_defaults_caller_without_changing_its_encoding() {
    let old = serde_json::json!({"Stop":{"run":"old-run","now":true}});
    let decoded: storyhook::cli::EngineAction = serde_json::from_value(old.clone()).unwrap();
    assert!(
        matches!(&decoded, storyhook::cli::EngineAction::Stop { caller, .. } if caller.is_empty())
    );
    assert_eq!(serde_json::to_value(decoded).unwrap(), old);
}
