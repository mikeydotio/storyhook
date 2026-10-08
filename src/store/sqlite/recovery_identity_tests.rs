//! Exercise the SQL boundary independently of native recovery proof construction.
use super::{host_recovery, integration_recovery};
use crate::store::{
    GlobalSeq, HostRecoveryPending, IntegrationPending, IntegrationReadmission, ProjectId, StoryNo,
};
use rusqlite::{Connection, params, types::Value};
use serde_json::json;

fn database() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    // Minimal parent catalog; the recovery tables and their checks/triggers are
    // the actual recovery migration. No filesystem, daemon or subprocess work.
    conn.execute_batch(
        "PRAGMA foreign_keys=ON;
         CREATE TABLE projects(id INTEGER PRIMARY KEY);
         CREATE TABLE stories(project_id INTEGER,story_no INTEGER,
             PRIMARY KEY(project_id,story_no));
         INSERT INTO projects VALUES(1),(2);",
    )
    .unwrap();
    conn.execute_batch(include_str!("../schema/0059_shared_recovery_ownership.sql"))
        .unwrap();
    for project in [1_i64, 2] {
        conn.execute(
            "INSERT INTO stories(project_id,story_no) VALUES(?1,?2)",
            params![project, i64::MAX],
        )
        .unwrap();
    }
    conn
}

#[test]
fn recovery_pending_sqlite_ids_roundtrip_without_narrowing_or_project_aliasing() {
    macro_rules! check {
        ($record:ident, $insert:path, $read:path, $table:literal) => {{
            let conn = database();
            let first = $record {
                id: "original".into(),
                project: ProjectId::new(1),
                story: StoryNo::new(i64::MAX),
                generation: GlobalSeq::new(i64::MAX - 1),
                evidence: json!({"original":true}),
            };
            let mut second = first.clone();
            second.id = "other-project".into();
            second.project = ProjectId::new(2);
            second.generation = GlobalSeq::new(i64::MAX);
            $insert(&conn, &first).unwrap();
            $insert(&conn, &second).unwrap();
            $insert(&conn, &first).unwrap(); // exact immutable replay
            assert_eq!($read(&conn, first.project).unwrap(), [first.clone()]);
            assert_eq!($read(&conn, second.project).unwrap(), [second]);
            let raw: (i64, i64, String, String) = conn
                .query_row(
                    concat!("SELECT story_no,generation,typeof(story_no),typeof(generation) FROM ", $table, " WHERE id='original'"),
                    [],
                    |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
                )
                .unwrap();
            assert_eq!(raw, (i64::MAX, i64::MAX - 1, "integer".into(), "integer".into()));
            let mut replacement = first.clone();
            replacement.generation = GlobalSeq::new(7);
            assert!($insert(&conn, &replacement).is_err());
            assert_eq!($read(&conn, first.project).unwrap(), [first]);
        }};
    }
    check!(
        HostRecoveryPending,
        host_recovery::insert_pending,
        host_recovery::pending,
        "host_recovery_pending"
    );
    check!(
        IntegrationPending,
        integration_recovery::insert_pending,
        integration_recovery::pending,
        "integration_pending"
    );
    check!(
        IntegrationReadmission,
        integration_recovery::insert_readmission,
        integration_recovery::readmissions,
        "integration_readmissions"
    );
}

#[test]
fn recovery_pending_sqlite_rejects_noninteger_stored_identity_without_coercion() {
    macro_rules! check {
        ($read:path, $table:literal) => {{
            for column in ["story_no", "generation"] {
                for malformed in [Value::Text("not-an-id".into()), Value::Real(1.5), Value::Blob(vec![1,2])] {
                    let conn = database();
                    // Deliberately model corrupt/imported storage. Only this
                    // scratch connection disables foreign keys to inject an
                    // invalid story identity; production constraints stay intact.
                    conn.pragma_update(None, "foreign_keys", false).unwrap();
                    let (story, generation) = if column == "story_no" {
                        (malformed.clone(), Value::Integer(9))
                    } else {
                        (Value::Integer(i64::MAX), malformed.clone())
                    };
                    conn.execute(
                        concat!("INSERT INTO ", $table, "(id,project_id,story_no,generation,evidence) VALUES('corrupt',1,?1,?2,'{}')"),
                        params![story,generation],
                    ).unwrap();
                    let error = $read(&conn, ProjectId::new(1)).expect_err(concat!($table, " must not coerce malformed identity"));
                    assert!(matches!(&error, crate::store::StoreError::Sqlite(rusqlite::Error::InvalidColumnType(_, _, _))), "{column} {malformed:?}: {error}");
                    assert!($read(&conn, ProjectId::new(2)).unwrap().is_empty());
                }
            }
        }};
    }
    check!(host_recovery::pending, "host_recovery_pending");
    check!(integration_recovery::pending, "integration_pending");
    check!(
        integration_recovery::readmissions,
        "integration_readmissions"
    );
}
