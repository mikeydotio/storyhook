//! The migration framework, its backup discipline, and the
//! forward-compatibility gate.
//!
//! Everything here is file-backed. A `:memory:` database cannot be reopened,
//! has no write-ahead log, and cannot be backed up — which are the three things
//! this module is about.

use std::path::Path;

use rusqlite::Connection;
use storyhook::domain::StoryEvent;
use storyhook::store::migrate::{self, Migration};
use storyhook::store::{SqliteStore, Store, StoreConfig, StoreError, StoryNo};
use storyhook_test_support::scratch_dir;

/// A second migration, used to exercise the parts of the framework that only
/// exist once a database has something in it worth backing up. The tree's own
/// migrations are the ones a store really applies; this appends one *past* them
/// so a test can choose what the next step does.
fn with_extra_migration(sql: &'static str) -> Vec<Migration> {
    let mut list = migrate::MIGRATIONS.to_vec();
    list.push(Migration {
        version: NEXT_VERSION,
        name: "test-only",
        sql,
    });
    list
}

/// The version [`with_extra_migration`]'s step produces — one past the tree's.
///
/// Derived rather than written down, because a hard-coded `2` is a test that
/// starts failing the day a real migration takes that number, which is what
/// happened when the commit-links migration did.
const NEXT_VERSION: u32 = migrate::MIGRATIONS.len() as u32 + 1;

fn user_version(path: &Path) -> u32 {
    let conn = Connection::open(path).unwrap();
    conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .unwrap()
        .try_into()
        .unwrap()
}

#[test]
fn a_fresh_database_migrates_to_the_current_version() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();

    let report = store.migrate().unwrap();

    assert_eq!(report.from_version, 0);
    assert_eq!(report.to_version, migrate::current_schema_version());
    assert_eq!(
        report.applied,
        migrate::MIGRATIONS
            .iter()
            .map(|m| m.name.to_string())
            .collect::<Vec<_>>(),
        "a fresh database applies every migration in the tree, in order"
    );
    assert!(!report.is_noop());
    assert_eq!(
        user_version(store.path()),
        migrate::current_schema_version()
    );
}

#[test]
fn an_empty_database_is_not_backed_up_because_it_has_nothing_to_lose() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();

    let report = store.migrate().unwrap();

    assert_eq!(report.backup, None);
    assert!(
        !store.backup_dir().exists(),
        "an empty database should not have produced a backup directory"
    );
}

#[test]
fn migrating_twice_changes_nothing() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate().unwrap();

    let report = store.migrate().unwrap();

    assert!(report.is_noop());
    assert_eq!(report.from_version, migrate::current_schema_version());
    assert_eq!(report.to_version, migrate::current_schema_version());
    assert_eq!(report.backup, None);
}

#[test]
fn the_migration_history_records_every_step() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate().unwrap();

    let conn = Connection::open(store.path()).unwrap();
    let rows: Vec<(u32, String)> = conn
        .prepare("SELECT version, name FROM schema_migrations ORDER BY version")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();

    assert_eq!(
        rows,
        migrate::MIGRATIONS
            .iter()
            .map(|m| (m.version, m.name.to_string()))
            .collect::<Vec<_>>()
    );
    let applied_at: String = conn
        .query_row("SELECT applied_at FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert!(
        applied_at.ends_with('Z') && applied_at.contains('T'),
        "applied_at should be RFC3339 UTC, got `{applied_at}`"
    );
}

#[test]
fn a_migrated_database_reopens_at_its_recorded_version() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    SqliteStore::open(&path).unwrap().migrate().unwrap();

    // A fresh process would do exactly this: open, find nothing pending.
    let reopened = SqliteStore::open(&path).unwrap();
    assert!(reopened.migrate().unwrap().is_noop());
}

#[test]
fn write_ahead_logging_survives_a_reopen() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    SqliteStore::open(&path).unwrap().migrate().unwrap();

    let conn = Connection::open(&path).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
}

// ---------------------------------------------------------------------------
// The forward-compatibility gate (SH-54)
// ---------------------------------------------------------------------------

#[test]
fn a_database_from_a_newer_storyhook_is_refused_with_one_clear_message() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    SqliteStore::open(&path).unwrap().migrate().unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch("PRAGMA user_version = 99")
        .unwrap();

    let error = SqliteStore::open(&path).unwrap_err();

    match error {
        StoreError::SchemaTooNew { found, supported } => {
            assert_eq!(found, 99);
            assert_eq!(supported, migrate::current_schema_version());
        }
        other => panic!("expected SchemaTooNew, got: {other}"),
    }
    let message = SqliteStore::open(&path).unwrap_err().to_string();
    assert!(message.contains("newer storyhook"), "{message}");
    assert!(message.contains("story update"), "{message}");
}

#[test]
fn migrating_a_database_from_a_newer_storyhook_is_also_refused() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    let store = SqliteStore::open(&path).unwrap();
    store.migrate().unwrap();
    // Bumped behind the store's back, as a newer binary sharing the file would.
    Connection::open(&path)
        .unwrap()
        .execute_batch("PRAGMA user_version = 5")
        .unwrap();

    let error = store.migrate().unwrap_err();

    assert!(
        matches!(error, StoreError::SchemaTooNew { found: 5, .. }),
        "expected SchemaTooNew, got: {error}"
    );
}

// ---------------------------------------------------------------------------
// Backups
// ---------------------------------------------------------------------------

#[test]
fn a_pending_migration_takes_a_verified_backup_first() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate().unwrap();

    let report = store
        .migrate_with(&with_extra_migration("CREATE TABLE later (x INTEGER);"))
        .unwrap();

    let backup = report
        .backup
        .expect("a database with a schema must be backed up");
    assert!(backup.exists(), "{} was not written", backup.display());
    assert!(
        backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains(&format!("-v{}", migrate::current_schema_version())),
        "the backup should name the version it was taken from: {}",
        backup.display()
    );
    assert_eq!(report.from_version, migrate::current_schema_version());
    assert_eq!(report.to_version, NEXT_VERSION);
}

#[test]
fn the_backup_is_a_real_restorable_database_not_a_hot_file_copy() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate().unwrap();
    // Leave data in the write-ahead log, unflushed. `fs::copy` of this file
    // would produce a backup missing the row.
    Connection::open(store.path())
        .unwrap()
        .execute_batch(
            "INSERT INTO projects (uuid, slug, name, prefix, created_at) \
             VALUES ('u', 's', 'n', 'SH', '2026-01-01T00:00:00Z')",
        )
        .unwrap();

    let report = store
        .migrate_with(&with_extra_migration("CREATE TABLE later (x INTEGER);"))
        .unwrap();

    let backup = Connection::open(report.backup.unwrap()).unwrap();
    let verdict: String = backup
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(verdict, "ok");
    let slug: String = backup
        .query_row("SELECT slug FROM projects", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        slug, "s",
        "the backup must contain data that was still in the write-ahead log"
    );
    let version: i64 = backup
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        u32::try_from(version).unwrap(),
        migrate::current_schema_version(),
        "the backup is of the pre-migration schema"
    );
}

#[test]
fn a_backup_that_cannot_be_written_stops_the_migration() {
    let dir = scratch_dir();
    let store = SqliteStore::open_with(StoreConfig {
        // A *file* where the backup directory should be: `create_dir_all` on it
        // fails, which is the cheapest honest way to make the backup step fail.
        backup_dir: dir.path().join("occupied"),
        ..StoreConfig::new(dir.path().join("store.db"))
    })
    .unwrap();
    store.migrate().unwrap();
    std::fs::write(dir.path().join("occupied"), b"not a directory").unwrap();

    let error = store
        .migrate_with(&with_extra_migration("CREATE TABLE later (x INTEGER);"))
        .unwrap_err();

    assert!(
        matches!(error, StoreError::Backup(_)),
        "expected a Backup error, got: {error}"
    );
    assert!(
        error.to_string().contains("no migration was attempted"),
        "the message must say the database was left alone: {error}"
    );
    assert_eq!(
        user_version(store.path()),
        migrate::current_schema_version(),
        "the database must be untouched when its backup failed"
    );
}

#[test]
fn a_backup_that_cannot_be_verified_stops_the_migration() {
    use storyhook::store::FaultPoint;
    use storyhook::store::fault::{FaultAction, arm};

    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate().unwrap();

    let _fault = arm(
        FaultPoint::BackupVerify,
        FaultAction::Fail("integrity check says no".to_string()),
    );
    let error = store
        .migrate_with(&with_extra_migration("CREATE TABLE later (x INTEGER);"))
        .unwrap_err();

    assert!(
        matches!(error, StoreError::Backup(_)),
        "expected a Backup error, got: {error}"
    );
    assert_eq!(
        user_version(store.path()),
        migrate::current_schema_version(),
        "an unverifiable backup must leave the database at its old version"
    );
}

// ---------------------------------------------------------------------------
// Failure isolation
// ---------------------------------------------------------------------------

#[test]
fn a_failing_migration_leaves_the_database_at_its_previous_version() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate().unwrap();

    let error = store
        .migrate_with(&with_extra_migration("THIS IS NOT SQL;"))
        .unwrap_err();

    assert!(
        matches!(error, StoreError::Migration { version, .. } if version == NEXT_VERSION),
        "expected a Migration error naming version {NEXT_VERSION}, got: {error}"
    );
    assert_eq!(
        user_version(store.path()),
        migrate::current_schema_version()
    );
    // And the store is still usable: the failed transaction rolled back rather
    // than leaving a half-applied schema behind.
    assert!(store.migrate().unwrap().is_noop());
}

#[test]
fn a_migration_interrupted_between_steps_keeps_what_it_finished() {
    use storyhook::store::FaultPoint;
    use storyhook::store::fault::{FaultAction, arm};

    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();

    // Fire on the *first* pending migration of a fresh database, before
    // anything is applied.
    let fault = arm(
        FaultPoint::MidMigration,
        FaultAction::Fail("interrupted".to_string()),
    );
    assert!(store.migrate().is_err());
    drop(fault);

    assert_eq!(
        user_version(store.path()),
        0,
        "nothing should have been applied"
    );
    assert_eq!(
        store.migrate().unwrap().to_version,
        migrate::current_schema_version()
    );
}

#[test]
fn concurrent_migrations_of_one_store_apply_exactly_once() {
    let dir = scratch_dir();
    let store = std::sync::Arc::new(SqliteStore::open(dir.path().join("store.db")).unwrap());

    let applied: usize = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let store = std::sync::Arc::clone(&store);
                scope.spawn(move || store.migrate().unwrap().applied.len())
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).sum()
    });

    assert_eq!(
        applied,
        migrate::MIGRATIONS.len(),
        "the migrations must be applied exactly once between eight concurrent \
         callers, not once each"
    );
    assert_eq!(
        user_version(store.path()),
        migrate::current_schema_version()
    );
}

// ---------------------------------------------------------------------------
// Migration 2: the commit-link backfill
// ---------------------------------------------------------------------------

/// A store at schema v1 with one project and one story, ready to be carried
/// forward. Returns the store, its project and the story number.
fn v1_store_with_a_story(dir: &Path) -> (SqliteStore, storyhook::store::ProjectId) {
    use storyhook::store::{NewProject, WriteOps};

    let store = SqliteStore::open(dir.join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..1]).unwrap();
    let project = store
        .write(|tx| {
            tx.create_project(&NewProject {
                uuid: "backfill".into(),
                slug: "backfill".into(),
                name: "backfill".into(),
                prefix: "SH".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            })
        })
        .unwrap();
    (store, project)
}

/// Writes `events` onto story 1 with **raw SQL**, the way a build that predates
/// `story_commit_links` would have.
///
/// Deliberately not through `WriteOps::append_events`: that path projects link
/// records into a table a v1 database does not have, which is the whole reason
/// the backfill exists. A fixture that could only be built by the code the
/// migration is catching up with would be testing nothing.
fn append_v1_events(
    store: &SqliteStore,
    project: storyhook::store::ProjectId,
    events: &[StoryEvent],
) {
    let conn = Connection::open(store.path()).unwrap();
    for (index, event) in events.iter().enumerate() {
        let value = serde_json::to_value(event).unwrap();
        let seq = i64::try_from(index).unwrap() + 1;
        conn.execute(
            "INSERT INTO events (project_id, story_no, seq, global_seq, kind, at, payload) \
             VALUES (?1, 1, ?2, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                project.get(),
                seq,
                value["kind"].as_str().unwrap(),
                value["at"].as_str().unwrap(),
                serde_json::to_string(&value).unwrap(),
            ],
        )
        .unwrap();
    }
}

/// Every `[git]` comment a project already had becomes a link record.
///
/// Without this, the first `commit-sync` after an upgrade re-links every commit
/// inside its window: the events say the story is linked, the table does not,
/// and the table is the authority now.
#[test]
fn the_migration_backfills_link_records_from_pre_existing_git_comments() {
    use storyhook::store::ReadOps;

    let dir = scratch_dir();
    let (store, project) = v1_store_with_a_story(dir.path());
    append_v1_events(
        &store,
        project,
        &[
            StoryEvent::StoryCreated {
                at: "2026-01-01T00:00:00Z".into(),
                title: "Linked the old way".into(),
                state: "todo".into(),
            },
            StoryEvent::StoryCommentAdded {
                at: "2026-01-02T00:00:00Z".into(),
                text: "[git] abc1234: feat: the work".into(),
            },
            StoryEvent::StoryCommentAdded {
                at: "2026-01-03T00:00:00Z".into(),
                text: "[git] def5678: fix: more of it".into(),
            },
            // Not a link record: a human comment that happens to open the same
            // way. `rebase` is not hexadecimal.
            StoryEvent::StoryCommentAdded {
                at: "2026-01-04T00:00:00Z".into(),
                text: "[git] rebase: I squashed this branch".into(),
            },
            // Not a link record either: no colon at all.
            StoryEvent::StoryCommentAdded {
                at: "2026-01-05T00:00:00Z".into(),
                text: "[git] nothing here".into(),
            },
        ],
    );

    store.migrate().unwrap();

    let linked = |sha: &str| {
        store
            .read(|tx| tx.commit_linked(project, StoryNo::new(1), sha))
            .unwrap()
    };
    assert!(
        linked("abc1234"),
        "the first link comment must be backfilled"
    );
    assert!(linked("def5678"), "and the second");
    assert!(
        !linked("rebase"),
        "`[git] rebase:` is prose, not a commit hash — treating it as one would \
         suppress a real commit whose abbreviation nobody can ever produce"
    );
}

/// The same duplicate a human could have typed twice must not fail the
/// migration.
#[test]
fn the_backfill_tolerates_two_comments_naming_one_commit() {
    let dir = scratch_dir();
    let (store, project) = v1_store_with_a_story(dir.path());
    append_v1_events(
        &store,
        project,
        &[
            StoryEvent::StoryCreated {
                at: "2026-01-01T00:00:00Z".into(),
                title: "Linked twice by hand".into(),
                state: "todo".into(),
            },
            StoryEvent::StoryCommentAdded {
                at: "2026-01-02T00:00:00Z".into(),
                text: "[git] abc1234: feat: the work".into(),
            },
            StoryEvent::StoryCommentAdded {
                at: "2026-01-03T00:00:00Z".into(),
                text: "[git] abc1234: feat: the work, said twice".into(),
            },
        ],
    );

    store
        .migrate()
        .expect("a duplicated comment must not fail the migration");
}

/// The SQL backfill and the Rust projection are two spellings of one rule.
///
/// The migration states it in SQL for rows that were already there;
/// `store::sqlite::write::project_commit_link` states it in Rust for events
/// appended afterwards — `story migrate` reads unmigrated `.storyhook` trees
/// full of `[git]` comments, so the second is not a legacy path. Two spellings
/// of one rule is how a rule drifts, so this compares them on the same inputs.
#[test]
fn the_sql_backfill_and_the_rust_parser_agree() {
    use storyhook::domain::git_link_sha;
    use storyhook::store::ReadOps;

    const CASES: [&str; 7] = [
        "[git] abc1234: feat: the work",
        "[git] 0123456789abcdef: a long hash",
        "[git] abc1234: a subject: with a colon in it",
        "[git] rebase: prose, not a hash",
        "[git] nothing here",
        "not a link record at all",
        "[git] : empty",
    ];

    let dir = scratch_dir();
    let (store, project) = v1_store_with_a_story(dir.path());
    let mut events = vec![StoryEvent::StoryCreated {
        at: "2026-01-01T00:00:00Z".into(),
        title: "Every shape".into(),
        state: "todo".into(),
    }];
    for (index, text) in CASES.iter().enumerate() {
        events.push(StoryEvent::StoryCommentAdded {
            at: format!("2026-01-{:02}T00:00:00Z", index + 2),
            text: (*text).to_string(),
        });
    }
    append_v1_events(&store, project, &events);
    store.migrate().unwrap();

    let rows: Vec<String> = store
        .read(|tx| {
            let mut found = Vec::new();
            for text in CASES {
                if let Some(sha) = git_link_sha(text)
                    && tx.commit_linked(project, StoryNo::new(1), sha)?
                {
                    found.push(sha.to_string());
                }
            }
            Ok(found)
        })
        .unwrap();
    let expected: Vec<String> = CASES
        .iter()
        .filter_map(|text| git_link_sha(text).map(str::to_string))
        .collect();
    assert_eq!(
        rows, expected,
        "every hash the Rust parser finds must be a row the SQL backfill wrote"
    );

    // And nothing else got in. The count is the other half of "agree".
    let total: i64 = Connection::open(store.path())
        .unwrap()
        .query_row("SELECT COUNT(*) FROM story_commit_links", [], |row| {
            row.get(0)
        })
        .unwrap();
    let distinct: std::collections::BTreeSet<&String> = expected.iter().collect();
    assert_eq!(
        usize::try_from(total).unwrap(),
        distinct.len(),
        "the SQL backfill must not have written a row the parser rejects — and \
         two comments naming one commit are still one link"
    );
}
