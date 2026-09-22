use predicates::prelude::*;
use storyhook_test_support::{TestEnv, scratch_dir};

#[test]
fn purge_preserves_surviving_history_and_rebuilds_activity() {
    use rusqlite::params;
    use storyhook::domain::{Priority, StoryEvent, fold_story};
    use storyhook::store::{SqliteStore, Store, migrate};
    const UNKNOWN_KIND: &str = "FutureEvent";
    assert!(
        !storyhook::domain::is_known_event_kind(UNKNOWN_KIND),
        "the purge fixture must include an event this binary cannot decode"
    );
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..47]).unwrap();
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    let states = storyhook_test_support::default_states();
    let map = states
        .iter()
        .map(|state| (state.slug.clone(), state.clone()))
        .collect();
    let created = "2026-01-01T00:00:00Z";
    let last = "2026-01-02T00:00:00Z";
    for project in 1..=2 {
        conn.execute("INSERT INTO projects (id, uuid, slug, name, prefix, created_at, next_story_no, next_global_seq) VALUES (?1, ?2, ?2, ?2, 'P', ?3, 4, 100)", params![project, format!("p{project}"), created]).unwrap();
        for (position, state) in states.iter().enumerate() {
            conn.execute("INSERT INTO project_states (project_id, position, slug, superstate, role, description) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![project, position, state.slug, state.super_state.as_str(), state.role, state.description]).unwrap();
        }
        for story in 1..=3 {
            let mut events = vec![
                StoryEvent::StoryCreated {
                    at: created.into(),
                    title: "Keep assignee in prose".into(),
                    state: "todo".into(),
                },
                StoryEvent::StoryPrioritySet {
                    at: created.into(),
                    priority: Priority::Low,
                },
                StoryEvent::StoryTypeSet {
                    at: created.into(),
                    story_type: "normal".into(),
                },
            ];
            if story == 2 {
                events.push(StoryEvent::StoryClosedAndArchived {
                    at: created.into(),
                    state: "done".into(),
                });
            }
            if story == 3 {
                events.push(StoryEvent::StoryDeleted {
                    at: created.into(),
                    reason: "legacy deletion".into(),
                });
            }
            let snapshot = fold_story(&format!("P-{story}"), &events, &map).unwrap();
            let mut old = serde_json::to_value(&snapshot).unwrap();
            old["assignee"] = "ada".into();
            old["updated_at"] = last.into();
            for (index, event) in events.iter().enumerate() {
                let raw = serde_json::to_string(event).unwrap();
                let kind = serde_json::to_value(event).unwrap()["kind"]
                    .as_str()
                    .unwrap()
                    .to_string();
                conn.execute("INSERT INTO events (project_id, story_no, seq, global_seq, kind, at, payload, command, actor) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'fixture', 'test:user')", params![project, story, index+1, story*10+index as i64, kind, created, raw]).unwrap();
            }
            conn.execute("INSERT INTO events (project_id,story_no,seq,global_seq,kind,at,payload) VALUES (?1,?2,5,?3,?4,?5,?6)", params![project,story,story*10+5,UNKNOWN_KIND,last,serde_json::json!({"kind":UNKNOWN_KIND,"at":last,"actor":"ada"}).to_string()]).unwrap();
            for (seq, kind) in [(6, "StoryAssigned"), (7, "StoryAssigneeCleared")] {
                let raw = serde_json::json!({"kind":kind,"at":last,"member_id":"ada"}).to_string();
                conn.execute("INSERT INTO events (project_id, story_no, seq, global_seq, kind, at, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![project,story,seq,story*10+seq,kind,last,raw]).unwrap();
            }
            conn.execute("INSERT INTO stories (project_id,story_no,head_seq,head_global_seq,title,state,superstate,priority,priority_rank,story_type,assignee,archived,created_at,updated_at,closed_at,snapshot,hidden_at) VALUES (?1,?2,7,?3,?4,?5,?6,'low',3,'normal','ada',?7,?8,?9,?10,?11,?12)", params![project,story,story*10+7,snapshot.title,snapshot.state,snapshot.superstate.as_str(),snapshot.closed_at.is_some(),created,last,snapshot.closed_at,old.to_string(),snapshot.hidden_at]).unwrap();
        }
    }
    let surviving = |conn: &rusqlite::Connection| {
        conn.prepare("SELECT project_id,story_no,seq,global_seq,payload,command,actor FROM events WHERE kind NOT IN ('StoryAssigned','StoryAssigneeCleared') ORDER BY project_id,global_seq").unwrap().query_map([], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,i64>(3)?,r.get::<_,String>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Option<String>>(6)?))).unwrap().collect::<Result<Vec<_>,_>>().unwrap()
    };
    let before = surviving(&conn);
    conn.execute_batch("CREATE TRIGGER fail_purge BEFORE UPDATE OF snapshot ON stories WHEN OLD.story_no=2 BEGIN SELECT RAISE(ABORT, 'forced purge failure'); END;").unwrap();
    assert!(
        store
            .migrate()
            .unwrap_err()
            .to_string()
            .contains("forced purge failure")
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM events WHERE kind IN ('StoryAssigned','StoryAssigneeCleared')",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        12
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM pragma_table_info('stories') WHERE name='assignee'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert!(
        conn.execute("DELETE FROM events WHERE project_id=1 AND story_no=1", [])
            .is_err()
    );
    assert_eq!(surviving(&conn), before);
    conn.execute_batch("DROP TRIGGER fail_purge").unwrap();
    drop(conn);
    let report = store.migrate().unwrap();
    let backup = rusqlite::Connection::open(report.backup.unwrap()).unwrap();
    assert_eq!(backup.query_row("SELECT count(*) FROM events WHERE kind IN ('StoryAssigned','StoryAssigneeCleared')", [], |r| r.get::<_,i64>(0)).unwrap(), 12);
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    assert_eq!(surviving(&conn), before);
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM events WHERE kind IN ('StoryAssigned','StoryAssigneeCleared')",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM projects WHERE next_global_seq=100",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        2
    );
    assert_eq!(conn.query_row("SELECT count(*) FROM stories WHERE updated_at=?1 AND json_extract(snapshot,'$.updated_at')=?1 AND json_type(snapshot,'$.assignee') IS NULL AND head_seq=(SELECT max(seq) FROM events e WHERE e.project_id=stories.project_id AND e.story_no=stories.story_no)", [created], |r| r.get::<_,i64>(0)).unwrap(),6);
    assert!(
        conn.execute("DELETE FROM events WHERE project_id=1 AND story_no=1", [])
            .is_err()
    );
    assert!(conn.execute("UPDATE events SET at='bad'", []).is_err());
    assert!(store.migrate().unwrap().is_noop());
    for project in 1..=2 {
        let project = storyhook::store::ProjectId::new(project);
        let diff = storyhook::store::rebuild::diff_read_model(&store, project).unwrap();
        assert!(diff.is_clean(), "{diff:?}");
        let ctx = storyhook::service::Ctx::new(
            &store,
            project,
            dir.path(),
            storyhook::env::Environment::at(dir.path()),
        );
        storyhook::service::StoryService::new(&ctx)
            .comment("P-1", "after purge")
            .unwrap();
        assert!(
            storyhook::store::rebuild::diff_read_model(&store, project)
                .unwrap()
                .is_clean()
        );
    }
}

#[test]
fn migration_removes_member_storage_and_assignment_column() {
    use storyhook::store::{SqliteStore, Store, migrate};
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..47]).unwrap();
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    conn.execute_batch("INSERT INTO projects (id, uuid, slug, name, prefix, created_at) VALUES (1, 'one', 'one', 'One', 'ONE', '2026-01-01T00:00:00Z'); INSERT INTO project_members (project_id, member_id, display_name, created_at) VALUES (1, 'ada', 'Ada', '2026-01-01T00:00:00Z');").unwrap();
    drop(conn);
    store.migrate().unwrap();
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    let members: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='project_members'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(members, 0);
    let assignee: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('stories') WHERE name='assignee'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(assignee, 0);
    assert!(store.migrate().unwrap().is_noop());
}

#[test]
fn assignment_is_absent_from_current_commands_and_outputs() {
    let dir = scratch_dir();
    let env = TestEnv::shared();
    env.story(dir.path())
        .args(["project", "new", "--prefix", "NA"])
        .assert()
        .success();
    env.story(dir.path())
        .args(["new", "Single user"])
        .assert()
        .success()
        .stdout(predicate::str::contains("assignee").not());
    for args in [
        vec!["assign", "NA-1", "ada"],
        vec!["member", "add", "Ada"],
        vec!["new", "Rejected", "--assignee", "ada"],
        vec!["list", "--assignee", "ada"],
        vec!["set", "NA-1", "--assignee", "ada"],
        vec!["set", "NA-1", "--json", "{\"assignee\":null}"],
    ] {
        env.story(dir.path()).args(args).assert().failure();
    }
    for args in [vec!["show", "NA-1", "--json"], vec!["export"]] {
        env.story(dir.path())
            .args(args)
            .assert()
            .success()
            .stdout(predicate::str::contains("\"assignee\"").not())
            .stdout(predicate::str::contains("\"members\"").not());
    }
}
