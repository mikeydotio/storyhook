//! The SQLite engine behind the [`Store`](crate::store::Store) trait.
//!
//! Concurrency model, in full:
//!
//! - **Reads** take any connection from an explicit pool and run inside a
//!   deferred transaction, so a multi-statement read sees one consistent
//!   snapshot. Write-ahead logging means they never block a writer and a
//!   writer never blocks them.
//! - **Writes** serialize on a process-wide mutex and then open with `BEGIN
//!   IMMEDIATE`, which takes SQLite's write lock up front rather than
//!   discovering halfway through a transaction that it cannot be had. The
//!   mutex makes contention *within* a process free; `busy_timeout` handles
//!   contention *between* processes, and is set to the same five seconds the
//!   old `.storyhook/lock` deadline used, so the `LockTimeout` contract is
//!   preserved exactly.
//! - **The pool is explicit**, a `Mutex<Vec<Connection>>` holding at most eight
//!   idle connections, rather than a thread-local. A thread-local pool leaks
//!   one connection per thread that ever touched the store and cannot be
//!   drained, closed, or counted — which makes the file impossible to delete on
//!   Windows and impossible to assert on in a test.
//!
//! Databases are always file-backed, in tests as much as in production.
//! `:memory:` has no write-ahead log, no reopen, and no crash, which are the
//! three things every guarantee in this module is about.

pub(crate) mod read;
pub(crate) mod write;

use std::collections::BTreeMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use rusqlite::Connection;

use crate::domain::{Member, StateDef, StoryEvent, StorySnapshot, TypeDef};
use crate::store::error::StoreError;
use crate::store::fault::{FaultPoint, fire};
use crate::store::ids::{EventSeq, ExpectedSeq, GlobalSeq, PathKind, ProjectId, StoryNo};
use crate::store::migrate::{self, MIGRATIONS, Migration, current_schema_version};
use crate::store::types::{
    FeedEvent, MigrationReport, NewProject, ProjectPathRecord, ProjectRecord, ProjectSettings,
    RawEvent, RelationEdge, StoredEvent, StoryQuery, StoryRow,
};
use crate::store::{ReadOps, Store, WriteOps};

/// Puts a database into write-ahead logging mode, and reports the mode it ended
/// up in.
///
/// Two things are going on, and both were learned the hard way.
///
/// **It reads before it writes.** Journal mode is persistent per *database*, so
/// on a store that is already in WAL the write has nothing to do — but
/// `PRAGMA journal_mode = WAL` still takes an exclusive lock to decide that.
/// Every process opening the store paid for that lock, and enough of them at
/// once turned `story init` into a `LockTimeout`.
///
/// **It retries.** SQLite's `busy_timeout` does not cover a journal-mode
/// change, so N processes racing to create the same database — which is what a
/// parallel test run is — collide on the one write that genuinely has to
/// happen. The retry re-reads the mode each time, so a process whose write lost
/// the race sees the winner's result and stops rather than fighting for a
/// change that has already been made.
fn ensure_wal(conn: &Connection, deadline: Duration) -> Result<String, StoreError> {
    let give_up_at = std::time::Instant::now() + deadline;
    loop {
        let current: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .map_err(|e| StoreError::from_sqlite(e, "reading the journal mode"))?;
        if current.eq_ignore_ascii_case("wal") {
            return Ok(current);
        }
        match conn.query_row("PRAGMA journal_mode = WAL", [], |row| {
            row.get::<_, String>(0)
        }) {
            Ok(mode) => return Ok(mode),
            Err(error) => {
                let failure = StoreError::from_sqlite(error, "enabling write-ahead logging");
                if !matches!(failure, StoreError::Busy(_))
                    || std::time::Instant::now() >= give_up_at
                {
                    return Err(failure);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// How many idle connections the pool keeps.
///
/// A cap on *idle* connections, not on concurrency: a read that arrives when
/// the pool is empty opens its own connection rather than waiting, so the pool
/// can never deadlock a caller. Eight is chosen to comfortably exceed the
/// daemon's request concurrency while keeping the process's open file handles
/// countable.
const DEFAULT_POOL_SIZE: usize = 8;

/// The busy timeout, matching the legacy project lock's five-second deadline.
const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Where a [`SqliteStore`] lives and how it behaves.
#[derive(Clone, Debug)]
pub struct StoreConfig {
    /// The database file. Its parent directory is created if missing.
    pub db_path: PathBuf,
    /// Where pre-migration backups are written.
    pub backup_dir: PathBuf,
    /// How many idle connections to keep pooled.
    pub pool_size: usize,
    /// How long a statement waits for another process's write lock.
    pub busy_timeout: Duration,
}

impl StoreConfig {
    /// Defaults for a database at `db_path`: backups in a `backups/` directory
    /// beside it, eight pooled connections, a five-second busy timeout.
    #[must_use]
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        let db_path = db_path.into();
        let backup_dir = db_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("backups");
        Self {
            db_path,
            backup_dir,
            pool_size: DEFAULT_POOL_SIZE,
            busy_timeout: DEFAULT_BUSY_TIMEOUT,
        }
    }
}

/// A [`Store`] backed by one SQLite database file.
pub struct SqliteStore {
    config: StoreConfig,
    pool: Mutex<Vec<Connection>>,
    write_lock: Mutex<()>,
    /// A connection that never enters the pool, reserved for
    /// [`Store::change_token`].
    ///
    /// `PRAGMA data_version` is **per connection**: it counts commits made by
    /// *other* connections since this one last looked. Asking a pooled
    /// connection therefore answers a different question every time — the pool
    /// hands out whichever connection is free, and two of them have unrelated
    /// counters — so the token has to come from one connection that lives as
    /// long as the store.
    change_conn: Mutex<Connection>,
}

impl SqliteStore {
    /// Opens (creating if necessary) the database at `db_path` with default
    /// configuration.
    ///
    /// Opening does not migrate. [`Store::migrate`] is a separate, explicit
    /// step so that a caller decides when a schema change — and the backup that
    /// precedes it — happens.
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_with(StoreConfig::new(db_path.as_ref()))
    }

    /// Opens the database described by `config`.
    pub fn open_with(config: StoreConfig) -> Result<Self, StoreError> {
        if let Some(parent) = config.db_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        // Open one connection eagerly: it applies the persistent pragmas
        // (`journal_mode`) and runs the forward-compatibility gate, so a
        // database written by a newer storyhook is refused here, once, with a
        // sentence — rather than later, repeatedly, as a serde error. It then
        // becomes the change-token connection rather than joining the pool.
        let mut store = Self {
            config,
            pool: Mutex::new(Vec::new()),
            write_lock: Mutex::new(()),
            change_conn: Mutex::new(Connection::open_in_memory()?),
        };
        let conn = store.new_connection()?;
        migrate::ensure_readable(&conn, current_schema_version())?;
        store.change_conn = Mutex::new(conn);
        Ok(store)
    }

    /// The database file this store is backed by.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.config.db_path
    }

    /// Where this store writes pre-migration backups.
    #[must_use]
    pub fn backup_dir(&self) -> &Path {
        &self.config.backup_dir
    }

    /// Applies a specific migration list.
    ///
    /// [`Store::migrate`] applies [`MIGRATIONS`]; this exists so that a test —
    /// or the author of a future migration — can exercise the framework
    /// against a list of their own. It is the only way to test "migration 2
    /// fails and the database is left at version 1" while there is only one
    /// migration in the tree.
    pub fn migrate_with(&self, migrations: &[Migration]) -> Result<MigrationReport, StoreError> {
        let _guard = self.write_guard();
        let conn = self.checkout()?;
        migrate::run(&conn, migrations, &self.config.backup_dir)
    }

    /// The number of connections currently idle in the pool.
    ///
    /// Exposed so a test can assert the pool is actually being reused and that
    /// a failed or panicking transaction still returns its connection.
    #[must_use]
    pub fn pooled_connections(&self) -> usize {
        self.pool
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Takes the process-wide write lock.
    ///
    /// Poisoning is deliberately ignored. A panic inside a write transaction
    /// leaves the *database* untouched — the transaction rolls back when it
    /// drops — so the invariant a poisoned mutex exists to protect was never
    /// broken. Propagating the poison would instead turn one panicking request
    /// into a permanently unusable store.
    fn write_guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.write_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn new_connection(&self) -> Result<Connection, StoreError> {
        let conn = Connection::open(&self.config.db_path)
            .map_err(|e| StoreError::from_sqlite(e, "opening the store"))?;
        conn.busy_timeout(self.config.busy_timeout)
            .map_err(|e| StoreError::from_sqlite(e, "setting the busy timeout"))?;
        // Foreign keys are OFF by default in SQLite, which would silently
        // demote every referential invariant in the schema to a comment.
        conn.pragma_update(None, "foreign_keys", true)
            .map_err(|e| StoreError::from_sqlite(e, "enabling foreign keys"))?;
        // Explicit rather than relying on the default: the relation mirror
        // triggers are only safe from re-entering themselves while this is off.
        conn.pragma_update(None, "recursive_triggers", false)
            .map_err(|e| StoreError::from_sqlite(e, "disabling recursive triggers"))?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|e| StoreError::from_sqlite(e, "setting synchronous mode"))?;
        // Journal mode is persistent per *database*, so this is read before it
        // is written. `PRAGMA journal_mode = WAL` is not a no-op on a database
        // that is already in WAL: it takes an exclusive lock to make the
        // change it is not going to make, and under concurrency that lock is
        // contended — every process opening the store paid for it, and enough
        // of them at once turned `story init` into a `LockTimeout`. Reading
        // first costs one lock-free statement and leaves the write to the one
        // connection that actually needs it.
        let mode = ensure_wal(&conn, self.config.busy_timeout)?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StoreError::Corrupt(format!(
                "{} would not enter write-ahead logging mode (reported `{mode}`)",
                self.config.db_path.display()
            )));
        }
        Ok(conn)
    }

    fn checkout(&self) -> Result<PooledConn<'_>, StoreError> {
        let pooled = self
            .pool
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop();
        let conn = match pooled {
            Some(conn) => conn,
            None => self.new_connection()?,
        };
        Ok(PooledConn {
            store: self,
            conn: Some(conn),
        })
    }

    fn release(&self, conn: Connection) {
        let mut pool = self.pool.lock().unwrap_or_else(PoisonError::into_inner);
        if pool.len() < self.config.pool_size {
            pool.push(conn);
        }
    }
}

impl std::fmt::Debug for SqliteStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteStore")
            .field("db_path", &self.config.db_path)
            .field("pooled", &self.pooled_connections())
            .finish()
    }
}

/// A connection borrowed from the pool, returned when dropped.
struct PooledConn<'a> {
    store: &'a SqliteStore,
    conn: Option<Connection>,
}

impl Deref for PooledConn<'_> {
    type Target = Connection;

    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("connection outlives its guard")
    }
}

impl Drop for PooledConn<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.store.release(conn);
        }
    }
}

// ---------------------------------------------------------------------------
// Transactions
// ---------------------------------------------------------------------------

/// A read transaction: a consistent snapshot for as long as it lives.
///
/// Built on raw `BEGIN`/`COMMIT` over an owned connection rather than
/// rusqlite's `Transaction`, which borrows its connection and would make this
/// type self-referential.
pub struct SqliteReadTx<'a> {
    conn: PooledConn<'a>,
    open: bool,
}

impl<'a> SqliteReadTx<'a> {
    fn begin(conn: PooledConn<'a>) -> Result<Self, StoreError> {
        conn.execute_batch("BEGIN")
            .map_err(|e| StoreError::from_sqlite(e, "beginning a read"))?;
        Ok(Self { conn, open: true })
    }

    fn end(mut self) -> Result<(), StoreError> {
        // `open` is cleared only once the COMMIT has actually succeeded. A
        // failed COMMIT leaves SQLite's transaction open, so the drop guard
        // below still has work to do — see [`SqliteWriteTx::commit`].
        self.conn
            .execute_batch("COMMIT")
            .map_err(|e| StoreError::from_sqlite(e, "ending a read"))?;
        self.open = false;
        Ok(())
    }
}

impl Drop for SqliteReadTx<'_> {
    fn drop(&mut self) {
        if self.open {
            // Rollback on unwind: a panicking read must not leave its
            // connection mid-transaction for whoever takes it from the pool
            // next.
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

/// A write transaction. Rolls back if dropped without committing.
pub struct SqliteWriteTx<'a> {
    conn: PooledConn<'a>,
    open: bool,
}

impl<'a> SqliteWriteTx<'a> {
    fn begin(conn: PooledConn<'a>) -> Result<Self, StoreError> {
        // `BEGIN IMMEDIATE` takes the write lock now rather than on the first
        // write, so contention is reported at a point where nothing has been
        // done yet.
        //
        // `defer_foreign_keys` holds referential checks until COMMIT, which
        // makes the order of writes within a transaction irrelevant: a story
        // may name a relation to a story written later in the same
        // transaction, which is what a whole-project rebuild needs. It resets
        // itself at the end of the transaction.
        conn.execute_batch("BEGIN IMMEDIATE; PRAGMA defer_foreign_keys = ON;")
            .map_err(|e| StoreError::from_sqlite(e, "beginning a write"))?;
        Ok(Self { conn, open: true })
    }

    fn commit(mut self) -> Result<(), StoreError> {
        fire(FaultPoint::BeforeCommit)?;
        // `open` stays true until the COMMIT has succeeded, and that ordering
        // is load-bearing. `defer_foreign_keys` holds referential checks until
        // COMMIT, so a transaction can fail *there* rather than in the
        // closure — and SQLite leaves the transaction open when it does.
        // Clearing the flag first would skip the rollback below and hand a
        // connection back to the pool mid-transaction, where the next caller's
        // `BEGIN` fails with an error describing this caller's mistake.
        self.conn
            .execute_batch("COMMIT")
            .map_err(|e| StoreError::from_sqlite(e, "committing"))?;
        self.open = false;
        Ok(())
    }
}

impl Drop for SqliteWriteTx<'_> {
    fn drop(&mut self) {
        if self.open {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

impl Store for SqliteStore {
    type ReadTx<'a>
        = SqliteReadTx<'a>
    where
        Self: 'a;
    type WriteTx<'a>
        = SqliteWriteTx<'a>
    where
        Self: 'a;

    fn read<T>(
        &self,
        f: impl FnOnce(&Self::ReadTx<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let tx = SqliteReadTx::begin(self.checkout()?)?;
        let value = f(&tx)?;
        tx.end()?;
        Ok(value)
    }

    fn write<T>(
        &self,
        f: impl FnOnce(&mut Self::WriteTx<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let _guard = self.write_guard();
        let mut tx = SqliteWriteTx::begin(self.checkout()?)?;
        // On the error path `tx` drops here and rolls back — the whole reason
        // the transaction owns its own teardown rather than relying on a call
        // to a `rollback` the caller might forget.
        let value = f(&mut tx)?;
        tx.commit()?;
        // Durable but unacknowledged: the point a crash here must not turn
        // into a false failure that a client retries into a duplicate write.
        fire(FaultPoint::AfterCommitBeforeAck)?;
        Ok(value)
    }

    fn migrate(&self) -> Result<MigrationReport, StoreError> {
        self.migrate_with(MIGRATIONS)
    }

    fn snapshot(&self, dir: &Path) -> Result<PathBuf, StoreError> {
        let conn = self.checkout()?;
        migrate::snapshot(&conn, dir, "snapshot")
    }

    /// SQLite's `PRAGMA data_version`.
    ///
    /// It is incremented when another *connection* commits, and — the detail
    /// that makes it usable here — is deliberately **not** incremented by
    /// commits on the connection that reads it. A pooled connection is
    /// therefore the wrong place to ask from if you want to see your own
    /// writes, and exactly the right place if you want to see everybody
    /// else's, which is what the change feed's safety net is for.
    fn change_token(&self) -> Result<u64, StoreError> {
        let conn = self
            .change_conn
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let version: i64 = conn.query_row("PRAGMA data_version", [], |row| row.get(0))?;
        Ok(version as u64)
    }
}

// ---------------------------------------------------------------------------
// ReadOps / WriteOps
// ---------------------------------------------------------------------------

/// Implements [`ReadOps`] by delegating to [`read`]'s free functions.
///
/// Both transaction types get the identical implementation, which is the point:
/// a read performed inside a write transaction must be the same read.
macro_rules! impl_read_ops {
    ($ty:ident) => {
        impl ReadOps for $ty<'_> {
            fn project(&self, project: ProjectId) -> Result<Option<ProjectRecord>, StoreError> {
                read::project(&self.conn, project)
            }

            fn project_by_uuid(&self, uuid: &str) -> Result<Option<ProjectRecord>, StoreError> {
                read::project_by_uuid(&self.conn, uuid)
            }

            fn project_by_slug(&self, slug: &str) -> Result<Option<ProjectRecord>, StoreError> {
                read::project_by_slug(&self.conn, slug)
            }

            fn project_by_path(&self, path: &Path) -> Result<Option<ProjectRecord>, StoreError> {
                read::project_by_path(&self.conn, path)
            }

            fn projects(&self) -> Result<Vec<ProjectRecord>, StoreError> {
                read::projects(&self.conn)
            }

            fn project_paths(
                &self,
                project: ProjectId,
            ) -> Result<Vec<ProjectPathRecord>, StoreError> {
                read::project_paths(&self.conn, project)
            }

            fn states(&self, project: ProjectId) -> Result<Vec<StateDef>, StoreError> {
                read::states(&self.conn, project)
            }

            fn state_map(
                &self,
                project: ProjectId,
            ) -> Result<BTreeMap<String, StateDef>, StoreError> {
                read::state_map(&self.conn, project)
            }

            fn types(&self, project: ProjectId) -> Result<Vec<TypeDef>, StoreError> {
                read::types(&self.conn, project)
            }

            fn members(&self, project: ProjectId) -> Result<Vec<Member>, StoreError> {
                read::members(&self.conn, project)
            }

            fn settings(&self, project: ProjectId) -> Result<ProjectSettings, StoreError> {
                read::settings(&self.conn, project)
            }

            fn events_for(
                &self,
                project: ProjectId,
                story: StoryNo,
            ) -> Result<Vec<StoredEvent>, StoreError> {
                read::events_for(&self.conn, project, story)
            }

            fn head_seq(&self, project: ProjectId, story: StoryNo) -> Result<EventSeq, StoreError> {
                read::head_seq(&self.conn, project, story)
            }

            fn events_since(
                &self,
                project: ProjectId,
                after: GlobalSeq,
                limit: u32,
            ) -> Result<Vec<FeedEvent>, StoreError> {
                read::events_since(&self.conn, project, after, limit)
            }

            fn max_global_seq(&self, project: ProjectId) -> Result<GlobalSeq, StoreError> {
                read::max_global_seq(&self.conn, project)
            }

            fn commit_linked(
                &self,
                project: ProjectId,
                story: StoryNo,
                sha: &str,
            ) -> Result<bool, StoreError> {
                read::commit_linked(&self.conn, project, story, sha)
            }

            fn story(
                &self,
                project: ProjectId,
                story: StoryNo,
            ) -> Result<Option<StoryRow>, StoreError> {
                read::story(&self.conn, project, story)
            }

            fn stories(
                &self,
                project: ProjectId,
                query: &StoryQuery,
            ) -> Result<Vec<StoryRow>, StoreError> {
                read::stories(&self.conn, project, query)
            }

            fn relations_from(
                &self,
                project: ProjectId,
                story: StoryNo,
            ) -> Result<Vec<RelationEdge>, StoreError> {
                read::relations_from(&self.conn, project, story)
            }

            fn relations_to(
                &self,
                project: ProjectId,
                story: StoryNo,
            ) -> Result<Vec<RelationEdge>, StoreError> {
                read::relations_to(&self.conn, project, story)
            }

            fn github_base(
                &self,
                project: ProjectId,
                story: StoryNo,
            ) -> Result<Option<StorySnapshot>, StoreError> {
                read::github_base(&self.conn, project, story)
            }
        }
    };
}

impl_read_ops!(SqliteReadTx);
impl_read_ops!(SqliteWriteTx);

impl WriteOps for SqliteWriteTx<'_> {
    fn create_project(&mut self, project: &NewProject) -> Result<ProjectId, StoreError> {
        write::create_project(&self.conn, project)
    }

    fn touch_project_path(
        &mut self,
        project: ProjectId,
        path: &Path,
        kind: PathKind,
    ) -> Result<(), StoreError> {
        write::touch_project_path(&self.conn, project, path, kind)
    }

    fn allocate_story_no(&mut self, project: ProjectId) -> Result<StoryNo, StoreError> {
        write::allocate_story_no(&self.conn, project)
    }

    fn forget_project_path(&mut self, project: ProjectId, path: &Path) -> Result<bool, StoreError> {
        write::forget_project_path(&self.conn, project, path)
    }

    fn rename_project(&mut self, project: ProjectId, name: &str) -> Result<(), StoreError> {
        write::rename_project(&self.conn, project, name)
    }

    fn reserve_story_no(&mut self, project: ProjectId, highest: StoryNo) -> Result<(), StoreError> {
        write::reserve_story_no(&self.conn, project, highest)
    }

    fn append_events(
        &mut self,
        project: ProjectId,
        story: StoryNo,
        expected: ExpectedSeq,
        events: &[StoryEvent],
    ) -> Result<EventSeq, StoreError> {
        write::append_events(&self.conn, project, story, expected, events)
    }

    fn append_raw_events(
        &mut self,
        project: ProjectId,
        story: StoryNo,
        expected: ExpectedSeq,
        events: &[RawEvent],
    ) -> Result<EventSeq, StoreError> {
        write::append_raw_events(&self.conn, project, story, expected, events)
    }

    fn put_story(
        &mut self,
        project: ProjectId,
        snapshot: &StorySnapshot,
        head: EventSeq,
    ) -> Result<(), StoreError> {
        write::put_story(&self.conn, project, snapshot, head)
    }

    fn put_states(&mut self, project: ProjectId, states: &[StateDef]) -> Result<(), StoreError> {
        write::put_states(&self.conn, project, states)
    }

    fn put_types(&mut self, project: ProjectId, types: &[TypeDef]) -> Result<(), StoreError> {
        write::put_types(&self.conn, project, types)
    }

    fn put_member(&mut self, project: ProjectId, member: &Member) -> Result<(), StoreError> {
        write::put_member(&self.conn, project, member)
    }

    fn remove_member(&mut self, project: ProjectId, member_id: &str) -> Result<bool, StoreError> {
        write::remove_member(&self.conn, project, member_id)
    }

    fn put_settings(
        &mut self,
        project: ProjectId,
        settings: &ProjectSettings,
    ) -> Result<(), StoreError> {
        write::put_settings(&self.conn, project, settings)
    }

    fn put_github_base(
        &mut self,
        project: ProjectId,
        story: StoryNo,
        snapshot: &StorySnapshot,
    ) -> Result<(), StoreError> {
        write::put_github_base(&self.conn, project, story, snapshot)
    }
}
