//! Fixtures for the two legacy trees W3 is tested against.
//!
//! The **real** one is `docs/rearch/baseline/legacy-tree.tar.gz`: storyhook's
//! own tracker, frozen at 61 stories and 486 events, with a history no
//! synthetic fixture reaches by accident. The **synthetic** one is built here,
//! and exists because the real tree's states and types are the *defaults* and
//! its member list is empty — so it proves nothing about the configuration
//! surface. Neither fixture is sufficient alone; see
//! `docs/rearch/baseline/README.md`.

#![allow(dead_code)] // each test binary uses a different subset

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use storyhook::domain::{Member, StateDef, StoryEvent, StorySnapshot, SuperState, TypeDef};
use storyhook::legacy;
use storyhook::service::{MigrationPlan, MigrationReport};
use storyhook::storage;
use tempfile::TempDir;

use storyhook_test_support::scratch_dir_named;

/// The repository root, found from this test binary's own source path.
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `docs/rearch/baseline`.
pub fn baseline_dir() -> PathBuf {
    repo_root().join("docs/rearch/baseline")
}

/// The frozen `story export` of the real tree.
pub fn golden_export_path() -> PathBuf {
    baseline_dir().join("golden-export.json")
}

/// A live checkout holding the real frozen legacy tree.
///
/// Extracted fresh per call, so a test that mutates one cannot reach another's.
pub fn real_tree() -> (TempDir, PathBuf) {
    let dir = scratch_dir_named("legacy-real-");
    let root = dir.path().to_path_buf();
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(baseline_dir().join("legacy-tree.tar.gz"))
        .arg("-C")
        .arg(&root)
        .status()
        .expect("running tar");
    assert!(status.success(), "extracting the frozen legacy tree");
    assert!(
        root.join(".storyhook/project.toml").is_file(),
        "the frozen tarball must unpack to a `.storyhook` tree at its root"
    );
    (dir, root)
}

/// A legacy tree covering everything the real one does not: a custom state with
/// a role, a second CLOSED state, a custom type, and two members.
///
/// Built through `storage`'s own writers rather than by hand, so the bytes are
/// the ones the legacy tool would have written — a fixture assembled with
/// `fs::write` would be testing the fixture's idea of the format.
pub fn custom_config_tree() -> (TempDir, PathBuf) {
    let dir = scratch_dir_named("legacy-custom-");
    let root = dir.path().to_path_buf();
    storage::init_project(&root, Some("ADA")).expect("init");

    storage::save_states(
        &root,
        &[
            StateDef {
                slug: "todo".to_string(),
                super_state: SuperState::Open,
                role: None,
                description: None,
            },
            StateDef {
                slug: "in-progress".to_string(),
                super_state: SuperState::Open,
                role: None,
                description: Some("Being worked".to_string()),
            },
            // The project's `active` role sits on a *custom* state, which is
            // the arrangement the real tree cannot exercise: there it is on
            // `in-progress`, where a reader that hard-coded the default catalog
            // would still look right.
            StateDef {
                slug: "review".to_string(),
                super_state: SuperState::Open,
                role: Some("active".to_string()),
                description: Some("Awaiting a second pair of eyes".to_string()),
            },
            StateDef {
                slug: "done".to_string(),
                super_state: SuperState::Closed,
                role: None,
                description: None,
            },
            StateDef {
                slug: "wont-fix".to_string(),
                super_state: SuperState::Closed,
                role: None,
                description: Some("Closed without doing it".to_string()),
            },
        ],
    )
    .expect("states");

    storage::save_types(
        &root,
        &[
            TypeDef {
                slug: "story".to_string(),
                description: None,
                emoji: None,
            },
            TypeDef {
                slug: "spike".to_string(),
                description: Some("Time-boxed investigation".to_string()),
                emoji: None,
            },
        ],
    )
    .expect("types");

    for member in [
        Member {
            id: "ada".to_string(),
            display_name: "Ada Lovelace".to_string(),
            email: Some("ada@example.com".to_string()),
            github: Some("adalovelace".to_string()),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        },
        Member {
            id: "grace".to_string(),
            display_name: "Grace Hopper".to_string(),
            email: None,
            github: None,
            created_at: "2026-01-02T00:00:00Z".to_string(),
        },
    ] {
        storage::store_member(&root, &member).expect("member");
    }

    // One story per shape that matters: an open one carrying every enrichment
    // field, a closed one in the *second* CLOSED state (which the real tree has
    // no equivalent of), a deleted one, and a pair holding both ends of a
    // relation.
    seed(
        &root,
        "ADA-1",
        &[
            created("2026-01-03T00:00:00Z", "Design the engine", "todo"),
            StoryEvent::StoryTypeSet {
                at: "2026-01-03T00:01:00Z".to_string(),
                story_type: "spike".to_string(),
            },
            StoryEvent::StoryPrioritySet {
                at: "2026-01-03T00:02:00Z".to_string(),
                priority: storyhook::domain::Priority::High,
            },
            StoryEvent::StoryLabelsSet {
                at: "2026-01-03T00:03:00Z".to_string(),
                labels: vec!["analytical".to_string(), "phase:1".to_string()],
            },
            StoryEvent::StoryAssigned {
                at: "2026-01-03T00:04:00Z".to_string(),
                member_id: "ada".to_string(),
            },
            StoryEvent::StoryDescriptionSet {
                at: "2026-01-03T00:05:00Z".to_string(),
                description: "A machine that weaves algebraic patterns.".to_string(),
            },
            StoryEvent::StoryStateChanged {
                at: "2026-01-03T00:06:00Z".to_string(),
                state: "review".to_string(),
            },
            StoryEvent::StoryAwaitingSet {
                at: "2026-01-03T00:07:00Z".to_string(),
                awaiting: "a punch-card supplier".to_string(),
            },
            StoryEvent::StoryCommentAdded {
                at: "2026-01-03T00:08:00Z".to_string(),
                text: "Notes on the Analytical Engine".to_string(),
            },
            StoryEvent::StoryRelationshipAdded {
                at: "2026-01-03T00:09:00Z".to_string(),
                other_id: "ADA-2".to_string(),
                relation: "parent-of".to_string(),
            },
        ],
    );
    seed(
        &root,
        "ADA-2",
        &[
            created("2026-01-04T00:00:00Z", "Punch the cards", "todo"),
            StoryEvent::StoryRelationshipAdded {
                at: "2026-01-04T00:01:00Z".to_string(),
                other_id: "ADA-1".to_string(),
                relation: "child-of".to_string(),
            },
        ],
    );
    seed(
        &root,
        "ADA-3",
        &[
            created("2026-01-05T00:00:00Z", "Abandoned idea", "todo"),
            StoryEvent::StoryStateChanged {
                at: "2026-01-05T00:01:00Z".to_string(),
                state: "wont-fix".to_string(),
            },
            StoryEvent::StoryClosedAndArchived {
                at: "2026-01-05T00:01:00Z".to_string(),
                state: "wont-fix".to_string(),
            },
        ],
    );
    storage::archive_story(&root, "ADA-3").expect("archive ADA-3");

    seed(
        &root,
        "ADA-4",
        &[created("2026-01-06T00:00:00Z", "Mistake", "todo")],
    );
    storage::delete_story(&root, "ADA-4", "filed twice").expect("delete ADA-4");

    std::fs::write(root.join(".storyhook/next-id"), "5\n").expect("next-id");
    (dir, root)
}

fn created(at: &str, title: &str, state: &str) -> StoryEvent {
    StoryEvent::StoryCreated {
        at: at.to_string(),
        title: title.to_string(),
        state: state.to_string(),
    }
}

fn seed(root: &Path, id: &str, events: &[StoryEvent]) {
    storage::write_story_events(root, id, events).unwrap_or_else(|e| panic!("seeding {id}: {e}"));
}

/// Every file under `root`, keyed by its path relative to `root`.
///
/// Contents rather than a digest: the trees are small, and a failure that can
/// print the differing file is worth more than one that prints two hashes.
pub fn tree_contents(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    collect(root, root, &mut files);
    files
}

fn collect(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(base, &path, out);
        } else if let Ok(bytes) = std::fs::read(&path) {
            out.insert(
                path.strip_prefix(base).unwrap_or(&path).to_path_buf(),
                bytes,
            );
        }
    }
}

/// Asserts that `root` holds exactly what it held when `before` was taken.
pub fn assert_tree_unchanged(root: &Path, before: &BTreeMap<PathBuf, Vec<u8>>, what: &str) {
    let after = tree_contents(root);
    let missing: Vec<_> = before.keys().filter(|k| !after.contains_key(*k)).collect();
    let added: Vec<_> = after.keys().filter(|k| !before.contains_key(*k)).collect();
    assert!(
        missing.is_empty() && added.is_empty(),
        "{what} changed the file set of the legacy tree: removed {missing:?}, added {added:?}"
    );
    for (path, bytes) in before {
        assert!(
            after[path] == *bytes,
            "{what} rewrote `{}` — the legacy tree is the operator's rollback and must survive \
             byte for byte",
            path.display()
        );
    }
}

/// An open, migrated, empty store in its own fixture directory.
///
/// The path is a parameter all the way down (`SqliteStore::open` takes one), so
/// an in-process test never reaches the developer's real data directory — the
/// trap W2d hit twice.
pub fn new_store() -> (TempDir, storyhook::store::SqliteStore) {
    use storyhook::store::Store as _;
    let dir = scratch_dir_named("migrate-store-");
    let store = storyhook::store::SqliteStore::open(dir.path().join("store.db"))
        .expect("opening a fresh store");
    store.migrate().expect("migrating a fresh store");
    (dir, store)
}

/// Migrates `root` into a brand-new store, returning both plus the report.
pub fn migrate(root: &Path) -> (TempDir, storyhook::store::SqliteStore, MigrationReport) {
    let (dir, store) = new_store();
    let report = plan(root)
        .apply(&store, root)
        .expect("applying the migration");
    (dir, store, report)
}

/// The plan for `root`, or a panic naming why it could not be built.
pub fn plan(root: &Path) -> MigrationPlan {
    MigrationPlan::build(legacy::read_project(root).expect("reading the legacy tree"))
        .unwrap_or_else(|e| panic!("planning a migration of {}: {e}", root.display()))
}

/// Every story in the store's only project, folded.
pub fn store_snapshots(store: &storyhook::store::SqliteStore) -> BTreeMap<String, StorySnapshot> {
    use storyhook::store::{ReadOps, Store as _};
    store
        .read(|tx| {
            let project = tx.projects()?.first().expect("one project").id;
            Ok(
                storyhook::service::QueryService::new(tx, project, "2026-01-01T00:00:00Z")
                    .story_map()?,
            )
        })
        .expect("reading the store")
}
