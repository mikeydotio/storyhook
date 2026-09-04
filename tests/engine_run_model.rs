//! The Full Auto engine's durable operational model (SH-462).
//!
//! Runs and lanes are intentionally outside the story event fold. This suite
//! proves both halves: their own schema is strict, and their presence cannot
//! perturb the read-model oracle `story doctor` relies on.

mod store_support;

use std::path::Path;
use std::sync::{Arc, Barrier};

use rusqlite::{Connection, params};
use store_support::{create_story, new_store, raw, seed_project};
use storyhook::domain::TypeDef;
use storyhook::error::AppError;
use storyhook::service::engine::{
    DispatchOutcome, EngineService, OPERATOR_STOPPED, OPERATOR_STOPPED_NOW, StartRequest,
};
use storyhook::service::{ConfigService, NewStoryInput, StoryService};
use storyhook::store::migrate;
use storyhook::store::{
    EngineAgent, EngineLaneRecord, EngineLaneState, EngineQuarantineRecord, EngineRunRecord,
    EngineRunState, EngineScope, NewProject, ReadOps, SqliteStore, Store, StoreError, WriteOps,
    diff_read_model,
};
use storyhook_test_support::{
    DispatcherCall, DispatcherStep, FIXTURE_NOW, FakeDispatcher, ServiceFixture, scratch_dir,
};

fn run(id: &str, project_slug: &str, state: EngineRunState) -> EngineRunRecord {
    EngineRunRecord {
        id: id.into(),
        project_slug: project_slug.into(),
        scope: EngineScope::Project,
        lanes: 2,
        agent: EngineAgent::Codex,
        state,
        consecutive_hard_stops: 0,
        recent_quarantines: Vec::new(),
        stop_reason: None,
        acknowledged_at: None,
        created_at: "2026-08-29T20:00:00Z".into(),
        updated_at: "2026-08-29T20:00:00Z".into(),
    }
}

fn lane(run_id: &str, lane_index: u32) -> EngineLaneRecord {
    EngineLaneRecord {
        run_id: run_id.into(),
        lane_index,
        state: EngineLaneState::Idle,
        story_id: None,
        pane_id: None,
        window_name: None,
        worktree_path: None,
        dispatched_at: None,
        last_observed_at: "2026-08-29T20:00:00Z".into(),
        last_progress_seq: None,
        last_progress_at: None,
        outcome: None,
        outcome_detail: None,
    }
}

fn insert_raw_run(
    conn: &Connection,
    id: &str,
    scope_kind: &str,
    scope_story_id: Option<&str>,
    lanes: i64,
    agent: &str,
    state: &str,
) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO engine_runs \
             (id, project_slug, scope_kind, scope_story_id, lanes, agent, state, \
              consecutive_hard_stops, created_at, updated_at) \
         VALUES (?1, 'alpha', ?2, ?3, ?4, ?5, ?6, 0, '2026-08-29T20:00:00Z', \
                 '2026-08-29T20:00:00Z')",
        params![id, scope_kind, scope_story_id, lanes, agent, state],
    )
}

fn rejected(result: rusqlite::Result<usize>, rule: &str) {
    let error = result.expect_err("the schema must reject this row");
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "{rule} should be a CHECK refusal, got: {error}"
    );
}

#[test]
fn migration_24_applies_forward_without_touching_story_data() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..23]).unwrap();
    let project = seed_project(&store, "alpha", "SH");
    let story = create_story(&store, project, "Already here", "2026-08-29T19:00:00Z");

    // Bounded to 24 rather than `store.migrate()`. This test is about what
    // migration 24 does, so it applies exactly migration 24: an unbounded
    // upgrade sweeps in every later migration too, which made this fail the
    // moment 25 existed and would have failed again on 26.
    let report = store.migrate_with(&migrate::MIGRATIONS[..24]).unwrap();

    assert_eq!(report.from_version, 23);
    assert_eq!(report.to_version, 24);
    assert_eq!(report.applied, ["engine_runs"]);
    assert_eq!(
        store
            .read(|tx| tx.story(project, story))
            .unwrap()
            .unwrap()
            .title,
        "Already here"
    );
}

#[test]
fn migration_27_preserves_live_lanes_without_inventing_a_pane_id() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..26]).unwrap();
    let conn = raw(&store);
    insert_raw_run(
        &conn,
        "run-before-pane-ids",
        "project",
        None,
        1,
        "codex",
        "running",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO engine_lanes \
             (run_id, lane_index, state, story_id, window_name, last_observed_at) \
         VALUES ('run-before-pane-ids', 0, 'working', 'AL-7', 'AL-7', \
                 '2026-08-29T20:00:00Z')",
        [],
    )
    .unwrap();
    drop(conn);

    let report = store.migrate_with(&migrate::MIGRATIONS[..27]).unwrap();

    assert_eq!(report.from_version, 26);
    assert_eq!(report.to_version, 27);
    assert_eq!(report.applied, ["engine_lane_pane_id"]);
    let lane = store
        .read(|tx| tx.engine_lanes("run-before-pane-ids"))
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(lane.window_name.as_deref(), Some("AL-7"));
    assert_eq!(lane.pane_id, None);
}

#[test]
fn migration_28_preserves_existing_runs_with_empty_quarantine_history() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..27]).unwrap();
    let conn = raw(&store);
    insert_raw_run(
        &conn,
        "run-before-history",
        "project",
        None,
        1,
        "codex",
        "running",
    )
    .unwrap();
    drop(conn);

    let report = store.migrate_with(&migrate::MIGRATIONS[..28]).unwrap();

    assert_eq!(report.from_version, 27);
    assert_eq!(report.to_version, 28);
    assert_eq!(report.applied, ["engine_recent_quarantines"]);
    assert!(
        store
            .read(|tx| tx.engine_run("run-before-history"))
            .unwrap()
            .unwrap()
            .recent_quarantines
            .is_empty()
    );
}

#[test]
fn run_and_lane_records_round_trip_update_order_and_reopen() {
    let (dir, store) = new_store();
    seed_project(&store, "beta", "BE");
    seed_project(&store, "alpha", "AL");

    let mut first = run("run-a", "alpha", EngineRunState::Finished);
    first.created_at = "2026-08-29T19:00:00Z".into();
    let second = run("run-b", "alpha", EngineRunState::Running);
    let beta = run("run-c", "beta", EngineRunState::Paused);
    store
        .write(|tx| {
            tx.create_engine_run(&second)?;
            tx.create_engine_run(&first)?;
            tx.create_engine_run(&beta)?;
            tx.put_engine_lane(&lane("run-b", 1))?;
            tx.put_engine_lane(&lane("run-b", 0))
        })
        .unwrap();

    assert_eq!(
        store
            .read(|tx| tx.engine_runs("alpha"))
            .unwrap()
            .iter()
            .map(|run| run.id.as_str())
            .collect::<Vec<_>>(),
        ["run-a", "run-b"]
    );
    assert_eq!(
        store
            .read(|tx| tx.live_engine_runs())
            .unwrap()
            .iter()
            .map(|run| run.id.as_str())
            .collect::<Vec<_>>(),
        ["run-b", "run-c"]
    );
    assert_eq!(
        store
            .read(|tx| tx.engine_lanes("run-b"))
            .unwrap()
            .iter()
            .map(|lane| lane.lane_index)
            .collect::<Vec<_>>(),
        [0, 1]
    );

    let mut updated = second.clone();
    updated.project_slug = "beta".into();
    updated.scope = EngineScope::Epic("AL-1".into());
    updated.lanes = 99;
    updated.agent = EngineAgent::Claude;
    updated.state = EngineRunState::Halted;
    updated.consecutive_hard_stops = 3;
    updated.recent_quarantines.push(EngineQuarantineRecord {
        lane_index: 1,
        story_id: Some("AL-7".into()),
        kind: "window-gone".into(),
        detail: Some("pane exited".into()),
        pane_id: Some("%112".into()),
        window_name: Some("AL-7".into()),
        worktree_path: Some("/tmp/wt/AL-7".into()),
        observed_at: "2026-08-29T20:04:00Z".into(),
    });
    updated.stop_reason = Some("breaker-tripped".into());
    updated.acknowledged_at = Some("2026-08-29T20:05:00Z".into());
    updated.updated_at = "2026-08-29T20:05:00Z".into();
    let mut working = lane("run-b", 1);
    working.state = EngineLaneState::Working;
    working.story_id = Some("AL-7".into());
    working.pane_id = Some("%112".into());
    working.window_name = Some("story-SH-462-lane-1".into());
    store
        .write(|tx| {
            tx.update_engine_run(&updated)?;
            tx.put_engine_lane(&working)
        })
        .unwrap();

    let stored = store.read(|tx| tx.engine_run("run-b")).unwrap().unwrap();
    assert_eq!(stored.project_slug, "alpha");
    assert_eq!(stored.scope, EngineScope::Project);
    assert_eq!(stored.lanes, 2);
    assert_eq!(stored.agent, EngineAgent::Codex);
    assert_eq!(stored.state, EngineRunState::Halted);
    assert_eq!(stored.consecutive_hard_stops, 3);
    assert_eq!(stored.recent_quarantines, updated.recent_quarantines);
    assert_eq!(stored.stop_reason.as_deref(), Some("breaker-tripped"));
    assert_eq!(
        store.read(|tx| tx.engine_lanes("run-b")).unwrap()[1],
        working
    );

    let path = store.path().to_path_buf();
    drop(store);
    let reopened = SqliteStore::open(path).unwrap();
    assert_eq!(
        reopened.read(|tx| tx.engine_run("run-b")).unwrap().unwrap(),
        stored
    );
    drop(reopened);
    drop(dir);
}

#[test]
fn every_run_check_is_enforced() {
    let (_dir, store) = new_store();
    let conn = raw(&store);

    rejected(
        insert_raw_run(&conn, "bad-scope", "team", None, 1, "codex", "finished"),
        "scope vocabulary",
    );
    rejected(
        insert_raw_run(
            &conn,
            "project-with-epic",
            "project",
            Some("SH-1"),
            1,
            "codex",
            "finished",
        ),
        "project scope cannot carry an epic",
    );
    rejected(
        insert_raw_run(
            &conn,
            "epic-without-id",
            "epic",
            None,
            1,
            "codex",
            "finished",
        ),
        "epic scope requires an epic",
    );
    rejected(
        insert_raw_run(&conn, "no-lanes", "project", None, 0, "codex", "finished"),
        "positive lane count",
    );
    rejected(
        insert_raw_run(&conn, "bad-agent", "project", None, 1, "gemini", "finished"),
        "agent vocabulary",
    );
    rejected(
        insert_raw_run(&conn, "bad-state", "project", None, 1, "codex", "waiting"),
        "run-state vocabulary",
    );
    insert_raw_run(
        &conn,
        "history-check",
        "project",
        None,
        1,
        "codex",
        "finished",
    )
    .unwrap();
    rejected(
        conn.execute(
            "UPDATE engine_runs SET recent_quarantines_json = '{}' WHERE id = 'history-check'",
            [],
        ),
        "recent quarantine history is an array",
    );
    rejected(
        conn.execute(
            "UPDATE engine_runs SET recent_quarantines_json = '[1,2,3,4]' \
             WHERE id = 'history-check'",
            [],
        ),
        "recent quarantine history is bounded by the breaker",
    );
}

#[test]
fn every_lane_check_is_enforced_in_both_directions() {
    let (_dir, store) = new_store();
    let conn = raw(&store);
    insert_raw_run(&conn, "run-a", "project", None, 1, "codex", "finished").unwrap();
    let insert = |index: i64, state: &str, story_id: Option<&str>| {
        conn.execute(
            "INSERT INTO engine_lanes \
                 (run_id, lane_index, state, story_id, last_observed_at) \
             VALUES ('run-a', ?1, ?2, ?3, '2026-08-29T20:00:00Z')",
            params![index, state, story_id],
        )
    };

    rejected(insert(0, "sleeping", None), "lane-state vocabulary");
    rejected(
        insert(1, "idle", Some("SH-1")),
        "idle lane cannot hold a story",
    );
    rejected(
        insert(2, "working", None),
        "non-idle lane must hold a story",
    );
}

#[test]
fn the_partial_index_allows_history_but_only_one_live_run() {
    let (_dir, store) = new_store();
    seed_project(&store, "alpha", "SH");
    seed_project(&store, "beta", "BE");
    store
        .write(|tx| {
            tx.create_engine_run(&run("finished-a", "alpha", EngineRunState::Finished))?;
            tx.create_engine_run(&run("halted-a", "alpha", EngineRunState::Halted))?;
            tx.create_engine_run(&run("live-a", "alpha", EngineRunState::Running))?;
            tx.create_engine_run(&run("live-b", "beta", EngineRunState::Paused))
        })
        .unwrap();

    let error = store
        .write(|tx| tx.create_engine_run(&run("second-live-a", "alpha", EngineRunState::Draining)))
        .unwrap_err();
    assert!(matches!(error, StoreError::Invariant(_)), "{error}");
}

#[test]
fn concurrent_live_run_creation_is_settled_by_sqlite() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    let setup = SqliteStore::open(&path).unwrap();
    setup.migrate().unwrap();
    seed_project(&setup, "alpha", "SH");
    drop(setup);

    let stores = [
        SqliteStore::open(&path).unwrap(),
        SqliteStore::open(&path).unwrap(),
    ];
    let barrier = Arc::new(Barrier::new(2));
    let results = std::thread::scope(|scope| {
        stores
            .iter()
            .zip(["racer-a", "racer-b"])
            .map(|(store, id)| {
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    store.write(|tx| {
                        tx.create_engine_run(&run(id, "alpha", EngineRunState::Running))
                    })
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>()
    });

    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(StoreError::Invariant(_))))
            .count(),
        1
    );
    assert_eq!(stores[0].read(|tx| tx.live_engine_runs()).unwrap().len(), 1);
}

#[test]
fn deleting_a_run_cascades_its_lanes() {
    let (_dir, store) = new_store();
    seed_project(&store, "alpha", "SH");
    store
        .write(|tx| {
            tx.create_engine_run(&run("run-a", "alpha", EngineRunState::Finished))?;
            tx.put_engine_lane(&lane("run-a", 0))
        })
        .unwrap();

    raw(&store)
        .execute("DELETE FROM engine_runs WHERE id = 'run-a'", [])
        .unwrap();
    assert!(
        store
            .read(|tx| tx.engine_lanes("run-a"))
            .unwrap()
            .is_empty(),
        "ON DELETE CASCADE must not leave an orphan lane"
    );
}

#[test]
fn deleting_a_project_removes_its_operational_state() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    store
        .write(|tx| {
            tx.create_engine_run(&run("run-a", "alpha", EngineRunState::Finished))?;
            tx.put_engine_lane(&lane("run-a", 0))?;
            tx.delete_project(project)?;
            Ok(())
        })
        .unwrap();

    assert!(store.read(|tx| tx.engine_runs("alpha")).unwrap().is_empty());
    assert!(
        store
            .read(|tx| tx.engine_lanes("run-a"))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn engine_state_is_outside_story_doctors_event_fold() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    create_story(
        &store,
        project,
        "Doctor still sees only stories",
        "2026-08-29T19:00:00Z",
    );
    store
        .write(|tx| {
            tx.create_engine_run(&run("run-a", "alpha", EngineRunState::Running))?;
            tx.put_engine_lane(&lane("run-a", 0))
        })
        .unwrap();

    let diff = diff_read_model(&store, project).unwrap();
    assert!(diff.is_clean(), "{}", diff.describe());
}

fn start_request(lanes: u32) -> StartRequest {
    StartRequest {
        scope: EngineScope::Project,
        lanes,
        agent: EngineAgent::Codex,
    }
}

fn ok_unclaim() -> DispatchOutcome {
    DispatchOutcome::from_payload(serde_json::json!({
        "ok": true,
        "closed_window": true,
        "worktree_status": "dirty"
    }))
}

fn occupy(fixture: &ServiceFixture, run_id: &str, index: u32, story: &str, worktree: &str) {
    let mut lane = fixture
        .store()
        .read(|tx| tx.engine_lanes(run_id))
        .unwrap()
        .into_iter()
        .find(|lane| lane.lane_index == index)
        .unwrap();
    lane.state = EngineLaneState::Working;
    lane.story_id = Some(story.to_string());
    lane.window_name = Some(format!("story-{story}"));
    lane.worktree_path = Some(worktree.to_string());
    lane.dispatched_at = Some(FIXTURE_NOW.to_string());
    fixture
        .store()
        .write(|tx| tx.put_engine_lane(&lane))
        .unwrap();
}

#[test]
fn engine_service_starts_project_and_epic_runs_with_atomic_idle_lanes() {
    let fixture = ServiceFixture::new();
    ConfigService::new(&fixture.ctx())
        .add_type("epic", None, None)
        .unwrap();
    let epic = StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "engine scope".into(),
            story_type: Some("epic".into()),
            ..NewStoryInput::default()
        })
        .unwrap();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);

    let project_run = service.start(start_request(3)).unwrap();
    assert_eq!(project_run.state, EngineRunState::Running);
    assert_eq!(project_run.created_at, FIXTURE_NOW);
    assert_eq!(project_run.updated_at, FIXTURE_NOW);
    assert_eq!(project_run.id.len(), 32);
    let view = service
        .status(Some(&project_run.id))
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(view.lanes.len(), 3);
    assert_eq!(
        view.lanes
            .iter()
            .map(|lane| lane.lane_index)
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert!(
        view.lanes
            .iter()
            .all(|lane| lane.state == EngineLaneState::Idle)
    );

    service.stop(&project_run.id, false).unwrap();
    let epic_run = service
        .start(StartRequest {
            scope: EngineScope::Epic(epic.id.clone()),
            lanes: 1,
            agent: EngineAgent::Claude,
        })
        .unwrap();
    assert_eq!(epic_run.scope, EngineScope::Epic(epic.id));
    assert_eq!(epic_run.agent, EngineAgent::Claude);
    assert_eq!(service.status(None).unwrap().len(), 2);
}

#[test]
fn engine_service_start_refusals_name_their_causes() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);

    let zero = service.start(start_request(0)).unwrap_err().to_string();
    assert!(zero.contains("between 1 and 255 lanes"), "{zero}");

    let ordinary = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "ordinary".into(),
            ..NewStoryInput::default()
        })
        .unwrap();
    let not_epic = service
        .start(StartRequest {
            scope: EngineScope::Epic(ordinary.id.clone()),
            lanes: 1,
            agent: EngineAgent::Codex,
        })
        .unwrap_err()
        .to_string();
    assert_eq!(
        not_epic,
        format!(
            "story `{}` is not an epic, so it cannot scope an engine run",
            ordinary.id
        )
    );

    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), None))
        .unwrap();
    let no_checkout = service.start(start_request(1)).unwrap_err().to_string();
    assert!(
        no_checkout.contains("project `fixture` has no checkout on this machine")
            && no_checkout.contains("story --project fixture project link checkout <path>"),
        "{no_checkout}"
    );
}

#[test]
fn duplicate_live_start_is_settled_by_the_partial_index_and_named() {
    let fixture = ServiceFixture::new();
    let barrier = Arc::new(Barrier::new(2));
    let results = std::thread::scope(|scope| {
        (0..2)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let fixture = &fixture;
                scope.spawn(move || {
                    let fake = FakeDispatcher::default();
                    let ctx = fixture.ctx();
                    barrier.wait();
                    EngineService::new(&ctx, &fake).start(start_request(1))
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>()
    });

    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let refusal = results
        .into_iter()
        .find_map(Result::err)
        .expect("one start must lose");
    assert_eq!(
        refusal.to_string(),
        "project `fixture` already has a live engine run"
    );
}

#[test]
fn status_and_controls_are_project_isolated_and_enforce_the_state_machine() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);
    let engine_run = service.start(start_request(1)).unwrap();

    let paused = service.pause(&engine_run.id).unwrap();
    assert_eq!(paused.run.state, EngineRunState::Paused);
    let error = service.pause(&engine_run.id).unwrap_err().to_string();
    assert!(error.contains("is `paused` and cannot `pause`"), "{error}");
    assert_eq!(
        service.resume(&engine_run.id).unwrap().run.state,
        EngineRunState::Running
    );

    occupy(&fixture, &engine_run.id, 0, "SH-99", "/preserved/SH-99");
    let draining = service.stop(&engine_run.id, false).unwrap();
    assert_eq!(draining.run.state, EngineRunState::Draining);
    assert_eq!(draining.run.stop_reason.as_deref(), Some(OPERATOR_STOPPED));
    let acknowledged = service.acknowledge(&engine_run.id).unwrap();
    assert_eq!(
        acknowledged.run.acknowledged_at.as_deref(),
        Some(FIXTURE_NOW)
    );
    assert_eq!(service.acknowledge(&engine_run.id).unwrap(), acknowledged);
    let resume_error = service.resume(&engine_run.id).unwrap_err().to_string();
    assert!(resume_error.contains("is `draining` and cannot `resume`"));

    let other = fixture
        .store()
        .write(|tx| {
            let project = tx.create_project(&NewProject {
                uuid: "other-uuid".into(),
                slug: "other".into(),
                name: "other".into(),
                prefix: "OT".into(),
                created_at: FIXTURE_NOW.into(),
            })?;
            tx.set_checkout_path(project, Some(Path::new("/checkouts/other")))?;
            tx.put_types(
                project,
                &[TypeDef {
                    slug: "normal".into(),
                    description: None,
                    emoji: None,
                }],
            )?;
            tx.create_engine_run(&run("other-run", "other", EngineRunState::Finished))?;
            Ok(project)
        })
        .unwrap();
    assert!(other.get() > 0);
    let other_run = "other-run".to_string();
    assert!(matches!(
        service.status(Some(&other_run)),
        Err(AppError::NotFound(_))
    ));
}

#[test]
fn immediate_stop_is_helper_backed_preserves_work_and_retries_partial_failure() {
    let fixture = ServiceFixture::new();
    let preserved = scratch_dir();
    let first_worktree = preserved.path().join("first");
    let second_worktree = preserved.path().join("second");
    std::fs::create_dir_all(&first_worktree).unwrap();
    std::fs::create_dir_all(&second_worktree).unwrap();
    std::fs::write(first_worktree.join("dirty.txt"), "keep me").unwrap();
    std::fs::write(second_worktree.join("dirty.txt"), "keep me too").unwrap();
    let refused = DispatchOutcome::from_payload(serde_json::json!({
        "ok": false,
        "reason": "unclaim-conflict",
        "display": "story is no longer claimed"
    }));
    let fake = FakeDispatcher::new([
        DispatcherStep::Unclaim(ok_unclaim()),
        DispatcherStep::Unclaim(refused),
        DispatcherStep::Unclaim(ok_unclaim()),
    ]);
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);
    let run = service.start(start_request(2)).unwrap();
    occupy(
        &fixture,
        &run.id,
        0,
        "SH-10",
        first_worktree.to_str().unwrap(),
    );
    occupy(
        &fixture,
        &run.id,
        1,
        "SH-11",
        second_worktree.to_str().unwrap(),
    );

    let error = service.stop(&run.id, true).unwrap_err().to_string();
    assert!(
        error.contains("lane 1 story `SH-11`: story is no longer claimed"),
        "{error}"
    );
    let partial = service.status(Some(&run.id)).unwrap().pop().unwrap();
    assert_eq!(partial.run.state, EngineRunState::Draining);
    assert_eq!(
        partial.run.stop_reason.as_deref(),
        Some(OPERATOR_STOPPED_NOW)
    );
    assert_eq!(partial.lanes[0].state, EngineLaneState::Idle);
    assert_eq!(partial.lanes[1].state, EngineLaneState::Working);
    assert!(first_worktree.join("dirty.txt").exists());
    assert!(second_worktree.join("dirty.txt").exists());

    let finished = service.stop(&run.id, true).unwrap();
    assert_eq!(finished.run.state, EngineRunState::Finished);
    assert!(
        finished
            .lanes
            .iter()
            .all(|lane| lane.state == EngineLaneState::Idle)
    );
    assert_eq!(
        fake.calls(),
        vec![
            DispatcherCall::Unclaim(storyhook::service::engine::UnclaimRequest {
                project: "fixture".into(),
                story: "SH-10".into(),
            }),
            DispatcherCall::Unclaim(storyhook::service::engine::UnclaimRequest {
                project: "fixture".into(),
                story: "SH-11".into(),
            }),
            DispatcherCall::Unclaim(storyhook::service::engine::UnclaimRequest {
                project: "fixture".into(),
                story: "SH-11".into(),
            }),
        ]
    );
}

#[test]
fn immediate_stop_clears_a_quarantined_lane_without_unclaiming_it() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);
    let run = service.start(start_request(1)).unwrap();
    occupy(&fixture, &run.id, 0, "SH-12", "/preserved/SH-12");
    let mut lane = fixture
        .store()
        .read(|tx| tx.engine_lanes(&run.id))
        .unwrap()
        .pop()
        .unwrap();
    lane.state = EngineLaneState::Quarantined;
    lane.outcome = Some("agent-blocked".into());
    lane.outcome_detail = Some("needs a person".into());
    fixture
        .store()
        .write(|tx| tx.put_engine_lane(&lane))
        .unwrap();

    let finished = service.stop(&run.id, true).unwrap();

    assert_eq!(finished.run.state, EngineRunState::Finished);
    assert_eq!(finished.lanes[0].state, EngineLaneState::Idle);
    assert_eq!(finished.lanes[0].outcome.as_deref(), Some("agent-blocked"));
    assert_eq!(
        finished.lanes[0].outcome_detail.as_deref(),
        Some("needs a person")
    );
    assert!(fake.calls().is_empty());
}
