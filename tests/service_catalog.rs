//! `CatalogService` — the dashboard's project list, over the store.
//!
//! The headline property is the one the legacy registry could not have: because
//! the catalog *is* the projects table, deregistering a checkout cannot lose
//! anything. The registry got that for free by holding no data; here it has to
//! be a rule, and these tests are the rule.

use std::path::Path;

use storyhook::error::AppError;
use storyhook::service::{
    CatalogService, InitOptions, NewStoryInput, ProjectService, StoryService,
};
use storyhook::store::{ReadOps, SqliteStore, Store, StoryQuery};
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
fn registering_a_project_twice_is_not_an_error() {
    // The legacy registry rejected a second registration. In the store the path
    // is recorded by `story init` itself, so the same rule would make the
    // command permanently unusable.
    let (store, dir) = store();
    let root = project(&store, dir.path(), "alpha");
    let service = CatalogService::new(&store);

    let first = service.register(&root, None).expect("registering");
    let second = service.register(&root, None).expect("registering again");
    assert_eq!(first.id, second.id);
    assert_eq!(
        CatalogService::new(&store).list().expect("listing").len(),
        1
    );
}

#[test]
fn registering_a_directory_that_is_not_a_project_says_so() {
    let (store, dir) = store();
    let stranger = dir.path().join("not-a-project");
    std::fs::create_dir_all(&stranger).expect("creating");

    let error = CatalogService::new(&store)
        .register(&stranger, None)
        .expect_err("an unknown directory");
    assert!(matches!(error, AppError::NotFound(_)), "{error}");
    assert!(error.to_string().contains("run `story init`"), "{error}");
}

#[test]
fn registering_a_directory_that_does_not_exist_says_which_one() {
    let (store, dir) = store();
    let error = CatalogService::new(&store)
        .register(&dir.path().join("ghost"), None)
        .expect_err("a missing directory");
    assert!(matches!(error, AppError::Usage(_)), "{error}");
    assert!(error.to_string().contains("cannot access"), "{error}");
}

#[test]
fn a_supplied_name_overrides_the_projects_own() {
    let (store, dir) = store();
    let root = project(&store, dir.path(), "alpha");
    let entry = CatalogService::new(&store)
        .register(&root, Some("The Alpha Project"))
        .expect("registering");
    assert_eq!(entry.name, "The Alpha Project");
}

#[test]
fn deregistering_by_path_forgets_the_checkout_and_keeps_the_stories() {
    let (store, dir) = store();
    let root = project(&store, dir.path(), "alpha");
    let id = store
        .read(|tx| tx.project_by_path(&root))
        .expect("reading")
        .expect("the project")
        .id;
    let ctx = storyhook::service::Ctx::new(&store, id, &root);
    StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Worth keeping".to_string(),
            ..NewStoryInput::default()
        })
        .expect("creating a story");

    let entry = CatalogService::new(&store)
        .deregister(root.to_str().unwrap())
        .expect("deregistering");
    assert_eq!(entry.id, "alpha");

    assert!(
        CatalogService::new(&store)
            .list()
            .expect("listing")
            .is_empty(),
        "the checkout is gone from the catalog"
    );
    assert_eq!(
        store
            .read(|tx| tx.stories(id, &StoryQuery::all()))
            .expect("reading stories")
            .len(),
        1,
        "the stories must survive"
    );
    assert!(
        store.read(|tx| tx.project(id)).expect("reading").is_some(),
        "the project must survive"
    );
}

#[test]
fn deregistering_by_slug_forgets_every_checkout() {
    let (store, dir) = store();
    let root = project(&store, dir.path(), "alpha");
    let second = dir.path().join("alpha-worktree");
    std::fs::create_dir_all(&second).expect("creating");
    let id = store
        .read(|tx| tx.project_by_path(&root))
        .expect("reading")
        .expect("the project")
        .id;
    store
        .write(|tx| {
            storyhook::store::WriteOps::touch_project_path(
                tx,
                id,
                &second.canonicalize().unwrap(),
                storyhook::store::PathKind::Worktree,
            )
        })
        .expect("recording a second checkout");

    CatalogService::new(&store)
        .deregister("alpha")
        .expect("deregistering by slug");
    assert!(
        store
            .read(|tx| tx.project_paths(id))
            .expect("reading paths")
            .is_empty()
    );
}

#[test]
fn a_deregistered_checkout_can_be_registered_again() {
    let (store, dir) = store();
    let root = project(&store, dir.path(), "alpha");
    let service = CatalogService::new(&store);
    service
        .deregister(root.to_str().unwrap())
        .expect("deregistering");

    // Without a pointer file the path is the only handle, and forgetting it
    // forgets the way back. This is exactly what the pointer file exists to fix,
    // and the wave that turns it on makes this case work.
    let error = service.register(&root, None).expect_err("no way back yet");
    assert!(matches!(error, AppError::NotFound(_)), "{error}");
}

#[test]
fn deregistering_something_unknown_says_so() {
    let (store, _dir) = store();
    let error = CatalogService::new(&store)
        .deregister("nothing-like-this")
        .expect_err("an unknown target");
    assert!(matches!(error, AppError::NotFound(_)), "{error}");
    assert!(
        error.to_string().contains("no registered repo matches"),
        "{error}"
    );
}

#[test]
fn a_checkout_whose_directory_vanished_is_still_deregisterable_by_its_stored_path() {
    let (store, dir) = store();
    let root = project(&store, dir.path(), "alpha");
    std::fs::remove_dir_all(&root).expect("removing the checkout");

    let entry = CatalogService::new(&store)
        .deregister(root.to_str().unwrap())
        .expect("deregistering a vanished checkout");
    assert_eq!(entry.id, "alpha");
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
