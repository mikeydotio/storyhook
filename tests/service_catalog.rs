//! `CatalogService` — the dashboard's project list, over the store.
//!
//! The headline property is the one the legacy registry could not have: because
//! the catalog *is* the projects table, deregistering a checkout cannot lose
//! anything. The registry got that for free by holding no data; here it has to
//! be a rule, and these tests are the rule.

use std::path::Path;

use storyhook::service::{CatalogService, InitOptions, ProjectService};
use storyhook::store::{ReadOps, SqliteStore, Store, WriteOps};
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

// ---------------------------------------------------------------------------
// Adopting the legacy dashboard registry
// ---------------------------------------------------------------------------

/// Writes a `registry.toml` naming `paths`, in the shape `story web register`
/// wrote.
fn registry(at: &Path, paths: &[&Path]) -> std::path::PathBuf {
    let mut text = String::from("schema = 1\n");
    for (n, path) in paths.iter().enumerate() {
        text.push_str(&format!(
            "\n[[repo]]\nid = \"repo-{n}\"\nname = \"repo {n}\"\npath = \"{}\"\n",
            path.display()
        ));
    }
    let file = at.join("registry.toml");
    std::fs::write(&file, text).expect("writing the registry");
    file
}

#[test]
fn adopting_a_registry_records_the_checkouts_of_projects_the_store_knows() {
    let (store, dir) = store();
    let root = dir.path().join("alpha");
    std::fs::create_dir_all(&root).expect("creating the checkout");
    ProjectService::new(&store, &root)
        .init(&InitOptions {
            agents_md: false,
            pointer: true,
            ..InitOptions::default()
        })
        .expect("initializing");
    let root = root.canonicalize().expect("canonicalizing");

    // Forget the path the way `story web deregister` would, so adoption has
    // something to put back rather than a row that was already there. The
    // pointer file is what makes the checkout findable again — the registry
    // knows a path, and the path alone is no longer an identity.
    let id = store
        .read(|tx| tx.project_by_path(&root))
        .expect("resolving")
        .expect("the project exists")
        .id;
    store
        .write(|tx| tx.forget_project_path(id, &root))
        .expect("deregistering");
    assert!(
        CatalogService::new(&store)
            .list()
            .expect("listing")
            .is_empty(),
        "a project with no known checkout is not a catalog row"
    );

    let file = registry(dir.path(), &[&root]);
    let adoption = storyhook::service::adopt_legacy_registry(&store, &file).expect("adopting");

    assert_eq!(adoption.adopted, vec![root.clone()]);
    assert!(adoption.unmigrated.is_empty());
    let listed: Vec<String> = CatalogService::new(&store)
        .list()
        .expect("listing")
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    assert_eq!(listed, ["alpha"]);
}

#[test]
fn a_registered_repo_the_store_has_never_heard_of_is_reported_not_created() {
    let (store, dir) = store();
    let stranger = dir.path().join("unmigrated");
    std::fs::create_dir_all(&stranger).expect("creating the checkout");
    let stranger = stranger.canonicalize().expect("canonicalizing");

    let file = registry(dir.path(), &[&stranger]);
    let adoption = storyhook::service::adopt_legacy_registry(&store, &file).expect("adopting");

    assert_eq!(adoption.unmigrated, vec![stranger]);
    assert!(
        adoption.adopted.is_empty(),
        "a repository still tracked by .storyhook/ must wait for `story migrate`; \
         minting a project row for it would produce an empty project that looks \
         like a lost one"
    );
    assert!(
        store.read(|tx| tx.projects()).expect("listing").is_empty(),
        "adoption must never create a project"
    );
}

#[test]
fn adoption_is_idempotent_and_never_writes_the_registry() {
    let (store, dir) = store();
    let root = project(&store, dir.path(), "alpha");
    let file = registry(dir.path(), &[&root]);
    let before = std::fs::read_to_string(&file).expect("reading the registry");

    let first = storyhook::service::adopt_legacy_registry(&store, &file).expect("adopting once");
    let second = storyhook::service::adopt_legacy_registry(&store, &file).expect("adopting twice");

    assert_eq!(
        first, second,
        "adoption must be safe to run on every command"
    );
    assert_eq!(
        std::fs::read_to_string(&file).expect("re-reading the registry"),
        before,
        "the legacy dashboard reads this file until the daemon wave; adoption must \
         leave it byte-identical"
    );
}

#[test]
fn a_missing_or_unparseable_registry_is_not_an_error() {
    let (store, dir) = store();
    assert_eq!(
        storyhook::service::adopt_legacy_registry(&store, &dir.path().join("nothing.toml"))
            .expect("a missing registry is not an error"),
        storyhook::service::RegistryAdoption::default()
    );

    let broken = dir.path().join("broken.toml");
    std::fs::write(&broken, "this is not toml [").expect("writing a broken registry");
    assert_eq!(
        storyhook::service::adopt_legacy_registry(&store, &broken).expect("a broken registry"),
        storyhook::service::RegistryAdoption::default(),
        "a file the dashboard owns must not be able to fail every storyhook command"
    );
}
