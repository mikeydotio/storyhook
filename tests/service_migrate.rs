//! `story migrate` — what it imports, what it repairs, and what it refuses.
//!
//! The refusals get as much room as the successes on purpose. A migration runs
//! once per repository and its output is kept forever, so the interesting
//! question is not "does it work on a good tree" but "what does it do with a
//! tree that has been accumulating oddities for a year".

mod legacy_support;

use std::collections::BTreeMap;

use legacy_support::{
    custom_config_tree, migrate, new_store, plan, real_tree, store_snapshots, tree_contents,
};
use storyhook::domain::{StoryEvent, SuperState};
use storyhook::legacy;
use storyhook::service::migrate::{MigrationPlan, RepairKind};
use storyhook::store::{ReadOps, Store as _, WriteOps};

/// Builds a plan and returns the refusal message, failing if it succeeded.
fn refusal(root: &std::path::Path) -> String {
    let project = legacy::read_project(root).expect("the tree must still be readable");
    let error = MigrationPlan::build(project).expect_err("this tree must not be migratable");
    assert_eq!(
        error.exit_code(),
        5,
        "an unrepresentable tree is an integrity failure"
    );
    error.to_string()
}

/// Appends raw JSONL to one story's log.
fn append_raw(root: &std::path::Path, id: &str, line: &str) {
    let path = root.join(format!(".storyhook/open/stories/{id}.jsonl"));
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(line);
    text.push('\n');
    std::fs::write(&path, text).unwrap();
}

#[test]
fn the_real_tree_migrates_with_every_count_the_baseline_recorded() {
    let (_tree, root) = real_tree();
    let (_dir, store, report) = migrate(&root);

    assert_eq!(report.stories, 61);
    assert_eq!(report.archived, 44);
    assert_eq!(report.deleted, 1);
    assert_eq!(report.prefix, "SH");
    assert_eq!(report.states, 5);
    assert_eq!(report.types, 5);
    assert_eq!(report.members, 0);
    assert_eq!(
        report.next_story_no, 62,
        "the counter comes from `next-id`, not from a story count"
    );
    assert_eq!(
        report.events,
        486 + report.repairs.len(),
        "every original event, plus exactly one per repair — a migration adds, it does not drop"
    );

    let snapshots = store_snapshots(&store);
    assert_eq!(snapshots.len(), 61);
    assert_eq!(
        snapshots
            .values()
            .filter(|s| s.superstate == SuperState::Closed)
            .count(),
        44
    );
    assert_eq!(snapshots.values().filter(|s| s.deleted).count(), 1);

    let mut per_state: BTreeMap<&str, usize> = BTreeMap::new();
    for snapshot in snapshots.values() {
        *per_state.entry(snapshot.state.as_str()).or_default() += 1;
    }
    assert_eq!(
        per_state,
        BTreeMap::from([("done", 43), ("todo", 18)]),
        "the frozen tree's stories land where they were"
    );
}

#[test]
fn the_real_trees_fifteen_sh_60_violations_are_all_accounted_for() {
    let (_tree, root) = real_tree();
    let (_dir, _store, report) = migrate(&root);

    let completed: Vec<_> = report
        .repairs
        .iter()
        .filter(|r| r.kind == RepairKind::CompletedInverse)
        .collect();
    let retracted: Vec<_> = report
        .repairs
        .iter()
        .filter(|r| r.kind != RepairKind::CompletedInverse)
        .collect();

    // Ten one-sided claims and five stories with two parents — the fifteen
    // `story doctor` violations this repository has carried for months. Five of
    // the ten one-sided claims are completed; the other five are the unilateral
    // parent claims, and retracting them is what settles all five conflicts.
    assert_eq!(completed.len(), 5, "{:#?}", report.repairs);
    assert_eq!(retracted.len(), 5, "{:#?}", report.repairs);

    for repair in &report.repairs {
        assert!(
            repair.at.as_str() < "2026-05",
            "a repair carries the instant of the claim it settles, never the migration's: {repair}"
        );
    }
    for repair in retracted {
        let RepairKind::RetractedUnilateralParent { child, winner } = &repair.kind else {
            unreachable!()
        };
        assert_ne!(winner, &repair.story, "the winner is the surviving parent");
        assert!(
            child == &repair.other || child == &repair.story,
            "a retraction names the child it was about: {repair:?}"
        );
    }
}

#[test]
fn a_migrated_project_has_no_integrity_issues_at_all() {
    let (_tree, root) = real_tree();
    let (_dir, store, _report) = migrate(&root);

    let issues = store
        .read(|tx| {
            let project = tx.projects()?.first().expect("one project").id;
            let stories =
                storyhook::service::QueryService::new(tx, project, "2026-01-01T00:00:00Z")
                    .story_map()?;
            Ok(storyhook::domain::compute_integrity_issues(&stories))
        })
        .expect("reading");
    assert!(
        issues.is_empty(),
        "the whole point of repairing on import is that the imported project is clean; \
         `story doctor` still finds: {issues:#?}"
    );
}

#[test]
fn a_one_sided_relation_is_completed_at_the_instant_it_was_claimed() {
    let (_tree, root) = custom_config_tree();
    // ADA-2 already claims `child-of ADA-1`; give ADA-1 a second, one-sided
    // edge to a story that does not reciprocate.
    append_raw(
        &root,
        "ADA-1",
        r#"{"kind":"StoryRelationshipAdded","at":"2026-02-01T00:00:00Z","other_id":"ADA-2","relation":"blocks"}"#,
    );

    let (_dir, store, report) = migrate(&root);
    assert_eq!(report.repairs.len(), 1, "{:#?}", report.repairs);
    let repair = &report.repairs[0];
    assert_eq!(repair.story, "ADA-2");
    assert_eq!(repair.relation, "blocked-by");
    assert_eq!(repair.other, "ADA-1");
    assert_eq!(
        repair.at, "2026-02-01T00:00:00Z",
        "the repair carries the claiming event's own instant"
    );

    let snapshots = store_snapshots(&store);
    assert!(
        snapshots["ADA-2"]
            .relationships
            .iter()
            .any(|edge| edge.relation == "blocked-by" && edge.other_id == "ADA-1"),
        "both ends must claim the edge after a repair"
    );
    assert_eq!(
        snapshots["ADA-2"].updated_at, "2026-02-01T00:00:00Z",
        "the edge really did appear then, and it is now the story's most recent activity"
    );
}

#[test]
fn a_repair_older_than_a_storys_last_event_does_not_move_its_updated_at() {
    // The other half of "timestamp order, not append". ADA-1's own history runs
    // to 00:09; give ADA-2 a one-sided claim stamped *earlier* than that, so
    // the repair on ADA-1 has to land in the middle of its log.
    let (_tree, root) = custom_config_tree();
    append_raw(
        &root,
        "ADA-2",
        r#"{"kind":"StoryRelationshipAdded","at":"2026-01-03T00:02:30Z","other_id":"ADA-1","relation":"blocks"}"#,
    );

    let (_dir, store, report) = migrate(&root);
    assert_eq!(report.repairs.len(), 1, "{:#?}", report.repairs);
    assert_eq!(report.repairs[0].story, "ADA-1");

    let snapshots = store_snapshots(&store);
    assert_eq!(
        snapshots["ADA-1"].updated_at, "2026-01-03T00:09:00Z",
        "appending the repair instead would have rewound this story's last-activity time by \
         six minutes, and every `story list --stale` answer with it"
    );

    let events = store
        .read(|tx| {
            let project = tx.projects()?.first().unwrap().id;
            tx.events_for(
                project,
                storyhook::store::StoryNo::parse_id("ADA", "ADA-1").unwrap(),
            )
        })
        .expect("reading");
    let stamps: Vec<&str> = events.iter().map(|e| e.at.as_str()).collect();
    let mut sorted = stamps.clone();
    sorted.sort_unstable();
    assert_eq!(
        stamps, sorted,
        "a repaired log stays in timestamp order; an out-of-order `at` makes every reader that \
         trusts the sequence wrong"
    );
}

#[test]
fn a_unilateral_parent_claim_loses_to_one_both_ends_record() {
    let (_tree, root) = custom_config_tree();
    // ADA-1 ↔ ADA-2 is mutual. ADA-3 unilaterally claims ADA-2 as well.
    append_raw(
        &root,
        "ADA-2",
        r#"{"kind":"StoryCreated","at":"2026-02-01T00:00:00Z","title":"never read","state":"todo"}"#,
    );
    std::fs::write(
        root.join(".storyhook/open/stories/ADA-5.jsonl"),
        format!(
            "{}\n{}\n",
            r#"{"kind":"StoryCreated","at":"2026-02-01T00:00:00Z","title":"Rival epic","state":"todo"}"#,
            r#"{"kind":"StoryRelationshipAdded","at":"2026-02-01T00:01:00Z","other_id":"ADA-2","relation":"parent-of"}"#
        ),
    )
    .unwrap();

    let (_dir, store, report) = migrate(&root);
    let retraction = report
        .repairs
        .iter()
        .find(|r| r.kind != RepairKind::CompletedInverse)
        .unwrap_or_else(|| panic!("expected a retraction, got {:#?}", report.repairs));
    assert_eq!(retraction.story, "ADA-5", "the unilateral claimant loses");
    assert_eq!(retraction.relation, "parent-of");
    assert_eq!(retraction.other, "ADA-2");

    let snapshots = store_snapshots(&store);
    assert!(
        snapshots["ADA-2"]
            .relationships
            .iter()
            .any(|edge| edge.relation == "child-of" && edge.other_id == "ADA-1"),
        "the parentage both ends recorded survives"
    );
    assert!(
        !snapshots["ADA-5"]
            .relationships
            .iter()
            .any(|edge| edge.other_id == "ADA-2"),
        "the unilateral claim is gone from the read model"
    );
    // The original assertion is still in the log; only the fold changed.
    let events = store
        .read(|tx| {
            let project = tx.projects()?.first().unwrap().id;
            let no = storyhook::store::StoryNo::parse_id("ADA", "ADA-5").unwrap();
            tx.events_for(project, no)
        })
        .expect("reading events");
    assert!(
        events
            .iter()
            .any(|e| e.kind == "StoryRelationshipAdded" && e.at == "2026-02-01T00:01:00Z"),
        "the claim itself must be imported verbatim — a retraction annuls it, it does not \
         rewrite history"
    );
    assert!(
        events.iter().any(|e| e.kind == "StoryRelationshipRemoved"),
        "and the retraction sits beside it, auditable"
    );
}

#[test]
fn two_parents_that_both_agree_are_refused_because_no_rule_can_rank_them() {
    let (_tree, root) = custom_config_tree();
    std::fs::write(
        root.join(".storyhook/open/stories/ADA-5.jsonl"),
        format!(
            "{}\n{}\n",
            r#"{"kind":"StoryCreated","at":"2026-02-01T00:00:00Z","title":"Rival epic","state":"todo"}"#,
            r#"{"kind":"StoryRelationshipAdded","at":"2026-02-01T00:01:00Z","other_id":"ADA-2","relation":"parent-of"}"#
        ),
    )
    .unwrap();
    append_raw(
        &root,
        "ADA-2",
        r#"{"kind":"StoryRelationshipAdded","at":"2026-02-01T00:01:00Z","other_id":"ADA-5","relation":"child-of"}"#,
    );

    let message = refusal(&root);
    assert!(message.contains("ADA-2"), "{message}");
    assert!(
        message.contains("both histories agree"),
        "the refusal must say why the rule cannot decide: {message}"
    );
    assert!(message.contains("storyhook will not choose"), "{message}");
}

#[test]
fn parents_that_all_disagree_are_refused_too() {
    let (_tree, root) = custom_config_tree();
    // ADA-2's only mutual parent is ADA-1; take that away and leave two
    // unilateral claims, so there is nothing to prefer.
    std::fs::write(
        root.join(".storyhook/open/stories/ADA-2.jsonl"),
        format!(
            "{}\n",
            r#"{"kind":"StoryCreated","at":"2026-01-04T00:00:00Z","title":"Punch the cards","state":"todo"}"#
        ),
    )
    .unwrap();
    std::fs::write(
        root.join(".storyhook/open/stories/ADA-5.jsonl"),
        format!(
            "{}\n{}\n",
            r#"{"kind":"StoryCreated","at":"2026-02-01T00:00:00Z","title":"Rival epic","state":"todo"}"#,
            r#"{"kind":"StoryRelationshipAdded","at":"2026-02-01T00:01:00Z","other_id":"ADA-2","relation":"parent-of"}"#
        ),
    )
    .unwrap();

    let message = refusal(&root);
    assert!(message.contains("no stronger claim to keep"), "{message}");
}

#[test]
fn a_relation_to_a_story_that_is_not_there_is_refused() {
    let (_tree, root) = custom_config_tree();
    append_raw(
        &root,
        "ADA-1",
        r#"{"kind":"StoryRelationshipAdded","at":"2026-02-01T00:00:00Z","other_id":"ADA-99","relation":"blocks"}"#,
    );
    let message = refusal(&root);
    assert!(message.contains("ADA-99"), "{message}");
    assert!(message.contains("does not exist in this tree"), "{message}");
}

#[test]
fn a_story_related_to_itself_is_refused() {
    let (_tree, root) = custom_config_tree();
    append_raw(
        &root,
        "ADA-1",
        r#"{"kind":"StoryRelationshipAdded","at":"2026-02-01T00:00:00Z","other_id":"ADA-1","relation":"blocks"}"#,
    );
    let message = refusal(&root);
    assert!(message.contains("relates to itself"), "{message}");
}

#[test]
fn a_story_that_is_both_open_and_archived_is_refused_by_name() {
    let (_tree, root) = custom_config_tree();
    // ADA-3 is archived; put an open log back beside it — the SH-20 shape a
    // half-completed archive leaves behind.
    std::fs::write(
        root.join(".storyhook/open/stories/ADA-3.jsonl"),
        format!(
            "{}\n",
            r#"{"kind":"StoryCreated","at":"2026-01-05T00:00:00Z","title":"Abandoned idea","state":"todo"}"#
        ),
    )
    .unwrap();

    let message = refusal(&root);
    assert!(message.contains("ADA-3"), "{message}");
    assert!(message.contains("exists twice"), "{message}");
    assert!(
        message.contains("the archive database") && message.contains("ADA-3.jsonl"),
        "the refusal must say where *each* copy lives, or it names the same file twice: \
         {message}"
    );
}

#[test]
fn a_story_sitting_in_an_undefined_state_is_refused_and_the_catalog_is_named() {
    let (_tree, root) = custom_config_tree();
    append_raw(
        &root,
        "ADA-2",
        r#"{"kind":"StoryStateChanged","at":"2026-02-01T00:00:00Z","state":"limbo"}"#,
    );
    let message = refusal(&root);
    assert!(message.contains("limbo"), "{message}");
    assert!(message.contains("states.toml"), "{message}");
    assert!(
        message.contains("will not guess whether a story in it is finished"),
        "replicating `fold_story`'s refusal is the decision here, and the message has to say \
         why inventing the state would be worse: {message}"
    );
}

#[test]
fn every_refusal_in_a_broken_tree_is_reported_at_once() {
    let (_tree, root) = custom_config_tree();
    append_raw(
        &root,
        "ADA-1",
        r#"{"kind":"StoryRelationshipAdded","at":"2026-02-01T00:00:00Z","other_id":"ADA-98","relation":"blocks"}"#,
    );
    append_raw(
        &root,
        "ADA-2",
        r#"{"kind":"StoryRelationshipAdded","at":"2026-02-01T00:00:00Z","other_id":"ADA-99","relation":"blocks"}"#,
    );
    let message = refusal(&root);
    assert!(
        message.contains("ADA-98") && message.contains("ADA-99"),
        "an operator repairing a tracker by hand needs the list, not the first item on it: \
         {message}"
    );
    assert!(message.contains("2 things"), "{message}");
}

#[test]
fn a_refused_tree_leaves_the_store_completely_empty() {
    let (_tree, root) = custom_config_tree();
    append_raw(
        &root,
        "ADA-1",
        r#"{"kind":"StoryRelationshipAdded","at":"2026-02-01T00:00:00Z","other_id":"ADA-99","relation":"blocks"}"#,
    );
    let (_dir, store) = new_store();
    let project = legacy::read_project(&root).expect("readable");
    assert!(MigrationPlan::build(project).is_err());
    let projects = store.read(|tx| tx.projects()).expect("reading");
    assert!(
        projects.is_empty(),
        "planning happens before a single write; a refused migration must not leave a project \
         behind for the operator to clean up"
    );
}

#[test]
fn a_corrupt_story_file_stops_the_migration_before_it_starts() {
    let (_tree, root) = custom_config_tree();
    append_raw(&root, "ADA-1", "{ this is not json");
    let (_dir, store) = new_store();
    let error = legacy::read_project(&root).expect_err("a corrupt log must not be read past");
    assert!(error.to_string().contains("ADA-1.jsonl"), "{error}");
    assert!(store.read(|tx| tx.projects()).expect("reading").is_empty());
}

#[test]
fn a_second_migration_of_one_checkout_is_refused() {
    let (_tree, root) = real_tree();
    let (_dir, store, _report) = migrate(&root);

    let again = plan(&root)
        .apply(&store, &root)
        .expect_err("a checkout is migrated once");
    let message = again.to_string();
    assert!(
        message.contains(".storyhook.toml") && message.contains("has been migrated"),
        "{message}"
    );
    assert!(
        message.contains("nothing to merge into"),
        "silence about *why* invites a retry: {message}"
    );
    assert_eq!(again.exit_code(), 2);
    assert_eq!(
        store.read(|tx| tx.projects()).expect("reading").len(),
        1,
        "and above all it must not have made a second project"
    );
}

#[test]
fn a_second_migration_is_refused_even_with_the_pointer_file_deleted() {
    let (_tree, root) = real_tree();
    let (_dir, store, _report) = migrate(&root);
    std::fs::remove_file(root.join(".storyhook.toml")).expect("removing the pointer");

    let message = plan(&root)
        .apply(&store, &root)
        .expect_err("the store's own record is the second guard")
        .to_string();
    assert!(
        message.contains("already holds project") && message.contains("61 stories"),
        "{message}"
    );
    assert_eq!(store.read(|tx| tx.projects()).expect("reading").len(), 1);
}

#[test]
fn two_trees_with_the_same_prefix_stay_completely_isolated() {
    // Every repository defaults to SH, so this is the *normal* case, not an
    // edge one: the moment a second project is migrated into a shared store,
    // `SH-1` means two different stories.
    let (_a, root_a) = real_tree();
    let (_b, root_b) = real_tree();
    let (_dir, store) = new_store();

    plan(&root_a).apply(&store, &root_a).expect("first");
    plan(&root_b).apply(&store, &root_b).expect("second");

    let projects = store.read(|tx| tx.projects()).expect("reading");
    assert_eq!(projects.len(), 2, "two checkouts, two projects");
    assert_ne!(projects[0].uuid, projects[1].uuid);
    assert_ne!(projects[0].slug, projects[1].slug);
    assert_eq!(projects[0].prefix, projects[1].prefix, "both are SH");

    for project in &projects {
        let count = store
            .read(|tx| tx.stories(project.id, &storyhook::store::StoryQuery::all()))
            .expect("reading")
            .len();
        assert_eq!(count, 61, "neither project may see the other's stories");
    }
}

#[test]
fn timestamps_and_event_order_survive_exactly() {
    let (_tree, root) = real_tree();
    let source = legacy::read_project(&root).expect("reading");
    let (_dir, store, _report) = migrate(&root);

    let prefix = "SH";
    for story in &source.stories {
        let stored = store
            .read(|tx| {
                let project = tx.projects()?.first().unwrap().id;
                tx.events_for(
                    project,
                    storyhook::store::StoryNo::parse_id(prefix, &story.id).unwrap(),
                )
            })
            .expect("reading events");
        // Repairs are inserted, so the stored log is a supersequence: every
        // original event must appear, in order, with its own timestamp.
        let mut stored = stored.iter();
        for original in &story.events {
            let found = stored
                .find(|candidate| candidate.kind == original.kind && candidate.at == original.at)
                .unwrap_or_else(|| {
                    panic!(
                        "{}: event `{}` at {} is missing or out of order",
                        story.id, original.kind, original.at
                    )
                });
            assert_eq!(
                found.at, original.at,
                "{}: a timestamp was rewritten",
                story.id
            );
        }
    }
}

#[test]
fn archived_stories_land_archived_and_the_deleted_one_stays_deleted() {
    let (_tree, root) = custom_config_tree();
    let (_dir, store, _report) = migrate(&root);

    let rows = store
        .read(|tx| {
            let project = tx.projects()?.first().unwrap().id;
            tx.stories(project, &storyhook::store::StoryQuery::all())
        })
        .expect("reading");
    let by_id: BTreeMap<_, _> = rows
        .into_iter()
        .map(|row| (row.snapshot.id.clone(), row))
        .collect();

    assert!(by_id["ADA-3"].archived, "a closed story arrives archived");
    assert_eq!(by_id["ADA-3"].snapshot.state, "wont-fix");
    assert_eq!(by_id["ADA-3"].snapshot.superstate, SuperState::Closed);
    assert!(by_id["ADA-4"].snapshot.deleted, "a soft delete survives");
    assert_eq!(
        by_id["ADA-4"].snapshot.deleted_reason.as_deref(),
        Some("filed twice"),
        "and so does its reason"
    );
    assert!(!by_id["ADA-1"].archived);
}

#[test]
fn the_next_story_number_comes_from_the_counter_not_the_story_count() {
    let (_tree, root) = custom_config_tree();
    // Two burned numbers: the counter says 9 while the highest story is 4.
    std::fs::write(root.join(".storyhook/next-id"), "9\n").unwrap();
    let (_dir, store, report) = migrate(&root);
    assert_eq!(
        report.next_story_no, 9,
        "a counter ahead of the highest id records numbers that were minted and lost; a `next` \
         derived from a count would hand one of them out twice"
    );

    let minted = store
        .write(|tx| {
            let project = tx.projects()?.first().unwrap().id;
            tx.allocate_story_no(project)
        })
        .expect("allocating");
    assert_eq!(minted.get(), 9);
}

#[test]
fn a_counter_behind_the_stories_still_cannot_collide() {
    let (_tree, root) = custom_config_tree();
    std::fs::write(root.join(".storyhook/next-id"), "1\n").unwrap();
    let (_dir, _store, report) = migrate(&root);
    assert_eq!(
        report.next_story_no, 5,
        "a truncated counter must not make the next `story new` overwrite ADA-4"
    );
}

#[test]
fn an_unknown_event_kind_is_imported_verbatim_and_named_in_the_report() {
    let (_tree, root) = custom_config_tree();
    let raw = r#"{"kind":"StoryPinned","at":"2026-02-01T00:00:00Z","by":"ada","note":"keep"}"#;
    append_raw(&root, "ADA-2", raw);

    let (_dir, store, report) = migrate(&root);
    assert_eq!(report.unknown_events.len(), 1);
    assert_eq!(report.unknown_events[0].story, "ADA-2");
    assert_eq!(report.unknown_events[0].kind, "StoryPinned");
    assert!(
        report.render().contains("StoryPinned"),
        "an operator must be told, not merely not-lied-to: {}",
        report.render()
    );

    let events = store
        .read(|tx| {
            let project = tx.projects()?.first().unwrap().id;
            tx.events_for(
                project,
                storyhook::store::StoryNo::parse_id("ADA", "ADA-2").unwrap(),
            )
        })
        .expect("reading");
    let pinned = events
        .iter()
        .find(|event| event.kind == "StoryPinned")
        .expect("the unknown event must be stored");
    assert!(
        matches!(
            &pinned.payload,
            storyhook::store::StoredPayload::Unknown { json, .. } if json == raw
        ),
        "byte for byte, or a storyhook upgrade loses data written by a newer one (SH-54)"
    );
}

#[test]
fn a_projects_settings_travel_with_it() {
    let (_tree, root) = custom_config_tree();
    let project_file = root.join(".storyhook/project.toml");
    let mut toml = std::fs::read_to_string(&project_file).unwrap();
    toml.push_str("\n[sync]\nauto_transition = false\n\n[doctor]\nstale_threshold = \"21d\"\n");
    std::fs::write(&project_file, toml).unwrap();

    let (_dir, store, report) = migrate(&root);
    let settings = store
        .read(|tx| {
            let project = tx.projects()?.first().unwrap().id;
            tx.settings(project)
        })
        .expect("reading");
    assert_eq!(settings.sync_auto_transition, Some(false));
    assert_eq!(settings.doctor_stale_threshold.as_deref(), Some("21d"));
    assert!(
        report.render().contains("sync.auto_transition = false"),
        "settings ride beside the export envelope rather than inside it, so the report is where \
         an operator learns a rollback will not carry them: {}",
        report.render()
    );
}

#[test]
fn the_project_keeps_its_original_birthday() {
    let (_tree, root) = real_tree();
    let (_dir, store, _report) = migrate(&root);
    let created = store
        .read(|tx| Ok(tx.projects()?.first().unwrap().created_at.clone()))
        .expect("reading");
    assert_eq!(
        created, "2026-03-24T03:27:08Z",
        "a migration moves a project; it does not create one, and `created_at` is the \
         difference between a five-month-old tracker and a brand-new one"
    );
}

#[test]
fn the_custom_config_tree_brings_its_whole_configuration_surface() {
    let (_tree, root) = custom_config_tree();
    let (_dir, store, report) = migrate(&root);
    assert_eq!(report.prefix, "ADA");

    let (states, types, members) = store
        .read(|tx| {
            let project = tx.projects()?.first().unwrap().id;
            Ok((
                tx.states(project)?,
                tx.types(project)?,
                tx.members(project)?,
            ))
        })
        .expect("reading");

    assert_eq!(
        states.iter().map(|s| s.slug.as_str()).collect::<Vec<_>>(),
        ["todo", "in-progress", "review", "done", "wont-fix"],
        "configured order is user-visible — it drives the board columns"
    );
    assert_eq!(states[2].role.as_deref(), Some("active"));
    assert_eq!(
        states[4].description.as_deref(),
        Some("Closed without doing it"),
        "a state's description is the field SH-49 destroyed; it must survive a migration"
    );
    assert!(types.iter().any(|t| t.slug == "spike"));
    assert_eq!(members.len(), 2);
    assert_eq!(members[0].display_name, "Ada Lovelace");
    assert_eq!(members[0].github.as_deref(), Some("adalovelace"));

    let snapshots = store_snapshots(&store);
    assert_eq!(snapshots["ADA-1"].story_type.as_deref(), Some("spike"));
    assert_eq!(snapshots["ADA-1"].assignee.as_deref(), Some("ada"));
    assert_eq!(
        snapshots["ADA-1"].awaiting.as_deref(),
        Some("a punch-card supplier")
    );
    assert_eq!(snapshots["ADA-1"].labels, ["analytical", "phase:1"]);
    assert_eq!(snapshots["ADA-1"].comments.len(), 1);
    assert_eq!(snapshots["ADA-1"].state, "review");
}

#[test]
fn a_dry_run_writes_nothing_anywhere() {
    let (_tree, root) = real_tree();
    let before = tree_contents(&root);
    let (_dir, store) = new_store();

    let report = plan(&root).report(true);
    assert!(report.dry_run);
    assert_eq!(report.stories, 61);
    assert!(report.render().contains("nothing was written"));

    assert!(
        store.read(|tx| tx.projects()).expect("reading").is_empty(),
        "a dry run must not reach the store"
    );
    legacy_support::assert_tree_unchanged(&root, &before, "a dry run");
    assert!(
        !root.join(".storyhook.toml").exists(),
        "nor leave a pointer file behind"
    );
}

#[test]
fn import_does_not_mutate_the_legacy_tree() {
    // The headline invariant. The legacy directory is the operator's rollback:
    // if a migration modifies it, there is nothing to go back to.
    let (_tree, root) = real_tree();
    let before = tree_contents(&root);
    let (_dir, store, report) = migrate(&root);
    assert_eq!(report.stories, 61);

    let mut after = tree_contents(&root);
    // The pointer file is written *beside* `.storyhook/`, not inside it, and it
    // is the one thing a migration does add to the checkout.
    let pointer = after
        .remove(std::path::Path::new(".storyhook.toml"))
        .expect("a successful migration writes the pointer file");
    assert!(String::from_utf8_lossy(&pointer).contains("uuid"));
    assert_eq!(
        after.keys().collect::<Vec<_>>(),
        before.keys().collect::<Vec<_>>(),
        "a migration adds the pointer and nothing else"
    );
    for (path, bytes) in &before {
        assert_eq!(
            &after[path],
            bytes,
            "`{}` was rewritten by a migration",
            path.display()
        );
    }
    assert!(store.read(|tx| tx.projects()).expect("reading").len() == 1);
}

#[test]
fn the_repairs_are_events_the_domain_understands() {
    // Guards against a repair that is stored but cannot be folded — the exact
    // shape a hand-built payload produces.
    let (_tree, root) = real_tree();
    let (_dir, store, report) = migrate(&root);
    assert!(!report.repairs.is_empty());

    for repair in &report.repairs {
        let events = store
            .read(|tx| {
                let project = tx.projects()?.first().unwrap().id;
                tx.events_for(
                    project,
                    storyhook::store::StoryNo::parse_id("SH", &repair.story).unwrap(),
                )
            })
            .expect("reading");
        let decoded: Vec<&StoryEvent> = events.iter().filter_map(|e| e.known()).collect();
        assert_eq!(
            decoded.len(),
            events.len(),
            "a repair must be an event this binary can decode, or the read model it produces \
             cannot be rebuilt"
        );
    }
}

// ---------------------------------------------------------------------------
// The command itself
// ---------------------------------------------------------------------------

/// Runs `git <args>` in `cwd` under `env`, asserting success.
fn git(env: &storyhook_test_support::TestEnv, cwd: &std::path::Path, args: &[&str]) {
    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(cwd);
    env.apply(&mut cmd);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    let out = cmd.args(args).output().expect("running git");
    assert!(
        out.status.success(),
        "`git {}` in {} failed: {}",
        args.join(" "),
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn migrating_from_a_linked_worktree_is_refused_and_the_main_checkout_is_named() {
    // The single most damaging thing this command could do. A worktree's
    // `.storyhook` is a diverged copy; migrating it mints a project holding
    // that copy's version of the truth, and the main checkout then mints a
    // second one with the same prefix and overlapping story numbers — the exact
    // corruption the store exists to end.
    let env = storyhook_test_support::TestEnv::shared();
    let project = env.project().git().worktree("a").legacy().build();
    project.new_story("Created before the checkouts diverged");
    git(env, project.path(), &["add", ".storyhook"]);
    git(env, project.path(), &["commit", "-qm", "track the tracker"]);
    git(
        env,
        project.worktree_path("a"),
        &["merge", "-q", "--ff-only", "main"],
    );

    let worktree = project.worktree_path("a");
    assert!(
        worktree.join(".storyhook/next-id").exists(),
        "fixture: the worktree must carry its own copy of .storyhook/"
    );

    let output = env
        .story(worktree)
        .arg("migrate")
        .arg("--dry-run")
        .output()
        .expect("running story migrate");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stdout: {}\nstderr: {stderr}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stderr.contains("linked git worktree"),
        "the refusal must say what it noticed: {stderr}"
    );
    assert!(
        stderr.contains(&project.path().display().to_string()),
        "and name the main checkout to run it from instead: {stderr}"
    );
    assert!(
        !worktree.join(".storyhook.toml").exists(),
        "a refused migration leaves no pointer file"
    );

    // …and the same command in the main checkout is fine.
    env.story(project.path())
        .args(["migrate", "--dry-run"])
        .assert()
        .success();
}

#[test]
fn the_command_finds_the_project_by_walking_up_from_where_it_is_run() {
    let env = storyhook_test_support::TestEnv::shared();
    let project = env.project().legacy().build();
    project.new_story("A story to migrate");
    let deep = project.path().join("src/inner");
    std::fs::create_dir_all(&deep).unwrap();

    let output = env
        .story(&deep)
        .args(["migrate", "--dry-run"])
        .output()
        .expect("running");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("would import 1 stories"), "{stdout}");
    assert!(stdout.contains("nothing was written"), "{stdout}");
}

#[test]
fn the_command_migrates_then_refuses_a_second_run() {
    let env = storyhook_test_support::TestEnv::shared();
    let project = env.project().legacy().build();
    project.new_story("A story to migrate");

    env.story(project.path())
        .arg("migrate")
        .assert()
        .success()
        .stdout(predicates::str::contains("imported 1 stories"));
    assert!(project.path().join(".storyhook.toml").exists());

    env.story(project.path())
        .arg("migrate")
        .assert()
        .code(2)
        .stderr(predicates::str::contains("has been migrated"));

    // The migrated project is readable through the ordinary CLI, which is the
    // store now — the point of having migrated it.
    env.story(project.path())
        .arg("list")
        .assert()
        .success()
        .stdout(predicates::str::contains("A story to migrate"));
}

#[test]
fn migrating_a_directory_with_no_legacy_tree_is_not_found_rather_than_a_crash() {
    let env = storyhook_test_support::TestEnv::shared();
    let dir = storyhook_test_support::scratch_dir();
    env.story(dir.path())
        .arg("migrate")
        .assert()
        .code(3)
        .stderr(predicates::str::contains("story project init"));
}

#[test]
fn an_explicit_path_argument_migrates_a_project_you_are_not_standing_in() {
    let env = storyhook_test_support::TestEnv::shared();
    let project = env.project().legacy().build();
    project.new_story("Elsewhere");
    let elsewhere = storyhook_test_support::scratch_dir();

    env.story(elsewhere.path())
        .arg("migrate")
        .arg(project.path())
        .assert()
        .success()
        .stdout(predicates::str::contains("imported 1 stories"));
    assert!(
        project.path().join(".storyhook.toml").exists(),
        "the pointer belongs to the migrated checkout, not to the working directory"
    );
    assert!(!elsewhere.path().join(".storyhook.toml").exists());
}

#[test]
fn an_unknown_flag_is_a_usage_error_rather_than_a_path() {
    // The premise is unchanged and was right before SH-62 existed: a mistyped
    // flag must never be read as the path to migrate. What changed is who
    // answers. `parse_migrate`'s own `!value.starts_with('-')` guard used to
    // produce a bare usage line; the flag gate now answers first and names the
    // token, which is strictly more useful for a typo this close to the real
    // flag. Same exit code, same class of error.
    let env = storyhook_test_support::TestEnv::shared();
    let project = env.project().legacy().build();
    env.story(project.path())
        .args(["migrate", "--dry-runn"])
        .assert()
        .code(2)
        .stderr(predicates::str::contains("unknown flag `--dry-runn`"))
        .stderr(predicates::str::contains("--dry-run"));
}

/// A migrated project's `[git]` comments become link records, so the first
/// `commit-sync` afterwards does not re-link the repository's whole log.
///
/// Every unmigrated `.storyhook` tree is full of `[git] <short>: <subject>`
/// comments — that is how `commit-sync` recorded a link before event kind #18
/// existed. They arrive through `append_raw_events`, which is the replay path,
/// and the store projects them into `story_commit_links` there precisely so
/// that this is true.
#[test]
fn a_migrated_projects_git_comments_arrive_as_link_records() {
    use storyhook::store::{ReadOps, StoryNo};

    let env = storyhook_test_support::TestEnv::shared();
    let project = env.project().legacy().build();
    let id = project.new_story("Linked before the migration");

    // A link record in its pre-#18 shape, appended to the story's log exactly
    // as the old `commit-sync` appended it.
    let log = project
        .path()
        .join(format!(".storyhook/open/stories/{id}.jsonl"));
    let mut text = std::fs::read_to_string(&log).expect("reading the story log");
    text.push_str(
        "{\"kind\":\"StoryCommentAdded\",\"at\":\"2026-01-02T00:00:00Z\",\
         \"text\":\"[git] abc1234: feat: the work\"}\n",
    );
    std::fs::write(&log, text).expect("seeding a legacy link comment");

    env.story(project.path()).arg("migrate").assert().success();

    let store = project.open_store();
    let project_id = project.project_id(&store);
    assert!(
        store
            .read(|tx| tx.commit_linked(project_id, StoryNo::new(1), "abc1234"))
            .expect("reading the link table"),
        "the migration must carry the story's existing links across, or the next \
         `commit-sync` re-links every commit in its window"
    );
}
