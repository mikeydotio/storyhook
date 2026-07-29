//! `src/legacy/` against real trees: what it reads, and what it refuses.
//!
//! The reader is the one component of the rearchitecture that never becomes
//! obsolete — a `.storyhook` directory can turn up in a branch, a clone or a
//! bug report years after the flip — so its tests are about *fidelity* and
//! about *not touching anything*, in that order.

mod legacy_support;

use std::collections::BTreeMap;
use std::path::Path;

use legacy_support::{custom_config_tree, real_tree, tree_contents};
use storyhook::domain::{StorySnapshot, SuperState, fold_story};
use storyhook::legacy::{self, LegacyPaths};

/// Folds every story the reader returns, the way an importer would.
fn fold_all(project: &legacy::LegacyProject) -> BTreeMap<String, StorySnapshot> {
    let states = project
        .states
        .iter()
        .map(|state| (state.slug.clone(), state.clone()))
        .collect();
    project
        .stories
        .iter()
        .map(|story| {
            let known: Vec<_> = story
                .events
                .iter()
                .filter_map(|event| event.decoded.clone())
                .collect();
            let snapshot = fold_story(&story.id, &known, &states)
                .unwrap_or_else(|e| panic!("folding {}: {e}", story.id));
            (story.id.clone(), snapshot)
        })
        .collect()
}

#[test]
fn the_real_tree_reads_back_with_the_shape_the_baseline_recorded() {
    let (_guard, root) = real_tree();
    let project = legacy::read_project(&root).expect("reading the frozen tree");

    assert_eq!(project.stories.len(), 61, "story count");
    assert_eq!(
        project
            .stories
            .iter()
            .map(|s| s.events.len())
            .sum::<usize>(),
        486,
        "event count"
    );
    assert_eq!(
        project.stories.iter().filter(|s| s.archived).count(),
        44,
        "archived count"
    );
    assert_eq!(project.prefix.as_deref(), Some("SH"));
    assert_eq!(project.schema, 1);
    assert_eq!(project.next_id, 62);
    assert!(
        project.members.is_empty(),
        "the real tree has no members — this is the gap the synthetic fixture fills"
    );
    assert_eq!(
        project
            .states
            .iter()
            .map(|s| s.slug.as_str())
            .collect::<Vec<_>>(),
        ["todo", "in-progress", "verifying", "blocked", "done"]
    );
    assert_eq!(
        project
            .types
            .iter()
            .map(|t| t.slug.as_str())
            .collect::<Vec<_>>(),
        ["story", "epic", "bug", "chore", "task"]
    );
    assert!(
        project.unknown_events().next().is_none(),
        "every event in the frozen tree is one this binary understands"
    );
}

#[test]
fn the_real_trees_stories_fold_and_land_in_the_states_the_baseline_counted() {
    let (_guard, root) = real_tree();
    let project = legacy::read_project(&root).expect("reading");
    let folded = fold_all(&project);

    let mut per_state: BTreeMap<&str, usize> = BTreeMap::new();
    for snapshot in folded.values() {
        *per_state.entry(snapshot.state.as_str()).or_default() += 1;
    }
    assert_eq!(
        folded.len(),
        61,
        "every story must fold — an unfoldable one is a migration that cannot finish"
    );
    assert_eq!(
        per_state.values().sum::<usize>(),
        61,
        "per-state counts must account for every story: {per_state:?}"
    );
    assert_eq!(
        folded.values().filter(|s| s.deleted).count(),
        1,
        "the frozen tree holds exactly one soft-deleted story"
    );
    assert_eq!(
        folded
            .values()
            .filter(|s| s.superstate == SuperState::Closed)
            .count(),
        44,
        "closed stories must equal the archived ones"
    );
}

#[test]
fn open_stories_come_before_archived_ones_in_export_order() {
    let (_guard, root) = real_tree();
    let project = legacy::read_project(&root).expect("reading");

    let first_archived = project
        .stories
        .iter()
        .position(|story| story.archived)
        .expect("the tree has archived stories");
    assert!(
        project.stories[..first_archived]
            .iter()
            .all(|story| !story.archived),
        "the reader must reproduce `export_project`'s order — every open story, then every \
         archived one — or the export document it feeds moves bytes against the golden corpus"
    );
    assert_eq!(project.stories[0].id, "SH-42", "open stories sort as text");
    assert_eq!(
        project.stories[first_archived].id, "SH-1",
        "archived stories sort as text too, so SH-10 follows SH-1"
    );
}

#[test]
fn the_custom_config_tree_reads_back_the_configuration_surface_the_real_one_lacks() {
    let (_guard, root) = custom_config_tree();
    let project = legacy::read_project(&root).expect("reading");

    assert_eq!(project.prefix.as_deref(), Some("ADA"));
    assert_eq!(project.effective_prefix(), "ADA");
    let review = project
        .states
        .iter()
        .find(|state| state.slug == "review")
        .expect("the custom state");
    assert_eq!(review.role.as_deref(), Some("active"));
    assert_eq!(
        review.description.as_deref(),
        Some("Awaiting a second pair of eyes")
    );
    assert_eq!(
        project
            .states
            .iter()
            .filter(|state| state.super_state == SuperState::Closed)
            .count(),
        2,
        "two CLOSED states is the shape the real tree cannot exercise"
    );
    assert!(
        project.types.iter().any(|t| t.slug == "spike"),
        "custom types must survive"
    );
    assert_eq!(project.members.len(), 2);
    assert_eq!(project.members[0].id, "ada");
    assert_eq!(project.members[0].github.as_deref(), Some("adalovelace"));
    assert!(
        project.members[1].email.is_none(),
        "an absent optional field must stay absent"
    );
    assert_eq!(project.next_id, 5);
}

#[test]
fn a_default_prefix_reads_back_as_absent_because_that_is_what_export_emits() {
    let dir = storyhook_test_support::scratch_dir();
    storyhook::storage::init_project(dir.path(), None).expect("init");
    let project = legacy::read_project(dir.path()).expect("reading");
    assert_eq!(
        project.prefix, None,
        "`story init` with no --prefix leaves project.toml's prefix unset, and `story export` \
         emits null for it; a reader that defaulted it to SH would move a byte in the golden \
         document"
    );
    assert_eq!(project.effective_prefix(), "SH");
}

#[test]
fn an_event_kind_this_binary_never_heard_of_survives_the_read() {
    let (_guard, root) = custom_config_tree();
    let log = root.join(".storyhook/open/stories/ADA-2.jsonl");
    let mut text = std::fs::read_to_string(&log).unwrap();
    text.push_str(r#"{"kind":"StoryPinned","at":"2026-06-01T00:00:00Z","by":"ada","note":"x"}"#);
    text.push('\n');
    std::fs::write(&log, text).unwrap();

    let project = legacy::read_project(&root).expect("an unknown kind must not fail the read");
    let unknown: Vec<_> = project.unknown_events().collect();
    assert_eq!(unknown.len(), 1, "the unknown event must be reported");
    assert_eq!(unknown[0].0.id, "ADA-2");
    assert_eq!(unknown[0].1.kind, "StoryPinned");
    assert_eq!(
        unknown[0].1.payload,
        r#"{"kind":"StoryPinned","at":"2026-06-01T00:00:00Z","by":"ada","note":"x"}"#,
        "retained byte for byte, which is what makes a migration reversible for a tree written \
         by a newer storyhook (SH-54)"
    );
}

#[test]
fn a_corrupt_story_log_is_refused_and_the_file_is_named() {
    let (_guard, root) = custom_config_tree();
    let log = root.join(".storyhook/open/stories/ADA-1.jsonl");
    let mut text = std::fs::read_to_string(&log).unwrap();
    text.push_str("{not json\n");
    std::fs::write(&log, text).unwrap();

    let error = legacy::read_project(&root).expect_err("a corrupt log must not be read past");
    let message = error.to_string();
    assert!(
        message.contains("ADA-1.jsonl"),
        "the refusal must name the file so the operator can look at it: {message}"
    );
    assert_eq!(
        storyhook::error::AppError::from(error).exit_code(),
        5,
        "a corrupt tree is a storage failure"
    );
}

#[test]
fn a_truncated_final_line_is_refused_rather_than_skipped() {
    let (_guard, root) = custom_config_tree();
    let log = root.join(".storyhook/open/stories/ADA-1.jsonl");
    let mut text = std::fs::read_to_string(&log).unwrap();
    text.push_str(r#"{"kind":"StoryCommentAdded","at":"2026-06-01T00:00:00Z","tex"#);
    std::fs::write(&log, text).unwrap();

    assert!(
        legacy::read_project(&root).is_err(),
        "`load_open_snapshots_tolerant` skips a torn final line so the TUI can reload mid-write; \
         a migration reads the tree once and keeps the answer forever, so it must not"
    );
}

#[test]
fn an_uncheckpointed_write_ahead_log_is_refused_by_name() {
    let (_guard, root) = custom_config_tree();
    let wal = root.join(".storyhook/archive/archive.db-wal");
    std::fs::write(&wal, vec![0_u8; 4096]).unwrap();

    let error = legacy::read_project(&root).expect_err("uncheckpointed writes must not be ignored");
    let message = error.to_string();
    assert!(
        message.contains("archive.db-wal"),
        "the refusal must name the write-ahead log: {message}"
    );
    assert!(
        message.contains("story web stop"),
        "and say how to get past it: {message}"
    );
}

#[test]
fn an_empty_write_ahead_log_is_not_an_obstacle() {
    let (_guard, root) = custom_config_tree();
    std::fs::write(root.join(".storyhook/archive/archive.db-wal"), b"").unwrap();
    assert!(
        legacy::read_project(&root).is_ok(),
        "SQLite leaves a zero-length WAL behind after a checkpoint; refusing on that would refuse \
         every tree a `story` process has ever touched"
    );
}

#[test]
fn a_next_id_that_is_not_a_number_is_refused() {
    let (_guard, root) = custom_config_tree();
    std::fs::write(root.join(".storyhook/next-id"), "seventeen\n").unwrap();
    let error = legacy::read_project(&root).expect_err("a broken counter must not be guessed at");
    assert!(error.to_string().contains("next-id"), "{error}");
}

#[test]
fn a_directory_with_no_project_is_not_found_rather_than_corrupt() {
    let dir = storyhook_test_support::scratch_dir();
    let error = legacy::read_project(dir.path()).expect_err("no project here");
    assert_eq!(
        storyhook::error::AppError::from(error).exit_code(),
        3,
        "`story migrate` in a directory with no legacy tree is the same NotFound every other \
         verb answers with"
    );
}

#[test]
fn the_root_is_found_by_walking_up_from_a_subdirectory() {
    let (_guard, root) = custom_config_tree();
    let deep = root.join("src/inner/deeper");
    std::fs::create_dir_all(&deep).unwrap();

    let found = legacy::find_root(&deep).expect("the walk must find the project above");
    assert_eq!(
        found.canonicalize().unwrap(),
        root.canonicalize().unwrap(),
        "`story migrate` is run once from wherever the operator is standing"
    );
    assert_eq!(
        legacy::find_root(&root).map(|p| p.canonicalize().unwrap()),
        Some(root.canonicalize().unwrap()),
        "the walk includes the starting directory"
    );
}

#[test]
fn reading_a_tree_changes_no_byte_of_it() {
    let (_guard, root) = real_tree();
    let before = tree_contents(&root);
    let project = legacy::read_project(&root).expect("reading");
    assert_eq!(project.stories.len(), 61);
    legacy_support::assert_tree_unchanged(&root, &before, "reading the tree");
    assert!(
        !LegacyPaths::new(&root).archive_wal().exists()
            && !root.join(".storyhook/archive/archive.db-shm").exists(),
        "an ordinary read-only SQLite connection creates a `-shm` sidecar; `immutable=1` is what \
         keeps the reader from leaving one in a tree it was asked only to look at"
    );
}

/// The structural half of "it never writes": no call under `src/legacy/` can
/// create, truncate, append to or remove anything.
///
/// A source-level guard rather than a runtime one because the runtime one can
/// only prove that the paths a test happened to exercise are clean. This proves
/// there is no such path at all, and it is what stops the next person adding a
/// convenience that caches a fold beside the tree it read.
#[test]
fn the_reader_contains_no_write_calls() {
    const FORBIDDEN: &[&str] = &[
        "fs::write",
        "File::create",
        "OpenOptions",
        "create_dir",
        "remove_file",
        "remove_dir",
        "set_permissions",
        "copy(",
        "rename(",
        "SQLITE_OPEN_READ_WRITE",
        "SQLITE_OPEN_CREATE",
    ];
    let dir = legacy_support::repo_root().join("src/legacy");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("src/legacy must exist") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        checked += 1;
        let production = production_code(&path);
        for needle in FORBIDDEN {
            assert!(
                !production.contains(needle),
                "`{}` contains `{needle}`. Nothing under src/legacy/ may write: the legacy tree \
                 is the operator's rollback, and a migration that modifies it has taken that \
                 away. If a write is genuinely needed, it belongs in the caller.",
                path.display()
            );
        }
    }
    assert!(
        checked >= 3,
        "expected to scan the whole module, saw {checked} files"
    );
}

/// The reader must not reach into `src/storage.rs`, in either direction.
///
/// The flip deletes storage.rs's write half and most of the rest of it; a
/// reader that imports from it acquires a deletion date. This is the coupling
/// the flip checklist assumes is zero.
#[test]
fn the_reader_does_not_depend_on_the_module_the_flip_deletes() {
    let dir = legacy_support::repo_root().join("src/legacy");
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let production = production_code(&path);
        assert!(
            !production.contains("storage::") && !production.contains("crate::storage"),
            "`{}` reaches into src/storage.rs. The two read the same bytes on purpose: this one \
             outlives the flip and that one does not.",
            path.display()
        );
    }
}

/// One source file with its comments and its `#[cfg(test)]` module removed.
///
/// Both guards below scan for *calls*, and both would otherwise be tripped by
/// the module documentation that explains why those calls are absent — an
/// assertion that fails on the sentence describing it teaches the next person
/// to delete the sentence.
fn production_code(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
        .split("#[cfg(test)]")
        .next()
        .expect("split always yields one part")
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_layout_is_the_one_the_legacy_writer_used() {
    // Guards against the reader and the writer drifting while both still exist.
    let root = Path::new("/checkout");
    let ours = LegacyPaths::new(root);
    let theirs = storyhook::storage::ProjectPaths::new(root);
    assert_eq!(ours.project_file(), theirs.project_file());
    assert_eq!(ours.states_file(), theirs.states_file());
    assert_eq!(ours.types_file(), theirs.types_file());
    assert_eq!(ours.members_file(), theirs.members_file());
    assert_eq!(ours.next_id_file(), theirs.next_id_file());
    assert_eq!(ours.open_stories_dir(), theirs.open_stories_dir());
    assert_eq!(ours.archive_db(), theirs.archive_db());
}
