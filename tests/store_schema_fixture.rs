//! The committed schema-version-1 fixture database.
//!
//! Every future migration needs an *old* database to migrate. Once this
//! version's writing code has moved on, one can no longer be produced — so one
//! is produced now, committed, and checked on every run against a database
//! freshly built from **migration 1 alone**. The check is what keeps the
//! fixture honest: a migration edited in place rather than added as a new
//! version would make the two disagree.
//!
//! **It stays at v1 forever.** Version 2 exists now (`commit_links`), and
//! regenerating the fixture to match it would destroy the only thing it is for.
//! `the_committed_fixture_migrates_forward` is the test that proves it is still
//! doing its job: it opens the file with the *whole* list and asserts a real
//! migration ran.
//!
//! Regenerate with `tests/fixtures/schema/regenerate.sh`, which is this same
//! test with `STORYHOOK_REGENERATE_SCHEMA_FIXTURE=1` set. Regenerating is a
//! deliberate act with a reviewable diff, never a silent side effect of a
//! failing run.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use storyhook::domain::{Priority, StateDef, StoryEvent, SuperState, TypeDef, fold_story};
use storyhook::store::migrate;
use storyhook::store::{
    EventSeq, ExpectedSeq, NewProject, ReadOps, SqliteStore, Store, StoryNo, WriteOps,
};
use storyhook_test_support::scratch_dir;

fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/schema/v1.db")
}

/// Every object SQLite knows about, in a stable order — the comparable form of
/// "the schema".
fn schema_objects(path: &Path) -> Vec<(String, String, String)> {
    let conn = Connection::open(path).unwrap();
    conn.prepare(
        "SELECT type, name, COALESCE(sql, '') FROM sqlite_master \
         WHERE name NOT LIKE 'sqlite_%' ORDER BY type, name",
    )
    .unwrap()
    .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

/// Builds a schema-v1 database carrying one small but *representative*
/// project: two stories, a relation (so both mirror rows exist), labels,
/// custom states and types, a member, settings, and a github base.
///
/// Representative matters more than large: a future migration that has to
/// rewrite `story_relations` needs a fixture where both directions of an edge
/// exist, and one that has to touch settings needs a row where every optional
/// column is populated.
fn build(path: &Path) {
    let store = SqliteStore::open(path).unwrap();
    // Migration 1 only. This is a *v1* fixture; applying the whole list would
    // produce a database at the current version, which is the one thing a
    // fixture that exists to be old must never be.
    store.migrate_with(&migrate::MIGRATIONS[..1]).unwrap();

    let states = vec![
        StateDef {
            slug: "todo".into(),
            super_state: SuperState::Open,
            role: None,
            description: Some("Not started".into()),
        },
        StateDef {
            slug: "in-progress".into(),
            super_state: SuperState::Open,
            role: Some("active".into()),
            description: None,
        },
        StateDef {
            slug: "done".into(),
            super_state: SuperState::Closed,
            role: None,
            description: Some("Finished".into()),
        },
    ];

    store
        .write(|tx| {
            let project = tx.create_project(&NewProject {
                uuid: "8f14e45f-ea8e-4b46-9f2c-000000000001".into(),
                slug: "fixture".into(),
                name: "Schema fixture".into(),
                prefix: "SH".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            })?;
            tx.put_states(project, &states)?;
            tx.put_types(
                project,
                &[
                    TypeDef {
                        slug: "feature".into(),
                        description: Some("New capability".into()),
                    },
                    TypeDef {
                        slug: "bug".into(),
                        description: None,
                    },
                ],
            )?;
            tx.put_member(
                project,
                &storyhook::domain::Member {
                    id: "ada".into(),
                    display_name: "Ada Lovelace".into(),
                    email: Some("ada@example.com".into()),
                    github: Some("ada".into()),
                    created_at: "2026-01-01T00:00:00Z".into(),
                },
            )?;
            tx.put_settings(
                project,
                &storyhook::store::ProjectSettings {
                    sync_auto_transition: Some(true),
                    doctor_stale_threshold: Some("14d".into()),
                    github_sync: Some(serde_json::json!({
                        "github": {"owner": "mikeydotio", "repo": "storyhook"},
                        "sync": {"mode": "manual"}
                    })),
                },
            )?;

            let state_map = tx.state_map(project)?;
            for (no, events) in [
                (
                    StoryNo::new(1),
                    vec![
                        StoryEvent::StoryCreated {
                            at: "2026-01-01T00:00:00Z".into(),
                            title: "First story".into(),
                            state: "todo".into(),
                        },
                        StoryEvent::StoryLabelsSet {
                            at: "2026-01-01T00:01:00Z".into(),
                            labels: vec!["alpha".into(), "beta".into()],
                        },
                        StoryEvent::StoryPrioritySet {
                            at: "2026-01-01T00:02:00Z".into(),
                            priority: Priority::High,
                        },
                        StoryEvent::StoryRelationshipAdded {
                            at: "2026-01-01T00:03:00Z".into(),
                            other_id: "SH-2".into(),
                            relation: "blocks".into(),
                        },
                    ],
                ),
                (
                    StoryNo::new(2),
                    vec![
                        StoryEvent::StoryCreated {
                            at: "2026-01-01T00:00:10Z".into(),
                            title: "Second story".into(),
                            state: "done".into(),
                        },
                        StoryEvent::StoryRelationshipAdded {
                            at: "2026-01-01T00:03:00Z".into(),
                            other_id: "SH-1".into(),
                            relation: "blocked-by".into(),
                        },
                        StoryEvent::StoryClosedAndArchived {
                            at: "2026-01-01T00:04:00Z".into(),
                            state: "done".into(),
                        },
                    ],
                ),
            ] {
                assert_eq!(tx.allocate_story_no(project)?, no);
                let head =
                    tx.append_events(project, no, ExpectedSeq::Exact(EventSeq::ZERO), &events)?;
                let snapshot = fold_story(&no.to_id("SH"), &events, &state_map).unwrap();
                tx.put_story(project, &snapshot, head)?;
            }

            let base = tx.story(project, StoryNo::new(1))?.unwrap().snapshot;
            tx.put_github_base(project, StoryNo::new(1), &base)?;
            Ok(())
        })
        .unwrap();

    // `project_paths` in raw SQL, because the writer that used to fill it is
    // deleted (SH-119) and this fixture is *v1*, where the table still exists.
    // Two rows, a main tree and a linked worktree, so that migration 8's
    // carry-over — main becomes `projects.checkout_path`, worktrees are
    // dropped — has both cases to act on when this file is migrated forward.
    let conn = Connection::open(path).unwrap();
    for (checkout, kind) in [("/fixture/main", "main"), ("/fixture/wt-a", "worktree")] {
        conn.execute(
            "INSERT INTO project_paths (project_id, path, kind, last_seen_at) \
             VALUES ((SELECT id FROM projects WHERE slug = 'fixture'), ?1, ?2, \
                     '2026-01-01T00:00:00Z')",
            rusqlite::params![checkout, kind],
        )
        .unwrap();
    }
}

#[test]
fn the_committed_fixture_is_a_schema_v1_database_that_still_matches_the_migrations() {
    let regenerating = std::env::var_os("STORYHOOK_REGENERATE_SCHEMA_FIXTURE").is_some();
    let fixture = fixture_path();

    if regenerating {
        std::fs::create_dir_all(fixture.parent().unwrap()).unwrap();
        for suffix in ["", "-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{suffix}", fixture.display()));
            if path.exists() {
                std::fs::remove_file(&path).unwrap();
            }
        }
        build(&fixture);
        // Checkpoint and close so the committed artifact is a single file with
        // no sidecars — a `-wal` file committed beside it would be a database
        // whose contents depend on files git was never told about.
        let conn = Connection::open(&fixture).unwrap();
        conn.execute_batch(
            "PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode = DELETE; VACUUM;",
        )
        .unwrap();
        drop(conn);
        for suffix in ["-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{suffix}", fixture.display()));
            if path.exists() {
                std::fs::remove_file(&path).unwrap();
            }
        }
        eprintln!("regenerated {}", fixture.display());
        return;
    }

    assert!(
        fixture.exists(),
        "the schema fixture is missing; regenerate it with \
         tests/fixtures/schema/regenerate.sh"
    );

    let scratch = scratch_dir();
    let fresh = scratch.path().join("fresh.db");
    // Copied out of the repository first: opening the committed file would put
    // it into write-ahead logging mode and dirty the working tree.
    let working = scratch.path().join("fixture.db");
    std::fs::copy(&fixture, &working).unwrap();
    SqliteStore::open(&fresh)
        .unwrap()
        .migrate_with(&migrate::MIGRATIONS[..1])
        .unwrap();

    assert_eq!(
        schema_objects(&working),
        schema_objects(&fresh),
        "the committed schema-v1 fixture no longer matches what the migration list produces; \
         a migration was edited in place instead of a new one being added"
    );
}

/// Opening the fixture with the current migration list carries it forward, and
/// the project inside it survives the trip.
///
/// This is what the fixture is *for*. A v1 database that nothing ever migrates
/// proves nothing; the value is in running today's migrations against a
/// database today's code did not write.
#[test]
fn the_committed_fixture_migrates_forward() {
    let fixture = fixture_path();
    let scratch = scratch_dir();
    let working = scratch.path().join("fixture.db");
    std::fs::copy(&fixture, &working).unwrap();

    let store = SqliteStore::open(&working).unwrap();
    let report = store.migrate().unwrap();

    assert_eq!(report.from_version, 1, "the committed fixture is at v1");
    assert_eq!(report.to_version, migrate::current_schema_version());
    assert_eq!(
        report.applied,
        migrate::MIGRATIONS[1..]
            .iter()
            .map(|m| m.name.to_string())
            .collect::<Vec<_>>(),
        "every migration after the first must have run"
    );
    assert!(
        report.backup.is_some(),
        "a database with a schema is backed up before it is migrated"
    );
    assert!(
        store.migrate().unwrap().is_noop(),
        "and it is at the current version afterwards"
    );
}

#[test]
fn the_committed_fixture_reads_back_through_the_store() {
    let fixture = fixture_path();
    let scratch = scratch_dir();
    let working = scratch.path().join("fixture.db");
    std::fs::copy(&fixture, &working).unwrap();

    let store = SqliteStore::open(&working).unwrap();
    store.migrate().unwrap();

    let (project, stories, edges, settings) = store
        .read(|tx| {
            let project = tx.project_by_slug("fixture")?.unwrap();
            let stories = tx.stories(project.id, &storyhook::store::StoryQuery::all())?;
            let edges = tx.relations_to(project.id, StoryNo::new(2))?;
            let settings = tx.settings(project.id)?;
            Ok((project, stories, edges, settings))
        })
        .unwrap();

    assert_eq!(project.prefix, "SH");
    assert_eq!(stories.len(), 2);
    assert_eq!(stories[0].labels, vec!["alpha", "beta"]);
    assert!(stories[1].archived, "SH-2 was closed and archived");
    assert_eq!(edges.len(), 1, "the mirror row must be in the fixture");
    assert_eq!(settings.doctor_stale_threshold.as_deref(), Some("14d"));
}
