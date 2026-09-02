//! An incompatible store degrades to read-only rather than taking the tool down
//! (SH-530).
//!
//! Before this, a database written by a newer storyhook was refused outright
//! with [`StoreError::SchemaTooNew`] and exit 5 — every `story` command,
//! including the ones that only wanted to read. On the machine that filed
//! SH-530 that state was already live: the store sat at schema 21 while the
//! newest published release understood 18, so the tracker was openable by
//! exactly one binary in existence, an unreleased local build. A tracker you
//! can read is worth a great deal more than a refusal.
//!
//! The asymmetry the degrade rests on: a newer schema's *additions* do not stop
//! this build reading the columns it already knows, but a newer schema's
//! *invariants* are ones this build has never heard of and cannot maintain. So
//! reads are served and writes refuse.
//!
//! **The load-bearing assertion in this file is the byte-for-byte one**, and
//! that is measured rather than asserted. Three mutations were run against the
//! implementation, restoring from a `cp` snapshot each time rather than with
//! `git checkout --`, which would revert the fix under test and let the suite
//! pass vacuously:
//!
//! - *The write gate never refuses.* 2 of 6 red — but caught by SQLite's own
//!   `SQLITE_OPEN_READ_ONLY`, not by anything this file asserts.
//! - *The gate never refuses AND degraded connections are read-write.* 2 of 6
//!   red, caught at `expect_err` because the write simply succeeds.
//! - *The gate refuses correctly, but degraded connections are read-write.*
//!   2 of 6 red, and **only** the byte-for-byte assertion catches it: every
//!   error type and every message assertion in this file passes, because the
//!   write really is refused — and the file moves anyway, because the
//!   connection checkpoints a write-ahead log on its way out. That is the whole
//!   case for hashing the sidecars, and it is not reachable by any assertion
//!   about the refusal itself.

use std::path::Path;

use rusqlite::Connection;
use storyhook::store::{
    Access, NewProject, ReadOps, SqliteStore, Store, StoreError, WriteOps, current_schema_version,
};
use storyhook_test_support::scratch_dir;

/// A version no build in this tree understands, expressed as a distance from
/// the tree's own so it cannot silently become reachable when a migration
/// lands — the bare `99` this file used to borrow from its neighbours would.
fn a_version_from_the_future() -> u32 {
    current_schema_version() + 1
}

/// Every byte of the database and both its sidecars.
///
/// The `-wal` and `-shm` are included on purpose: a connection that checkpoints
/// or recovers a log has written to the store just as surely as one that ran an
/// `INSERT`, and a hash over `store.db` alone would call that clean.
fn fingerprint(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for suffix in ["", "-wal", "-shm"] {
        let candidate = path.with_file_name(format!(
            "{}{suffix}",
            path.file_name().unwrap().to_string_lossy()
        ));
        let bytes = std::fs::read(&candidate).unwrap_or_default();
        out.push((suffix.to_string(), bytes));
    }
    out
}

/// A distinct project, named by its prefix so a test reads as one sentence.
fn new_project(prefix: &str) -> NewProject {
    NewProject {
        uuid: format!("uuid-{prefix}"),
        slug: prefix.to_lowercase(),
        name: format!("{prefix} project"),
        prefix: prefix.to_string(),
        created_at: "2026-09-01T00:00:00Z".to_string(),
    }
}

/// A healthy store, then its recorded version bumped behind its back — exactly
/// what a newer binary sharing the file does. Written through rusqlite rather
/// than fabricated, so the pragma under test is the one a real writer sets.
fn store_from_the_future(path: &Path) {
    SqliteStore::open(path).unwrap().migrate().unwrap();
    Connection::open(path)
        .unwrap()
        .execute_batch(&format!(
            "PRAGMA user_version = {}",
            a_version_from_the_future()
        ))
        .unwrap();
}

#[test]
fn a_store_from_a_newer_storyhook_opens_read_only_instead_of_being_refused() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    store_from_the_future(&path);

    let store = SqliteStore::open(&path).expect("a readable store must still open");

    assert_eq!(
        store.access(),
        Access::ReadOnly {
            found: a_version_from_the_future(),
            supported: current_schema_version(),
        },
        "the store must name both versions it is degraded between"
    );
}

#[test]
fn reads_are_still_served_from_a_degraded_store() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    {
        let store = SqliteStore::open(&path).unwrap();
        store.migrate().unwrap();
        store
            .write(|tx| tx.create_project(&new_project("RO")))
            .expect("seeding a project");
    }
    store_from_the_future(&path);

    let store = SqliteStore::open(&path).expect("opening degraded");
    let projects = store
        .read(|tx| tx.projects())
        .expect("a degraded store must still answer reads");

    assert!(
        projects.iter().any(|p| p.prefix == "RO"),
        "the project seeded before the store went out of range must still be readable: {projects:#?}"
    );
}

#[test]
fn every_write_is_refused_and_the_database_is_not_touched() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    {
        let store = SqliteStore::open(&path).unwrap();
        store.migrate().unwrap();
        store
            .write(|tx| tx.create_project(&new_project("RO")))
            .expect("seeding a project");
    }
    store_from_the_future(&path);

    let store = SqliteStore::open(&path).expect("opening degraded");
    let before = fingerprint(&path);

    let error = store
        .write(|tx| tx.create_project(&new_project("NO")))
        .expect_err("a write against a degraded store must be refused");

    match error {
        StoreError::SchemaReadOnly { found, supported } => {
            assert_eq!(found, a_version_from_the_future());
            assert_eq!(supported, current_schema_version());
        }
        other => panic!("expected SchemaReadOnly, got: {other}"),
    }

    let message = store
        .write(|tx| tx.create_project(&new_project("NO")))
        .unwrap_err()
        .to_string();
    assert!(
        message.contains("READ-ONLY"),
        "the refusal must say what mode it is in: {message}"
    );
    assert!(
        message.contains("`story update`"),
        "the refusal must name a way out, never a dead end: {message}"
    );

    drop(store);
    assert_eq!(
        before,
        fingerprint(&path),
        "a refused write must leave the database and both sidecars byte-identical"
    );
}

#[test]
fn migrating_a_degraded_store_is_refused_by_the_same_gate() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    store_from_the_future(&path);

    let store = SqliteStore::open(&path).expect("opening degraded");
    let before = fingerprint(&path);

    let error = store
        .migrate()
        .expect_err("a degraded store must not be carried forward by this build");

    assert!(
        matches!(error, StoreError::SchemaReadOnly { .. }),
        "expected SchemaReadOnly, got: {error}"
    );
    drop(store);
    assert_eq!(
        before,
        fingerprint(&path),
        "a refused migration must leave the database byte-identical"
    );
}

#[test]
fn a_store_whose_read_model_this_build_cannot_see_is_still_refused_outright() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    store_from_the_future(&path);
    // The case degradation must NOT cover: a newer storyhook that restructured
    // what this build reads, rather than only adding to it. `migrate.rs`'s own
    // comment anticipates exactly this, and reading such a store would be
    // guessing at columns that are gone.
    Connection::open(&path)
        .unwrap()
        .execute_batch("ALTER TABLE stories RENAME COLUMN title TO headline")
        .unwrap();

    let error = SqliteStore::open(&path).expect_err("an unreadable store must still be refused");

    match error {
        StoreError::SchemaTooNew { found, supported } => {
            assert_eq!(found, a_version_from_the_future());
            assert_eq!(supported, current_schema_version());
        }
        other => panic!("expected SchemaTooNew, got: {other}"),
    }
}

#[test]
fn a_store_this_build_understands_is_untouched_by_any_of_this() {
    let dir = scratch_dir();
    let path = dir.path().join("store.db");
    let store = SqliteStore::open(&path).unwrap();
    store.migrate().unwrap();

    assert_eq!(
        store.access(),
        Access::ReadWrite,
        "the ordinary case must not be degraded"
    );
    store
        .write(|tx| tx.create_project(&new_project("OK")))
        .expect("an ordinary store must still accept writes");
}
