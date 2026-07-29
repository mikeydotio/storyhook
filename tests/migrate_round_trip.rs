//! **The two-way door.** A project that has been migrated into the store must
//! be reconstructible as a legacy tree, exactly.
//!
//! This file is the W4 revert policy's evidence, and nothing in it may be
//! skipped, `#[ignore]`d or weakened. The flip is only reversible for as long
//! as the round trip below is green:
//!
//! ```text
//! legacy tree ──migrate──▶ store ──export──▶ ProjectExport ──import-project──▶ legacy tree
//!                            └────────────── read models must be equal ──────────────┘
//! ```
//!
//! The comparison is *snapshot-level equality over every story*, both sides —
//! not counts, not ids, not a spot check. `StorySnapshot` derives `PartialEq`
//! over every field a user can see: title, state, superstate, assignee,
//! awaiting, comments, relationships, priority, labels, type, description,
//! `closed_at`, `deleted` and its reason. A field lost anywhere in the loop is
//! a failing assertion naming the story.
//!
//! # What the round trip does *not* carry, and why that is written here
//!
//! An export document holds states, types, members and stories. It does not
//! hold `project.toml`'s `sync`/`doctor` settings, its `created_at`, or the
//! `next-id` counter — those ride beside the envelope during a migration and
//! have nowhere to sit on the way back. Widening the document would move bytes
//! in a format `golden-export.json` compares literally, so the gap is recorded
//! rather than closed. It is one of the reasons `.storyhook/` stays in the
//! repository until W7: the real rollback is the directory that was never
//! touched, and this round trip is the guarantee for everything created *after*
//! the flip.

mod legacy_support;

use std::collections::BTreeMap;
use std::path::Path;

use legacy_support::{custom_config_tree, golden_export_path, migrate, real_tree, store_snapshots};
use storyhook::domain::StorySnapshot;
use storyhook::service::transfer::ProjectExport;
use storyhook::service::{Clock, Ctx, TransferService};
use storyhook::storage;
use storyhook::store::{ReadOps, SqliteStore, Store as _};

/// The store's project as an export document.
fn export(store: &SqliteStore) -> ProjectExport {
    let project = store
        .read(|tx| Ok(tx.projects()?.first().expect("one project").id))
        .expect("reading");
    let cwd = std::env::temp_dir();
    let ctx = Ctx::new(store, project, &cwd, storyhook::env::Environment::at(&cwd))
        .no_hooks(true)
        .clock(Clock::Fixed("2026-01-01T00:00:00Z".to_string()));
    TransferService::new(&ctx).export().expect("exporting")
}

/// Materializes `document` as a legacy tree in a fresh directory.
fn rebuild_legacy_tree(document: &ProjectExport) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = storyhook_test_support::scratch_dir_named("rollback-");
    let root = dir.path().to_path_buf();
    storage::import_project(&root, document).expect("the legacy importer must accept the document");
    (dir, root)
}

/// Every story the legacy tree at `root` holds, folded, keyed by id.
fn legacy_snapshots(root: &Path) -> BTreeMap<String, StorySnapshot> {
    storage::load_all_snapshots(root)
        .expect("reading the rebuilt tree")
        .into_iter()
        .map(|snapshot| (snapshot.id.clone(), snapshot))
        .collect()
}

/// Runs the whole loop for one legacy tree and asserts the two read models are
/// identical, story by story.
fn assert_round_trips(root: &Path, expected_stories: usize) {
    let (_store_dir, store, report) = migrate(root);
    assert_eq!(report.stories, expected_stories);

    let document = export(&store);
    let (_dir, rebuilt) = rebuild_legacy_tree(&document);

    let from_store = store_snapshots(&store);
    let from_legacy = legacy_snapshots(&rebuilt);

    assert_eq!(
        from_store.keys().collect::<Vec<_>>(),
        from_legacy.keys().collect::<Vec<_>>(),
        "the rebuilt tree must hold exactly the same stories"
    );
    for (id, expected) in &from_store {
        assert_eq!(
            &from_legacy[id], expected,
            "story {id} does not survive the round trip intact"
        );
    }

    // Whole-project fidelity, not just stories.
    let (states, types, members, prefix) = store
        .read(|tx| {
            let projects = tx.projects()?;
            let project = projects.first().expect("one project");
            Ok((
                tx.states(project.id)?,
                tx.types(project.id)?,
                tx.members(project.id)?,
                project.prefix.clone(),
            ))
        })
        .expect("reading");
    assert_eq!(storage::load_states(&rebuilt).expect("states"), states);
    assert_eq!(storage::load_types(&rebuilt).expect("types"), types);
    assert_eq!(storage::load_members(&rebuilt).expect("members"), members);
    assert_eq!(
        storage::load_project_prefix(&rebuilt).expect("prefix"),
        prefix,
        "a project that came back under a different prefix has different story ids"
    );

    // And the counter, so the reconstructed tree cannot re-mint an id.
    let next_id = std::fs::read_to_string(rebuilt.join(".storyhook/next-id")).expect("next-id");
    let highest = from_store
        .keys()
        .filter_map(|id| id.rsplit('-').next()?.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    assert!(
        next_id.trim().parse::<u64>().expect("a number") > highest,
        "the rebuilt tree's counter ({}) must be past its highest story ({highest})",
        next_id.trim()
    );
}

#[test]
fn the_real_tree_round_trips_through_the_store_and_back() {
    let (_tree, root) = real_tree();
    assert_round_trips(&root, 61);
}

#[test]
fn the_custom_config_tree_round_trips_through_the_store_and_back() {
    // The configuration surface the real tree has never used: a custom prefix,
    // a custom state carrying the project's `active` role, a second CLOSED
    // state, a custom type, two members, an archived story and a deleted one.
    let (_tree, root) = custom_config_tree();
    assert_round_trips(&root, 4);
}

#[test]
fn a_round_trip_survives_a_second_lap() {
    // One lap can hide a loss that a second one makes visible — a field dropped
    // on the way out and defaulted on the way in looks stable until something
    // else reads it. Asserted twice, like the byte-identical export round trip
    // W0.3 built for the same reason.
    let (_tree, root) = custom_config_tree();
    let (_store_dir, store, _report) = migrate(&root);

    let first = export(&store);
    let (_a, rebuilt) = rebuild_legacy_tree(&first);
    let second = storage::export_project(&rebuilt).expect("re-exporting the rebuilt tree");
    let (_b, again) = rebuild_legacy_tree(&second);

    assert_eq!(
        serde_json::to_string_pretty(&first).unwrap(),
        serde_json::to_string_pretty(&second).unwrap(),
        "the document a rebuilt tree exports must equal the one it was built from, byte for byte"
    );
    assert_eq!(legacy_snapshots(&rebuilt), legacy_snapshots(&again));
}

#[test]
fn the_real_trees_export_equals_the_golden_document_modulo_the_repairs() {
    // `golden-export.json` is what `story export` produced from this tree
    // before any of this work started. Everything the migration does to it must
    // be a *repair the report names* — not a reordering, not a dropped field,
    // not a rewritten timestamp.
    let (_tree, root) = real_tree();
    let (_store_dir, store, report) = migrate(&root);
    let ours = export(&store);
    let golden: ProjectExport =
        serde_json::from_str(&std::fs::read_to_string(golden_export_path()).expect("golden"))
            .expect("parsing the golden document");

    assert_eq!(ours.schema, golden.schema);
    assert_eq!(ours.states, golden.states);
    assert_eq!(ours.types, golden.types);
    assert_eq!(ours.members, golden.members);
    assert_eq!(
        ours.stories.iter().map(|s| &s.id).collect::<Vec<_>>(),
        golden.stories.iter().map(|s| &s.id).collect::<Vec<_>>(),
        "open stories first, then archived, each group sorted as text — the order the golden \
         document froze"
    );
    assert_eq!(ours.prefix, None);
    assert_eq!(
        golden.prefix.as_deref(),
        Some("SH"),
        "the one known difference, and it is contract rather than loss: `story export` emits \
         `null` for the default prefix, and this project was initialized with an explicit \
         `--prefix SH`. Both documents import to the same project."
    );

    // Every event of every story: the golden document's events must all still
    // be there, in order, and every *extra* event must be one the report names.
    let mut unexplained = Vec::new();
    let mut accounted = 0_usize;
    for (ours, golden) in ours.stories.iter().zip(&golden.stories) {
        assert_eq!(ours.archived, golden.archived, "{} archived flag", ours.id);
        let mut theirs = golden.events.iter();
        for event in &ours.events {
            if theirs.clone().next() == Some(event) {
                theirs.next();
                continue;
            }
            let named = report.repairs.iter().any(|repair| {
                repair.story == ours.id
                    && serde_json::to_value(event).ok()
                        == serde_json::to_value(
                            &storyhook::domain::StoryEvent::StoryRelationshipAdded {
                                at: repair.at.clone(),
                                other_id: repair.other.clone(),
                                relation: repair.relation.clone(),
                            },
                        )
                        .ok()
                    || repair.story == ours.id
                        && serde_json::to_value(event).ok()
                            == serde_json::to_value(
                                &storyhook::domain::StoryEvent::StoryRelationshipRemoved {
                                    at: repair.at.clone(),
                                    other_id: repair.other.clone(),
                                    relation: repair.relation.clone(),
                                },
                            )
                            .ok()
            });
            if named {
                accounted += 1;
            } else {
                unexplained.push(format!("{}: {event:?}", ours.id));
            }
        }
        assert_eq!(
            theirs.count(),
            0,
            "{}: the golden document has events the migration did not import",
            ours.id
        );
    }

    assert!(
        unexplained.is_empty(),
        "the migrated export differs from the golden document in ways the repair report does not \
         explain:\n{}",
        unexplained.join("\n")
    );
    assert_eq!(
        accounted,
        report.repairs.len(),
        "every repair the report names must actually appear in the document, and nothing else"
    );
    assert_eq!(report.repairs.len(), 10);
}
