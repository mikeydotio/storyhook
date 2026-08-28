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
//! An export document holds states, types, members, stories, the project's
//! settings and its registered origins. It does **not** hold `project.toml`'s
//! `created_at`, the `next-id` counter's burned numbers, or `projects.uuid` —
//! those ride beside the envelope during a migration and have nowhere to sit
//! on the way back.
//!
//! **Settings were on that list until SH-133, and the reason given for it was
//! false.** The note here said widening the document would move bytes in a
//! format `golden-export.json` compares literally. It does not compare
//! literally: `the_real_trees_export_equals_the_golden_document_modulo_the_repairs`
//! parses that file into a `ProjectExport` and asserts field by field, so a
//! field that is absent when unset moves nothing at all and the golden document
//! needed no regeneration. What *was* true is that the gap was benign while
//! `story migrate` was the only writer of those columns — a value in the store
//! had come from a tree, so the tree it would roll back to already held it.
//! SH-129 shipped `story project settings`, the columns became live user data,
//! and a rollback started silently restoring `sync.auto_transition` to its
//! default — which is `true`, so the one setting whose purpose is stopping
//! `commit-sync` came back switched on.
//!
//! **Registered origins joined the document at SH-138, and the round trip
//! above still does not carry them past `ProjectExport`.** The document holds
//! them because the store-side restore (`service::transfer::import_project`,
//! what `story import-project` actually runs) is the *primary* consumer now
//! that `story export > backup.json` is the documented backup — a project
//! whose only remote is a bare repository with no working checkout anywhere
//! has no other record of it. But the legacy tree this file rebuilds has
//! nowhere to put them: `project.toml` has never had a table for a registered
//! origin, before or after the rearchitecture, so `storage::export_project`
//! always answers an empty list and `storage::import_project` never looks at
//! the field. That is not a leftover gap of the kind SH-133 closed — it is a
//! carry this leg of the round trip structurally cannot make, and the only one
//! left.
//!
//! **`github.sync` used to be a third one, and this note used to explain the
//! carry rather than its absence.** SH-189 and SH-233 once made the whole loop
//! carry it end to end — a partial carry (the blob without its per-story merge
//! bases) is worse than none, because the next sync would treat local as base
//! and file every stale remote value as an ordinary pull, so both moved
//! together on every leg. SH-408 retired the engine that read either side of
//! that carry, which makes the whole question moot rather than merely
//! answered differently: `ProjectExport::github_sync`/`github_bases` still
//! deserialize an *old* document's blob (so a backup from before this story
//! is not silently corrupted on the way in), but `export` never populates
//! either from a current store and `story migrate` never carries either out of
//! a legacy tree — see `ProjectExport::github_sync`'s own doc comment, and
//! `src/service/migrate.rs`'s D5 report logic, for where the two files a tree
//! still holding them are *named* instead.

mod legacy_support;

use std::collections::BTreeMap;
use std::path::Path;

use legacy_support::{
    MIGRATION_AT, custom_config_tree, golden_export_path, migrate, real_tree, store_snapshots,
};
use storyhook::domain::{StorySnapshot, fold_story};
use storyhook::service::transfer::ProjectExport;
use storyhook::service::{Clock, Ctx, GitService, StoryService, TransferService};
use storyhook::storage;
use storyhook::store::{
    ReadOps, SqliteStore, Store as _, StoryQuery, WriteOps as _, partition_known,
};

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
///
/// Asserts that nothing was left behind on the way: the fixtures below hold no
/// event kind this build cannot decode, so an `UncarriedEvent` here would mean
/// the loop had quietly become lossy. The one test that *does* build such a
/// store calls `storage::import_project` itself and reads the list.
fn rebuild_legacy_tree(document: &ProjectExport) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = storyhook_test_support::scratch_dir_named("rollback-");
    let root = dir.path().to_path_buf();
    let uncarried = storage::import_project(&root, document)
        .expect("the legacy importer must accept the document");
    assert!(
        uncarried.is_empty(),
        "a tree rebuilt from a fully-decodable document must carry every event: {uncarried:?}"
    );
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

/// Every stored history folded without query-time epic-state projection.
///
/// The rollback contract compares the event-sourced artifact that crosses the
/// export boundary. Effective epic state is intentionally a read projection
/// and has its own query regressions.
fn stored_snapshots(store: &SqliteStore) -> BTreeMap<String, StorySnapshot> {
    store
        .read(|tx| {
            let project = tx.projects()?.first().expect("one project").id;
            let states = tx.state_map(project)?;
            let mut snapshots = BTreeMap::new();
            for row in tx.stories(project, &StoryQuery::all())? {
                let stored = tx.events_for(project, row.story_no)?;
                let (known, _unknown) = partition_known(row.story_no, &stored);
                let snapshot = fold_story(&row.snapshot.id, &known, &states)?;
                snapshots.insert(snapshot.id.clone(), snapshot);
            }
            Ok(snapshots)
        })
        .expect("folding stored histories")
}

/// Runs the whole loop for one legacy tree and asserts the two read models are
/// identical, story by story.
fn assert_round_trips(root: &Path, expected_stories: usize) {
    let (_store_dir, store, report) = migrate(root);
    assert_eq!(report.stories, expected_stories);

    let document = export(&store);
    let (_dir, rebuilt) = rebuild_legacy_tree(&document);

    let from_store = stored_snapshots(&store);
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
    // Settings, which the envelope carried nowhere until SH-133. Read back
    // through the legacy exporter rather than by parsing `project.toml` here,
    // because that is the reader a reverted binary's equivalent would be.
    assert_eq!(
        storage::export_project(&rebuilt)
            .expect("re-exporting the rebuilt tree")
            .settings,
        document.settings,
        "the settings a user wrote must reach the tree a rollback hands back"
    );
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
fn an_event_the_reverted_binary_could_not_read_is_dropped_by_name() {
    // The one edge of the two-way door that is genuinely one-way, and SH-67 is
    // where it stopped being silent. A legacy tree cannot hold an event kind
    // this build does not understand — `read_story_events` parses every line as
    // a `StoryEvent`, and the reverted binary a rollback hands the data back to
    // is older still — so `storage::import_project` drops it. What it must not
    // do is drop it quietly: the return value names what did not come back, and
    // the tree it does write is whole.
    let (_tree, root) = custom_config_tree();
    let (_store_dir, store, _report) = migrate(&root);
    let project = store
        .read(|tx| Ok(tx.projects()?.first().expect("one project").id))
        .expect("reading");
    storyhook::store::test_support::inject_raw_events(
        &store,
        project,
        storyhook::store::StoryNo::new(1),
        &[storyhook::store::RawEvent {
            kind: "ADA-Pinned".to_string(),
            at: "2030-01-01T00:00:00Z".to_string(),
            payload: r#"{"kind":"ADA-Pinned","at":"2030-01-01T00:00:00Z","by":"ada"}"#.to_string(),
        }],
    )
    .expect("injecting an event from the future");

    let document = export(&store);
    let dir = storyhook_test_support::scratch_dir_named("rollback-lossy-");
    let uncarried =
        storage::import_project(dir.path(), &document).expect("the rollback must still happen");

    assert_eq!(uncarried.len(), 1, "{uncarried:?}");
    assert_eq!(uncarried[0].kind, "ADA-Pinned");
    assert_eq!(uncarried[0].story, "ADA-1");
    assert!(
        uncarried[0].position > 1,
        "the position must locate it inside the history, not merely count it"
    );

    // And the tree is otherwise complete: every story is there, and the one
    // that lost an event still folds.
    let snapshots = legacy_snapshots(dir.path());
    assert_eq!(snapshots.len(), store_snapshots(&store).len());
    assert!(snapshots.contains_key("ADA-1"));
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

/// A comment appended *after* a story closed reaches the tree a rollback hands
/// back (SH-261).
///
/// Named as its own case rather than folded into the fixture, because the risk
/// it covers is structural and one-directional: SH-261 made a closed story
/// appendable, and the legacy format's own writers put closed stories in
/// `archive/`. An exporter that decided where a story lives *before* replaying
/// its whole log, or that stopped reading events at the closure, would lose
/// exactly the evidence SH-261 exists to preserve — and would lose it only on
/// rollback, which is the one moment nobody is watching. `make test` gates the
/// W4 revert policy on this file, so the guarantee belongs here.
#[test]
fn a_comment_added_after_a_story_closed_survives_the_round_trip() {
    let (_tree, root) = custom_config_tree();
    let (_store_dir, store, _report) = migrate(&root);

    // ADA-3 is the fixture's archived story: closed into `wont-fix` and moved
    // to `archive/` by the legacy writer before the migration ever saw it.
    let project = store
        .read(|tx| Ok(tx.projects()?.first().expect("one project").id))
        .expect("reading");
    let cwd = std::env::temp_dir();
    let ctx = Ctx::new(&store, project, &cwd, storyhook::env::Environment::at(&cwd))
        .no_hooks(true)
        .clock(Clock::Fixed("2026-02-02T00:00:00Z".to_string()));
    let commented = StoryService::new(&ctx)
        .comment("ADA-3", "verified after closure")
        .expect("a closed story takes a comment");
    assert_eq!(
        commented.state, "wont-fix",
        "the fixture story must still be closed, or this proves nothing"
    );

    let document = export(&store);
    let (_dir, rebuilt) = rebuild_legacy_tree(&document);

    let from_store = store_snapshots(&store);
    let from_legacy = legacy_snapshots(&rebuilt);
    assert_eq!(
        from_legacy["ADA-3"], from_store["ADA-3"],
        "a closed story's post-closure comment must survive store -> document -> legacy tree"
    );
    assert!(
        from_legacy["ADA-3"]
            .comments
            .iter()
            .any(|comment| comment.text == "verified after closure"),
        "the comment itself must be in the rebuilt tree, not merely an equal-looking snapshot"
    );
}

/// A commit link added *after* a story closed reaches the tree a rollback
/// hands back (SH-279).
///
/// The same structural risk `a_comment_added_after_a_story_closed_survives_the_round_trip`
/// guards for the sibling append SH-261 granted: an exporter that decided
/// where a story lives before replaying its whole log, or stopped reading
/// events at the closure, would lose exactly the evidence SH-279 exists to
/// preserve, and only on rollback.
#[test]
fn a_commit_link_added_after_a_story_closed_survives_the_round_trip() {
    let (_tree, root) = custom_config_tree();
    let (_store_dir, store, _report) = migrate(&root);

    // ADA-3 is the fixture's archived story: closed into `wont-fix` and moved
    // to `archive/` by the legacy writer before the migration ever saw it.
    let project = store
        .read(|tx| Ok(tx.projects()?.first().expect("one project").id))
        .expect("reading");

    // `commit_sync` reads `git log` over its cwd, which must be a real
    // repository — a fresh one, separate from `root`, since all that matters
    // is a commit whose message names the closed story.
    let repo = storyhook_test_support::scratch_dir_named("commit-sync-repo-");
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        let output = std::process::Command::new("git")
            .current_dir(repo.path())
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(&args)
            .output()
            .expect("running git");
        assert!(output.status.success(), "git {args:?} failed");
    }
    let output = std::process::Command::new("git")
        .current_dir(repo.path())
        .env("GIT_TERMINAL_PROMPT", "0")
        .args([
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "chore: reference ADA-3 after close",
        ])
        .output()
        .expect("running git commit");
    assert!(output.status.success(), "git commit failed");

    let ctx = Ctx::new(
        &store,
        project,
        repo.path(),
        storyhook::env::Environment::at(repo.path()),
    )
    .no_hooks(true)
    .clock(Clock::Fixed("2026-02-02T00:00:00Z".to_string()));
    let message = GitService::new(&ctx)
        .commit_sync(None)
        .expect("a closed story takes a commit link");
    assert!(
        message.contains("linked 1 commits to 1 stories"),
        "{message}"
    );

    let after = store
        .read(|tx| {
            let no = storyhook::store::StoryNo::parse_id("ADA", "ADA-3").expect("a well-formed id");
            Ok(tx.story(project, no)?.expect("the story exists").snapshot)
        })
        .expect("reading");
    assert_eq!(
        after.state, "wont-fix",
        "the fixture story must still be closed, or this proves nothing"
    );
    assert_eq!(after.referenced_by_commits.len(), 1);

    let document = export(&store);
    let (_dir, rebuilt) = rebuild_legacy_tree(&document);

    let from_store = store_snapshots(&store);
    let from_legacy = legacy_snapshots(&rebuilt);
    assert_eq!(
        from_legacy["ADA-3"], from_store["ADA-3"],
        "a closed story's post-closure commit link must survive store -> document -> legacy tree"
    );
    assert_eq!(
        from_legacy["ADA-3"].referenced_by_commits.len(),
        1,
        "the link itself must be in the rebuilt tree, not merely an equal-looking snapshot"
    );
}

#[test]
fn a_projects_settings_survive_the_round_trip() {
    // Without this, nothing in the loop would notice: `custom_config_tree`'s
    // `project.toml` comes from `storage::init_project` with no `[sync]` and no
    // `[doctor]`, so with settings encoded as "absent when unset" the whole
    // round trip stays green whether or not the legs carry them. The fixture
    // has to hold a value for the assertions to mean anything.
    //
    // Appended here rather than added to `custom_config_tree` itself:
    // `tests/service_migrate.rs::a_projects_settings_travel_with_it` appends
    // the same two tables to that fixture, and a duplicate table is a TOML
    // parse error.
    let (_tree, root) = custom_config_tree();
    let project_file = root.join(".storyhook/project.toml");
    let mut toml = std::fs::read_to_string(&project_file).expect("reading project.toml");
    toml.push_str("\n[sync]\nauto_transition = false\n\n[doctor]\nstale_threshold = \"21d\"\n");
    std::fs::write(&project_file, toml).expect("writing project.toml");

    assert_round_trips(&root, 4);

    // And the value itself, named — `sync.auto_transition` is read as
    // `.unwrap_or(true)`, so losing it does not forget a preference, it inverts
    // one.
    let (_store_dir, store, _report) = migrate(&root);
    let document = export(&store);
    let (_dir, rebuilt) = rebuild_legacy_tree(&document);
    let settings = storage::export_project(&rebuilt)
        .expect("re-exporting")
        .settings
        .expect("the rebuilt tree must carry the settings");
    assert_eq!(settings.auto_transition(), Some(false));
    assert_eq!(settings.stale_threshold(), Some("21d"));
}

#[test]
fn a_projects_registered_origins_reach_the_document_but_not_a_rebuilt_legacy_tree() {
    // The document carries them (SH-138) because the store-side restore —
    // `story import-project`, the documented backup's other half — is the
    // primary consumer. This leg of the two-way door cannot follow: a legacy
    // tree has never had anywhere to write one, before or after the
    // rearchitecture. It is the last thing the loop structurally cannot carry —
    // `github.sync`, which this comment used to name beside it, has its own
    // files in the tree and travels the whole way (SH-189, SH-233).
    let (_tree, root) = custom_config_tree();
    let (_store_dir, store, _report) = migrate(&root);
    let project = store
        .read(|tx| Ok(tx.projects()?.first().expect("one project").id))
        .expect("reading");
    let url =
        storyhook::domain::remote::RemoteUrl::normalize("https://github.com/acme/widgets.git")
            .expect("the fixture url should normalize");
    store
        .write(|tx| tx.link_remote(project, &url, "2026-01-01T00:00:00Z"))
        .expect("registering an origin");

    let document = export(&store);
    assert_eq!(
        document.remotes.len(),
        1,
        "the document must carry the registration"
    );
    assert_eq!(document.remotes[0].normalized, "github.com/acme/widgets");

    let (_dir, rebuilt) = rebuild_legacy_tree(&document);
    assert!(
        storage::export_project(&rebuilt)
            .expect("re-exporting the rebuilt tree")
            .remotes
            .is_empty(),
        "a legacy tree has no table for a registered origin, so re-exporting it must not \
         invent one"
    );
}

#[test]
fn a_tree_still_holding_github_sync_files_round_trips_without_them() {
    // SH-233 once made this loop carry github-sync end to end. SH-408 retired
    // the engine that read either file, so `story migrate` now only *names*
    // them in its report (`src/service/migrate.rs`'s D5 logic; covered in
    // `tests/service_migrate.rs`) — the two files themselves are left exactly
    // where they sit, untouched by anything in this loop, and the round trip
    // below must not be perturbed by their presence.
    let (_tree, root) = custom_config_tree();
    legacy_support::add_github_sync(&root, &["ADA-1", "ADA-3"]);

    assert_round_trips(&root, 4);

    let (_store_dir, store, _report) = migrate(&root);
    let document = export(&store);
    assert_eq!(document.github_sync, None);
    assert!(document.github_bases.is_empty());

    let (_dir, rebuilt) = rebuild_legacy_tree(&document);
    let reverted = storage::export_project(&rebuilt).expect("re-exporting the rebuilt tree");
    assert_eq!(reverted.github_sync, None);
    assert!(reverted.github_bases.is_empty());
}

/// **The preventative, not the instance.** SH-133 was one setting that could not
/// reach a rollback; a fourth setting added later could be another, and nobody
/// would find out until somebody's rollback ate it.
///
/// So the coverage is derived from `settings::registry()` rather than listed
/// here: every key the registry says a user may write is written, and the whole
/// loop — store → document → legacy tree → store — must give it back. A new
/// settable key inherits this check with no production code depending on the
/// registry and no list here to remember to update.
///
/// **`github.sync` no longer has a row in `settings::registry()` at all**
/// (SH-408 retired the key along with the engine it configured), so this
/// loop's `SettingKind::Document` guard below is now unreachable in practice
/// — kept anyway, because the registry-driven proof is exactly what would
/// catch a *future* document-shaped setting reaching this loop with no home
/// in a legacy tree, which is the failure mode this test exists to prevent.
#[test]
fn every_settable_setting_survives_the_whole_loop() {
    use storyhook::output::SettingKind;
    use storyhook::service::settings;

    let (_tree, root) = custom_config_tree();
    let (_store_dir, store, _report) = migrate(&root);
    let project = store
        .read(|tx| Ok(tx.projects()?.first().expect("one project").id))
        .expect("reading");

    let mut written = 0;
    store
        .write(|tx| {
            let mut row = tx.settings(project)?;
            for spec in settings::registry() {
                if !spec.settable() {
                    continue;
                }
                match spec.kind() {
                    SettingKind::Boolean => row.sync_auto_transition = Some(false),
                    SettingKind::Duration => {
                        row.doctor_stale_threshold = Some("30d".to_string());
                    }
                    // A settable document would have no home in `project.toml`,
                    // which is the whole reason `github.sync` is not settable.
                    SettingKind::Document => panic!(
                        "`{}` is settable and is a document: the rollback leg has nowhere to \
                         put it, so either it must stop being settable or this loop must grow \
                         a home for it — see `ExportedSettings`",
                        spec.key()
                    ),
                }
                written += 1;
            }
            tx.put_settings(project, &row)
        })
        .expect("writing every settable setting");
    assert!(written > 0, "the registry must have settable keys to check");

    let document = export(&store);
    let (_dir, rebuilt) = rebuild_legacy_tree(&document);
    let carried = storage::export_project(&rebuilt)
        .expect("re-exporting the rebuilt tree")
        .settings
        .expect("a tree rebuilt from a document with settings must carry them");

    assert_eq!(carried.auto_transition(), Some(false));
    assert_eq!(carried.stale_threshold(), Some("30d"));
    assert_eq!(
        carried,
        document.settings.expect("the document carried settings"),
        "every settable setting must survive the whole loop, not merely some of them"
    );
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
    // The golden document is FROZEN — captured by a pre-rearchitecture binary
    // and not regenerable — so it names the floor as it stood when it was
    // taken. A state the floor has gained since is a *repair*, exactly what
    // this test's name licenses, so the golden states must be a **prefix** of
    // ours and every extra one must be a required state the repair added.
    // Equality here would mean the floor could never grow again (SH-505).
    assert_eq!(
        &ours.states[..golden.states.len()],
        &golden.states[..],
        "the migration may append required states; it may not reorder, drop or \
         rewrite the ones the golden document froze"
    );
    for extra in &ours.states[golden.states.len()..] {
        let required = storyhook::domain::REQUIRED_STATES
            .iter()
            .find(|r| r.slug == extra.slug)
            .unwrap_or_else(|| {
                panic!("the migration invented the state `{}`, which is not on the floor", extra.slug)
            });
        assert_eq!(
            extra.super_state, required.super_state,
            "`{}` was added under the wrong superstate",
            extra.slug
        );
        assert!(
            extra.role.is_none() && extra.description.is_none(),
            "a repaired state carries no role and no description, so a migrated \
             project and a `doctor --fix`ed one cannot disagree: {extra:?}"
        );
    }
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
        // Compared as JSON rather than by `PartialEq`: since SH-67 an exported
        // event is a `Known`/`Unknown` union whose whole contract is its wire
        // form, so the wire form is what a golden comparison should assert.
        let json = |event: &storyhook::service::transfer::ExportedEvent| {
            serde_json::to_value(event).expect("an exported event serializes")
        };
        let mut theirs = golden.events.iter().map(json).peekable();
        for event in ours.events.iter().map(json) {
            if theirs.peek() == Some(&event) {
                theirs.next();
                continue;
            }
            let named = report.repairs.iter().any(|repair| {
                repair.story == ours.id
                    && serde_json::to_value(
                        &storyhook::domain::StoryEvent::StoryRelationshipAdded {
                            at: repair.at.clone(),
                            other_id: repair.other.clone(),
                            relation: repair.relation.clone(),
                        },
                    )
                    .ok()
                    .as_ref()
                        == Some(&event)
                    || repair.story == ours.id
                        && serde_json::to_value(
                            &storyhook::domain::StoryEvent::StoryRelationshipRemoved {
                                at: repair.at.clone(),
                                other_id: repair.other.clone(),
                                relation: repair.relation.clone(),
                            },
                        )
                        .ok()
                        .as_ref()
                            == Some(&event)
            }) || report.metadata_repairs.iter().any(|repair| {
                repair.story == ours.id
                    && serde_json::to_value(repair.event(MIGRATION_AT))
                        .ok()
                        .as_ref()
                        == Some(&event)
            }) || (report.computed_epics.contains(&ours.id)
                && serde_json::to_value(storyhook::domain::StoryEvent::StoryStateCleared {
                    at: MIGRATION_AT.to_string(),
                })
                .ok()
                .as_ref()
                    == Some(&event));
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
        report.repairs.len() + report.metadata_repairs.len() + report.computed_epics.len(),
        "every repair the report names must actually appear in the document, and nothing else"
    );
    assert_eq!(report.repairs.len(), 10);
}
