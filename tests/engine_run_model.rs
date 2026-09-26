//! The Full Auto engine's durable operational model (SH-462).
//!
//! Runs and lanes are intentionally outside the story event fold. This suite
//! proves both halves: their own schema is strict, and their presence cannot
//! perturb the read-model oracle `story doctor` relies on.

mod store_support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex, mpsc};
use std::time::Duration;

use rusqlite::{Connection, params};
use store_support::{create_story, new_store, raw, seed_project};
use storyhook::domain::{CLEANUP_LEASE_VERSION, StoryCleanupLease, TmuxCleanupTarget, TypeDef};
use storyhook::error::AppError;
use storyhook::service::engine::{
    ConfigureRequest, DispatchOutcome, EngineService, OPERATOR_STOPPED, StartRequest,
};
use storyhook::service::{Clock, ConfigService, Ctx, NewStoryInput, StoryService};
use storyhook::store::ids::StoryNo;
use storyhook::store::migrate;
use storyhook::store::{
    Access, EngineAgent, EngineLaneRecord, EngineLaneState, EngineQuarantineRecord,
    EngineRunRecord, EngineRunState, EngineScope, EngineSpeed, MigrationReport, NewProject,
    ReadOps, SqliteStore, Store, StoreError, WriteOps, WriteWithSnapshot, diff_read_model,
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
        model: None,
        effort: None,
        speed: None,
        state,
        consecutive_hard_stops: 0,
        recent_quarantines: Vec::new(),
        stop_reason: None,
        acknowledged_at: None,
        created_at: "2026-08-29T20:00:00Z".into(),
        updated_at: "2026-08-29T20:00:00Z".into(),
    }
}

#[test]
fn migration_32_adds_nullable_run_configuration_without_inventing_defaults() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..31]).unwrap();
    seed_project(&store, "alpha", "AL");
    insert_raw_run(
        &raw(&store),
        "run-before-options",
        "project",
        None,
        1,
        EngineAgent::Codex.as_str(),
        EngineRunState::Running.as_str(),
    )
    .unwrap();

    let report = store.migrate_with(&migrate::MIGRATIONS[..32]).unwrap();

    assert_eq!(report.from_version, 31);
    assert_eq!(report.to_version, 32);
    assert_eq!(report.applied, ["engine_run_options"]);
    let stored = store
        .read(|tx| tx.engine_run("run-before-options"))
        .unwrap()
        .unwrap();
    assert_eq!(stored.model, None);
    assert_eq!(stored.effort, None);
    assert_eq!(stored.speed, None);
}

fn lane(run_id: &str, lane_index: u32) -> EngineLaneRecord {
    EngineLaneRecord {
        adopted_identity: None,
        run_id: run_id.into(),
        lane_index,
        state: EngineLaneState::Idle,
        story_id: None,
        pane_id: None,
        window_name: None,
        worktree_path: None,
        cleanup_lease: None,
        dispatched_at: None,
        last_observed_at: "2026-08-29T20:00:00Z".into(),
        last_progress_seq: None,
        last_progress_at: None,
        outcome: None,
        outcome_detail: None,
        probe_detail: None,
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
    let conn = raw(&store);
    let (window_name, pane_id): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT window_name, pane_id FROM engine_lanes \
             WHERE run_id = 'run-before-pane-ids' AND lane_index = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(window_name.as_deref(), Some("AL-7"));
    assert_eq!(pane_id, None);
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
    let history: String = raw(&store)
        .query_row(
            "SELECT recent_quarantines_json FROM engine_runs WHERE id = 'run-before-history'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(history, "[]");
}

#[test]
fn run_and_lane_records_round_trip_update_order_and_reopen() {
    let (dir, store) = new_store();
    seed_project(&store, "beta", "BE");
    seed_project(&store, "alpha", "AL");

    let mut first = run("run-a", "alpha", EngineRunState::Finished);
    first.created_at = "2026-08-29T19:00:00Z".into();
    let mut second = run("run-b", "alpha", EngineRunState::Running);
    second.model = Some("gpt-5.3-codex".into());
    second.effort = Some("high".into());
    second.speed = Some(EngineSpeed::Fast);
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
    updated.model = Some("claude-opus-4-6".into());
    updated.effort = Some("max".into());
    updated.speed = Some(EngineSpeed::Standard);
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
    working.worktree_path = Some("/repos/original/.codex/worktrees/AL-7".into());
    working.cleanup_lease = Some(cleanup_lease(
        "AL-7",
        Path::new("/repos/original/.codex/worktrees/AL-7"),
    ));
    store
        .write(|tx| {
            tx.update_engine_run(&updated)?;
            tx.put_engine_lane(&working)
        })
        .unwrap();

    let stored = store.read(|tx| tx.engine_run("run-b")).unwrap().unwrap();
    assert_eq!(stored.project_slug, "alpha");
    assert_eq!(stored.scope, EngineScope::Project);
    assert_eq!(stored.lanes, 99);
    assert_eq!(stored.agent, EngineAgent::Claude);
    assert_eq!(stored.model.as_deref(), Some("claude-opus-4-6"));
    assert_eq!(stored.effort.as_deref(), Some("max"));
    assert_eq!(stored.speed, Some(EngineSpeed::Standard));
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
        "speed-check",
        "project",
        None,
        1,
        EngineAgent::Codex.as_str(),
        EngineRunState::Finished.as_str(),
    )
    .unwrap();
    conn.execute(
        "UPDATE engine_runs SET speed = ?1 WHERE id = 'speed-check'",
        [EngineSpeed::Fast.as_str()],
    )
    .unwrap();
    rejected(
        conn.execute(
            "UPDATE engine_runs SET speed = 'turbo' WHERE id = 'speed-check'",
            [],
        ),
        "speed vocabulary",
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
fn invalid_stored_dispatch_tokens_fail_loudly_on_read() {
    let (_dir, store) = new_store();
    seed_project(&store, "alpha", "AL");
    store
        .write(|tx| tx.create_engine_run(&run("corrupt-options", "alpha", EngineRunState::Running)))
        .unwrap();

    raw(&store)
        .execute(
            "UPDATE engine_runs SET model = 'gpt;unsafe' WHERE id = 'corrupt-options'",
            [],
        )
        .unwrap();
    let model_error = store
        .read(|tx| tx.engine_run("corrupt-options"))
        .unwrap_err()
        .to_string();
    assert!(
        model_error.contains("engine_runs.model holds invalid value"),
        "{model_error}"
    );

    raw(&store)
        .execute(
            "UPDATE engine_runs SET model = NULL, effort = 'high effort' \
             WHERE id = 'corrupt-options'",
            [],
        )
        .unwrap();
    let effort_error = store
        .read(|tx| tx.engine_run("corrupt-options"))
        .unwrap_err()
        .to_string();
    assert!(
        effort_error.contains("engine_runs.effort holds invalid value"),
        "{effort_error}"
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
        model: None,
        effort: None,
        speed: None,
    }
}

fn configure_request(lanes: u32) -> ConfigureRequest {
    ConfigureRequest {
        lanes,
        agent: EngineAgent::Claude,
        model: Some("claude-opus-4-6".into()),
        effort: Some("high".into()),
        speed: Some(EngineSpeed::Fast),
    }
}

fn cleanup_lease(story: &str, worktree: &Path) -> StoryCleanupLease {
    StoryCleanupLease {
        version: CLEANUP_LEASE_VERSION,
        project_slug: "fixture".into(),
        story_id: story.into(),
        repository_path: "/repos/original".into(),
        worktree_path: worktree.into(),
        branch: format!("worktree-{story}"),
        tmux: TmuxCleanupTarget {
            socket_path: "/tmp/tmux-original/default".into(),
        },
    }
}

fn occupy(fixture: &ServiceFixture, run_id: &str, index: u32, story: &str, worktree: &str) {
    // Positive reset paths acquire the leased repository's real Git-common lock.
    let repository = fixture.cwd().canonicalize().unwrap();
    let initialized = storyhook::env::git_env::command(&repository)
        .args(["init", "--initial-branch=main"])
        .output()
        .unwrap();
    assert!(initialized.status.success(), "{initialized:?}");
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), Some(&repository)))
        .unwrap();
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
    let mut lease = cleanup_lease(story, Path::new(worktree));
    lease.repository_path = repository;
    lane.cleanup_lease = Some(lease);
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
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap();
    assert_eq!(epic_run.scope, EngineScope::Epic(epic.id));
    assert_eq!(epic_run.agent, EngineAgent::Claude);
    assert_eq!(service.status(None).unwrap().len(), 2);
}

#[test]
fn a_live_run_configuration_expands_and_contracts_without_interrupting_occupied_lanes() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);
    let run = service.start(start_request(3)).unwrap();
    for index in 0..3 {
        occupy(
            &fixture,
            &run.id,
            index,
            &format!("SH-{}", index + 1),
            &format!("/tmp/wt/SH-{}", index + 1),
        );
    }

    let contracted = service.configure(&run.id, configure_request(2)).unwrap();
    assert_eq!(contracted.run.lanes, 2);
    assert_eq!(contracted.run.agent, EngineAgent::Claude);
    assert_eq!(contracted.run.model.as_deref(), Some("claude-opus-4-6"));
    assert_eq!(contracted.run.effort.as_deref(), Some("high"));
    assert_eq!(contracted.run.speed, Some(EngineSpeed::Fast));
    assert_eq!(
        contracted.lanes.len(),
        3,
        "occupied surplus lanes stay visible"
    );
    assert!(
        contracted
            .lanes
            .iter()
            .all(|lane| lane.state == EngineLaneState::Working)
    );

    let mut surplus = contracted.lanes[2].clone();
    surplus.state = EngineLaneState::Idle;
    surplus.story_id = None;
    fixture
        .store()
        .write(|tx| tx.put_engine_lane(&surplus))
        .unwrap();
    let normalized = service.configure(&run.id, configure_request(2)).unwrap();
    assert_eq!(
        normalized
            .lanes
            .iter()
            .map(|lane| lane.lane_index)
            .collect::<Vec<_>>(),
        [0, 1],
        "an idle lane above the new cap is retired"
    );

    let expanded = service.configure(&run.id, configure_request(4)).unwrap();
    assert_eq!(expanded.run.lanes, 4);
    assert_eq!(
        expanded
            .lanes
            .iter()
            .map(|lane| lane.lane_index)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
}

#[test]
fn configuring_a_run_refuses_invalid_values_and_non_claiming_states() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);
    let run = service.start(start_request(1)).unwrap();

    let lanes = service
        .configure(&run.id, configure_request(0))
        .unwrap_err()
        .to_string();
    assert!(lanes.contains("between 1 and 255 lanes"), "{lanes}");

    service.stop(&run.id, false).unwrap();
    let state = service
        .configure(&run.id, configure_request(2))
        .unwrap_err()
        .to_string();
    assert!(state.contains("cannot `configure`"), "{state}");
    assert!(state.contains("finished"), "{state}");
}

#[test]
fn engine_service_start_refusals_name_their_causes() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);

    let zero = service.start(start_request(0)).unwrap_err().to_string();
    assert!(zero.contains("between 1 and 255 lanes"), "{zero}");

    let unsafe_model = service
        .start(StartRequest {
            model: Some("gpt;unsafe".into()),
            ..start_request(1)
        })
        .unwrap_err()
        .to_string();
    assert!(
        unsafe_model.contains("invalid engine model"),
        "{unsafe_model}"
    );

    let unsafe_effort = service
        .start(StartRequest {
            effort: Some("high effort".into()),
            ..start_request(1)
        })
        .unwrap_err()
        .to_string();
    assert!(
        unsafe_effort.contains("invalid engine effort"),
        "{unsafe_effort}"
    );

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
            model: None,
            effort: None,
            speed: None,
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

/// A second connection whose selected write pauses before opening its SQLite
/// transaction. This makes the old run-check/claim gap deterministic: stop can
/// commit through the first connection while reconcile is between those two
/// operations, without adding a production timing seam.
struct WriteGateStore {
    inner: SqliteStore,
    write_index: AtomicUsize,
    gated_index: usize,
    gate_after_commit: bool,
    entered: mpsc::SyncSender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl WriteGateStore {
    fn before(
        inner: SqliteStore,
        gated_index: usize,
    ) -> (Self, mpsc::Receiver<()>, mpsc::SyncSender<()>) {
        Self::at(inner, gated_index, false)
    }

    fn after(
        inner: SqliteStore,
        gated_index: usize,
    ) -> (Self, mpsc::Receiver<()>, mpsc::SyncSender<()>) {
        Self::at(inner, gated_index, true)
    }

    fn at(
        inner: SqliteStore,
        gated_index: usize,
        gate_after_commit: bool,
    ) -> (Self, mpsc::Receiver<()>, mpsc::SyncSender<()>) {
        let (entered_tx, entered_rx) = mpsc::sync_channel(0);
        let (release_tx, release_rx) = mpsc::sync_channel(0);
        (
            Self {
                inner,
                write_index: AtomicUsize::new(0),
                gated_index,
                gate_after_commit,
                entered: entered_tx,
                release: Mutex::new(release_rx),
            },
            entered_rx,
            release_tx,
        )
    }
}

impl Store for WriteGateStore {
    fn access(&self) -> Access {
        self.inner.access()
    }

    type ReadTx<'a> = <SqliteStore as Store>::ReadTx<'a>;
    type WriteTx<'a> = <SqliteStore as Store>::WriteTx<'a>;

    fn read<T>(
        &self,
        f: impl FnOnce(&Self::ReadTx<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.inner.read(f)
    }

    fn write<T>(
        &self,
        f: impl FnOnce(&mut Self::WriteTx<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let gated = self.write_index.fetch_add(1, Ordering::SeqCst) == self.gated_index;
        if gated && !self.gate_after_commit {
            self.entered.send(()).expect("announce gated engine write");
            self.release
                .lock()
                .expect("write gate release mutex")
                .recv_timeout(Duration::from_secs(5))
                .expect("release gated engine write");
        }
        let result = self.inner.write(f);
        if gated && self.gate_after_commit {
            self.entered.send(()).expect("announce gated engine write");
            self.release
                .lock()
                .expect("write gate release mutex")
                .recv_timeout(Duration::from_secs(5))
                .expect("release gated engine write");
        }
        result
    }

    fn migrate(&self) -> Result<MigrationReport, StoreError> {
        self.inner.migrate()
    }

    fn change_token(&self) -> Result<u64, StoreError> {
        self.inner.change_token()
    }

    fn snapshot(&self, dir: &Path, label: &str) -> Result<PathBuf, StoreError> {
        self.inner.snapshot(dir, label)
    }

    fn write_with_snapshot<T>(
        &self,
        dir: &Path,
        label: &str,
        f: impl FnOnce(&mut Self::WriteTx<'_>) -> Result<T, StoreError>,
    ) -> Result<WriteWithSnapshot<T>, StoreError> {
        self.inner.write_with_snapshot(dir, label, f)
    }
}

#[test]
fn stop_linearizes_before_the_next_engine_claim_and_prevents_late_dispatch() {
    let fixture = ServiceFixture::new();
    StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "must not be reclaimed after stop".into(),
            ..NewStoryInput::default()
        })
        .unwrap();
    let start_ctx = fixture.ctx();
    let start_dispatcher = FakeDispatcher::default();
    let run = EngineService::new(&start_ctx, &start_dispatcher)
        .start(start_request(2))
        .unwrap();

    let second_connection = SqliteStore::open(fixture.env().store_path()).unwrap();
    // Reconcile's first write is its no-op breaker fold. The second is the
    // ready claim, immediately after the old standalone run-state read.
    let (gated_store, claim_entered, release_claim) = WriteGateStore::before(second_connection, 1);
    let dispatch = FakeDispatcher::default();

    let reconcile = std::thread::scope(|scope| {
        let run_id = run.id.clone();
        let ctx = Ctx::new(
            &gated_store,
            fixture.project(),
            fixture.cwd(),
            fixture.env().clone(),
        )
        .clock(Clock::Fixed(FIXTURE_NOW.into()));
        let worker_dispatch = dispatch.clone();
        let worker =
            scope.spawn(move || EngineService::new(&ctx, &worker_dispatch).reconcile(&run_id));

        claim_entered
            .recv_timeout(Duration::from_secs(5))
            .expect("reconcile reached the claim transaction boundary");
        let stop_ctx = fixture.ctx();
        let stop_dispatcher = FakeDispatcher::default();
        let stopped = EngineService::new(&stop_ctx, &stop_dispatcher)
            .stop(&run.id, true)
            .unwrap();
        assert_eq!(stopped.run.state, EngineRunState::Finished);
        release_claim.send(()).expect("release the pending claim");
        worker.join()
    });

    reconcile
        .expect("reconcile must not reach the unscripted dispatcher after stop")
        .unwrap();
    assert!(dispatch.calls().is_empty());
    let story = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(story.state, "todo");
    let lanes = fixture.store().read(|tx| tx.engine_lanes(&run.id)).unwrap();
    assert!(lanes.iter().all(|lane| lane.state == EngineLaneState::Idle));
}

#[test]
fn a_claim_and_its_dispatching_lane_become_visible_in_one_commit() {
    let fixture = ServiceFixture::new();
    StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "claim and lane share ownership".into(),
            ..NewStoryInput::default()
        })
        .unwrap();
    let start_ctx = fixture.ctx();
    let start_dispatcher = FakeDispatcher::default();
    let run = EngineService::new(&start_ctx, &start_dispatcher)
        .start(start_request(2))
        .unwrap();

    let second_connection = SqliteStore::open(fixture.env().store_path()).unwrap();
    // Pause after the claim transaction commits but before reconcile can make
    // any later write. A reader must never observe the claimed story beside an
    // idle lane: stop would treat that combination as safe to finish.
    let (gated_store, claim_committed, release_reconcile) =
        WriteGateStore::after(second_connection, 1);
    let dispatch = FakeDispatcher::new([DispatcherStep::Dispatch(DispatchOutcome::from_payload(
        serde_json::json!({
            "ok": true,
            "cleanup_lease": cleanup_lease("SH-1", Path::new("/preserved/SH-1")),
        }),
    ))]);

    let atomic_view = std::thread::scope(|scope| {
        let run_id = run.id.clone();
        let ctx = Ctx::new(
            &gated_store,
            fixture.project(),
            fixture.cwd(),
            fixture.env().clone(),
        )
        .clock(Clock::Fixed(FIXTURE_NOW.into()));
        let worker_dispatch = dispatch.clone();
        let worker =
            scope.spawn(move || EngineService::new(&ctx, &worker_dispatch).reconcile(&run_id));

        claim_committed
            .recv_timeout(Duration::from_secs(5))
            .expect("reconcile committed its claim transaction");
        let story = fixture
            .store()
            .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap();
        let lane = fixture
            .store()
            .read(|tx| tx.engine_lanes(&run.id))
            .unwrap()
            .into_iter()
            .find(|lane| lane.story_id.as_deref() == Some("SH-1"));
        let atomic = story.state == "in-progress"
            && lane.is_some_and(|lane| lane.state == EngineLaneState::Dispatching);
        release_reconcile
            .send(())
            .expect("release reconcile after observing its commit");
        worker.join().unwrap().unwrap();
        atomic
    });

    assert!(
        atomic_view,
        "a claimed engine story was visible before its dispatching lane"
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

fn active_reset_story(fixture: &ServiceFixture, title: &str) -> String {
    StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: title.into(),
            state: Some("in-progress".into()),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id
}

#[test]
fn immediate_stop_retries_only_failed_targets_and_retains_reservation_identity() {
    let fixture = ServiceFixture::new();
    let first = active_reset_story(&fixture, "first");
    let second = active_reset_story(&fixture, "second");
    let fake = FakeDispatcher::new([
        DispatcherStep::ResetFailure("window refused".into()),
        DispatcherStep::Reset,
        DispatcherStep::Reset,
    ]);
    let ctx = fixture.ctx();
    let engine = EngineService::new(&ctx, &fake);
    let run = engine.start(start_request(2)).unwrap();
    occupy(&fixture, &run.id, 0, &first, "/owned/first");
    occupy(&fixture, &run.id, 1, &second, "/owned/second");
    assert!(
        engine
            .stop(&run.id, true)
            .unwrap_err()
            .to_string()
            .contains("window refused")
    );
    let partial = engine.status(Some(&run.id)).unwrap().remove(0);
    assert_eq!(partial.run.state, EngineRunState::Draining);
    assert_eq!(partial.lanes[0].state, EngineLaneState::Working);
    assert_eq!(partial.lanes[1].state, EngineLaneState::Idle);
    let reserved = fixture
        .store()
        .read(|tx| tx.engine_reset(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert!(
        reserved
            .failure
            .as_deref()
            .unwrap()
            .contains("window refused")
    );
    assert_eq!(
        engine.reset_target(&run.id, &reserved.token).unwrap(),
        reserved
    );
    let stories = StoryService::new(&ctx);
    for state in ["verifying", "todo", "done", "in-progress"] {
        assert!(
            stories
                .set_state(&first, state, None, None, None)
                .unwrap_err()
                .to_string()
                .contains("reset in progress")
        );
    }
    assert!(
        stories
            .delete(&first)
            .unwrap_err()
            .to_string()
            .contains("reset in progress")
    );
    assert!(
        stories
            .set_awaiting(&first, "cancelled")
            .unwrap_err()
            .to_string()
            .contains("reset in progress")
    );
    let before = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(before.awaiting, None);
    assert_eq!(before.state, "in-progress");
    assert_eq!(
        engine.stop(&run.id, true).unwrap().run.state,
        EngineRunState::Finished
    );
    assert!(
        fixture
            .store()
            .read(|tx| tx.engine_reset(fixture.project(), StoryNo::new(1)))
            .unwrap()
            .is_none()
    );
    let calls = fake.calls();
    let targets: Vec<_> = calls
        .iter()
        .map(|call| match call {
            DispatcherCall::Reset(reset) => reset,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(targets.len(), 3);
    assert_eq!(targets[0].token, targets[2].token);
    assert_eq!(targets[0].lease, targets[2].lease);
    assert_eq!(targets[1].lease.story_id, second);
    assert!(engine.reset_target(&run.id, &reserved.token).is_err());
    assert_eq!(
        engine.stop(&run.id, true).unwrap().run.state,
        EngineRunState::Finished
    );
    assert_eq!(fake.calls().len(), 3);
}

/// Occupies lane 0 of `run` with `story` and then removes its cleanup lease:
/// the shape a refused dispatch leaves, and the shape of pre-SH-706 lanes.
fn occupy_without_lease(fixture: &ServiceFixture, run: &str, story: &str, state: EngineLaneState) {
    occupy(fixture, run, 0, story, "/preserved/SH-13");
    let mut lane = fixture
        .store()
        .read(|tx| tx.engine_lanes(run))
        .unwrap()
        .into_iter()
        .find(|lane| lane.lane_index == 0)
        .unwrap();
    lane.state = state;
    lane.cleanup_lease = None;
    fixture
        .store()
        .write(|tx| tx.put_engine_lane(&lane))
        .unwrap();
}

/// SH-774 (reverses the SH-706 legacy-lane refusal): a lane without a cleanup
/// lease can never be reset, because no retry produces the missing proof of
/// ownership. Refusing it kept the run draining forever (run 22e4276a,
/// 2026-09-25). Stop Now now releases the lane and keeps the work: the claim,
/// the resources, and a diagnosis on the story.
#[test]
fn immediate_stop_releases_a_leaseless_lane_and_preserves_its_story() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);
    let run = service.start(start_request(1)).unwrap();
    let story = active_reset_story(&fixture, "legacy");
    occupy_without_lease(&fixture, &run.id, &story, EngineLaneState::Working);

    let stopped = service.stop(&run.id, true).unwrap();

    assert_eq!(stopped.run.state, EngineRunState::Finished);
    assert_eq!(stopped.lanes[0].state, EngineLaneState::Idle);
    assert_eq!(
        stopped.lanes[0].outcome.as_deref(),
        Some("operator-stopped-now")
    );
    let detail = stopped.lanes[0].outcome_detail.clone().unwrap();
    assert!(detail.contains("no cleanup lease"), "{detail}");
    assert!(detail.contains(&format!("story reset {story}")), "{detail}");
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "in-progress", "the claim is preserved");
    assert_eq!(row.awaiting.as_deref(), Some(detail.as_str()));
    assert_eq!(row.snapshot.comments.last().unwrap().text, detail);
    assert!(
        fixture
            .store()
            .read(|tx| tx.engine_reset(fixture.project(), StoryNo::new(1)))
            .unwrap()
            .is_none(),
        "no cleanup identity is invented"
    );
    assert!(fake.calls().is_empty(), "no helper touches unproven resources");
}

/// SH-774: the incident's lane was quarantined by a refused dispatch, so its
/// story already says why. Releasing the lane adds a comment and keeps that
/// first diagnosis as the awaiting reason.
#[test]
fn immediate_stop_keeps_an_existing_quarantine_diagnosis() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);
    let run = service.start(start_request(1)).unwrap();
    let story = active_reset_story(&fixture, "refused");
    occupy_without_lease(&fixture, &run.id, &story, EngineLaneState::Quarantined);
    StoryService::new(&ctx)
        .set_awaiting(&story, "could not confirm Codex is running")
        .unwrap();

    let stopped = service.stop(&run.id, true).unwrap();

    assert_eq!(stopped.run.state, EngineRunState::Finished);
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "in-progress");
    assert_eq!(
        row.awaiting.as_deref(),
        Some("could not confirm Codex is running")
    );
    let comment = &row.snapshot.comments.last().unwrap().text;
    assert!(comment.contains("lane 0 (quarantined)"), "{comment}");
    assert!(fake.calls().is_empty());
}

/// SH-774: a lane whose story was purged has no story to restore. Refusing
/// it failed Stop Now forever, and its failed lookup also refused the
/// helper's authorization (`engine reset-target`) for every other lane.
#[test]
fn immediate_stop_releases_a_lane_whose_story_was_deleted() {
    let fixture = ServiceFixture::new();
    let doomed = active_reset_story(&fixture, "deleted mid-run");
    let kept = active_reset_story(&fixture, "still owned");
    let fake = FakeDispatcher::new([DispatcherStep::Reset]);
    let ctx = fixture.ctx();
    let engine = EngineService::new(&ctx, &fake);
    let run = engine.start(start_request(2)).unwrap();
    engine.pause(&run.id).unwrap();
    occupy(&fixture, &run.id, 0, &doomed, "/owned/doomed");
    occupy(&fixture, &run.id, 1, &kept, "/owned/kept");
    StoryService::new(&ctx).delete(&doomed).unwrap();

    let stopped = engine.stop(&run.id, true).unwrap();

    assert_eq!(stopped.run.state, EngineRunState::Finished);
    let released = &stopped.lanes[0];
    assert_eq!(released.state, EngineLaneState::Idle);
    let detail = released.outcome_detail.as_deref().unwrap();
    assert!(detail.contains("no longer exists"), "{detail}");
    assert_eq!(fake.calls().len(), 1, "the leased lane still resets");
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(2)))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "todo");
}

/// SH-774: the leased helper authorizes itself through `engine reset-target`,
/// which scans every lane of the run. One lane whose story no longer
/// resolves failed that scan, so no other lane's helper could run.
#[test]
fn reset_authorization_skips_a_lane_whose_story_is_gone() {
    let fixture = ServiceFixture::new();
    let kept = active_reset_story(&fixture, "still owned");
    let fake = FakeDispatcher::new([DispatcherStep::ResetFailure("hold".into())]);
    let ctx = fixture.ctx();
    let engine = EngineService::new(&ctx, &fake);
    let run = engine.start(start_request(2)).unwrap();
    engine.pause(&run.id).unwrap();
    occupy(&fixture, &run.id, 1, &kept, "/owned/kept");
    assert!(engine.stop(&run.id, true).is_err());
    let token = fixture
        .store()
        .read(|tx| tx.engine_reset(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap()
        .token;
    // The scan visits lanes in order: the unresolvable lane comes first.
    occupy(&fixture, &run.id, 0, "SH-99", "/owned/gone");

    assert_eq!(engine.reset_target(&run.id, &token).unwrap().token, token);
}

/// SH-774: a card reset that owns the lane's story made Stop Now fail
/// ("reset in progress") until that reset finished or was retried. Stop Now
/// now defers to it: success, run still draining, lane untouched. Once the
/// card reset has returned the story to todo, the next Stop Now finishes.
#[test]
fn stop_now_defers_to_a_card_reset_and_finishes_after_it() {
    use storyhook::service::story_reset::StoryResetService;
    let fixture = ServiceFixture::new();
    let story = active_reset_story(&fixture, "card reset in flight");
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let engine = EngineService::new(&ctx, &fake);
    let run = engine.start(start_request(1)).unwrap();
    occupy(&fixture, &run.id, 0, &story, "/owned/card");
    let card = StoryResetService::new(&ctx).reserve(&story, &story).unwrap();

    let deferred = engine.stop(&run.id, true).unwrap();

    assert_eq!(deferred.run.state, EngineRunState::Draining);
    assert_eq!(deferred.lanes[0].state, EngineLaneState::Working);
    assert!(fake.calls().is_empty(), "the card reset owns the cleanup");

    // The card reset's own finish: receipt completed, story back to todo.
    let mut completed = card;
    completed.completed = true;
    fixture
        .store()
        .write(|tx| tx.put_story_reset(&completed))
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&story, "todo", None, None, None)
        .unwrap();
    let finished = engine.stop(&run.id, true).unwrap();
    assert_eq!(finished.run.state, EngineRunState::Finished);
    assert!(fake.calls().is_empty());
}

/// SH-774: a lane whose story a card reset owns failed Stop Now before the
/// later lanes were tried, and its refused bookkeeping write replaced the
/// error with "reset owns engine lane". The deferral must not hide a real
/// failure on another lane.
#[test]
fn a_deferred_lane_does_not_hide_another_lanes_failure() {
    use storyhook::service::story_reset::StoryResetService;
    let fixture = ServiceFixture::new();
    let owned = active_reset_story(&fixture, "card reset in flight");
    let failing = active_reset_story(&fixture, "helper refuses");
    let fake = FakeDispatcher::new([DispatcherStep::ResetFailure("window refused".into())]);
    let ctx = fixture.ctx();
    let engine = EngineService::new(&ctx, &fake);
    let run = engine.start(start_request(2)).unwrap();
    occupy(&fixture, &run.id, 0, &owned, "/owned/card");
    occupy(&fixture, &run.id, 1, &failing, "/owned/failing");
    StoryResetService::new(&ctx).reserve(&owned, &owned).unwrap();

    let error = engine.stop(&run.id, true).unwrap_err().to_string();

    assert!(error.contains("window refused"), "{error}");
    assert!(error.contains("lane 1"), "{error}");
    assert_eq!(fake.calls().len(), 1, "the later lane was attempted");
}

#[test]
fn immediate_stop_detaches_verification_without_calling_a_cleanup_helper() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let stories = StoryService::new(&ctx);
    let story = stories
        .create(&NewStoryInput {
            title: "Verifier owns this work".into(),
            state: Some("verifying".into()),
            ..NewStoryInput::default()
        })
        .unwrap();
    let fake = FakeDispatcher::default();
    let engine = EngineService::new(&ctx, &fake);
    let run = engine.start(start_request(1)).unwrap();
    occupy(&fixture, &run.id, 0, &story.id, "/verifier/owned");
    let before = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap();
    let stopped = engine.stop(&run.id, true).unwrap();
    assert_eq!(stopped.run.state, EngineRunState::Finished);
    assert!(fake.calls().is_empty());
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
            .unwrap(),
        before
    );
}

#[test]
fn immediate_stop_resets_a_retained_quarantined_story() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let stories = StoryService::new(&ctx);
    let story = stories
        .create(&NewStoryInput {
            title: "Reset this interrupted attempt".into(),
            state: Some("in-progress".into()),
            ..NewStoryInput::default()
        })
        .unwrap();
    stories
        .set_awaiting(&story.id, "Full Auto: window-gone")
        .unwrap();
    let fake = FakeDispatcher::new([DispatcherStep::Reset]);
    let engine = EngineService::new(&ctx, &fake);
    let run = engine.start(start_request(1)).unwrap();
    occupy(&fixture, &run.id, 0, &story.id, "/interrupted/owned");
    fixture
        .store()
        .write(|tx| {
            let mut lane = tx.engine_lanes(&run.id)?.remove(0);
            lane.state = EngineLaneState::Quarantined;
            tx.put_engine_lane(&lane)
        })
        .unwrap();
    engine.stop(&run.id, true).unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "todo");
    assert_eq!(row.awaiting, None);
}

#[test]
fn immediate_stop_preserves_a_quarantined_verifying_story_and_its_diagnosis() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let stories = StoryService::new(&ctx);
    let story = stories
        .create(&NewStoryInput {
            title: "Verifier incident".into(),
            state: Some("verifying".into()),
            ..NewStoryInput::default()
        })
        .unwrap();
    stories
        .set_awaiting(&story.id, "verifier owns recovery")
        .unwrap();
    let fake = FakeDispatcher::default();
    let engine = EngineService::new(&ctx, &fake);
    let run = engine.start(start_request(1)).unwrap();
    occupy(&fixture, &run.id, 0, &story.id, "/verifier/owned");
    fixture
        .store()
        .write(|tx| {
            let mut lane = tx.engine_lanes(&run.id)?.remove(0);
            lane.state = EngineLaneState::Quarantined;
            tx.put_engine_lane(&lane)
        })
        .unwrap();
    let before = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap();
    assert_eq!(
        engine.stop(&run.id, true).unwrap().run.state,
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
fn migration_40_preserves_existing_lanes_and_refuses_incomplete_adoption() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..38]).unwrap();
    seed_project(&store, "alpha", "AL");
    raw(&store).execute("INSERT INTO engine_runs (id,project_slug,scope_kind,lanes,agent,state,created_at,updated_at) VALUES ('legacy','alpha','project',1,'codex','running','2026-01-01','2026-01-01')", []).unwrap();
    raw(&store).execute("INSERT INTO engine_lanes (run_id,lane_index,state,last_observed_at) VALUES ('legacy',0,'idle','2026-01-01')", []).unwrap();
    store.migrate().unwrap();
    let lanes = store.read(|tx| tx.engine_lanes("legacy")).unwrap();
    assert!(lanes[0].adopted_identity.is_none());
    assert!(
        raw(&store)
            .execute(
                "UPDATE engine_lanes SET adopted_identity_json='{}' WHERE run_id='legacy'",
                []
            )
            .is_err()
    );
}

#[test]
fn migration_40_upgrades_landed_recovery_schema_without_losing_receipts_or_lanes() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..39]).unwrap();
    seed_project(&store, "alpha", "AL");
    let receipt = r#"{"incident":"alpha:17","outcome":"recovered"}"#;
    raw(&store).execute(
        "INSERT INTO verification_recovery (project_id,receipt) SELECT id,?1 FROM projects WHERE slug='alpha'",
        [receipt],
    ).unwrap();
    raw(&store).execute("INSERT INTO engine_runs (id,project_slug,scope_kind,lanes,agent,state,created_at,updated_at) VALUES ('legacy','alpha','project',1,'codex','running','2026-01-01','2026-01-01')", []).unwrap();
    raw(&store).execute("INSERT INTO engine_lanes (run_id,lane_index,state,last_observed_at) VALUES ('legacy',0,'idle','2026-01-01')", []).unwrap();

    store.migrate().unwrap();
    let lanes = store.read(|tx| tx.engine_lanes("legacy")).unwrap();
    assert_eq!(lanes.len(), 1);
    assert_eq!(lanes[0].state, EngineLaneState::Idle);
    assert!(lanes[0].adopted_identity.is_none());
    let retained: String = raw(&store).query_row(
        "SELECT receipt FROM verification_recovery WHERE project_id=(SELECT id FROM projects WHERE slug='alpha')",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(retained, receipt);
    let history: Vec<(u32, String)> = raw(&store)
        .prepare(
            "SELECT version,name FROM schema_migrations WHERE version IN (39,40) ORDER BY version",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        history,
        vec![
            (39, "verification_recovery".into()),
            (40, "engine_adoption".into())
        ]
    );
    assert!(
        raw(&store)
            .execute(
                "UPDATE engine_lanes SET adopted_identity_json='{}' WHERE run_id='legacy'",
                []
            )
            .is_err()
    );
    store.migrate().unwrap();
    assert_eq!(store.read(|tx| tx.engine_lanes("legacy")).unwrap(), lanes);
}

#[test]
fn reset_migration_follows_shipped_adoption_and_continuation_schema() {
    assert_reset_migration_preserves_v41_records(migrate::MIGRATIONS[41].sql);
}

// Each mutant runs the same upgrade and assertions in its own scratch store.
// Expected diagnostics prevent SQL errors or unrelated fixture panics from passing.
#[test]
#[should_panic(expected = "continuation records changed")]
fn reset_migration_retention_detects_deleted_continuations() {
    assert_reset_migration_preserves_v41_records(concat!(
        include_str!("../src/store/schema/0042_engine_resets.sql"),
        "DELETE FROM continuations;"
    ));
}

#[test]
#[should_panic(expected = "adopted lane ownership changed")]
fn reset_migration_retention_detects_erased_adoption_identity() {
    assert_reset_migration_preserves_v41_records(concat!(
        include_str!("../src/store/schema/0042_engine_resets.sql"),
        "UPDATE engine_lanes SET adopted_identity_json=NULL;"
    ));
}

#[test]
#[should_panic(expected = "adopted lane ownership changed")]
fn reset_migration_retention_detects_rebound_cleanup_lease() {
    assert_reset_migration_preserves_v41_records(concat!(
        include_str!("../src/store/schema/0042_engine_resets.sql"),
        "UPDATE engine_lanes SET cleanup_lease_json=json_set(cleanup_lease_json, '$.branch', 'foreign-branch');"
    ));
}

fn assert_reset_migration_preserves_v41_records(reset_sql: &'static str) {
    use serde_json::json;
    use storyhook::store::{
        AdoptedIdentity, Continuation, ContinuationPhase, ContinuationStatus, ExpectedSeq,
    };

    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..41]).unwrap();
    let project = seed_project(&store, "alpha", "AL");
    let at = "2026-08-29T20:01:00Z";
    let story = create_story(&store, project, "Retain adopted continuation", at);
    let head = store.read(|tx| tx.story(project, story)).unwrap().unwrap();
    store_support::append_and_fold(
        &store,
        project,
        story,
        ExpectedSeq::Exact(head.head_seq),
        &[storyhook::domain::StoryEvent::StoryStateChanged {
            at: at.into(),
            state: "in-progress".into(),
        }],
    )
    .unwrap();
    let story_id = story.to_id("AL");
    let prior = run("run-before-reset", "alpha", EngineRunState::Paused);
    let mut adopted = lane(&prior.id, 0);
    let worktree = dir.path().join("retained-worktree");
    let mut lease = cleanup_lease(&story_id, &worktree);
    lease.project_slug = "alpha".into();
    lease.repository_path = dir.path().join("repository");
    lease.tmux.socket_path = dir.path().join("tmux.sock");
    adopted.state = EngineLaneState::Working;
    adopted.story_id = Some(story_id.clone());
    adopted.pane_id = Some("%17".into());
    adopted.window_name = Some(story_id.clone());
    adopted.worktree_path = Some(worktree.to_string_lossy().into_owned());
    adopted.cleanup_lease = Some(lease.clone());
    adopted.dispatched_at = Some(at.into());
    adopted.adopted_identity = Some(AdoptedIdentity {
        provider: EngineAgent::Codex,
        pane_pid: 123,
        window_id: "@7".into(),
    });
    let sequence = store
        .read(|tx| tx.story(project, story))
        .unwrap()
        .unwrap()
        .head_global_seq;
    adopted.last_progress_seq = Some(sequence);
    adopted.last_progress_at = Some(at.into());
    let completed = Continuation {
        id: "f5462488-c1c7-47c9-ae29-d46ef2ec11a1".into(),
        project_id: project,
        story_no: story,
        story_id: story_id.clone(),
        handoff: json!({
            "type": "storyhook.session-handoff", "version": 1,
            "story_id": story_id, "kind": "context",
            "evidence": {"context": "context exhausted", "outstanding_work": "verify reset"}
        }),
        generation: json!({"provider": "codex", "session_id": "session-1", "turn_id": "turn-1"}),
        capture: json!({
            "lease": lease, "head": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "fingerprint": "dirty-1", "provider": "codex", "session_id": "session-1",
            "turn_id": "turn-1", "mode": "default", "socket": lease.tmux.socket_path,
            "pane": "%17", "pid": 123, "started": at,
            "model": "gpt", "effort": "high", "speed": "standard", "autonomy": true,
            "engine_lane": {"run_id": prior.id, "lane_index": 0}
        }),
        status: ContinuationStatus::Acknowledged,
        phase: ContinuationPhase::Complete,
        revision: 2,
        attempts: 1,
        created_at: at.into(),
        updated_at: "2026-09-01T00:05:00Z".into(),
        detail: "receiving session reviewed the retained work".into(),
        reviewed_seq: Some(sequence.get()),
        reviewed_head: Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into()),
    };
    let mut outstanding = completed.clone();
    outstanding.id = "2f1a4dac-b8e8-486c-b569-c17c77398941".into();
    outstanding.generation["turn_id"] = json!("turn-2");
    outstanding.capture["turn_id"] = json!("turn-2");
    outstanding.capture["fingerprint"] = json!("dirty-2");
    outstanding.status = ContinuationStatus::AwaitingAck;
    outstanding.phase = ContinuationPhase::NativeContinuation;
    outstanding.created_at = "2026-09-01T00:06:00Z".into();
    outstanding.updated_at = outstanding.created_at.clone();
    outstanding.revision = 0;
    outstanding.attempts = 0;
    outstanding.detail = "native feedback delivered; receiving review remains outstanding".into();
    outstanding.reviewed_seq = None;
    outstanding.reviewed_head = None;
    let continuations = vec![completed, outstanding];
    store
        .write(|tx| {
            tx.create_engine_run(&prior)?;
            tx.put_engine_lane(&adopted)?;
            for record in &continuations {
                tx.insert_continuation(record)?;
            }
            Ok(())
        })
        .unwrap();

    let conn = raw(&store);
    // Typed equality covers the public records; raw equality also covers duplicate
    // ownership keys/revisions and the exact serialized JSON in the backing rows.
    let snapshots: Vec<_> = [
        "SELECT * FROM engine_runs ORDER BY id",
        "SELECT * FROM engine_lanes ORDER BY run_id,lane_index",
        "SELECT * FROM continuations ORDER BY id",
        "SELECT * FROM schema_migrations WHERE version <= 41 ORDER BY version",
    ]
    .into_iter()
    .map(|sql| (sql, migration_rows(&conn, sql)))
    .collect();
    let assert_retained = || {
        assert_eq!(
            store.read(|tx| tx.engine_run(&prior.id)).unwrap(),
            Some(prior.clone())
        );
        assert_eq!(
            store.read(|tx| tx.engine_lanes(&prior.id)).unwrap(),
            vec![adopted.clone()],
            "adopted lane ownership changed"
        );
        assert_eq!(
            store.read(|tx| tx.continuations(project)).unwrap(),
            continuations,
            "continuation records changed"
        );
        for (sql, before) in &snapshots {
            assert_eq!(
                &migration_rows(&conn, sql),
                before,
                "persisted records changed: {sql}"
            );
        }
    };
    assert_eq!(store.schema_version().unwrap(), 41);
    assert_retained();
    let mut migrations = migrate::MIGRATIONS[..42].to_vec();
    migrations[41].sql = reset_sql;
    for from_version in [41, 42] {
        let report = store.migrate_with(&migrations).unwrap();
        assert_eq!(report.from_version, from_version);
        assert_eq!(report.to_version, 42);
        assert_eq!(
            report.applied,
            if from_version == 41 {
                vec!["engine_resets"]
            } else {
                vec![]
            }
        );
        assert_eq!(store.schema_version().unwrap(), 42);
        assert_retained();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM engine_resets", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

fn migration_rows(conn: &Connection, sql: &str) -> Vec<Vec<rusqlite::types::Value>> {
    let mut statement = conn.prepare(sql).unwrap();
    let columns = statement.column_count();
    statement
        .query_map([], |row| {
            (0..columns).map(|column| row.get(column)).collect()
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}
