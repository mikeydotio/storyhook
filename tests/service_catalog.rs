//! `CatalogService` — the dashboard's project list, over the store.
//!
//! The headline property is the one the legacy registry could not have: because
//! the catalog *is* the projects table, deregistering a checkout cannot lose
//! anything. The registry got that for free by holding no data; here it has to
//! be a rule, and these tests are the rule.

use std::path::Path;

use storyhook::service::{CatalogService, InitOptions, ProjectService};
use storyhook::store::{SqliteStore, Store};
use storyhook_test_support::scratch_dir;

/// An empty store, and a scratch directory to anchor projects in.
fn store() -> (SqliteStore, tempfile::TempDir) {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).expect("opening the store");
    store.migrate().expect("migrating");
    (store, dir)
}

/// A checkout directory with a project initialized in it.
fn project(store: &SqliteStore, parent: &Path, name: &str) -> std::path::PathBuf {
    let root = parent.join(name);
    std::fs::create_dir_all(&root).expect("creating the checkout");
    ProjectService::new(store, &root)
        .init(&InitOptions {
            agents_md: false,
            ..InitOptions::default()
        })
        .expect("initializing");
    root.canonicalize().expect("canonicalizing")
}

#[test]
fn an_empty_store_lists_nothing() {
    let (store, _dir) = store();
    assert!(
        CatalogService::new(&store)
            .list()
            .expect("listing")
            .is_empty()
    );
}

#[test]
fn init_puts_a_project_in_the_catalog_without_a_separate_register() {
    let (store, dir) = store();
    let root = project(&store, dir.path(), "alpha");

    let entries = CatalogService::new(&store).list().expect("listing");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "alpha");
    assert_eq!(entries[0].path.as_deref(), Some(root.as_path()));
}

#[test]
fn the_catalog_lists_projects_in_slug_order() {
    let (store, dir) = store();
    project(&store, dir.path(), "zulu");
    project(&store, dir.path(), "alpha");
    project(&store, dir.path(), "mike");

    let ids: Vec<String> = CatalogService::new(&store)
        .list()
        .expect("listing")
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    assert_eq!(ids, ["alpha", "mike", "zulu"]);
}
