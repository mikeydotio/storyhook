//! Stable and development schema histories converge without losing operational authority.
use rusqlite::Connection;
use storyhook::store::{
    SqliteStore, Store,
    migrate::{self, Migration},
};
use storyhook_test_support::scratch_dir;

fn historical_main(version: usize) -> Vec<Migration> {
    let mut migrations = migrate::MIGRATIONS[..37].to_vec();
    migrations.push(Migration {
        version: 38,
        name: "landing_intents",
        sql: include_str!("../src/store/schema/0038_landing_intents.sql"),
        foreign_keys_off: false,
    });
    if version == 39 {
        migrations.push(Migration {
            version: 39,
            name: "story_reset",
            sql: include_str!("../src/store/schema/0039_story_reset.sql"),
            foreign_keys_off: false,
        });
    }
    migrations
}

fn history(conn: &Connection) -> Vec<(u32, String, String)> {
    conn.prepare("SELECT version,name,applied_at FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn every_supported_lineage_converges_and_reopens_idempotently() {
    for (main, version) in [
        (false, 0),
        (false, 37),
        (true, 38),
        (true, 39),
        (false, 38),
        (false, 39),
        (false, 40),
        (false, 41),
        (false, 42),
        (false, 43),
        (false, 44),
    ] {
        let dir = scratch_dir();
        let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
        let migrations = if main {
            historical_main(version)
        } else {
            migrate::MIGRATIONS[..version].to_vec()
        };
        store.migrate_with(&migrations).unwrap();
        let before = history_if_present(store.path(), version);
        let report = store.migrate().unwrap();
        assert_eq!(report.from_version, version as u32);
        assert_eq!(report.to_version, migrate::current_schema_version());
        assert_eq!(report.backup.is_some(), version > 0);
        let conn = Connection::open(store.path()).unwrap();
        for table in [
            "landing_intents",
            "story_reset_reservations",
            "block_deliveries",
            "verification_recovery",
            "continuations",
            "engine_resets",
            "story_resets",
            "dropped_cleanups",
        ] {
            assert!(
                conn.prepare(&format!("SELECT * FROM {table}")).is_ok(),
                "{main}/{version}: {table}"
            );
        }
        let canonical = history(&conn);
        assert_eq!(
            canonical
                .iter()
                .map(|(_, name, _)| name.as_str())
                .collect::<Vec<_>>(),
            migrate::MIGRATIONS
                .iter()
                .map(|m| m.name)
                .collect::<Vec<_>>()
        );
        if main {
            let archived: String = conn
                .query_row(
                    "SELECT migrations_json FROM schema_lineage WHERE source = 'main'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                serde_json::from_str::<Vec<(u32, String, String)>>(&archived).unwrap(),
                before
            );
        }
        drop(conn);
        let reopened = SqliteStore::open(store.path()).unwrap();
        assert!(reopened.migrate().unwrap().is_noop());
    }
}

fn history_if_present(path: &std::path::Path, version: usize) -> Vec<(u32, String, String)> {
    if version == 0 {
        Vec::new()
    } else {
        history(&Connection::open(path).unwrap())
    }
}

#[test]
fn unknown_or_structurally_inconsistent_lineages_refuse_without_writing() {
    for corruption in [
        "UPDATE schema_migrations SET name='unknown' WHERE version=38",
        "DROP TABLE landing_intents",
        "CREATE TABLE block_deliveries (unexpected TEXT)",
        "UPDATE schema_migrations SET name='block_deliveries' WHERE version=38",
    ] {
        let dir = scratch_dir();
        let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
        store.migrate_with(&historical_main(39)).unwrap();
        let conn = Connection::open(store.path()).unwrap();
        conn.execute_batch(corruption).unwrap();
        let before = history(&conn);
        assert!(store.migrate().is_err(), "accepted {corruption}");
        assert_eq!(history(&conn), before);
        assert_eq!(migrate::schema_version(&conn).unwrap(), 39);
    }
}

#[test]
fn bridge_failure_rolls_back_and_retry_preserves_original_lineage() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&historical_main(39)).unwrap();
    let conn = Connection::open(store.path()).unwrap();
    let before = history(&conn);
    conn.execute_batch("CREATE TRIGGER fail_bridge BEFORE INSERT ON schema_migrations WHEN NEW.version=44 BEGIN SELECT RAISE(ABORT,'fixture bridge fault'); END;").unwrap();
    assert!(store.migrate().is_err());
    assert_eq!(migrate::schema_version(&conn).unwrap(), 39);
    assert_eq!(history(&conn), before);
    assert!(conn.prepare("SELECT reservation FROM story_resets").is_ok());
    assert!(
        conn.prepare("SELECT * FROM story_reset_reservations")
            .is_err()
    );
    conn.execute_batch("DROP TRIGGER fail_bridge").unwrap();
    store.migrate().unwrap();
    assert!(store.migrate().unwrap().is_noop());
}

#[test]
fn concurrent_main_upgraders_share_one_atomic_bridge() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&historical_main(39)).unwrap();
    let barrier = std::sync::Barrier::new(4);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let barrier = &barrier;
            let path = store.path();
            scope.spawn(move || {
                let store = SqliteStore::open(path).unwrap();
                barrier.wait();
                store.migrate().unwrap();
            });
        }
    });
    let conn = Connection::open(store.path()).unwrap();
    assert_eq!(
        migrate::schema_version(&conn).unwrap(),
        migrate::current_schema_version()
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM schema_lineage WHERE source='main'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

fn seed_stories(store: &SqliteStore) {
    use storyhook::domain::{Priority, StoryEvent, fold_story};
    use storyhook::store::{EventSeq, ExpectedSeq, NewProject, ReadOps, Store, WriteOps};
    store
        .write(|tx| {
            let project = tx.create_project(&NewProject {
                uuid: "lineage-fixture".into(),
                slug: "lineage".into(),
                name: "Lineage".into(),
                prefix: "SH".into(),
                created_at: "2026-09-13T00:00:00Z".into(),
            })?;
            tx.put_states(project, &storyhook::service::project::default_states())?;
            tx.put_types(project, &storyhook::service::project::default_types())?;
            for _ in 0..5 {
                let no = tx.allocate_story_no(project)?;
                let events = vec![
                    StoryEvent::StoryCreated {
                        at: "2026-09-13T00:00:00Z".into(),
                        title: "Pending owner".into(),
                        state: "verifying".into(),
                    },
                    StoryEvent::StoryTypeSet {
                        at: "2026-09-13T00:00:00Z".into(),
                        story_type: "normal".into(),
                    },
                    StoryEvent::StoryPrioritySet {
                        at: "2026-09-13T00:00:00Z".into(),
                        priority: Priority::Low,
                    },
                ];
                let seq = tx.append_events(
                    project,
                    no,
                    ExpectedSeq::Exact(EventSeq::ZERO),
                    &events,
                    &storyhook::domain::provenance::Provenance::unrecorded(),
                )?;
                let snapshot = fold_story(&no.to_id("SH"), &events, &tx.state_map(project)?)?;
                tx.put_story(project, &snapshot, seq)?;
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn populated_histories_keep_original_receipts_bytes_and_backup_authority() {
    use storyhook::store::{ProjectId, ReadOps, Store, StoryNo};
    for (main, version) in [
        (true, 38),
        (true, 39),
        (false, 38),
        (false, 39),
        (false, 40),
        (false, 41),
        (false, 42),
        (false, 43),
    ] {
        let dir = scratch_dir();
        let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
        store.migrate_with(&migrate::MIGRATIONS[..37]).unwrap();
        seed_stories(&store);
        let migrations = if main {
            historical_main(version)
        } else {
            migrate::MIGRATIONS[..version].to_vec()
        };
        store.migrate_with(&migrations).unwrap();
        let conn = Connection::open(store.path()).unwrap();
        let native = "{\n \"operation\" : \"native-old\", \"force\":true, \"lease\":null, \"previous_awaiting\":\"Needs approval\", \"detail\":\"retained ☃\"\n}";
        let landing = serde_json::json!({"id":"landing-old","project":1,"story":1,"story_id":"SH-1","project_slug":"lineage","generation":1,"pull_request":"https://github.com/acme/widgets/pull/1","checkout":"/retained/repo","certification":{"head":"a".repeat(40),"tree":"b".repeat(40),"gate":"true"},"created_at":"2026-09-13T00:00:00Z"}).to_string();
        let recovery = "{ \"request\": null, \"acknowledgement\": null }";
        let engine = serde_json::json!({"project":1,"story":3,"run_id":"engine-old","lane_index":0,"token":"engine-token","lease":{"version":1,"project_slug":"lineage","story_id":"SH-3","repository_path":"/retained/repo","worktree_path":"/retained/work","branch":"feature","tmux":{"socket_path":"/retained/socket"}},"restore_to":"in-progress","failure":"retained failure"}).to_string();
        let card = |story, completed| {
            serde_json::json!({"project":1,"story":story,"story_id":format!("SH-{story}"),"token":format!("card-{story}"),"original_state":"in-progress","lanes":[],"resources":null,"paths":[],"completed":completed,"failure":null}).to_string()
        };
        if main {
            conn.execute("INSERT INTO landing_intents(id,project_id,story_no,payload) VALUES ('landing-old',1,1,?1)",[&landing]).unwrap();
            if version == 39 {
                conn.execute(
                    "INSERT INTO story_resets(project_id,story_no,reservation) VALUES (1,2,?1)",
                    [native],
                )
                .unwrap();
            }
        } else {
            conn.execute("INSERT INTO block_deliveries(project_id,story_no,action,status,detail) VALUES (1,1,'interrupt','uncertain','preserve pending delivery')",[]).unwrap();
            if version >= 39 {
                conn.execute(
                    "INSERT INTO verification_recovery(project_id,receipt) VALUES (1,?1)",
                    [recovery],
                )
                .unwrap();
            }
            if version >= 41 {
                conn.execute("INSERT INTO continuations(id,project_id,story_no,revision,record) VALUES ('handoff',1,2,0,'{\"status\":\"requested\"}')",[]).unwrap();
            }
            if version >= 42 {
                conn.execute_batch("INSERT INTO engine_runs(id,project_slug,scope_kind,lanes,agent,state,created_at,updated_at) VALUES ('engine-old','lineage','project',1,'codex','running','then','then'); INSERT INTO engine_lanes(run_id,lane_index,state,story_id,last_observed_at) VALUES ('engine-old',0,'working','SH-3','then');").unwrap();
                conn.execute("INSERT INTO engine_resets(project_id,story_no,run_id,lane_index,token,record_json) VALUES (1,3,'engine-old',0,'engine-token',?1)",[&engine]).unwrap();
            }
            if version >= 43 {
                for (story, completed) in [(4, false), (5, true)] {
                    conn.execute("INSERT INTO story_resets(project_id,story_no,token,record_json) VALUES (1,?1,?2,?3)",rusqlite::params![story,format!("card-{story}"),card(story,completed)]).unwrap();
                }
            }
        }
        let report = store.migrate().unwrap();
        let backup = Connection::open(report.backup.unwrap()).unwrap();
        assert_eq!(migrate::schema_version(&backup).unwrap(), version as u32);
        if main {
            assert_eq!(
                conn.query_row("SELECT payload FROM landing_intents", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                landing
            );
            assert_eq!(
                store.read(|tx| tx.landing_intents()).unwrap()[0].id,
                "landing-old"
            );
            if version == 39 {
                assert_eq!(
                    store.read(|tx| tx.story_resets(ProjectId::new(1))).unwrap()[&StoryNo::new(2)],
                    native
                );
                assert_eq!(
                    backup
                        .query_row("SELECT reservation FROM story_resets", [], |r| r
                            .get::<_, String>(0))
                        .unwrap(),
                    native
                );
            }
        } else {
            assert_eq!(
                conn.query_row("SELECT detail FROM block_deliveries", [], |r| r
                    .get::<_, String>(0))
                    .unwrap(),
                "preserve pending delivery"
            );
            if version >= 39 {
                assert_eq!(
                    conn.query_row("SELECT receipt FROM verification_recovery", [], |r| r
                        .get::<_, String>(0))
                        .unwrap(),
                    recovery
                );
            }
            if version >= 41 {
                assert_eq!(
                    conn.query_row("SELECT record FROM continuations", [], |r| r
                        .get::<_, String>(0))
                        .unwrap(),
                    "{\"status\":\"requested\"}"
                );
            }
            if version >= 42 {
                assert_eq!(
                    conn.query_row("SELECT record_json FROM engine_resets", [], |r| r
                        .get::<_, String>(0))
                        .unwrap(),
                    engine
                );
                assert_eq!(
                    store
                        .read(|tx| tx.engine_reset(ProjectId::new(1), StoryNo::new(3)))
                        .unwrap()
                        .unwrap()
                        .token,
                    "engine-token"
                );
            }
            if version >= 43 {
                for (story, completed) in [(4, false), (5, true)] {
                    assert_eq!(
                        conn.query_row(
                            "SELECT record_json FROM story_resets WHERE story_no=?1",
                            [story],
                            |r| r.get::<_, String>(0)
                        )
                        .unwrap(),
                        card(story, completed)
                    );
                    assert_eq!(
                        store
                            .read(|tx| tx.story_reset(ProjectId::new(1), StoryNo::new(story)))
                            .unwrap()
                            .unwrap()
                            .completed,
                        completed
                    );
                }
            }
        }
    }
}

#[test]
fn unconstrained_legacy_receipt_tables_refuse_without_writing() {
    for (table, replacement) in [
        (
            "landing_intents",
            "CREATE TABLE landing_intents (id TEXT, project_id INTEGER, story_no INTEGER, payload TEXT)",
        ),
        (
            "story_resets",
            "CREATE TABLE story_resets (project_id INTEGER, story_no INTEGER, reservation TEXT)",
        ),
    ] {
        let dir = scratch_dir();
        let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
        store.migrate_with(&historical_main(39)).unwrap();
        let conn = Connection::open(store.path()).unwrap();
        conn.execute_batch(&format!("DROP TABLE {table}; {replacement};"))
            .unwrap();
        let before = history(&conn);
        let schema_before: String = conn
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type='table' AND name=?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();

        assert!(store.migrate().is_err(), "accepted unconstrained {table}");

        assert_eq!(migrate::schema_version(&conn).unwrap(), 39);
        assert_eq!(history(&conn), before);
        assert_eq!(
            conn.query_row(
                "SELECT sql FROM sqlite_schema WHERE type='table' AND name=?1",
                [table],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            schema_before
        );
        assert!(conn.prepare("SELECT reservation FROM story_resets").is_ok());
        assert!(conn.prepare("SELECT * FROM schema_lineage").is_err());
    }
}

#[test]
fn concurrent_released_upgrader_switching_lineage_is_reclassified_under_write_lock() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    static CONTENDED: AtomicBool = AtomicBool::new(false);

    fn bounded_busy_handler(retries: i32) -> bool {
        CONTENDED.store(true, Ordering::Release);
        std::thread::sleep(Duration::from_millis(10));
        retries < 500
    }

    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..37]).unwrap();
    seed_stories(&store);
    let released = Connection::open(store.path()).unwrap();
    let current = Connection::open(store.path()).unwrap();
    current.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    current.busy_handler(Some(bounded_busy_handler)).unwrap();
    assert_eq!(migrate::schema_version(&current).unwrap(), 37);
    CONTENDED.store(false, Ordering::Release);
    released.execute_batch("BEGIN IMMEDIATE").unwrap();
    let native = "{ \"operation\": \"concurrent-native\", \"force\": false, \"lease\": null, \"previous_awaiting\": null, \"detail\": null }";

    std::thread::scope(|scope| {
        let backup_dir = dir.path().join("concurrent-backups");
        let upgrade = scope.spawn(move || migrate::run(&current, migrate::MIGRATIONS, &backup_dir));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !CONTENDED.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let saw_contention = CONTENDED.load(Ordering::Acquire);

        // The released writer commits main's real tail only after the new
        // upgrader has inspected shared37 and reached its first write lock.
        for migration in &historical_main(39)[37..] {
            released.execute_batch(migration.sql).unwrap();
            released
                .execute(
                    "INSERT INTO schema_migrations(version,name,applied_at) VALUES (?1,?2,'released-writer')",
                    rusqlite::params![migration.version, migration.name],
                )
                .unwrap();
        }
        released
            .execute(
                "INSERT INTO story_resets(project_id,story_no,reservation) VALUES (1,2,?1)",
                [native],
            )
            .unwrap();
        released
            .execute_batch("PRAGMA user_version=39; COMMIT")
            .unwrap();
        let result = upgrade.join().unwrap();
        assert!(
            saw_contention,
            "upgrader never reached the bounded write-lock barrier"
        );
        result.unwrap();
    });

    assert_eq!(
        migrate::schema_version(&released).unwrap(),
        migrate::current_schema_version()
    );
    assert_eq!(
        history(&released)
            .iter()
            .map(|(_, name, _)| name.as_str())
            .collect::<Vec<_>>(),
        migrate::MIGRATIONS
            .iter()
            .map(|m| m.name)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        released
            .query_row(
                "SELECT source_version FROM schema_lineage WHERE source='main'",
                [],
                |row| row.get::<_, u32>(0),
            )
            .unwrap(),
        39
    );
    assert_eq!(
        released
            .query_row(
                "SELECT reservation FROM story_reset_reservations",
                [],
                |row| { row.get::<_, String>(0) }
            )
            .unwrap(),
        native
    );
}

#[test]
fn legacy_pending_deliveries_retire_without_replaying_started_effects() {
    use storyhook::store::{DeliveryStatus, ProjectId, ReadOps};
    for version in 38..=43 {
        let dir = scratch_dir();
        let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
        store.migrate_with(&migrate::MIGRATIONS[..37]).unwrap();
        seed_stories(&store);
        store.migrate_with(&migrate::MIGRATIONS[..version]).unwrap();
        let conn = Connection::open(store.path()).unwrap();
        conn.execute_batch(
            "INSERT INTO block_deliveries(project_id,story_no,action,status,target,detail) VALUES
            (1,1,'interrupt','pending',NULL,'old pending interrupt'),
            (1,2,'resume','pending','old resume target','old pending resume'),
            (1,3,'interrupt','attempting','started target','started delivery'),
            (1,4,'interrupt','delivered','acknowledged target','acknowledged delivery');",
        )
        .unwrap();
        drop(conn);
        store.migrate().unwrap();
        let project = ProjectId::new(1);
        let before = store.read(|tx| tx.block_deliveries(project)).unwrap();
        assert_eq!(before.len(), 4);
        assert!(
            before[..2]
                .iter()
                .all(|row| row.status == DeliveryStatus::Superseded
                    && row.detail.contains("Legacy pending"))
        );
        assert_eq!(before[1].target.as_deref(), Some("old resume target"));
        assert_eq!(before[2].status, DeliveryStatus::Uncertain);
        assert!(before[2].detail.contains("helper quiescence is unknown"));
        assert!(before[2].detail.contains("no automatic replay"));
        assert_eq!(before[2].target.as_deref(), Some("started target"));
        assert_eq!(before[3].status, DeliveryStatus::Delivered);
        assert_eq!(before[3].target.as_deref(), Some("acknowledged target"));
        let env = storyhook::env::Environment::at(dir.path());
        storyhook::daemon::block_delivery::recover(&store, &env).unwrap();
        let recovered = store.read(|tx| tx.block_deliveries(project)).unwrap();
        assert_eq!(recovered[2].status, DeliveryStatus::Uncertain);
        assert_eq!(recovered[2].target, before[2].target);
        assert_eq!(recovered[3], before[3]);
        assert!(
            !storyhook::daemon::block_delivery::process_one(
                &store,
                &env,
                Some(std::path::Path::new("/must-not-run"))
            )
            .unwrap()
        );
    }
}
