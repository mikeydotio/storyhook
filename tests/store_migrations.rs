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
    with_extra_migration_flagged(sql, false)
}

/// [`with_extra_migration`], choosing whether the appended step declares that it
/// needs foreign keys disabled.
///
/// Split out rather than folded into the caller so every existing test keeps
/// reading as "a migration", and only the rebuild tests mention the flag.
fn with_extra_migration_flagged(sql: &'static str, foreign_keys_off: bool) -> Vec<Migration> {
    let mut list = migrate::MIGRATIONS.to_vec();
    list.push(Migration {
        version: NEXT_VERSION,
        name: "test-only",
        sql,
        foreign_keys_off,
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
        .execute_batch("PRAGMA user_version = 99")
        .unwrap();

    let error = store.migrate().unwrap_err();

    assert!(
        matches!(error, StoreError::SchemaTooNew { found: 99, .. }),
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

// ---------------------------------------------------------------------------
// Rebuilding a referenced table — SH-130
// ---------------------------------------------------------------------------

/// The twelve-step rebuild, as a migration would write it.
///
/// `stories` is the parent of `story_labels`, `story_relations` and friends, so
/// this is the exact shape SH-130's constraint needs and the exact shape that
/// destroys data when foreign keys are left on.
///
/// It opens by dropping `events_reject_delete` and closes by putting it back,
/// and that bracket is **not optional** — it is steps 3 and 8 of SQLite's own
/// procedure, applied to the one trigger that is not attached to `stories` and
/// still has to be reconstructed. `ALTER TABLE … RENAME TO` re-parses every
/// trigger in the schema so it can rewrite references to the table being
/// renamed; since SH-130's second half, `events_reject_delete` references
/// `stories`, and between the `DROP TABLE` and the rename there is no such
/// table to resolve. `a_stories_rebuild_that_leaves_the_append_guard_up_fails`
/// is the measurement that keeps this paragraph true.
const REBUILD_STORIES: &str = "
    DROP TRIGGER events_reject_delete;

    CREATE TABLE stories_new (
        project_id    INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        story_no      INTEGER NOT NULL,
        head_seq      INTEGER NOT NULL,
        title         TEXT NOT NULL,
        state         TEXT NOT NULL,
        superstate    TEXT NOT NULL CHECK (superstate IN ('OPEN', 'CLOSED')),
        priority      TEXT NOT NULL,
        priority_rank INTEGER NOT NULL,
        story_type    TEXT,
        assignee      TEXT,
        awaiting      TEXT,
        deleted       INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1)),
        archived      INTEGER NOT NULL DEFAULT 0 CHECK (archived IN (0, 1)),
        created_at    TEXT NOT NULL,
        updated_at    TEXT NOT NULL,
        closed_at     TEXT,
        description   TEXT,
        snapshot      TEXT NOT NULL,
        PRIMARY KEY (project_id, story_no)
    );
    INSERT INTO stories_new SELECT
        project_id, story_no, head_seq, title, state, superstate, priority,
        priority_rank, story_type, assignee, awaiting, deleted, archived,
        created_at, updated_at, closed_at, description, snapshot
    FROM stories;
    DROP TABLE stories;
    ALTER TABLE stories_new RENAME TO stories;

    CREATE TRIGGER events_reject_delete
    BEFORE DELETE ON events
    WHEN EXISTS (SELECT 1 FROM projects WHERE id = OLD.project_id)
     AND EXISTS (SELECT 1 FROM stories
                 WHERE project_id = OLD.project_id AND story_no = OLD.story_no)
    BEGIN
        SELECT RAISE(ABORT, 'events are append-only: DELETE is not permitted');
    END;
";

/// Seeds one project holding one story that carries one label.
///
/// Written straight to the tables rather than through the services: this file
/// tests the migration framework, and a fixture that had to stay valid under
/// every future service rule would be testing something else.
fn seed_a_labelled_story(path: &Path) {
    let conn = Connection::open(path).unwrap();
    // The state catalog is not optional scenery: since SH-130 `stories` carries
    // a composite foreign key into `project_states`, so a story naming a state
    // the project does not define is refused — correctly.
    conn.execute_batch(
        "INSERT INTO projects (id, uuid, slug, name, prefix, created_at)
             VALUES (1, 'u-1', 'proj', 'Proj', 'SH', '2026-01-01T00:00:00Z');
         INSERT INTO project_states (project_id, position, slug, superstate)
             VALUES (1, 0, 'todo', 'OPEN'), (1, 1, 'done', 'CLOSED');
         INSERT INTO stories (project_id, story_no, head_seq, title, state, superstate,
                              priority, priority_rank, created_at, updated_at, snapshot)
             VALUES (1, 1, 1, 'A story', 'todo', 'OPEN', 'none', 4,
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '{}');
         INSERT INTO story_labels (project_id, story_no, label) VALUES (1, 1, 'bug');",
    )
    .unwrap();
}

fn label_count(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM story_labels", [], |row| row.get(0))
        .unwrap()
}

#[test]
fn a_table_rebuild_with_foreign_keys_left_on_silently_destroys_child_rows() {
    // Not a wish — a measurement, kept as a test so the reason
    // `foreign_keys_off` exists cannot be forgotten and quietly deleted.
    //
    // `DROP TABLE` fires every child's ON DELETE CASCADE while foreign keys are
    // enforced. Nothing errors, the transaction COMMITs, and the migration
    // reports success. On the bundled SQLite this empties `story_labels`,
    // `story_relations` and `github_bases` for every project in the store.
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate().unwrap();
    seed_a_labelled_story(store.path());
    assert_eq!(label_count(store.path()), 1);

    let conn = Connection::open(store.path()).unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    migrate::run(
        &conn,
        &with_extra_migration_flagged(REBUILD_STORIES, false),
        &dir.path().join("backups"),
    )
    .expect("the migration reports success — that is the hazard");

    assert_eq!(
        label_count(store.path()),
        0,
        "this is the damage `foreign_keys_off` exists to prevent; if this now \
         reads 1, SQLite changed its cascade behaviour and the flag's \
         justification must be re-measured rather than assumed"
    );
}

#[test]
fn a_rebuild_migration_that_declares_foreign_keys_off_keeps_its_child_rows() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate().unwrap();
    seed_a_labelled_story(store.path());

    let conn = Connection::open(store.path()).unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    migrate::run(
        &conn,
        &with_extra_migration_flagged(REBUILD_STORIES, true),
        &dir.path().join("backups"),
    )
    .unwrap();

    assert_eq!(
        label_count(store.path()),
        1,
        "the label must survive a rebuild of the table it references"
    );
    assert_eq!(user_version(store.path()), NEXT_VERSION);
}

#[test]
fn the_foreign_key_pragma_is_restored_after_a_rebuild_migration() {
    // The connection outlives the migration and is handed straight to the
    // store. A migration that left enforcement off would disarm every foreign
    // key in the process that ran it, which is worse than the damage it avoided.
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate().unwrap();

    let conn = Connection::open(store.path()).unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    migrate::run(
        &conn,
        &with_extra_migration_flagged(REBUILD_STORIES, true),
        &dir.path().join("backups"),
    )
    .unwrap();

    let enforced: bool = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    assert!(
        enforced,
        "foreign key enforcement must be switched back on when the migration ends"
    );
}

#[test]
fn a_rebuild_that_leaves_a_dangling_reference_is_refused() {
    // The price of switching enforcement off is paid here. With foreign keys
    // disabled a rebuild can orphan a child row and COMMIT happily, so the
    // framework runs `PRAGMA foreign_key_check` inside the transaction. This
    // rebuild drops a story its label still names.
    const REBUILD_LOSING_A_ROW: &str = "
        DROP TRIGGER events_reject_delete;
        CREATE TABLE stories_new (
            project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            story_no   INTEGER NOT NULL,
            head_seq   INTEGER NOT NULL,
            title      TEXT NOT NULL,
            state      TEXT NOT NULL,
            superstate TEXT NOT NULL,
            priority   TEXT NOT NULL,
            priority_rank INTEGER NOT NULL,
            story_type TEXT, assignee TEXT, awaiting TEXT,
            deleted    INTEGER NOT NULL DEFAULT 0,
            archived   INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL, closed_at TEXT,
            description TEXT, snapshot TEXT NOT NULL,
            PRIMARY KEY (project_id, story_no)
        );
        DROP TABLE stories;
        ALTER TABLE stories_new RENAME TO stories;
        CREATE TRIGGER events_reject_delete
        BEFORE DELETE ON events
        WHEN EXISTS (SELECT 1 FROM projects WHERE id = OLD.project_id)
         AND EXISTS (SELECT 1 FROM stories
                     WHERE project_id = OLD.project_id AND story_no = OLD.story_no)
        BEGIN
            SELECT RAISE(ABORT, 'events are append-only: DELETE is not permitted');
        END;
    ";

    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate().unwrap();
    seed_a_labelled_story(store.path());

    let conn = Connection::open(store.path()).unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    let error = migrate::run(
        &conn,
        &with_extra_migration_flagged(REBUILD_LOSING_A_ROW, true),
        &dir.path().join("backups"),
    )
    .expect_err("a rebuild that orphans a child row must not commit");

    assert!(
        error.to_string().contains("dangling foreign-key reference"),
        "the failure must name what dangles, got: {error}"
    );
    assert_eq!(
        user_version(store.path()),
        migrate::current_schema_version(),
        "the failed migration must have rolled back"
    );
}

#[test]
fn a_stories_rebuild_that_leaves_the_append_guard_up_fails() {
    // Measured, not assumed, and kept as a test because the cost it records is
    // the price SH-130's purge pays for its guard.
    //
    // `events_reject_delete` names `stories` in its `WHEN` clause, which is
    // what lets a purge delete a story's events and nothing else's. The
    // consequence is that `stories` can no longer be rebuilt the way migration
    // 4 rebuilt it: `ALTER TABLE … RENAME TO` re-parses every trigger in the
    // schema to rewrite references to the renamed table, and between the
    // `DROP TABLE` and the rename there is no `stories` to resolve.
    //
    // The failure is loud, immediate, and rolls back — which is why the cost is
    // acceptable. `REBUILD_STORIES` shows the remedy: drop the guard at the top
    // of the migration and recreate it at the bottom, exactly as SQLite's own
    // twelve-step procedure says to do for every trigger associated with the
    // table being rebuilt.
    const REBUILD_WITHOUT_LOWERING_THE_GUARD: &str = "
        CREATE TABLE stories_new (
            project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
            story_no   INTEGER NOT NULL,
            head_seq   INTEGER NOT NULL,
            title      TEXT NOT NULL,
            state      TEXT NOT NULL,
            superstate TEXT NOT NULL,
            priority   TEXT NOT NULL,
            priority_rank INTEGER NOT NULL,
            story_type TEXT, assignee TEXT, awaiting TEXT,
            deleted    INTEGER NOT NULL DEFAULT 0,
            archived   INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL, closed_at TEXT,
            description TEXT, snapshot TEXT NOT NULL,
            PRIMARY KEY (project_id, story_no)
        );
        INSERT INTO stories_new SELECT
            project_id, story_no, head_seq, title, state, superstate, priority,
            priority_rank, story_type, assignee, awaiting, deleted, archived,
            created_at, updated_at, closed_at, description, snapshot
        FROM stories;
        DROP TABLE stories;
        ALTER TABLE stories_new RENAME TO stories;
    ";

    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate().unwrap();
    seed_a_labelled_story(store.path());

    let conn = Connection::open(store.path()).unwrap();
    let error = migrate::run(
        &conn,
        &with_extra_migration_flagged(REBUILD_WITHOUT_LOWERING_THE_GUARD, true),
        &dir.path().join("backups"),
    )
    .expect_err("SQLite cannot re-parse a trigger naming a table that is not there");

    let message = error.to_string();
    assert!(
        message.contains("events_reject_delete") && message.contains("stories"),
        "the failure must name the trigger and the table it could not resolve, \
         got: {message} (SQLite {})",
        rusqlite::version()
    );
    assert_eq!(
        user_version(store.path()),
        migrate::current_schema_version(),
        "the failed migration must have rolled back"
    );
    assert_eq!(
        label_count(store.path()),
        1,
        "and it must have rolled back before touching anything"
    );
}

// ---------------------------------------------------------------------------
// Migration 8: the resolution index is deleted
// ---------------------------------------------------------------------------

/// A store at schema v7 — the last version that still has `project_paths` —
/// carrying `rows` in it.
///
/// Raw SQL, for the reason `append_v1_events` gives: the writer that filled
/// this table is deleted, so a fixture built through the current API could not
/// produce the state migration 8 exists to carry forward.
fn v7_store_with_paths(dir: &Path, rows: &[(&str, &str)]) -> SqliteStore {
    use storyhook::store::{NewProject, WriteOps};

    let store = SqliteStore::open(dir.join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..7]).unwrap();
    let project = store
        .write(|tx| {
            tx.create_project(&NewProject {
                uuid: "carried".into(),
                slug: "carried".into(),
                name: "carried".into(),
                prefix: "SH".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            })
        })
        .unwrap();
    let conn = Connection::open(store.path()).unwrap();
    for (path, kind) in rows {
        conn.execute(
            "INSERT INTO project_paths (project_id, path, kind, last_seen_at) \
             VALUES (?1, ?2, ?3, '2026-01-01T00:00:00Z')",
            rusqlite::params![project.get(), path, kind],
        )
        .unwrap();
    }
    store
}

/// The recorded checkout of a project, read back through the store.
fn checkout_of(store: &SqliteStore, slug: &str) -> Option<std::path::PathBuf> {
    use storyhook::store::ReadOps;
    store
        .read(|tx| {
            let project = tx.project_by_slug(slug)?.expect("the project");
            tx.checkout_path(project.id)
        })
        .unwrap()
}

#[test]
fn migration_eight_carries_the_main_checkout_into_the_column() {
    let dir = scratch_dir();
    let store = v7_store_with_paths(
        dir.path(),
        &[
            ("/repos/carried", "main"),
            ("/repos/carried/wt", "worktree"),
        ],
    );
    assert_eq!(checkout_of(&store, "carried"), None, "v7 leaves it NULL");

    store.migrate().unwrap();

    assert_eq!(
        checkout_of(&store, "carried").as_deref(),
        Some(Path::new("/repos/carried")),
        "the main working tree becomes the project's checkout"
    );
}

#[test]
fn migration_eight_drops_a_project_that_had_only_worktrees_rather_than_electing_one() {
    // A linked worktree is a branch somebody is working on. It was never the
    // right answer to "where does this project's repo-side work run", and
    // `preferred_checkout` electing one when it sorted first is the defect this
    // deletion closes rather than carries forward.
    let dir = scratch_dir();
    let store = v7_store_with_paths(dir.path(), &[("/repos/carried/wt", "worktree")]);

    store.migrate().unwrap();

    assert_eq!(
        checkout_of(&store, "carried"),
        None,
        "a worktree must not be promoted to the project's checkout"
    );
}

#[test]
fn migration_eight_leaves_a_checkout_somebody_linked_on_purpose_alone() {
    use storyhook::store::{ReadOps, WriteOps};

    let dir = scratch_dir();
    let store = v7_store_with_paths(dir.path(), &[("/repos/carried", "main")]);
    let project = store
        .read(|tx| Ok(tx.project_by_slug("carried")?.expect("the project").id))
        .unwrap();
    store
        .write(|tx| tx.set_checkout_path(project, Some(Path::new("/somewhere/else"))))
        .unwrap();

    store.migrate().unwrap();

    assert_eq!(
        checkout_of(&store, "carried").as_deref(),
        Some(Path::new("/somewhere/else")),
        "`story project link checkout` outranks what the index remembered"
    );
}

#[test]
fn a_migrated_store_has_no_resolution_index_left() {
    let dir = scratch_dir();
    let store = v7_store_with_paths(dir.path(), &[("/repos/carried", "main")]);
    store.migrate().unwrap();

    let conn = Connection::open(store.path()).unwrap();
    let objects: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE name LIKE '%project_paths%'")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        objects.is_empty(),
        "the table and its unique index must both be gone, found: {objects:?}"
    );
}
