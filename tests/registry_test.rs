//! Tests for `storyhook::registry` — the `~/.storyhook/registry.toml`
//! registry of repos the multi-repo web dashboard (#20) is built on.
//!
//! These exercise the `Registry` API directly against explicit temp paths
//! (`Registry::load_from` / `registry::with_lock_at`) rather than the real
//! `$HOME`, so the suite never touches the developer's actual registry and
//! can run fully in parallel.

use std::path::Path;

use storyhook::error::AppError;
use storyhook::registry::{self, RegisteredRepo, Registry};
use storyhook_test_support::scratch_dir;

/// Initializes a real storyhook project at `dir` so it passes the
/// `storage::ensure_project` check `Registry::register` performs.
fn init_project(dir: &Path) {
    storyhook::storage::init_project(dir, None).unwrap();
}

// --- load / save ---

#[test]
fn load_from_missing_path_returns_empty_registry() {
    let dir = scratch_dir();
    let path = dir.path().join("registry.toml");

    let registry = Registry::load_from(&path).unwrap();
    assert_eq!(registry.schema, 1);
    assert!(registry.repos.is_empty());
}

#[test]
fn save_then_load_roundtrips() {
    let dir = scratch_dir();
    let registry_path = dir.path().join("registry.toml");
    let project_dir = dir.path().join("proj");
    std::fs::create_dir_all(&project_dir).unwrap();
    init_project(&project_dir);

    let mut registry = Registry::load_from(&registry_path).unwrap();
    let repo = registry.register(&project_dir, None).unwrap();
    registry.save().unwrap();

    let reloaded = Registry::load_from(&registry_path).unwrap();
    assert_eq!(reloaded.repos.len(), 1);
    assert_eq!(reloaded.repos[0].id, repo.id);
    assert_eq!(reloaded.repos[0].path, repo.path);
    assert!(registry_path.exists());
}

// --- register ---

#[test]
fn register_valid_project_succeeds() {
    let dir = scratch_dir();
    let project_dir = dir.path().join("my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    init_project(&project_dir);

    let mut registry = Registry::load_from(&dir.path().join("registry.toml")).unwrap();
    let repo = registry.register(&project_dir, None).unwrap();

    assert_eq!(repo.id, "my-app");
    assert_eq!(repo.name, "my-app");
    assert_eq!(repo.path, project_dir.canonicalize().unwrap());
    assert_eq!(registry.repos.len(), 1);
}

#[test]
fn register_accepts_explicit_name_override() {
    let dir = scratch_dir();
    let project_dir = dir.path().join("my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    init_project(&project_dir);

    let mut registry = Registry::load_from(&dir.path().join("registry.toml")).unwrap();
    let repo = registry
        .register(&project_dir, Some("Custom Display Name"))
        .unwrap();

    assert_eq!(repo.name, "Custom Display Name");
    // id is still derived from the path, not the display name.
    assert_eq!(repo.id, "my-app");
}

#[test]
fn register_non_project_directory_fails() {
    let dir = scratch_dir();
    let not_a_project = dir.path().join("empty-dir");
    std::fs::create_dir_all(&not_a_project).unwrap();

    let mut registry = Registry::load_from(&dir.path().join("registry.toml")).unwrap();
    let err = registry.register(&not_a_project, None).unwrap_err();

    assert!(matches!(err, AppError::NotFound(_)), "got: {err:?}");
    assert!(registry.repos.is_empty());
}

#[test]
fn register_nonexistent_path_fails() {
    let dir = scratch_dir();
    let missing = dir.path().join("does-not-exist");

    let mut registry = Registry::load_from(&dir.path().join("registry.toml")).unwrap();
    let err = registry.register(&missing, None).unwrap_err();

    assert!(matches!(err, AppError::Usage(_)), "got: {err:?}");
}

#[test]
fn register_duplicate_path_is_rejected() {
    let dir = scratch_dir();
    let project_dir = dir.path().join("my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    init_project(&project_dir);

    let mut registry = Registry::load_from(&dir.path().join("registry.toml")).unwrap();
    registry.register(&project_dir, None).unwrap();
    let err = registry.register(&project_dir, None).unwrap_err();

    assert!(matches!(err, AppError::Validation(_)), "got: {err:?}");
    assert_eq!(
        registry.repos.len(),
        1,
        "second call must not add a duplicate"
    );
}

#[test]
fn register_same_basename_different_paths_mints_unique_ids() {
    let dir = scratch_dir();
    let parent_a = dir.path().join("a");
    let parent_b = dir.path().join("b");
    let project_a = parent_a.join("app");
    let project_b = parent_b.join("app");
    std::fs::create_dir_all(&project_a).unwrap();
    std::fs::create_dir_all(&project_b).unwrap();
    init_project(&project_a);
    init_project(&project_b);

    let mut registry = Registry::load_from(&dir.path().join("registry.toml")).unwrap();
    let repo_a = registry.register(&project_a, None).unwrap();
    let repo_b = registry.register(&project_b, None).unwrap();

    assert_eq!(repo_a.id, "app");
    assert_eq!(repo_b.id, "app-2");
    assert_ne!(repo_a.id, repo_b.id);
}

#[test]
fn register_slugifies_non_alphanumeric_basenames() {
    let dir = scratch_dir();
    let project_dir = dir.path().join("My Cool App!!");
    std::fs::create_dir_all(&project_dir).unwrap();
    init_project(&project_dir);

    let mut registry = Registry::load_from(&dir.path().join("registry.toml")).unwrap();
    let repo = registry.register(&project_dir, None).unwrap();

    assert_eq!(repo.id, "my-cool-app");
    assert_eq!(repo.name, "My Cool App!!");
}

// --- deregister ---

#[test]
fn deregister_by_id_removes_entry() {
    let dir = scratch_dir();
    let project_dir = dir.path().join("my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    init_project(&project_dir);

    let mut registry = Registry::load_from(&dir.path().join("registry.toml")).unwrap();
    let repo = registry.register(&project_dir, None).unwrap();

    let removed = registry.deregister(&repo.id).unwrap();
    assert_eq!(removed.id, repo.id);
    assert!(registry.repos.is_empty());
}

#[test]
fn deregister_by_path_removes_entry() {
    let dir = scratch_dir();
    let project_dir = dir.path().join("my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    init_project(&project_dir);

    let mut registry = Registry::load_from(&dir.path().join("registry.toml")).unwrap();
    registry.register(&project_dir, None).unwrap();

    let canonical = project_dir.canonicalize().unwrap();
    let removed = registry.deregister(&canonical.to_string_lossy()).unwrap();
    assert_eq!(removed.path, canonical);
    assert!(registry.repos.is_empty());
}

#[test]
fn deregister_unknown_target_fails() {
    let dir = scratch_dir();
    let mut registry = Registry::load_from(&dir.path().join("registry.toml")).unwrap();

    let err = registry.deregister("does-not-exist").unwrap_err();
    assert!(matches!(err, AppError::NotFound(_)), "got: {err:?}");
}

#[test]
fn deregister_does_not_touch_the_registered_repos_files() {
    // Deregistering is a registry-only operation; it must never delete or
    // modify the target repo's own .storyhook/ directory.
    let dir = scratch_dir();
    let project_dir = dir.path().join("my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    init_project(&project_dir);

    let mut registry = Registry::load_from(&dir.path().join("registry.toml")).unwrap();
    let repo = registry.register(&project_dir, None).unwrap();
    registry.deregister(&repo.id).unwrap();

    assert!(project_dir.join(".storyhook/project.toml").exists());
}

// --- find ---

#[test]
fn find_returns_registered_repo_by_id() {
    let dir = scratch_dir();
    let project_dir = dir.path().join("my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    init_project(&project_dir);

    let mut registry = Registry::load_from(&dir.path().join("registry.toml")).unwrap();
    let repo = registry.register(&project_dir, None).unwrap();

    let found = registry.find(&repo.id).unwrap();
    assert_eq!(found.path, repo.path);
}

#[test]
fn find_returns_none_for_unknown_id() {
    let dir = scratch_dir();
    let registry = Registry::load_from(&dir.path().join("registry.toml")).unwrap();
    assert!(registry.find("nope").is_none());
}

// --- with_lock_at: locked read-modify-write, used by both the CLI and the
// --- web server's register/deregister routes ---

#[test]
fn with_lock_at_persists_successful_mutation() {
    let dir = scratch_dir();
    let registry_path = dir.path().join("registry.toml");
    let project_dir = dir.path().join("my-app");
    std::fs::create_dir_all(&project_dir).unwrap();
    init_project(&project_dir);

    let repo: RegisteredRepo =
        registry::with_lock_at(&registry_path, |r| r.register(&project_dir, None)).unwrap();

    let reloaded = Registry::load_from(&registry_path).unwrap();
    assert_eq!(reloaded.repos.len(), 1);
    assert_eq!(reloaded.repos[0].id, repo.id);
}

#[test]
fn with_lock_at_does_not_persist_a_failed_mutation() {
    let dir = scratch_dir();
    let registry_path = dir.path().join("registry.toml");
    let not_a_project = dir.path().join("empty-dir");
    std::fs::create_dir_all(&not_a_project).unwrap();

    let result: Result<RegisteredRepo, AppError> =
        registry::with_lock_at(&registry_path, |r| r.register(&not_a_project, None));
    assert!(result.is_err());

    // A failed mutation must not create (or corrupt) the registry file.
    let reloaded = Registry::load_from(&registry_path).unwrap();
    assert!(reloaded.repos.is_empty());
}

#[test]
fn with_lock_at_serializes_concurrent_registrations() {
    let dir = scratch_dir();
    let registry_path = dir.path().join("registry.toml");

    let project_dirs: Vec<_> = (0..8)
        .map(|i| {
            let p = dir.path().join(format!("proj-{i}"));
            std::fs::create_dir_all(&p).unwrap();
            init_project(&p);
            p
        })
        .collect();

    std::thread::scope(|scope| {
        for project_dir in &project_dirs {
            let registry_path = registry_path.clone();
            scope.spawn(move || {
                registry::with_lock_at(&registry_path, |r| r.register(project_dir, None)).unwrap();
            });
        }
    });

    let reloaded = Registry::load_from(&registry_path).unwrap();
    assert_eq!(
        reloaded.repos.len(),
        8,
        "every concurrent registration must survive — no lost updates"
    );
    let mut ids: Vec<_> = reloaded.repos.iter().map(|r| r.id.clone()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(
        ids.len(),
        8,
        "ids must stay unique under concurrent registration"
    );
}

// --- default_registry_path / registry_dir ---

#[test]
fn default_registry_path_is_under_home_dot_storyhook() {
    // We don't mutate the real $HOME here (that would race with other tests
    // and the developer's own registry); we only check the function reads
    // the current HOME and derives the expected shape.
    let home = std::env::var("HOME").expect("HOME must be set to run this test");
    let path = registry::default_registry_path().unwrap();
    assert_eq!(
        path,
        std::path::PathBuf::from(home)
            .join(".storyhook")
            .join("registry.toml")
    );
}
