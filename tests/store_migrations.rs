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

    const CASES: [&str; 8] = [
        "[git] abc1234: feat: the work",
        "[git] 0123456789abcdef: a long hash",
        "[git] abc1234: a subject: with a colon in it",
        "[git] rebase: prose, not a hash",
        "[git] nothing here",
        "not a link record at all",
        "[git] : empty",
        // No space after the colon: both spellings must still find this hash
        // — the SQL backfill's `instr(text, ':')` does not require one, so a
        // parser that does would leave a row the migration wrote unrecognized
        // at fold time.
        "[git] abc1234:no space",
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

// ---------------------------------------------------------------------------
// Migration 9: type emoji, `story` is renamed to `normal`, and `task` is
// retired (SH-157)
// ---------------------------------------------------------------------------

/// A store at schema v8 — the last version before `project_types.emoji`
/// existed — carrying one project with the full default catalog (`task`
/// deliberately placed in the *middle* of the position order, so the
/// migration's gap-closing renumbering has a real gap to close rather than
/// one that already happens to be contiguous) and three stories: one typed
/// `task`, one typed `story` (both rename to `normal`), and one typed `epic`
/// — the control that proves the migration is selective rather than
/// retyping every story it can see.
///
/// `project_types` is seeded in raw SQL at the v8 shape (no `emoji` column) —
/// the same reason `tests/store_schema_fixture.rs` seeds it that way now:
/// `WriteOps::put_types` assumes the column this migration adds.
///
/// Writes one story's read-model row at the v8 column shape (no `hidden_at`,
/// added by migration 10) — deliberately raw SQL rather than
/// `WriteOps::put_story`, which always writes the *current* binary's full
/// column set. Shared by every fixture in this section that needs a v8-shape
/// `stories` row: one place stating the v8 column list is one place to update
/// if a future migration adds another `stories` column and this needs a v9,
/// v10, ... variant.
fn insert_v8_story_row(
    conn: &Connection,
    project: storyhook::store::ProjectId,
    no: StoryNo,
    head: storyhook::store::EventSeq,
    snapshot: &storyhook::domain::StorySnapshot,
) {
    use storyhook::store::types::priority_rank;
    conn.execute(
        "INSERT INTO stories (project_id, story_no, head_seq, title, state, superstate, \
         priority, priority_rank, story_type, assignee, awaiting, deleted, archived, \
         created_at, updated_at, closed_at, description, snapshot) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, 0, ?12, ?13, ?14, ?15, ?16)",
        rusqlite::params![
            project.get(),
            no.get(),
            head.get(),
            snapshot.title,
            snapshot.state,
            snapshot.superstate.as_str(),
            snapshot.priority.as_str(),
            priority_rank(&snapshot.priority),
            snapshot.story_type,
            snapshot.assignee,
            snapshot.awaiting,
            snapshot.created_at,
            snapshot.updated_at,
            snapshot.closed_at,
            snapshot.description,
            serde_json::to_string(snapshot).unwrap(),
        ],
    )
    .unwrap();
}

/// Writes `events` onto one story with **raw SQL**, and advances the project's
/// change-feed counter to match.
///
/// Deliberately not through `WriteOps::append_events`, for the same reason
/// [`insert_v8_story_row`] does not use `put_story`: that path writes the
/// *current* binary's full column set, which since migration 13 includes
/// `command` and `actor` — columns a genuine v8 database does not have. A
/// fixture that could only be built by the code the migration is catching up
/// with would be testing nothing.
///
/// The counter matters as much as the rows: migration 9 appends events of its
/// own, and a stale `next_global_seq` collides with these on
/// `UNIQUE (project_id, global_seq)`.
fn append_v8_events(
    conn: &Connection,
    project: storyhook::store::ProjectId,
    story: StoryNo,
    events: &[StoryEvent],
) {
    let base: i64 = conn
        .query_row(
            "SELECT next_global_seq FROM projects WHERE id = ?1",
            rusqlite::params![project.get()],
            |row| row.get(0),
        )
        .unwrap();
    for (index, event) in events.iter().enumerate() {
        let value = serde_json::to_value(event).unwrap();
        let offset = i64::try_from(index).unwrap();
        conn.execute(
            "INSERT INTO events (project_id, story_no, seq, global_seq, kind, at, payload) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                project.get(),
                story.get(),
                offset + 1,
                base + offset,
                value["kind"].as_str().unwrap(),
                value["at"].as_str().unwrap(),
                serde_json::to_string(&value).unwrap(),
            ],
        )
        .unwrap();
    }
    conn.execute(
        "UPDATE projects SET next_global_seq = ?2 WHERE id = ?1",
        rusqlite::params![project.get(), base + i64::try_from(events.len()).unwrap()],
    )
    .unwrap();
}

fn v8_store_with_renamed_and_retired_type_stories(
    dir: &Path,
) -> (
    SqliteStore,
    storyhook::store::ProjectId,
    StoryNo,
    StoryNo,
    StoryNo,
) {
    use storyhook::domain::{StateDef, SuperState, fold_story};
    use storyhook::store::{EventSeq, NewProject, ReadOps, WriteOps};

    let store = SqliteStore::open(dir.join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..8]).unwrap();

    // Collects each story's read-model row rather than writing it through
    // `WriteOps::put_story`: that call always writes the *current* binary's
    // full column set, which since migration 10 includes `hidden_at` — a
    // column a genuine v8 database does not have. Written below with raw SQL
    // instead, the same reason `seed_a_labelled_story` and `append_v1_events`
    // in this file bypass the service/store write path for a fixture that
    // must stay buildable on a schema older than the code that built it.
    let (project, rows) = store
        .write(|tx| {
            let project = tx.create_project(&NewProject {
                uuid: "types".into(),
                slug: "types".into(),
                name: "types".into(),
                prefix: "SH".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            })?;
            tx.put_states(
                project,
                &[
                    StateDef {
                        slug: "todo".into(),
                        super_state: SuperState::Open,
                        role: None,
                        description: None,
                    },
                    StateDef {
                        slug: "done".into(),
                        super_state: SuperState::Closed,
                        role: None,
                        description: None,
                    },
                ],
            )?;
            let state_map = tx.state_map(project)?;

            let mut make_story =
                |title: &str, created_at: &str, typed_at: &str, story_type: &str| {
                    let events = vec![
                        StoryEvent::StoryCreated {
                            at: created_at.to_string(),
                            title: title.to_string(),
                            state: "todo".into(),
                        },
                        StoryEvent::StoryTypeSet {
                            at: typed_at.to_string(),
                            story_type: story_type.to_string(),
                        },
                    ];
                    let no = tx.allocate_story_no(project)?;
                    // The events are written below with raw SQL, for the same
                    // reason the read-model rows are (see this function's
                    // header): `WriteOps::append_events` writes the *current*
                    // binary's full column set, which since migration 13
                    // includes `command` and `actor` — columns a genuine v8
                    // database does not have.
                    let head = EventSeq::new(i64::try_from(events.len()).unwrap());
                    let snapshot = fold_story(&no.to_id("SH"), &events, &state_map).unwrap();
                    Ok::<_, storyhook::store::StoreError>((no, head, snapshot, events))
                };

            let task_row = make_story(
                "A task-typed story",
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:01:00Z",
                "task",
            )?;
            let story_row = make_story(
                "An ordinary story",
                "2026-01-01T00:00:10Z",
                "2026-01-01T00:01:10Z",
                "story",
            )?;
            let epic_row = make_story(
                "An epic",
                "2026-01-01T00:00:20Z",
                "2026-01-01T00:01:20Z",
                "epic",
            )?;

            Ok((project, [task_row, story_row, epic_row]))
        })
        .unwrap();

    let conn = Connection::open(store.path()).unwrap();
    let mut global_seq = 0i64;
    for (no, head, snapshot, events) in &rows {
        insert_v8_story_row(&conn, project, *no, *head, snapshot);
        for (index, event) in events.iter().enumerate() {
            let value = serde_json::to_value(event).unwrap();
            global_seq += 1;
            conn.execute(
                "INSERT INTO events (project_id, story_no, seq, global_seq, kind, at, payload) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    project.get(),
                    no.get(),
                    i64::try_from(index).unwrap() + 1,
                    global_seq,
                    value["kind"].as_str().unwrap(),
                    value["at"].as_str().unwrap(),
                    serde_json::to_string(&value).unwrap(),
                ],
            )
            .unwrap();
        }
    }
    // The raw inserts above bypass `allocate_global_seqs`, so the project's
    // own counter has to be advanced by hand — migration 9 appends events of
    // its own, and would otherwise collide with these on
    // `UNIQUE (project_id, global_seq)`.
    conn.execute(
        "UPDATE projects SET next_global_seq = ?2 WHERE id = ?1",
        rusqlite::params![project.get(), global_seq + 1],
    )
    .unwrap();
    let [(task_story, ..), (story_story, ..), (epic_story, ..)] = rows;

    for (position, slug) in [
        (0, "story"),
        (1, "task"),
        (2, "epic"),
        (3, "bug"),
        (4, "chore"),
    ] {
        conn.execute(
            "INSERT INTO project_types (project_id, position, slug, description) \
             VALUES (?1, ?2, ?3, NULL)",
            rusqlite::params![project.get(), position, slug],
        )
        .unwrap();
    }

    (store, project, task_story, story_story, epic_story)
}

/// `(story_type, updated_at, head_seq)` for one story, read back through the
/// store.
/// `(story_type, updated_at, head_seq)` for one story, read with **raw SQL**
/// naming only these three columns.
///
/// Deliberately not `ReadOps::story`: this helper's whole purpose is to
/// capture a "before" snapshot *ahead of* `store.migrate()`, while the
/// database may still be missing a column a later migration adds (SH-43 added
/// `hidden_at` at v10) — `ReadOps::story` always selects the *current*
/// binary's full column set and would fail against that older shape. The
/// three columns here have all existed since v8, so this reads correctly on
/// either side of any migration boundary this file exercises.
fn story_type_updated_head(
    store: &SqliteStore,
    project: storyhook::store::ProjectId,
    no: StoryNo,
) -> (Option<String>, String, i64) {
    let conn = Connection::open(store.path()).unwrap();
    conn.query_row(
        "SELECT story_type, updated_at, head_seq FROM stories \
         WHERE project_id = ?1 AND story_no = ?2",
        rusqlite::params![project.get(), no.get()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .unwrap()
}

/// The events recorded for one story, in `seq` order: `(kind, story_type)`.
///
/// `story_type` is `None` for every event kind but `StoryTypeSet`, which is
/// all this test file needs from the payload.
fn events_of(
    store: &SqliteStore,
    project: storyhook::store::ProjectId,
    no: StoryNo,
) -> Vec<(String, Option<String>)> {
    let conn = Connection::open(store.path()).unwrap();
    conn.prepare(
        "SELECT kind, payload FROM events WHERE project_id = ?1 AND story_no = ?2 ORDER BY seq",
    )
    .unwrap()
    .query_map(rusqlite::params![project.get(), no.get()], |row| {
        let kind: String = row.get(0)?;
        let payload: String = row.get(1)?;
        Ok((kind, payload))
    })
    .unwrap()
    .map(|r| {
        let (kind, payload) = r.unwrap();
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let story_type = value
            .get("story_type")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        (kind, story_type)
    })
    .collect()
}

fn next_global_seq(store: &SqliteStore, project: storyhook::store::ProjectId) -> i64 {
    let conn = Connection::open(store.path()).unwrap();
    conn.query_row(
        "SELECT next_global_seq FROM projects WHERE id = ?1",
        rusqlite::params![project.get()],
        |row| row.get(0),
    )
    .unwrap()
}

fn type_catalog(store: &SqliteStore, project: storyhook::store::ProjectId) -> Vec<(String, i64)> {
    let conn = Connection::open(store.path()).unwrap();
    conn.prepare("SELECT slug, position FROM project_types WHERE project_id = ?1 ORDER BY position")
        .unwrap()
        .query_map(rusqlite::params![project.get()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn migration_nine_retypes_task_stories_to_normal_by_appending_a_real_event() {
    let dir = scratch_dir();
    let (store, project, task_story, _story_story, _epic_story) =
        v8_store_with_renamed_and_retired_type_stories(dir.path());
    let before = next_global_seq(&store, project);

    store.migrate().unwrap();

    let (story_type, updated_at, head_seq) = story_type_updated_head(&store, project, task_story);
    assert_eq!(story_type.as_deref(), Some("normal"));
    assert_ne!(
        updated_at, "2026-01-01T00:01:00Z",
        "updated_at must move: the fold sets it from the appended event's `at`"
    );
    assert_eq!(
        head_seq, 3,
        "StoryCreated, StoryTypeSet(task), StoryTypeSet(normal)"
    );

    let events = events_of(&store, project, task_story);
    assert_eq!(
        events,
        vec![
            ("StoryCreated".to_string(), None),
            ("StoryTypeSet".to_string(), Some("task".to_string())),
            ("StoryTypeSet".to_string(), Some("normal".to_string())),
        ],
        "the original `task` event survives; the migration appends rather than rewrites"
    );

    assert_eq!(
        next_global_seq(&store, project),
        before + 2,
        "the project's counter advances past both events this migration appended \
         — one for the `task`-typed story, one for the `story`-typed story"
    );
}

#[test]
fn migration_nine_retypes_story_stories_to_normal_by_appending_a_real_event() {
    let dir = scratch_dir();
    let (store, project, _task_story, story_story, _epic_story) =
        v8_store_with_renamed_and_retired_type_stories(dir.path());

    store.migrate().unwrap();

    let (story_type, updated_at, head_seq) = story_type_updated_head(&store, project, story_story);
    assert_eq!(
        story_type.as_deref(),
        Some("normal"),
        "the catalog's `story` slug is renamed to `normal`, so a `story`-typed \
         story must move with it — leaving it behind would orphan its \
         story_type against a catalog entry that no longer exists"
    );
    assert_ne!(updated_at, "2026-01-01T00:01:10Z");
    assert_eq!(
        head_seq, 3,
        "StoryCreated, StoryTypeSet(story), StoryTypeSet(normal)"
    );

    let events = events_of(&store, project, story_story);
    assert_eq!(
        events,
        vec![
            ("StoryCreated".to_string(), None),
            ("StoryTypeSet".to_string(), Some("story".to_string())),
            ("StoryTypeSet".to_string(), Some("normal".to_string())),
        ],
        "the original `story` event survives; the migration appends rather than rewrites"
    );
}

#[test]
fn migration_nine_leaves_an_epic_typed_story_completely_alone() {
    let dir = scratch_dir();
    let (store, project, _task_story, _story_story, epic_story) =
        v8_store_with_renamed_and_retired_type_stories(dir.path());
    let before = story_type_updated_head(&store, project, epic_story);

    store.migrate().unwrap();

    let after = story_type_updated_head(&store, project, epic_story);
    assert_eq!(
        before, after,
        "a story typed neither `task` nor `story` must not be touched by this \
         migration at all — proves it is selective, not \"retype everything\""
    );
    assert_eq!(
        events_of(&store, project, epic_story).len(),
        2,
        "no event is appended to a story the migration has no reason to touch"
    );
}

#[test]
fn migration_nine_read_model_agrees_with_the_event_log_afterward() {
    use storyhook::store::diff_read_model;

    let dir = scratch_dir();
    let (store, project, _task_story, _story_story, _epic_story) =
        v8_store_with_renamed_and_retired_type_stories(dir.path());

    store.migrate().unwrap();

    let diff = diff_read_model(&store, project).unwrap();
    assert!(
        diff.is_clean(),
        "the rows the migration edited must still agree with their own event log: {}",
        diff.describe()
    );
}

#[test]
fn migration_nine_drops_task_and_renames_story_closing_the_position_gap() {
    let dir = scratch_dir();
    let (store, project, _task_story, _story_story, _epic_story) =
        v8_store_with_renamed_and_retired_type_stories(dir.path());

    store.migrate().unwrap();

    let catalog = type_catalog(&store, project);
    let slugs: Vec<&str> = catalog.iter().map(|(slug, _)| slug.as_str()).collect();
    assert_eq!(
        slugs,
        vec!["normal", "epic", "bug", "chore"],
        "`task` is gone, `story` is `normal`; the survivors keep their relative order"
    );
    let positions: Vec<i64> = catalog.iter().map(|(_, position)| *position).collect();
    assert_eq!(
        positions,
        vec![0, 1, 2, 3],
        "positions are renumbered contiguous from 0, closing the gap `task` left at 1"
    );
}

#[test]
fn migration_nine_backfills_emoji_for_the_four_default_slugs_only() {
    use storyhook::store::ReadOps;

    let dir = scratch_dir();
    let (store, project, _task_story, _story_story, _epic_story) =
        v8_store_with_renamed_and_retired_type_stories(dir.path());

    store.migrate().unwrap();

    let types = store.read(|tx| tx.types(project)).unwrap();
    let emoji_of = |slug: &str| {
        types
            .iter()
            .find(|t| t.slug == slug)
            .and_then(|t| t.emoji.clone())
    };
    assert_eq!(emoji_of("normal").as_deref(), Some("📙"));
    assert_eq!(emoji_of("epic").as_deref(), Some("📚"));
    assert_eq!(emoji_of("bug").as_deref(), Some("🐞"));
    assert_eq!(emoji_of("chore").as_deref(), Some("🧺"));
}

#[test]
fn migration_nine_does_not_invent_an_emoji_for_a_custom_type() {
    use storyhook::store::ReadOps;

    let dir = scratch_dir();
    let (store, project, _task_story, _story_story, _epic_story) =
        v8_store_with_renamed_and_retired_type_stories(dir.path());
    let conn = Connection::open(store.path()).unwrap();
    conn.execute(
        "INSERT INTO project_types (project_id, position, slug, description) \
         VALUES (?1, 5, 'spike', NULL)",
        rusqlite::params![project.get()],
    )
    .unwrap();
    drop(conn);

    store.migrate().unwrap();

    let types = store.read(|tx| tx.types(project)).unwrap();
    let spike = types.iter().find(|t| t.slug == "spike").unwrap();
    assert_eq!(
        spike.emoji, None,
        "only the four named default slugs are backfilled; a custom type gets \
         no emoji invented for it and falls back to the dashboard's generic 🏷️"
    );
}

#[test]
fn migration_nine_leaves_a_project_with_no_task_stories_and_no_task_type_untouched() {
    use storyhook::domain::{StateDef, SuperState, fold_story};
    use storyhook::store::{EventSeq, NewProject, ReadOps, WriteOps};

    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..8]).unwrap();

    let (project, no, head, snapshot, events) = store
        .write(|tx| {
            let project = tx.create_project(&NewProject {
                uuid: "clean".into(),
                slug: "clean".into(),
                name: "clean".into(),
                prefix: "SH".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            })?;
            tx.put_states(
                project,
                &[
                    StateDef {
                        slug: "todo".into(),
                        super_state: SuperState::Open,
                        role: None,
                        description: None,
                    },
                    StateDef {
                        slug: "done".into(),
                        super_state: SuperState::Closed,
                        role: None,
                        description: None,
                    },
                ],
            )?;
            let state_map = tx.state_map(project)?;
            let events = vec![StoryEvent::StoryCreated {
                at: "2026-01-01T00:00:00Z".into(),
                title: "Never typed at all".into(),
                state: "todo".into(),
            }];
            let no = tx.allocate_story_no(project)?;
            // Events are written with raw SQL after this transaction — see
            // `append_v8_events` for why the store's own write path cannot
            // build a v8 fixture.
            let head = EventSeq::new(i64::try_from(events.len()).unwrap());
            let snapshot = fold_story(&no.to_id("SH"), &events, &state_map).unwrap();
            Ok((project, no, head, snapshot, events))
        })
        .unwrap();
    let conn = Connection::open(store.path()).unwrap();
    insert_v8_story_row(&conn, project, no, head, &snapshot);
    append_v8_events(&conn, project, no, &events);
    drop(conn);
    let before = story_type_updated_head(&store, project, no);
    let before_global_seq = next_global_seq(&store, project);

    store.migrate().unwrap();

    assert_eq!(before, story_type_updated_head(&store, project, no));
    assert_eq!(before_global_seq, next_global_seq(&store, project));
    assert_eq!(events_of(&store, project, no).len(), 1);
    assert!(type_catalog(&store, project).is_empty());
}

#[test]
fn migration_nine_removes_an_unused_task_catalog_entry_without_touching_any_story() {
    use storyhook::domain::{StateDef, SuperState, fold_story};
    use storyhook::store::{EventSeq, NewProject, ReadOps, WriteOps};

    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..8]).unwrap();

    let (project, no, head, snapshot, events) = store
        .write(|tx| {
            let project = tx.create_project(&NewProject {
                uuid: "unused-task".into(),
                slug: "unused-task".into(),
                name: "unused-task".into(),
                prefix: "SH".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            })?;
            tx.put_states(
                project,
                &[
                    StateDef {
                        slug: "todo".into(),
                        super_state: SuperState::Open,
                        role: None,
                        description: None,
                    },
                    StateDef {
                        slug: "done".into(),
                        super_state: SuperState::Closed,
                        role: None,
                        description: None,
                    },
                ],
            )?;
            let state_map = tx.state_map(project)?;
            let events = vec![StoryEvent::StoryCreated {
                at: "2026-01-01T00:00:00Z".into(),
                title: "Typed story, never task".into(),
                state: "todo".into(),
            }];
            let no = tx.allocate_story_no(project)?;
            // Events are written with raw SQL after this transaction — see
            // `append_v8_events` for why the store's own write path cannot
            // build a v8 fixture.
            let head = EventSeq::new(i64::try_from(events.len()).unwrap());
            let snapshot = fold_story(&no.to_id("SH"), &events, &state_map).unwrap();
            Ok((project, no, head, snapshot, events))
        })
        .unwrap();
    let conn = Connection::open(store.path()).unwrap();
    insert_v8_story_row(&conn, project, no, head, &snapshot);
    append_v8_events(&conn, project, no, &events);
    for (position, slug) in [(0, "story"), (1, "task")] {
        conn.execute(
            "INSERT INTO project_types (project_id, position, slug, description) \
             VALUES (?1, ?2, ?3, NULL)",
            rusqlite::params![project.get(), position, slug],
        )
        .unwrap();
    }
    drop(conn);
    let before = story_type_updated_head(&store, project, no);
    let before_global_seq = next_global_seq(&store, project);

    store.migrate().unwrap();

    assert_eq!(
        before,
        story_type_updated_head(&store, project, no),
        "the catalog shrinking must not touch a story that was never typed `task`"
    );
    assert_eq!(before_global_seq, next_global_seq(&store, project));
    assert_eq!(events_of(&store, project, no).len(), 1);
    assert_eq!(
        type_catalog(&store, project),
        vec![("normal".to_string(), 0)],
        "the `task` row is removed and `story` is renamed to `normal`, \
         even with no story left to retype"
    );
}

// ---------------------------------------------------------------------------
// Migration 10: `stories.hidden_at` (SH-43)
// ---------------------------------------------------------------------------

/// Seeds one v9 project holding one story, straight to the tables — the same
/// reason `seed_a_labelled_story` does: this file tests the migration
/// framework, not the service layer, and this fixture must stay buildable on
/// a schema that predates the column migration 10 adds.
fn seed_a_v9_story(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "INSERT INTO projects (id, uuid, slug, name, prefix, created_at)
             VALUES (1, 'u-1', 'proj', 'Proj', 'SH', '2026-01-01T00:00:00Z');
         INSERT INTO project_states (project_id, position, slug, superstate)
             VALUES (1, 0, 'todo', 'OPEN'), (1, 1, 'done', 'CLOSED');
         INSERT INTO stories (project_id, story_no, head_seq, title, state, superstate,
                              priority, priority_rank, created_at, updated_at, snapshot)
             VALUES (1, 1, 1, 'A story', 'todo', 'OPEN', 'none', 4,
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '{}');",
    )
    .unwrap();
}

#[test]
fn migration_ten_adds_a_hidden_at_column_that_pre_existing_stories_read_as_null() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..9]).unwrap();
    seed_a_v9_story(store.path());

    assert!(
        Connection::open(store.path())
            .unwrap()
            .prepare("SELECT hidden_at FROM stories")
            .is_err(),
        "a v9 database must not already have `hidden_at` — otherwise this test \
         proves nothing about the migration that adds it"
    );

    store.migrate().unwrap();

    let hidden_at: Option<String> = Connection::open(store.path())
        .unwrap()
        .query_row(
            "SELECT hidden_at FROM stories WHERE project_id = 1 AND story_no = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        hidden_at, None,
        "a story that existed before this migration is not retroactively archived"
    );
}

#[test]
fn migration_ten_leaves_every_other_column_and_the_event_log_untouched() {
    use storyhook::store::ReadOps;

    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..9]).unwrap();
    seed_a_v9_story(store.path());
    let before = next_global_seq(&store, storyhook::store::ProjectId::new(1));

    store.migrate().unwrap();

    assert_eq!(
        before,
        next_global_seq(&store, storyhook::store::ProjectId::new(1)),
        "no data-migrating event is appended — this is a bare `ADD COLUMN`"
    );
    let events = store
        .read(|tx| tx.events_for(storyhook::store::ProjectId::new(1), StoryNo::new(1)))
        .unwrap();
    assert!(
        events.is_empty(),
        "the pre-existing story was seeded with no events of its own; migration \
         10 must not have added any"
    );
}

// ---------------------------------------------------------------------------
// Migration 12: `stories.draft` (SH-175)
// ---------------------------------------------------------------------------

/// Seeds one v11 project holding one story, straight to the tables — the same
/// reason `seed_a_v9_story` does: this fixture must stay buildable on a
/// schema that predates the column migration 12 adds.
fn seed_a_v11_story(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "INSERT INTO projects (id, uuid, slug, name, prefix, created_at)
             VALUES (1, 'u-1', 'proj', 'Proj', 'SH', '2026-01-01T00:00:00Z');
         INSERT INTO project_states (project_id, position, slug, superstate)
             VALUES (1, 0, 'todo', 'OPEN'), (1, 1, 'done', 'CLOSED');
         INSERT INTO stories (project_id, story_no, head_seq, title, state, superstate,
                              priority, priority_rank, created_at, updated_at, snapshot)
             VALUES (1, 1, 1, 'A story', 'todo', 'OPEN', 'none', 4,
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '{}');",
    )
    .unwrap();
}

#[test]
fn migration_twelve_adds_a_draft_column_that_pre_existing_stories_read_as_live() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..11]).unwrap();
    seed_a_v11_story(store.path());

    assert!(
        Connection::open(store.path())
            .unwrap()
            .prepare("SELECT draft FROM stories")
            .is_err(),
        "a v11 database must not already have `draft` — otherwise this test \
         proves nothing about the migration that adds it"
    );

    store.migrate().unwrap();

    let draft: bool = Connection::open(store.path())
        .unwrap()
        .query_row(
            "SELECT draft FROM stories WHERE project_id = 1 AND story_no = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        !draft,
        "a story that existed before this migration is not retroactively drafted — \
         SH-175's own text: new stories are live unless a caller opts into draft"
    );
}

#[test]
fn migration_twelve_leaves_every_other_column_and_the_event_log_untouched() {
    use storyhook::store::ReadOps;

    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..11]).unwrap();
    seed_a_v11_story(store.path());
    let before = next_global_seq(&store, storyhook::store::ProjectId::new(1));

    store.migrate().unwrap();

    assert_eq!(
        before,
        next_global_seq(&store, storyhook::store::ProjectId::new(1)),
        "no data-migrating event is appended — this is a bare `ADD COLUMN`"
    );
    let events = store
        .read(|tx| tx.events_for(storyhook::store::ProjectId::new(1), StoryNo::new(1)))
        .unwrap();
    assert!(
        events.is_empty(),
        "the pre-existing story was seeded with no events of its own; migration \
         12 must not have added any"
    );
}
