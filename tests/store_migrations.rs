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
fn a_database_this_build_cannot_read_is_refused_with_one_clear_message() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    SqliteStore::open(&path).unwrap().migrate().unwrap();
    let future = migrate::current_schema_version() + 1;
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(&format!("PRAGMA user_version = {future}"))
        .unwrap();
    // SH-530 narrowed this gate: a future store whose read model is intact now
    // opens READ-ONLY instead of being refused (`tests/readonly_store.rs` owns
    // that half). What still earns the outright refusal is a store this build
    // genuinely cannot read — one whose columns a newer storyhook restructured
    // rather than merely added to. So the fixture renames a column the read
    // model depends on, which is what the refusal is now *for*.
    conn.execute_batch("ALTER TABLE stories RENAME COLUMN title TO headline")
        .unwrap();

    let error = SqliteStore::open(&path).unwrap_err();

    match error {
        StoreError::SchemaTooNew { found, supported } => {
            assert_eq!(found, future);
            assert_eq!(supported, migrate::current_schema_version());
        }
        other => panic!("expected SchemaTooNew, got: {other}"),
    }
    let message = SqliteStore::open(&path).unwrap_err().to_string();
    assert!(message.contains("newer storyhook"), "{message}");
    assert!(
        message.contains("`story update` if a newer release is available"),
        "release recovery: {message}"
    );
    assert!(
        message.contains("otherwise check out the source revision")
            && message.contains("`make install`"),
        "unreleased-build recovery: {message}"
    );
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
        priority_rank, story_type, assignee, awaiting, archived,
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
         INSERT INTO project_types (project_id, position, slug, description, emoji)
             VALUES (1, 0, 'normal', NULL, NULL);
         INSERT INTO stories (project_id, story_no, head_seq, title, state, superstate,
                              priority, priority_rank, story_type, created_at, updated_at, snapshot)
             VALUES (1, 1, 1, 'A story', 'todo', 'OPEN', 'low', 3, 'normal',
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z',
                     '{\"priority\":\"low\",\"priority_assessed\":true,\"story_type\":\"normal\"}');
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
    // reports success. On the bundled SQLite this empties `story_labels` and
    // `story_relations` for every project in the store.
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
    // the price SH-130's permanent delete pays for its guard.
    //
    // `events_reject_delete` names `stories` in its `WHEN` clause, which is
    // what lets a permanent delete remove a story's events and nothing else's. The
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
            archived   INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL, closed_at TEXT,
            description TEXT, snapshot TEXT NOT NULL,
            PRIMARY KEY (project_id, story_no)
        );
        INSERT INTO stories_new SELECT
            project_id, story_no, head_seq, title, state, superstate, priority,
            priority_rank, story_type, assignee, awaiting, archived,
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

    store.migrate_with(&migrate::MIGRATIONS[..9]).unwrap();

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

    store.migrate_with(&migrate::MIGRATIONS[..9]).unwrap();

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

    store.migrate_with(&migrate::MIGRATIONS[..9]).unwrap();

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

    store.migrate_with(&migrate::MIGRATIONS[..15]).unwrap();

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

    store.migrate_with(&migrate::MIGRATIONS[..9]).unwrap();

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

    store.migrate_with(&migrate::MIGRATIONS[..9]).unwrap();

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

    store.migrate_with(&migrate::MIGRATIONS[..9]).unwrap();

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

    store.migrate_with(&migrate::MIGRATIONS[..9]).unwrap();

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

    store.migrate_with(&migrate::MIGRATIONS[..9]).unwrap();

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

    store.migrate_with(&migrate::MIGRATIONS[..10]).unwrap();

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

    store.migrate_with(&migrate::MIGRATIONS[..13]).unwrap();

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

    store.migrate_with(&migrate::MIGRATIONS[..12]).unwrap();

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

    store.migrate_with(&migrate::MIGRATIONS[..13]).unwrap();

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

// ---------------------------------------------------------------------------
// Migration 15: `stories.head_global_seq` (SH-336)
// ---------------------------------------------------------------------------

/// Seeds one v14 project holding two stories, straight to the tables —
/// migrations 14 and earlier have no `head_global_seq` column, so `WriteOps`
/// cannot write this fixture; it must be built with the column set migration
/// 15 will find, the same reason `seed_a_v9_story`/`seed_a_v11_story` do.
///
/// **Alpha** carries three events with `global_seq` values (37, 42, 50)
/// deliberately *not* equal to their `seq` values (1, 2, 3) — a naive
/// migration that joined `seq = global_seq` would still pass a fixture where
/// the two coincide, so they must not. `head_seq = 2` names the *second*
/// event; the third (`global_seq = 50`) stands for an event appended after
/// the read model's row was last folded — a stale row. The backfill must
/// read 42 (the `global_seq` of the event `head_seq` actually names), not
/// `MAX(global_seq)` across the story (which would wrongly read 50 for a
/// stale row) and not the story's *first* event either (which would read 37).
///
/// **Bravo** has `head_seq = 0` and no events at all — the `extra_rows` case
/// (a read-model row with nothing behind it) — which must backfill to `0`.
fn seed_a_v14_project(path: &Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "INSERT INTO projects (id, uuid, slug, name, prefix, created_at, next_global_seq)
             VALUES (1, 'u-1', 'proj', 'Proj', 'SH', '2026-01-01T00:00:00Z', 100);
         INSERT INTO project_states (project_id, position, slug, superstate)
             VALUES (1, 0, 'todo', 'OPEN'), (1, 1, 'done', 'CLOSED');
         INSERT INTO stories (project_id, story_no, head_seq, title, state, superstate,
                              priority, priority_rank, created_at, updated_at, snapshot)
             VALUES (1, 1, 2, 'Alpha', 'todo', 'OPEN', 'none', 4,
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '{}');
         INSERT INTO stories (project_id, story_no, head_seq, title, state, superstate,
                              priority, priority_rank, created_at, updated_at, snapshot)
             VALUES (1, 2, 0, 'Bravo', 'todo', 'OPEN', 'none', 4,
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '{}');",
    )
    .unwrap();

    // Alpha's log, with `seq` and `global_seq` chosen so the three readings a
    // backfill could take are all different: `head_seq` names seq 2, whose
    // `global_seq` is 42, where the story's highest is 50 and `seq ==
    // global_seq` would give 2.
    //
    // Written event by event rather than as one `VALUES` list so that `kind`
    // and `payload` come from the production encoder. Typing them out is how
    // this fixture spent fourteen migrations seeding `'story_created'` and
    // `'story_priority_set'`, spellings no writer emits (SH-364).
    for (seq, global_seq, event) in [
        (
            1,
            37,
            StoryEvent::StoryCreated {
                at: "2026-01-01T00:00:00Z".to_string(),
                title: "Alpha".to_string(),
                state: "todo".to_string(),
            },
        ),
        (
            2,
            42,
            StoryEvent::StoryPrioritySet {
                at: "2026-01-01T00:00:00Z".to_string(),
                priority: storyhook::domain::Priority::None,
            },
        ),
        (
            3,
            50,
            StoryEvent::StoryPrioritySet {
                at: "2026-01-01T00:00:01Z".to_string(),
                priority: storyhook::domain::Priority::None,
            },
        ),
    ] {
        let payload = serde_json::to_value(&event).unwrap();
        conn.execute(
            "INSERT INTO events (project_id, story_no, seq, global_seq, kind, at, payload) \
             VALUES (1, 1, ?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                seq,
                global_seq,
                kind_of(&event),
                payload["at"].as_str().unwrap(),
                serde_json::to_string(&payload).unwrap(),
            ],
        )
        .unwrap();
    }
}

fn head_global_seq_of(store: &SqliteStore, story_no: i64) -> i64 {
    Connection::open(store.path())
        .unwrap()
        .query_row(
            "SELECT head_global_seq FROM stories WHERE project_id = 1 AND story_no = ?1",
            rusqlite::params![story_no],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn migration_fifteen_backfills_each_row_with_its_head_events_feed_position() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..14]).unwrap();
    seed_a_v14_project(store.path());

    assert!(
        Connection::open(store.path())
            .unwrap()
            .prepare("SELECT head_global_seq FROM stories")
            .is_err(),
        "a v14 database must not already have `head_global_seq` — otherwise \
         this test proves nothing about the migration that adds it"
    );

    store.migrate_with(&migrate::MIGRATIONS[..15]).unwrap();

    assert_eq!(
        head_global_seq_of(&store, 1),
        42,
        "must read the global_seq of the event head_seq (2) names, not the \
         story's own highest global_seq (50) and not seq==global_seq (which \
         would read 2)"
    );
}

#[test]
fn migration_fifteen_leaves_a_row_with_no_events_at_zero() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..14]).unwrap();
    seed_a_v14_project(store.path());

    store.migrate_with(&migrate::MIGRATIONS[..15]).unwrap();

    assert_eq!(
        head_global_seq_of(&store, 2),
        0,
        "Bravo has no events; a row with nothing behind it backfills to 0, \
         the same reading head_seq itself already gives an eventless row"
    );
}

#[test]
fn migration_fifteen_leaves_the_event_log_and_the_change_feed_untouched() {
    use storyhook::store::ReadOps;

    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..14]).unwrap();
    seed_a_v14_project(store.path());
    let before = next_global_seq(&store, storyhook::store::ProjectId::new(1));

    store.migrate_with(&migrate::MIGRATIONS[..15]).unwrap();

    assert_eq!(
        before,
        next_global_seq(&store, storyhook::store::ProjectId::new(1)),
        "no event is appended — this migration only reads `events`, it never writes it"
    );
    let events = store
        .read(|tx| tx.events_for(storyhook::store::ProjectId::new(1), StoryNo::new(1)))
        .unwrap();
    assert_eq!(
        events.len(),
        3,
        "the three seeded events for Alpha must survive the migration unchanged"
    );
}

#[test]
fn migration_fifteen_leaves_the_recency_index_covering_its_sort() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..14]).unwrap();
    seed_a_v14_project(store.path());

    store.migrate_with(&migrate::MIGRATIONS[..15]).unwrap();

    let index_sql: String = Connection::open(store.path())
        .unwrap()
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'idx_stories_updated'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(
        index_sql.contains("head_global_seq"),
        "idx_stories_updated must cover head_global_seq, or StorySort::UpdatedAt's \
         `ORDER BY updated_at DESC, head_global_seq DESC, story_no` degrades to a sort \
         instead of a reverse index scan: {index_sql}"
    );
}

// ---------------------------------------------------------------------------
// Migration 16 — `snapshot.priority_assessed` (SH-359)
// ---------------------------------------------------------------------------

/// The `kind` string a seeded event goes into the store under, taken from the
/// **production** encoder rather than typed out here.
///
/// This is not fussiness. `seed_a_v14_project` seeded `'story_created'` and
/// `'story_priority_set'` for fourteen migrations — snake_case spellings that
/// no storyhook writer has ever put in that column (`domain::event_kind` emits
/// PascalCase, and `write.rs` inserts its return value verbatim). Migration
/// 15's backfill joins on `seq = head_seq` and never reads `kind`, so that
/// fixture's invented vocabulary was harmless exactly as long as nothing
/// consulted it.
///
/// Migration 16 makes `kind` the *entire* predicate, and a seat on SH-359's
/// council read the fixture as precedent and proposed the snake_case spelling
/// for the migration too. A test that seeds a spelling and a migration that
/// matches the same spelling agree with each other, pass, and match **zero
/// rows** against a real store — every story in the tracker silently
/// backfilling to "never assessed", which is a worse defect than the one
/// SH-359 fixes. Deriving the string is what makes that disagreement
/// impossible rather than merely unlikely.
///
/// SH-364 corrected the older fixture and pointed it here, and
/// `tests/event_kind_vocabulary.rs` now fails on any hand-typed `events.kind`
/// literal the binary does not emit.
fn kind_of(event: &StoryEvent) -> &'static str {
    storyhook::domain::event_kind(event)
}

/// One seeded row: `(story_no, head_seq, [(seq, event kind)])`.
///
/// Named rather than written inline because the nested tuple trips
/// `clippy::type_complexity`, and this suite treats warnings as errors.
type SeededStory<'a> = (i64, i64, &'a [(i64, &'a str)]);

/// A v15 store holding four stories whose priority histories differ, with the
/// `snapshot` blobs written the way a pre-SH-359 binary wrote them: no
/// `priority_assessed` key at all.
///
/// Raw inserts rather than the write path, because the write path is *this*
/// binary and would helpfully write the very key the migration is supposed to
/// add. The kind strings are still derived, so the seed cannot drift from
/// production even though the rows are hand-built.
fn v15_store_with_priority_histories(dir: &Path) -> SqliteStore {
    let store = SqliteStore::open(dir.join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..15]).unwrap();

    let set = kind_of(&StoryEvent::StoryPrioritySet {
        at: String::new(),
        priority: storyhook::domain::Priority::None,
    });
    let cleared = kind_of(&StoryEvent::StoryPriorityCleared { at: String::new() });
    let created = kind_of(&StoryEvent::StoryCreated {
        at: String::new(),
        title: String::new(),
        state: String::new(),
    });

    let conn = Connection::open(store.path()).unwrap();
    conn.execute_batch(
        "INSERT INTO projects (id, uuid, slug, name, prefix, created_at, next_global_seq)
             VALUES (1, 'u-1', 'proj', 'Proj', 'SH', '2026-01-01T00:00:00Z', 100);
         INSERT INTO project_states (project_id, position, slug, superstate)
             VALUES (1, 0, 'todo', 'OPEN'), (1, 1, 'done', 'CLOSED');",
    )
    .unwrap();

    // Every `snapshot` blob deliberately lacks `priority_assessed`, exactly as
    // a pre-migration row does.
    let rows: &[SeededStory] = &[
        // 1: parked on purpose — somebody ran `--priority none`.
        (1, 2, &[(1, created), (2, set)]),
        // 2: nobody ever said anything.
        (2, 1, &[(1, created)]),
        // 3: set, then cleared — unassessed again. This is the row an
        //    `EXISTS(kind = 'StoryPrioritySet')` predicate gets wrong.
        (3, 3, &[(1, created), (2, set), (3, cleared)]),
        // 4: set, cleared, set again — assessed, and only last-event-wins says so.
        (4, 4, &[(1, created), (2, set), (3, cleared), (4, set)]),
        // 5: a *stale* row — its head_seq stops before a later clear, so the
        //    backfill must read the story as it was folded, not as it now is.
        (5, 2, &[(1, created), (2, set), (3, cleared)]),
    ];

    let mut global = 1i64;
    for (story_no, head_seq, events) in rows {
        conn.execute(
            "INSERT INTO stories (project_id, story_no, head_seq, head_global_seq, title, state, \
                 superstate, priority, priority_rank, created_at, updated_at, snapshot) \
             VALUES (1, ?1, ?2, 0, 'S', 'todo', 'OPEN', 'none', 4, \
                 '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', \
                 '{\"id\":\"SH-1\",\"title\":\"S\",\"created_at\":\"2026-01-01T00:00:00Z\",\
                   \"updated_at\":\"2026-01-01T00:00:00Z\",\"state\":\"todo\",\
                   \"superstate\":\"OPEN\",\"priority\":\"none\"}')",
            rusqlite::params![story_no, head_seq],
        )
        .unwrap();
        for (seq, kind) in *events {
            conn.execute(
                "INSERT INTO events (project_id, story_no, seq, global_seq, kind, at, payload) \
                 VALUES (1, ?1, ?2, ?3, ?4, '2026-01-01T00:00:00Z', '{}')",
                rusqlite::params![story_no, seq, global, kind],
            )
            .unwrap();
            global += 1;
        }
    }
    store
}

/// Reads the migrated flag straight out of the embedded document, which is the
/// only place it lives — SH-359 adds no column.
fn assessed_in_snapshot(store: &SqliteStore, story_no: i64) -> bool {
    Connection::open(store.path())
        .unwrap()
        .query_row(
            "SELECT COALESCE(json_extract(snapshot, '$.priority_assessed'), 0) \
             FROM stories WHERE project_id = 1 AND story_no = ?1",
            rusqlite::params![story_no],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
        == 1
}

#[test]
fn migration_sixteen_backfills_assessment_from_the_last_priority_event() {
    let dir = scratch_dir();
    let store = v15_store_with_priority_histories(dir.path());

    store.migrate_with(&migrate::MIGRATIONS[..16]).unwrap();

    assert!(
        assessed_in_snapshot(&store, 1),
        "an explicit `--priority none` is a decision and must survive as one"
    );
    assert!(
        !assessed_in_snapshot(&store, 2),
        "no priority event was ever written for this story"
    );
    assert!(
        !assessed_in_snapshot(&store, 3),
        "set then cleared is unassessed — the case an EXISTS predicate marks \
         assessed forever, because it can never un-assert"
    );
    assert!(
        assessed_in_snapshot(&store, 4),
        "cleared then set again is assessed; only last-event-wins reads this right"
    );
}

#[test]
fn migration_sixteen_reads_a_stale_row_as_of_its_own_head_seq() {
    // Migration 15's bound, for migration 15's reason: `head_seq` is the event
    // this row was folded from, and reading past it would make one field of a
    // stale row fresher than the rest of it — stale in one coordinate and
    // current in another, which is harder to diagnose than consistently behind.
    let dir = scratch_dir();
    let store = v15_store_with_priority_histories(dir.path());

    store.migrate_with(&migrate::MIGRATIONS[..16]).unwrap();

    assert!(
        assessed_in_snapshot(&store, 5),
        "story 5's head_seq stops at the priority-set; the later clear is past \
         the row's own horizon and must not be read"
    );
}

#[test]
fn migration_sixteen_writes_no_key_for_a_story_nobody_assessed() {
    // `priority_assessed` is `skip_serializing_if = "is_false"`, so a fresh
    // fold omits the key entirely. Writing `false` into these rows would move
    // bytes for no change in meaning and leave every one of them differing from
    // what `put_story` writes next — which `story doctor` would then report.
    let dir = scratch_dir();
    let store = v15_store_with_priority_histories(dir.path());

    store.migrate_with(&migrate::MIGRATIONS[..16]).unwrap();

    let present: i64 = Connection::open(store.path())
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM stories \
             WHERE json_extract(snapshot, '$.priority_assessed') IS NOT NULL \
               AND story_no IN (2, 3)",
            [],
            |row| row.get(0),
        )
        .unwrap();

    assert_eq!(
        present, 0,
        "an unassessed story keeps no `priority_assessed` key at all"
    );
}

// ---------------------------------------------------------------------------
// Migration 19: type and priority are required (SH-449)
// ---------------------------------------------------------------------------

struct V18RequiredMetadataFixture {
    store: SqliteStore,
    primary: storyhook::store::ProjectId,
    secondary: storyhook::store::ProjectId,
}

fn v18_store_with_legacy_metadata(dir: &Path) -> V18RequiredMetadataFixture {
    use storyhook::domain::provenance::Provenance;
    use storyhook::domain::{Priority, StoryEvent, TypeDef, fold_story};
    use storyhook::service::project::default_states;
    use storyhook::store::{EventSeq, ExpectedSeq, NewProject, ReadOps, WriteOps};

    let store = SqliteStore::open(dir.join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..18]).unwrap();

    let (primary, secondary) = store
        .write(|tx| {
            let primary = tx.create_project(&NewProject {
                uuid: "required-primary".into(),
                slug: "required-primary".into(),
                name: "Required primary".into(),
                prefix: "SH".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            })?;
            tx.put_states(primary, &default_states())?;
            tx.put_types(
                primary,
                &[
                    TypeDef {
                        slug: "incident".into(),
                        description: None,
                        emoji: None,
                    },
                    TypeDef {
                        slug: "normal".into(),
                        description: None,
                        emoji: None,
                    },
                    TypeDef {
                        slug: "bug".into(),
                        description: None,
                        emoji: None,
                    },
                    TypeDef {
                        slug: "chore".into(),
                        description: None,
                        emoji: None,
                    },
                ],
            )?;

            let states = tx.state_map(primary)?;
            let created = |title: &str| StoryEvent::StoryCreated {
                at: "2026-01-01T00:00:00Z".into(),
                title: title.into(),
                state: "todo".into(),
            };
            let typed = |story_type: &str| StoryEvent::StoryTypeSet {
                at: "2026-01-01T00:01:00Z".into(),
                story_type: story_type.into(),
            };
            let prioritised = |priority| StoryEvent::StoryPrioritySet {
                at: "2026-01-01T00:02:00Z".into(),
                priority,
            };

            // Respectively: type-only repair, priority-only repair, both,
            // explicitly parked, cleared, already-valid control, closed, and
            // deleted. The final two prove lifecycle state is not rewritten.
            let mut histories = vec![
                vec![
                    created("missing type"),
                    prioritised(Priority::High),
                    StoryEvent::StoryLabelsSet {
                        at: "2026-01-01T00:03:00Z".into(),
                        labels: vec!["preserved".into()],
                    },
                ],
                vec![created("missing priority"), typed("normal")],
                vec![created("missing both")],
                vec![
                    created("explicitly parked"),
                    typed("chore"),
                    prioritised(Priority::None),
                ],
                vec![
                    created("cleared priority"),
                    typed("bug"),
                    prioritised(Priority::Medium),
                    StoryEvent::StoryPriorityCleared {
                        at: "2026-01-01T00:03:00Z".into(),
                    },
                ],
                vec![
                    created("already valid"),
                    prioritised(Priority::Critical),
                    typed("incident"),
                ],
                vec![
                    created("closed legacy story"),
                    StoryEvent::StoryClosedAndArchived {
                        at: "2026-01-01T00:04:00Z".into(),
                        state: "done".into(),
                    },
                ],
                vec![
                    created("deleted legacy story"),
                    StoryEvent::StoryDeleted {
                        at: "2026-01-01T00:04:00Z".into(),
                        reason: "fixture".into(),
                    },
                ],
            ];

            let mut story_nos = Vec::new();
            for events in &histories {
                let story_no = tx.allocate_story_no(primary)?;
                let head = tx.append_events(
                    primary,
                    story_no,
                    ExpectedSeq::Exact(EventSeq::new(0)),
                    events,
                    &Provenance::unrecorded(),
                )?;
                let snapshot = fold_story(&story_no.to_id("SH"), events, &states)?;
                tx.put_story(primary, &snapshot, head)?;
                story_nos.push(story_no);
            }

            // A real, symmetric dependent relation between the type-only row
            // and the valid control. It and the label above must survive the
            // parent-table rebuild, and both remain vouched by history.
            let relation_pairs = [(0usize, 5usize, "SH-6"), (5usize, 0usize, "SH-1")];
            for (index, _other_index, other_id) in relation_pairs {
                let event = StoryEvent::StoryRelationshipAdded {
                    at: "2026-01-01T00:05:00Z".into(),
                    other_id: other_id.into(),
                    relation: "relates-to".into(),
                };
                let previous_head = EventSeq::new(i64::try_from(histories[index].len()).unwrap());
                let head = tx.append_events(
                    primary,
                    story_nos[index],
                    ExpectedSeq::Exact(previous_head),
                    std::slice::from_ref(&event),
                    &Provenance::unrecorded(),
                )?;
                histories[index].push(event);
                let snapshot =
                    fold_story(&story_nos[index].to_id("SH"), &histories[index], &states)?;
                tx.put_story(primary, &snapshot, head)?;
            }

            let secondary = tx.create_project(&NewProject {
                uuid: "required-secondary".into(),
                slug: "required-secondary".into(),
                name: "Required secondary".into(),
                prefix: "OT".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            })?;
            tx.put_states(secondary, &default_states())?;
            tx.put_types(
                secondary,
                &[TypeDef {
                    slug: "normal".into(),
                    description: None,
                    emoji: None,
                }],
            )?;
            let secondary_states = tx.state_map(secondary)?;
            let secondary_story = tx.allocate_story_no(secondary)?;
            let secondary_events = vec![StoryEvent::StoryCreated {
                at: "2026-01-01T00:00:00Z".into(),
                title: "other project".into(),
                state: "todo".into(),
            }];
            let secondary_head = tx.append_events(
                secondary,
                secondary_story,
                ExpectedSeq::Exact(EventSeq::new(0)),
                &secondary_events,
                &Provenance::unrecorded(),
            )?;
            let secondary_snapshot = fold_story("OT-1", &secondary_events, &secondary_states)?;
            tx.put_story(secondary, &secondary_snapshot, secondary_head)?;

            Ok((primary, secondary))
        })
        .unwrap();

    V18RequiredMetadataFixture {
        store,
        primary,
        secondary,
    }
}

fn project_counter(path: &Path, project: storyhook::store::ProjectId) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT next_global_seq FROM projects WHERE id = ?1",
            rusqlite::params![project.get()],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn migration_nineteen_repairs_every_legacy_shape_with_ordered_real_events() {
    use storyhook::domain::{Priority, StoryEvent};
    use storyhook::store::{GlobalSeq, ReadOps, StoredPayload};

    let dir = scratch_dir();
    let fixture = v18_store_with_legacy_metadata(dir.path());
    let primary_before = project_counter(fixture.store.path(), fixture.primary);
    let secondary_before = project_counter(fixture.store.path(), fixture.secondary);
    let control_before = fixture
        .store
        .read(|tx| tx.story(fixture.primary, StoryNo::new(6)))
        .unwrap()
        .unwrap();

    fixture.store.migrate().unwrap();

    assert_eq!(
        project_counter(fixture.store.path(), fixture.primary),
        primary_before + 10,
        "the primary project receives exactly its ten repair events"
    );
    assert_eq!(
        project_counter(fixture.store.path(), fixture.secondary),
        secondary_before + 2,
        "global allocation is independent per project"
    );

    let primary_events = fixture
        .store
        .read(|tx| tx.events_since(fixture.primary, GlobalSeq::new(primary_before - 1), 20))
        .unwrap();
    let observed: Vec<(i64, &str)> = primary_events
        .iter()
        .map(|event| (event.story_no.get(), event.event.kind.as_str()))
        .collect();
    assert_eq!(
        observed,
        [
            (1, "StoryTypeSet"),
            (2, "StoryPrioritySet"),
            (3, "StoryTypeSet"),
            (3, "StoryPrioritySet"),
            (4, "StoryPrioritySet"),
            (5, "StoryPrioritySet"),
            (7, "StoryTypeSet"),
            (7, "StoryPrioritySet"),
            (8, "StoryTypeSet"),
            (8, "StoryPrioritySet"),
        ],
        "story order is stable and type precedes priority when both are absent"
    );
    assert!(
        primary_events
            .windows(2)
            .all(|pair| pair[1].event.global_seq.get() == pair[0].event.global_seq.get() + 1),
        "the migration consumes one contiguous project-global range"
    );
    let repair_at = &primary_events[0].event.at;
    assert!(
        primary_events
            .iter()
            .all(|event| &event.event.at == repair_at),
        "all repair events share the migration timestamp"
    );
    for event in &primary_events {
        match event.event.known().expect("repair events are decodable") {
            StoryEvent::StoryTypeSet { story_type, .. } => {
                assert_eq!(story_type, "incident", "the first configured type wins");
            }
            StoryEvent::StoryPrioritySet { priority, .. } => {
                assert_eq!(priority, &Priority::Low);
            }
            other => panic!("unexpected repair event: {other:?}"),
        }
        assert!(matches!(event.event.payload, StoredPayload::Known(_)));
    }

    let rows = fixture
        .store
        .read(|tx| tx.stories(fixture.primary, &storyhook::store::StoryQuery::all()))
        .unwrap();
    assert_eq!(rows.len(), 8);
    for row in &rows {
        assert!(row.snapshot.story_type.is_some());
        assert_ne!(row.snapshot.priority, Priority::None);
        assert!(row.snapshot.priority_assessed);
        if row.story_no != StoryNo::new(6) {
            assert_eq!(row.snapshot.updated_at, *repair_at);
        }
    }
    assert_eq!(rows[6].snapshot.superstate.as_str(), "CLOSED");
    assert!(rows[6].archived);
    assert!(rows[7].archived);
    assert_eq!(rows[7].snapshot.state, "closed");

    let control_after = rows
        .iter()
        .find(|row| row.story_no == StoryNo::new(6))
        .unwrap();
    assert_eq!(control_after, &control_before, "valid rows are byte-stable");

    let secondary = fixture
        .store
        .read(|tx| tx.story(fixture.secondary, StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(secondary.snapshot.story_type.as_deref(), Some("normal"));
    assert_eq!(secondary.snapshot.priority, Priority::Low);
}

#[test]
fn migration_nineteen_preserves_dependents_constraints_trigger_and_doctor_agreement() {
    use storyhook::store::{ReadOps, diff_read_model};

    let dir = scratch_dir();
    let fixture = v18_store_with_legacy_metadata(dir.path());
    let conn = Connection::open(fixture.store.path()).unwrap();
    let before: (i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM story_labels),
                    (SELECT COUNT(*) FROM story_relations)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    drop(conn);

    fixture.store.migrate().unwrap();

    let conn = Connection::open(fixture.store.path()).unwrap();
    let after: (i64, i64) = conn
        .query_row(
            "SELECT (SELECT COUNT(*) FROM story_labels),
                    (SELECT COUNT(*) FROM story_relations)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(after, before);
    assert_eq!(after, (1, 2), "both kinds of dependent row are exercised");

    let type_error = conn
        .execute(
            "UPDATE stories SET story_type = NULL WHERE project_id = ?1 AND story_no = 1",
            rusqlite::params![fixture.primary.get()],
        )
        .unwrap_err();
    assert!(type_error.to_string().contains("NOT NULL"), "{type_error}");
    let priority_error = conn
        .execute(
            "UPDATE stories SET priority = 'none', priority_rank = 4
             WHERE project_id = ?1 AND story_no = 1",
            rusqlite::params![fixture.primary.get()],
        )
        .unwrap_err();
    assert!(
        priority_error
            .to_string()
            .contains("CHECK constraint failed"),
        "{priority_error}"
    );
    let append_only_error = conn
        .execute(
            "DELETE FROM events WHERE project_id = ?1 AND story_no = 1 AND seq = 1",
            rusqlite::params![fixture.primary.get()],
        )
        .unwrap_err();
    assert!(
        append_only_error
            .to_string()
            .contains("events are append-only"),
        "{append_only_error}"
    );
    let foreign_key_faults: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(foreign_key_faults, 0);
    drop(conn);

    for project in [fixture.primary, fixture.secondary] {
        let diff = diff_read_model(&fixture.store, project).unwrap();
        assert!(diff.is_clean(), "{}", diff.describe());
    }
    let labels = fixture
        .store
        .read(|tx| {
            Ok(tx
                .story(fixture.primary, StoryNo::new(1))?
                .unwrap()
                .snapshot
                .labels)
        })
        .unwrap();
    assert_eq!(labels, ["preserved"]);
}

#[test]
fn migration_nineteen_rolls_back_when_a_project_has_no_default_type() {
    use storyhook::domain::provenance::Provenance;
    use storyhook::domain::{Priority, StoryEvent, fold_story};
    use storyhook::service::project::default_states;
    use storyhook::store::{EventSeq, ExpectedSeq, NewProject, ReadOps, WriteOps};

    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..18]).unwrap();
    let project = store
        .write(|tx| {
            let project = tx.create_project(&NewProject {
                uuid: "empty-types".into(),
                slug: "empty-types".into(),
                name: "Empty types".into(),
                prefix: "ET".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            })?;
            tx.put_states(project, &default_states())?;
            let states = tx.state_map(project)?;
            let story = tx.allocate_story_no(project)?;
            let events = vec![
                StoryEvent::StoryCreated {
                    at: "2026-01-01T00:00:00Z".into(),
                    title: "cannot default type".into(),
                    state: "todo".into(),
                },
                StoryEvent::StoryPrioritySet {
                    at: "2026-01-01T00:01:00Z".into(),
                    priority: Priority::Low,
                },
            ];
            let head = tx.append_events(
                project,
                story,
                ExpectedSeq::Exact(EventSeq::new(0)),
                &events,
                &Provenance::unrecorded(),
            )?;
            let snapshot = fold_story("ET-1", &events, &states)?;
            tx.put_story(project, &snapshot, head)?;
            Ok(project)
        })
        .unwrap();
    let counter_before = project_counter(store.path(), project);
    let conn = Connection::open(store.path()).unwrap();
    let events_before: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    drop(conn);

    let error = store.migrate().unwrap_err();
    assert!(error.to_string().contains("NOT NULL"), "{error}");
    assert_eq!(store.schema_version().unwrap(), 18);
    assert_eq!(project_counter(store.path(), project), counter_before);
    let conn = Connection::open(store.path()).unwrap();
    let events_after: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        events_after, events_before,
        "the appended repair rolled back"
    );
    let still_untyped: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM stories WHERE story_type IS NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(still_untyped, 1);
    let trigger_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'trigger' AND name = 'events_reject_delete'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        trigger_exists, 1,
        "DDL inside the failed migration rolled back"
    );
}

// ---------------------------------------------------------------------------
// Migration 20: structural epics have computed state (SH-446)
// ---------------------------------------------------------------------------

#[test]
fn migration_twenty_clears_existing_epic_state_and_drops_the_single_parent_index() {
    use storyhook::domain::provenance::Provenance;
    use storyhook::domain::{Priority, TypeDef, fold_story};
    use storyhook::service::project::default_states;
    use storyhook::store::{EventSeq, ExpectedSeq, NewProject, ReadOps, WriteOps};

    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..19]).unwrap();
    let project = store
        .write(|tx| {
            let project = tx.create_project(&NewProject {
                uuid: "computed-epic".into(),
                slug: "computed-epic".into(),
                name: "Computed epic".into(),
                prefix: "SH".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            })?;
            tx.put_states(project, &default_states())?;
            tx.put_types(
                project,
                &[TypeDef {
                    slug: "normal".into(),
                    description: None,
                    emoji: None,
                }],
            )?;
            let states = tx.state_map(project)?;
            for (story_no, title, relation, other_id) in [
                (StoryNo::new(1), "parent", "parent-of", "SH-2"),
                (StoryNo::new(2), "child", "child-of", "SH-1"),
            ] {
                let events = vec![
                    StoryEvent::StoryCreated {
                        at: "2026-01-01T00:00:00Z".into(),
                        title: title.into(),
                        state: "todo".into(),
                    },
                    StoryEvent::StoryTypeSet {
                        at: "2026-01-01T00:01:00Z".into(),
                        story_type: "normal".into(),
                    },
                    StoryEvent::StoryPrioritySet {
                        at: "2026-01-01T00:02:00Z".into(),
                        priority: Priority::Low,
                    },
                    StoryEvent::StoryRelationshipAdded {
                        at: "2026-01-01T00:03:00Z".into(),
                        other_id: other_id.into(),
                        relation: relation.into(),
                    },
                ];
                let allocated = tx.allocate_story_no(project)?;
                assert_eq!(allocated, story_no);
                let head = tx.append_events(
                    project,
                    story_no,
                    ExpectedSeq::Exact(EventSeq::new(0)),
                    &events,
                    &Provenance::unrecorded(),
                )?;
                let snapshot = fold_story(&story_no.to_id("SH"), &events, &states)?;
                tx.put_story(project, &snapshot, head)?;
            }
            Ok(project)
        })
        .unwrap();

    store.migrate().unwrap();

    let parent = store
        .read(|tx| tx.story(project, StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert!(parent.snapshot.state_computed);
    let last = store
        .read(|tx| tx.events_for(project, StoryNo::new(1)))
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(last.kind, "StoryStateCleared");
    assert!(matches!(
        last.known(),
        Some(StoryEvent::StoryStateCleared { .. })
    ));

    let conn = Connection::open(store.path()).unwrap();
    let index_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_story_relations_single_parent'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index_count, 0);
}

// ---------------------------------------------------------------------------
// Migration 21: the `closed` state, and soft deletes coming to rest in it
// (SH-505)
// ---------------------------------------------------------------------------

/// Seeds one v20 project holding a soft-deleted story, straight to the tables.
///
/// Straight to the tables for the reason every seeder in this file is: the
/// fixture must stay buildable on the schema that precedes the migration under
/// test. A soft-deleted story on v20 is already `done`/CLOSED/archived —
/// migration 4 made `superstate` a pure function of the slug and the composite
/// foreign key enforces it — so this seeds the shape a real store actually
/// holds, not the pre-migration-4 one.
///
/// The story is CLOSED BEFORE it is deleted, deliberately: that is the one
/// shape where `closed_at` and the `StoryDeleted` event's own `at` differ, and
/// it is the case `fold_story_deleted_while_closed_keeps_original_closed_at`
/// already pins on the fold side. A fixture whose two timestamps coincided
/// would pass against a migration that read `hidden_at` off the wrong one.
///
/// `extra_events` is appended after the `StoryDeleted`, which is how the
/// hidden/unhidden cases are built.
///
/// The event kinds are the production spellings (`domain::event_kind`'s
/// PascalCase), never a fixture dialect: a seeder and a migration that agree on
/// a spelling nothing else emits pass together and match zero rows in every
/// real store (SH-364).
fn seed_a_v20_soft_deleted_story(path: &Path, extra_events: &str) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(&format!(
        "INSERT INTO projects (id, uuid, slug, name, prefix, created_at, next_story_no, next_global_seq)
             VALUES (1, 'u-1', 'proj', 'Proj', 'SH', '2026-01-01T00:00:00Z', 2, 4);
         INSERT INTO project_states (project_id, position, slug, superstate)
             VALUES (1, 0, 'todo', 'OPEN'), (1, 1, 'done', 'CLOSED');
         INSERT INTO events (project_id, story_no, seq, global_seq, kind, at, payload)
             VALUES (1, 1, 1, 1, 'StoryCreated', '2026-01-01T00:00:00Z',
                     json_object('kind', 'StoryCreated', 'at', '2026-01-01T00:00:00Z',
                                 'title', 'Filed by mistake', 'state', 'todo')),
                    (1, 1, 2, 2, 'StoryClosedAndArchived', '2026-01-01T12:00:00Z',
                     json_object('kind', 'StoryClosedAndArchived', 'at', '2026-01-01T12:00:00Z',
                                 'state', 'done')),
                    (1, 1, 3, 3, 'StoryDeleted', '2026-01-02T00:00:00Z',
                     json_object('kind', 'StoryDeleted', 'at', '2026-01-02T00:00:00Z',
                                 'reason', 'created in error')){extra_events};
         INSERT INTO stories (project_id, story_no, head_seq, head_global_seq, title, state,
                              superstate, priority, priority_rank, story_type, deleted, archived,
                              created_at, updated_at, closed_at, snapshot)
             VALUES (1, 1, {head_seq}, {head_seq}, 'Filed by mistake', 'done', 'CLOSED',
                     'low', 3, 'normal', 1, 1,
                     '2026-01-01T00:00:00Z', '2026-01-02T00:00:00Z', '2026-01-01T12:00:00Z',
                     json_object('id', 'SH-1', 'title', 'Filed by mistake', 'state', 'done',
                                 'superstate', 'CLOSED', 'deleted', json('true'),
                                 'deleted_reason', 'created in error',
                                 'created_at', '2026-01-01T00:00:00Z',
                                 'updated_at', '2026-01-02T00:00:00Z',
                                 'closed_at', '2026-01-01T12:00:00Z'));",
        extra_events = extra_events,
        head_seq = 3 + extra_events.matches("(1, 1,").count(),
    ))
    .unwrap();
}

fn one_story_row(path: &Path) -> (String, Option<String>, String) {
    Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT state, hidden_at, snapshot FROM stories WHERE project_id = 1 AND story_no = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

fn state_slugs(path: &Path) -> Vec<String> {
    let conn = Connection::open(path).unwrap();
    let mut stmt = conn
        .prepare("SELECT slug FROM project_states WHERE project_id = 1 ORDER BY position")
        .unwrap();
    let rows = stmt.query_map([], |row| row.get(0)).unwrap();
    rows.map(Result::unwrap).collect()
}

#[test]
fn migration_twenty_one_gives_every_project_a_closed_state_at_the_end() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..20]).unwrap();
    seed_a_v20_soft_deleted_story(store.path(), "");

    assert_eq!(state_slugs(store.path()), ["todo", "done"]);

    store.migrate_with(&migrate::MIGRATIONS[..21]).unwrap();

    assert_eq!(
        state_slugs(store.path()),
        ["todo", "done", "closed"],
        "the repair appends; it never reorders what the project already had"
    );
    let (role, description): (Option<String>, Option<String>) = Connection::open(store.path())
        .unwrap()
        .query_row(
            "SELECT role, description FROM project_states WHERE project_id = 1 AND slug = 'closed'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(
        (role, description),
        (None, None),
        "`with_required_states` writes neither, so a migrated project and a \
         `doctor --fix`ed one must not disagree"
    );
}

#[test]
fn migration_twenty_one_rests_a_soft_deleted_story_in_closed_and_archives_it() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..20]).unwrap();
    seed_a_v20_soft_deleted_story(store.path(), "");

    store.migrate_with(&migrate::MIGRATIONS[..21]).unwrap();

    let (state, hidden_at, snapshot) = one_story_row(store.path());
    assert_eq!(state, "closed");
    assert_eq!(
        hidden_at.as_deref(),
        Some("2026-01-02T00:00:00Z"),
        "the stamp is the StoryDeleted event's own `at` — never `closed_at`, \
         which is the earlier of the two whenever a story was closed before \
         being deleted"
    );

    // The blob is what `read::hydrate` deserializes; it never re-folds, so a
    // document left behind would show the wrong state in `story show` and make
    // `story doctor` report every such story as divergent.
    let doc: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
    assert_eq!(doc["state"], "closed");
    assert_eq!(doc["hidden_at"], "2026-01-02T00:00:00Z");
    assert!(
        doc.get("deleted").is_none() || doc["deleted"] == serde_json::Value::Bool(true),
        "the `deleted` key is left alone by this migration — it still describes \
         something real until `story delete` becomes permanent"
    );
}

#[test]
fn migration_twenty_one_leaves_an_unhidden_soft_delete_unhidden() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..20]).unwrap();
    seed_a_v20_soft_deleted_story(
        store.path(),
        ",\n                    (1, 1, 4, 4, 'StoryUnhidden', '2026-01-03T00:00:00Z',
                     json_object('kind', 'StoryUnhidden', 'at', '2026-01-03T00:00:00Z'))",
    );

    store.migrate_with(&migrate::MIGRATIONS[..21]).unwrap();

    let (state, hidden_at, snapshot) = one_story_row(store.path());
    assert_eq!(state, "closed");
    assert_eq!(
        hidden_at, None,
        "the last of the three hidden-affecting kinds wins, exactly as the fold \
         replays them — otherwise `story unarchive` would be undone by an upgrade"
    );
    let doc: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
    assert!(
        doc.get("hidden_at").is_none(),
        "`hidden_at` is skip_serializing_if = is_none, so the NULL case removes \
         the key rather than writing JSON null: {doc}"
    );
}

#[test]
fn migration_twenty_one_leaves_alone_a_project_whose_closed_state_is_open() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..20]).unwrap();
    seed_a_v20_soft_deleted_story(store.path(), "");
    Connection::open(store.path())
        .unwrap()
        .execute(
            "INSERT INTO project_states (project_id, position, slug, superstate)
                 VALUES (1, 2, 'closed', 'OPEN')",
            [],
        )
        .unwrap();

    store.migrate_with(&migrate::MIGRATIONS[..21]).unwrap();

    let (state, hidden_at, _snapshot) = one_story_row(store.path());
    assert_eq!(
        state, "done",
        "nothing may reclassify a state the project already defines, so these \
         stories stay where they are — and `resting_state_for_closure` answers \
         `done` for them too, which is what keeps a fresh fold agreeing with \
         the stored row"
    );
    assert_eq!(hidden_at, None);
    assert_eq!(
        state_slugs(store.path()),
        ["todo", "done", "closed"],
        "the existing slug is left exactly as the project wrote it"
    );
}

/// The case that makes keying on the `deleted` COLUMN load-bearing rather than
/// stylistic, and migration 16's lesson on a different fact.
///
/// `[StoryDeleted, StoryStateChanged(todo)]` is a live, reachable history —
/// `story delete` then `story reopen --force` — whose story is *not* deleted:
/// the fold retracts the closure markers on a move into an OPEN state. A
/// migration keyed on `EXISTS (… kind = 'StoryDeleted')` would archive it,
/// silently taking an open story off the board on upgrade.
#[test]
fn migration_twenty_one_leaves_an_undeleted_story_open() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..20]).unwrap();
    let conn = Connection::open(store.path()).unwrap();
    conn.execute_batch(
        "INSERT INTO projects (id, uuid, slug, name, prefix, created_at, next_story_no, next_global_seq)
             VALUES (1, 'u-1', 'proj', 'Proj', 'SH', '2026-01-01T00:00:00Z', 2, 4);
         INSERT INTO project_states (project_id, position, slug, superstate)
             VALUES (1, 0, 'todo', 'OPEN'), (1, 1, 'done', 'CLOSED');
         INSERT INTO events (project_id, story_no, seq, global_seq, kind, at, payload)
             VALUES (1, 1, 1, 1, 'StoryCreated', '2026-01-01T00:00:00Z',
                     json_object('kind', 'StoryCreated', 'at', '2026-01-01T00:00:00Z',
                                 'title', 'Deleted then reopened', 'state', 'todo')),
                    (1, 1, 2, 2, 'StoryDeleted', '2026-01-02T00:00:00Z',
                     json_object('kind', 'StoryDeleted', 'at', '2026-01-02T00:00:00Z',
                                 'reason', 'created in error')),
                    (1, 1, 3, 3, 'StoryStateChanged', '2026-01-03T00:00:00Z',
                     json_object('kind', 'StoryStateChanged', 'at', '2026-01-03T00:00:00Z',
                                 'state', 'todo'));
         INSERT INTO stories (project_id, story_no, head_seq, head_global_seq, title, state,
                              superstate, priority, priority_rank, story_type, deleted, archived,
                              created_at, updated_at, snapshot)
             VALUES (1, 1, 3, 3, 'Deleted then reopened', 'todo', 'OPEN',
                     'low', 3, 'normal', 0, 0,
                     '2026-01-01T00:00:00Z', '2026-01-03T00:00:00Z',
                     json_object('id', 'SH-1', 'title', 'Deleted then reopened', 'state', 'todo',
                                 'superstate', 'OPEN',
                                 'created_at', '2026-01-01T00:00:00Z',
                                 'updated_at', '2026-01-03T00:00:00Z'));",
    )
    .unwrap();
    drop(conn);

    store.migrate_with(&migrate::MIGRATIONS[..21]).unwrap();

    let (state, hidden_at, snapshot) = one_story_row(store.path());
    assert_eq!(state, "todo", "the undelete stands");
    assert_eq!(hidden_at, None);
    let doc: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
    assert_eq!(doc["state"], "todo");
    assert!(doc.get("hidden_at").is_none(), "{doc}");
}

// ---------------------------------------------------------------------------
// Migration 22: CLOSED blockers own no live task dependencies (SH-500)
// ---------------------------------------------------------------------------

#[test]
fn migration_twenty_two_retracts_closed_blocker_edges_with_real_events() {
    use storyhook::domain::provenance::Provenance;
    use storyhook::domain::{Priority, TypeDef, fold_story};
    use storyhook::service::project::default_states;
    use storyhook::store::rebuild::diff_read_model;
    use storyhook::store::{EventSeq, ExpectedSeq, NewProject, ReadOps, WriteOps};

    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..21]).unwrap();

    let project = store
        .write(|tx| {
            let project = tx.create_project(&NewProject {
                uuid: "closed-blocker-edges".into(),
                slug: "closed-blocker-edges".into(),
                name: "Closed blocker edges".into(),
                prefix: "SH".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            })?;
            tx.put_states(project, &default_states())?;
            tx.put_types(
                project,
                &[TypeDef {
                    slug: "normal".into(),
                    description: None,
                    emoji: None,
                }],
            )?;
            let states = tx.state_map(project)?;
            let provenance = Provenance::unrecorded();

            // All endpoints must exist before relation rows can satisfy their
            // immediate foreign keys. Seed their scalar histories first.
            let mut logs = std::collections::BTreeMap::new();
            for title in [
                "closed blocker",
                "dependent",
                "related",
                "open blocker",
                "legacy half-dependent",
            ] {
                let story_no = tx.allocate_story_no(project)?;
                let events = vec![
                    StoryEvent::StoryCreated {
                        at: "2026-01-01T00:00:00Z".into(),
                        title: title.into(),
                        state: "todo".into(),
                    },
                    StoryEvent::StoryPrioritySet {
                        at: "2026-01-01T00:01:00Z".into(),
                        priority: Priority::Low,
                    },
                    StoryEvent::StoryTypeSet {
                        at: "2026-01-01T00:02:00Z".into(),
                        story_type: "normal".into(),
                    },
                ];
                let head = tx.append_events(
                    project,
                    story_no,
                    ExpectedSeq::Exact(EventSeq::ZERO),
                    &events,
                    &provenance,
                )?;
                let snapshot = fold_story(&story_no.to_id("SH"), &events, &states)?;
                tx.put_story(project, &snapshot, head)?;
                logs.insert(story_no, events);
            }

            let additions: [(StoryNo, Vec<StoryEvent>); 4] = [
                (
                    StoryNo::new(1),
                    vec![
                        StoryEvent::StoryRelationshipAdded {
                            at: "2026-01-02T00:00:00Z".into(),
                            other_id: "SH-2".into(),
                            relation: "blocks".into(),
                        },
                        StoryEvent::StoryRelationshipAdded {
                            at: "2026-01-02T00:00:00Z".into(),
                            other_id: "SH-3".into(),
                            relation: "relates-to".into(),
                        },
                        // No matching blocked-by event is written to SH-5:
                        // the relation-index mirror still knows the pair,
                        // exercising migration 22's legacy half-edge path.
                        StoryEvent::StoryRelationshipAdded {
                            at: "2026-01-02T00:00:00Z".into(),
                            other_id: "SH-5".into(),
                            relation: "blocks".into(),
                        },
                        StoryEvent::StoryStateChanged {
                            at: "2026-01-03T00:00:00Z".into(),
                            state: "done".into(),
                        },
                        StoryEvent::StoryClosedAndArchived {
                            at: "2026-01-03T00:00:00Z".into(),
                            state: "done".into(),
                        },
                    ],
                ),
                (
                    StoryNo::new(2),
                    vec![
                        StoryEvent::StoryRelationshipAdded {
                            at: "2026-01-02T00:00:00Z".into(),
                            other_id: "SH-1".into(),
                            relation: "blocked-by".into(),
                        },
                        StoryEvent::StoryRelationshipAdded {
                            at: "2026-01-02T00:00:00Z".into(),
                            other_id: "SH-4".into(),
                            relation: "blocked-by".into(),
                        },
                    ],
                ),
                (
                    StoryNo::new(3),
                    vec![StoryEvent::StoryRelationshipAdded {
                        at: "2026-01-02T00:00:00Z".into(),
                        other_id: "SH-1".into(),
                        relation: "relates-to".into(),
                    }],
                ),
                (
                    StoryNo::new(4),
                    vec![StoryEvent::StoryRelationshipAdded {
                        at: "2026-01-02T00:00:00Z".into(),
                        other_id: "SH-2".into(),
                        relation: "blocks".into(),
                    }],
                ),
            ];
            for (story_no, added) in additions {
                let events = logs.get_mut(&story_no).unwrap();
                let expected = EventSeq::new(events.len() as i64);
                events.extend(added);
                let new_events = &events[expected.get() as usize..];
                let head = tx.append_events(
                    project,
                    story_no,
                    ExpectedSeq::Exact(expected),
                    new_events,
                    &provenance,
                )?;
                let snapshot = fold_story(&story_no.to_id("SH"), events, &states)?;
                tx.put_story(project, &snapshot, head)?;
            }
            Ok(project)
        })
        .unwrap();

    let before_counter = project_counter(store.path(), project);
    store.migrate().unwrap();

    let (blocker, dependent) = store
        .read(|tx| {
            Ok((
                tx.story(project, StoryNo::new(1))?.unwrap(),
                tx.story(project, StoryNo::new(2))?.unwrap(),
            ))
        })
        .unwrap();
    assert_eq!(
        blocker.snapshot.relationships,
        [storyhook::domain::StoryRelation {
            relation: "relates-to".into(),
            other_id: "SH-3".into(),
        }]
    );
    assert_eq!(
        dependent.snapshot.relationships,
        [storyhook::domain::StoryRelation {
            relation: "blocked-by".into(),
            other_id: "SH-4".into(),
        }]
    );
    for story_no in [StoryNo::new(1), StoryNo::new(2)] {
        let last = store
            .read(|tx| tx.events_for(project, story_no))
            .unwrap()
            .pop()
            .unwrap();
        assert!(matches!(
            last.known(),
            Some(StoryEvent::StoryRelationshipRemoved { .. })
        ));
    }
    assert_eq!(
        project_counter(store.path(), project),
        before_counter + 3,
        "the symmetric edge gets two compensations and the half-edge gets one"
    );
    let half_dependent_events = store
        .read(|tx| tx.events_for(project, StoryNo::new(5)))
        .unwrap();
    assert!(
        half_dependent_events.iter().all(|event| !matches!(
            event.known(),
            Some(StoryEvent::StoryRelationshipRemoved { .. })
        )),
        "a history that never asserted the inverse must not invent a removal"
    );
    let edges = store
        .read(|tx| tx.relations_from(project, StoryNo::new(1)))
        .unwrap();
    assert_eq!(
        edges
            .into_iter()
            .map(|edge| (edge.relation, edge.other_no))
            .collect::<Vec<_>>(),
        [("relates-to".to_string(), StoryNo::new(3))]
    );
    assert!(
        diff_read_model(&store, project).unwrap().is_clean(),
        "migration output must agree with a fresh event replay"
    );
}

// ---------------------------------------------------------------------------
// Migration 23: deletion is hard and no tombstone fields remain (SH-498)
// ---------------------------------------------------------------------------

#[test]
fn migration_twenty_three_drops_the_deleted_column_and_snapshot_keys() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..20]).unwrap();
    seed_a_v20_soft_deleted_story(store.path(), "");
    store.migrate_with(&migrate::MIGRATIONS[..21]).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..22]).unwrap();

    store.migrate_with(&migrate::MIGRATIONS[..23]).unwrap();

    let conn = Connection::open(store.path()).unwrap();
    let mut columns = conn.prepare("PRAGMA table_info(stories)").unwrap();
    let names: Vec<String> = columns
        .query_map([], |row| row.get(1))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(!names.iter().any(|name| name == "deleted"), "{names:?}");

    let snapshot: String = conn
        .query_row(
            "SELECT snapshot FROM stories WHERE project_id = 1 AND story_no = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let doc: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
    assert!(doc.get("deleted").is_none(), "{doc}");
    assert!(doc.get("deleted_reason").is_none(), "{doc}");

    let legacy_events: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE kind = 'StoryDeleted'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(legacy_events, 1, "migration preserves replayable history");
}

// ---------------------------------------------------------------------------
// Migration 25: verification is a required centralized handoff (SH-521)
// ---------------------------------------------------------------------------

#[test]
fn migration_twenty_five_inserts_verifying_before_required_blocked() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..24]).unwrap();
    let conn = Connection::open(store.path()).unwrap();
    conn.execute_batch(
        "INSERT INTO projects
             (id, uuid, slug, name, prefix, created_at, next_story_no, next_global_seq)
         VALUES
             (1, 'u-1', 'first', 'First', 'A', '2026-01-01T00:00:00Z', 1, 1),
             (2, 'u-2', 'second', 'Second', 'B', '2026-01-01T00:00:00Z', 1, 1);
         INSERT INTO project_states (project_id, position, slug, superstate, role, description)
         VALUES
             (1, 0, 'todo',        'OPEN',   NULL,     NULL),
             (1, 1, 'in-progress', 'OPEN',   'active', NULL),
             (1, 2, 'review',      'OPEN',   NULL,     NULL),
             (1, 3, 'blocked',     'OPEN',   NULL,     NULL),
             (1, 4, 'done',        'CLOSED', NULL,     NULL),
             (1, 5, 'closed',      'CLOSED', NULL,     NULL),
             (2, 0, 'todo',        'OPEN',   NULL,     NULL),
             (2, 1, 'in-progress', 'OPEN',   'active', NULL),
             (2, 2, 'verifying',   'OPEN',   NULL,     'custom description'),
             (2, 3, 'blocked',     'OPEN',   NULL,     NULL),
             (2, 4, 'done',        'CLOSED', NULL,     NULL),
             (2, 5, 'closed',      'CLOSED', NULL,     NULL);",
    )
    .unwrap();
    drop(conn);

    store.migrate_with(&migrate::MIGRATIONS[..25]).unwrap();

    let conn = Connection::open(store.path()).unwrap();
    let slugs = |project: i64| {
        let mut statement = conn
            .prepare(
                "SELECT slug FROM project_states
                  WHERE project_id = ?1 ORDER BY position",
            )
            .unwrap();
        statement
            .query_map([project], |row| row.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        slugs(1),
        [
            "todo",
            "in-progress",
            "review",
            "verifying",
            "blocked",
            "done",
            "closed"
        ]
    );
    assert_eq!(
        slugs(2),
        [
            "todo",
            "in-progress",
            "verifying",
            "blocked",
            "done",
            "closed"
        ],
        "an existing verifying state is neither duplicated nor rewritten"
    );
    let description: Option<String> = conn
        .query_row(
            "SELECT description FROM project_states
              WHERE project_id = 2 AND slug = 'verifying'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(description.as_deref(), Some("custom description"));
}

// ---------------------------------------------------------------------------
// Migration 29: engine lanes retain exact cleanup identity (SH-539)
// ---------------------------------------------------------------------------

#[test]
fn migration_twenty_nine_adds_a_nullable_cleanup_lease_without_rewriting_lanes() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..28]).unwrap();
    let conn = Connection::open(store.path()).unwrap();
    conn.execute_batch(
        "INSERT INTO engine_runs
             (id, project_slug, scope_kind, lanes, agent, state,
              consecutive_hard_stops, created_at, updated_at)
         VALUES
             ('run-legacy', 'alpha', 'project', 1, 'codex', 'running',
              0, '2026-09-04T00:00:00Z', '2026-09-04T00:00:00Z');
         INSERT INTO engine_lanes
             (run_id, lane_index, state, story_id, window_name, worktree_path,
              dispatched_at, last_observed_at, outcome, outcome_detail,
              last_progress_seq, last_progress_at)
         VALUES
             ('run-legacy', 0, 'working', 'SH-1', 'SH-1', '/old/SH-1',
              '2026-09-04T00:00:00Z', '2026-09-04T00:00:00Z', NULL, NULL,
              1, '2026-09-04T00:00:00Z');",
    )
    .unwrap();
    drop(conn);

    store.migrate_with(&migrate::MIGRATIONS[..29]).unwrap();

    let conn = Connection::open(store.path()).unwrap();
    let lease: Option<String> = conn
        .query_row(
            "SELECT cleanup_lease_json FROM engine_lanes
             WHERE run_id = 'run-legacy' AND lane_index = 0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        lease, None,
        "legacy lanes carry no invented cleanup identity"
    );
    let story_id: String = conn
        .query_row(
            "SELECT story_id FROM engine_lanes
             WHERE run_id = 'run-legacy' AND lane_index = 0",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(story_id, "SH-1", "the occupied lane survives unchanged");
}

// ---------------------------------------------------------------------------
// Migration 30: labels have one lowercase identity (SH-204)
// ---------------------------------------------------------------------------

#[test]
fn migration_thirty_normalizes_every_story_with_real_events() {
    use storyhook::domain::provenance::Provenance;
    use storyhook::domain::{Priority, StoryEvent, TypeDef, fold_story};
    use storyhook::service::project::default_states;
    use storyhook::store::rebuild::diff_read_model;
    use storyhook::store::{EventSeq, ExpectedSeq, NewProject, ReadOps, WriteOps};

    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..29]).unwrap();

    let project = store
        .write(|tx| {
            let project = tx.create_project(&NewProject {
                uuid: "lowercase-labels".into(),
                slug: "lowercase-labels".into(),
                name: "Lowercase labels".into(),
                prefix: "SH".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            })?;
            tx.put_states(project, &default_states())?;
            tx.put_types(
                project,
                &[TypeDef {
                    slug: "normal".into(),
                    description: None,
                    emoji: None,
                }],
            )?;
            let states = tx.state_map(project)?;

            for (title, labels, closed) in [
                ("open", vec!["Web", "WEB", "CAFÉ"], false),
                ("closed", vec!["ÄPPLE", "API,SSE"], true),
                ("control", vec!["already", "canonical"], false),
                ("label-free control", vec![], false),
            ] {
                let story_no = tx.allocate_story_no(project)?;
                let mut events = vec![
                    StoryEvent::StoryCreated {
                        at: "2026-01-01T00:00:00Z".into(),
                        title: title.into(),
                        state: "todo".into(),
                    },
                    StoryEvent::StoryPrioritySet {
                        at: "2026-01-01T00:01:00Z".into(),
                        priority: Priority::Low,
                    },
                    StoryEvent::StoryTypeSet {
                        at: "2026-01-01T00:02:00Z".into(),
                        story_type: "normal".into(),
                    },
                    StoryEvent::StoryLabelsSet {
                        at: "2026-01-01T00:03:00Z".into(),
                        labels: labels.into_iter().map(str::to_string).collect(),
                    },
                ];
                if closed {
                    events.extend([
                        StoryEvent::StoryStateChanged {
                            at: "2026-01-01T00:04:00Z".into(),
                            state: "done".into(),
                        },
                        StoryEvent::StoryClosedAndArchived {
                            at: "2026-01-01T00:04:00Z".into(),
                            state: "done".into(),
                        },
                    ]);
                }
                let head = tx.append_events(
                    project,
                    story_no,
                    ExpectedSeq::Exact(EventSeq::ZERO),
                    &events,
                    &Provenance::unrecorded(),
                )?;
                let snapshot = fold_story(&story_no.to_id("SH"), &events, &states)?;
                tx.put_story(project, &snapshot, head)?;
            }
            Ok(project)
        })
        .unwrap();

    let before_counter = project_counter(store.path(), project);
    let control_before = store
        .read(|tx| tx.story(project, StoryNo::new(3)))
        .unwrap()
        .unwrap();
    let label_free_before = store
        .read(|tx| tx.story(project, StoryNo::new(4)))
        .unwrap()
        .unwrap();

    store.migrate().unwrap();

    let rows = store
        .read(|tx| tx.stories(project, &storyhook::store::StoryQuery::all()))
        .unwrap();
    assert_eq!(rows[0].snapshot.labels, ["café", "web"]);
    assert_eq!(rows[1].snapshot.labels, ["api", "sse", "äpple"]);
    assert_eq!(rows[1].snapshot.superstate.as_str(), "CLOSED");
    assert_eq!(rows[2], control_before, "canonical rows stay byte-stable");
    assert_eq!(
        rows[3], label_free_before,
        "an omitted empty-label field stays byte-stable"
    );
    assert_eq!(project_counter(store.path(), project), before_counter + 2);

    for story_no in [StoryNo::new(1), StoryNo::new(2)] {
        let events = store.read(|tx| tx.events_for(project, story_no)).unwrap();
        assert!(
            events.iter().any(|event| matches!(
                event.known(),
                Some(StoryEvent::StoryLabelsSet { labels, .. })
                    if labels.iter().any(|label| label.chars().any(char::is_uppercase))
                        || labels.iter().any(|label| label.contains(','))
            )),
            "the historical spelling must survive"
        );
        assert!(matches!(
            events.last().and_then(|event| event.known()),
            Some(StoryEvent::StoryLabelsSet { labels, .. })
                if labels.iter().all(|label| label == &label.to_lowercase())
                    && labels.iter().all(|label| !label.contains(','))
        ));
    }
    assert!(diff_read_model(&store, project).unwrap().is_clean());
}
