//! The Full Auto engine's durable operational model (SH-462).
//!
//! Runs and lanes are intentionally outside the story event fold. This suite
//! proves both halves: their own schema is strict, and their presence cannot
//! perturb the read-model oracle `story doctor` relies on.

mod store_support;

use std::sync::{Arc, Barrier};

use rusqlite::{Connection, params};
use store_support::{create_story, new_store, raw, seed_project};
use storyhook::store::migrate;
use storyhook::store::{
    EngineAgent, EngineLaneRecord, EngineLaneState, EngineRunRecord, EngineRunState, EngineScope,
    ReadOps, SqliteStore, Store, StoreError, WriteOps, diff_read_model,
};
use storyhook_test_support::scratch_dir;

fn run(id: &str, project_slug: &str, state: EngineRunState) -> EngineRunRecord {
    EngineRunRecord {
        id: id.into(),
        project_slug: project_slug.into(),
        scope: EngineScope::Project,
        lanes: 2,
        agent: EngineAgent::Codex,
        state,
        consecutive_hard_stops: 0,
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
        window_name: None,
        worktree_path: None,
        dispatched_at: None,
        last_observed_at: "2026-08-29T20:00:00Z".into(),
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

    let report = store.migrate().unwrap();

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
    updated.stop_reason = Some("breaker-tripped".into());
    updated.acknowledged_at = Some("2026-08-29T20:05:00Z".into());
    updated.updated_at = "2026-08-29T20:05:00Z".into();
    let mut working = lane("run-b", 1);
    working.state = EngineLaneState::Working;
    working.story_id = Some("AL-7".into());
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
