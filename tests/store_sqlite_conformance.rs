//! The store conformance suite, instantiated for the SQLite engine.
//!
//! Everything in here comes from `storyhook::store_conformance_suite!`. A
//! second implementation — the design admits Postgres later — gets the entire
//! suite by adding one fixture and one macro call to a file like this one.

use storyhook::store::{ConformanceFixture, SqliteStore, Store};
use storyhook_test_support::scratch_dir;
use tempfile::TempDir;

/// A migrated SQLite store in its own scratch directory.
///
/// File-backed, never `:memory:`. The suite asserts durability across a
/// reopen, and an in-memory database has nothing to reopen; it also has no
/// write-ahead log, which is where the concurrency guarantees live.
struct SqliteFixture {
    dir: TempDir,
    store: SqliteStore,
}

impl ConformanceFixture for SqliteFixture {
    type Store = SqliteStore;

    fn create() -> Self {
        let dir = scratch_dir();
        let store = SqliteStore::open(dir.path().join("store.db")).expect("opening the store");
        store.migrate().expect("migrating the store");
        Self { dir, store }
    }

    fn store(&self) -> &Self::Store {
        &self.store
    }

    fn snapshot_dir(&self) -> std::path::PathBuf {
        self.dir.path().join("backups")
    }

    fn open_snapshot(&self, path: &std::path::Path) -> Self::Store {
        SqliteStore::open(path).expect("opening a copy")
    }

    fn reopen(self) -> Self {
        // The old store drops first, releasing its pooled connections, so the
        // reopened one genuinely re-reads the file rather than inheriting a
        // handle that never closed.
        let Self { dir, store } = self;
        drop(store);
        let store = SqliteStore::open(dir.path().join("store.db")).expect("reopening the store");
        Self { dir, store }
    }
}

storyhook::store_conformance_suite!(SqliteFixture);
