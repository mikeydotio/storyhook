//! The rebuild-and-diff oracle, and the fault-injection points it shares the
//! wave with.
//!
//! Every assertion here damages the read model *underneath* the store, using a
//! second connection. That is deliberate and it is the measure of the schema:
//! none of these states can be produced through the store's own API.

mod store_support;

use store_support::{append_and_fold, create_story, new_store, raw, seed_project};
use storyhook::domain::{Priority, StoryEvent};
use storyhook::store::fault::{FaultAction, arm};
use storyhook::store::{
    EventSeq, ExpectedSeq, FaultPoint, ReadOps, Store, StoreError, StoryNo, StoryQuery, WriteOps,
    diff_read_model, rebuild_read_model, repair_read_model,
};

// ---------------------------------------------------------------------------
// Schema constraints the store's own API cannot violate
//
// These are the defence-in-depth half of the schema: rules the current write
// path already respects, asserted against a *raw* connection so that a future
// writer — a migration, an importer, a hand-run statement — cannot quietly
// break them. Without these cases the constraints would be untested, because
// nothing reachable through `WriteOps` can trip them.
// ---------------------------------------------------------------------------

fn rejects(store: &storyhook::store::SqliteStore, sql: &str, expected: &str) {
    let error = raw(store)
        .execute(sql, [])
        .expect_err(&format!("the schema should have refused: {sql}"));
    assert!(
        error.to_string().contains(expected),
        "expected `{expected}` in: {error}"
    );
}

#[test]
fn the_archive_flag_cannot_be_set_without_a_close_timestamp() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    create_story(&store, project, "First", "2026-01-01T00:00:00Z");

    rejects(
        &store,
        "UPDATE stories SET archived = 1",
        "archived = (closed_at IS NOT NULL)",
    );
    rejects(
        &store,
        "UPDATE stories SET closed_at = '2026-01-01T00:01:00Z'",
        "archived = (closed_at IS NOT NULL)",
    );
}

#[test]
fn a_priority_slug_and_its_sort_rank_cannot_disagree() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    create_story(&store, project, "First", "2026-01-01T00:00:00Z");

    rejects(
        &store,
        "UPDATE stories SET priority_rank = 0",
        "priority_rank",
    );
    // And a priority storyhook has never heard of is refused by its own
    // constraint. It needs one: `priority_rank = CASE priority ... END` yields
    // NULL for an unknown slug, and a constraint that evaluates to NULL passes.
    rejects(
        &store,
        "UPDATE stories SET priority = 'urgent'",
        "priority IN",
    );
}

#[test]
fn the_event_log_cannot_be_rewritten_or_erased() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    create_story(&store, project, "First", "2026-01-01T00:00:00Z");

    rejects(
        &store,
        "UPDATE events SET payload = '{}'",
        "events are append-only",
    );
    rejects(&store, "DELETE FROM events", "events are append-only");
}

/// Migration 3 narrows the append-only guard so that `delete_project` can tear
/// an event log down. These are the cases that stop that narrowing from
/// becoming a hole: the guard has to keep firing for every caller that is not
/// mid-teardown, and the one that is has to be recognisable by something no
/// ordinary write can fake.
#[test]
fn the_append_only_guard_only_stands_down_for_a_project_that_is_already_gone() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    create_story(&store, project, "First", "2026-01-01T00:00:00Z");

    // Narrowed, not lifted: the project still exists, so this is still refused.
    rejects(
        &store,
        "DELETE FROM events WHERE project_id = 1",
        "events are append-only",
    );

    // And the teardown order is the whole licence. Deleting the project first
    // is what makes the trigger abstain; nothing else does.
    raw(&store)
        .execute_batch(
            "BEGIN IMMEDIATE; PRAGMA defer_foreign_keys = ON; \
             DELETE FROM projects WHERE id = 1; \
             DELETE FROM events WHERE project_id = 1; COMMIT;",
        )
        .expect("a project teardown may clear its own event log");

    let left: i64 = raw(&store)
        .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(left, 0);
}

/// The teardown deletes `projects` before `events`, which is only legal because
/// a write transaction runs under `defer_foreign_keys`. This pins the failure
/// mode if that ever stops being true: the first statement is refused and
/// nothing is written — loud, not silent.
#[test]
fn tearing_a_project_down_without_the_deferral_fails_before_it_writes_anything() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    create_story(&store, project, "First", "2026-01-01T00:00:00Z");
    let conn = raw(&store);

    let error = conn
        .execute_batch("BEGIN IMMEDIATE; DELETE FROM projects WHERE id = 1;")
        .expect_err("events still reference the project");
    assert!(
        error.to_string().contains("FOREIGN KEY constraint failed"),
        "{error}"
    );
    let _ = conn.execute_batch("ROLLBACK");

    let projects: i64 = conn
        .query_row("SELECT count(*) FROM projects", [], |row| row.get(0))
        .unwrap();
    let events: i64 = conn
        .query_row("SELECT count(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!((projects, events), (1, 1), "nothing may have been written");
    let _ = project;
}

#[test]
fn a_story_cannot_be_given_a_superstate_outside_the_two_that_exist() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    create_story(&store, project, "First", "2026-01-01T00:00:00Z");

    rejects(&store, "UPDATE stories SET superstate = 'PENDING'", "CHECK");
}

#[test]
fn a_checkout_path_cannot_be_claimed_by_two_projects_even_by_hand() {
    let (_dir, store) = new_store();
    let alpha = seed_project(&store, "alpha", "SH");
    let beta = seed_project(&store, "beta", "SH");

    let error = raw(&store)
        .execute(
            "INSERT INTO project_paths (project_id, path, kind, last_seen_at) \
             VALUES (?1, ?2, 'main', '2026-01-01T00:00:00Z')",
            rusqlite::params![beta.get(), "/checkouts/alpha"],
        )
        .expect_err("the unique index should have refused it");
    assert!(error.to_string().contains("project_paths.path"), "{error}");
    let _ = alpha;
}

// ---------------------------------------------------------------------------
// A healthy project
// ---------------------------------------------------------------------------

#[test]
fn a_project_written_through_the_store_has_no_divergence() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    let one = create_story(&store, project, "First", "2026-01-01T00:00:00Z");
    let two = create_story(&store, project, "Second", "2026-01-01T00:00:01Z");
    append_and_fold(
        &store,
        project,
        one,
        ExpectedSeq::Any,
        &[
            StoryEvent::StoryPrioritySet {
                at: "2026-01-01T00:01:00Z".into(),
                priority: Priority::Critical,
            },
            StoryEvent::StoryLabelsSet {
                at: "2026-01-01T00:02:00Z".into(),
                labels: vec!["infra".into()],
            },
            StoryEvent::StoryRelationshipAdded {
                at: "2026-01-01T00:03:00Z".into(),
                other_id: two.to_id("SH"),
                relation: "blocks".into(),
            },
        ],
    )
    .unwrap();
    append_and_fold(
        &store,
        project,
        two,
        ExpectedSeq::Any,
        &[StoryEvent::StoryRelationshipAdded {
            at: "2026-01-01T00:03:00Z".into(),
            other_id: one.to_id("SH"),
            relation: "blocked-by".into(),
        }],
    )
    .unwrap();

    let diff = diff_read_model(&store, project).unwrap();

    assert!(diff.is_clean(), "{}", diff.describe());
}

#[test]
fn a_rebuild_reports_every_story_with_the_head_it_folded_to() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    let one = create_story(&store, project, "First", "2026-01-01T00:00:00Z");
    create_story(&store, project, "Second", "2026-01-01T00:00:01Z");
    append_and_fold(
        &store,
        project,
        one,
        ExpectedSeq::Any,
        &[StoryEvent::StoryTitleSet {
            at: "2026-01-01T00:01:00Z".into(),
            title: "Renamed".into(),
        }],
    )
    .unwrap();

    let rebuilt = rebuild_read_model(&store, project).unwrap();

    assert_eq!(rebuilt.len(), 2);
    assert_eq!(rebuilt[0].story_no, StoryNo::new(1));
    assert_eq!(rebuilt[0].head_seq, EventSeq::new(2));
    assert_eq!(rebuilt[0].snapshot.as_ref().unwrap().title, "Renamed");
    assert_eq!(rebuilt[1].head_seq, EventSeq::new(1));
}

#[test]
fn an_empty_project_rebuilds_to_nothing_and_diffs_clean() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");

    assert!(rebuild_read_model(&store, project).unwrap().is_empty());
    assert!(diff_read_model(&store, project).unwrap().is_clean());
}

#[test]
fn rebuilding_an_unknown_project_says_so() {
    let (_dir, store) = new_store();
    let error = rebuild_read_model(&store, storyhook::store::ProjectId::new(404)).unwrap_err();
    assert!(
        matches!(error, StoreError::NotFound(_)),
        "expected NotFound, got: {error}"
    );
}

// ---------------------------------------------------------------------------
// Detecting damage
// ---------------------------------------------------------------------------

#[test]
fn a_column_edited_underneath_the_store_is_reported_by_field_and_by_both_values() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    create_story(&store, project, "First", "2026-01-01T00:00:00Z");

    raw(&store)
        .execute("UPDATE stories SET title = 'tampered'", [])
        .unwrap();

    let diff = diff_read_model(&store, project).unwrap();

    assert!(!diff.is_clean());
    let title = diff
        .divergences
        .iter()
        .find(|d| d.field == "title")
        .expect("the title divergence");
    assert_eq!(title.story_no, StoryNo::new(1));
    assert_eq!(title.persisted, "tampered");
    assert_eq!(title.rebuilt, "First");
    assert!(diff.describe().contains("tampered"), "{}", diff.describe());
}

#[test]
fn a_stale_head_seq_is_reported_as_its_own_divergence() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    create_story(&store, project, "First", "2026-01-01T00:00:00Z");

    raw(&store)
        .execute("UPDATE stories SET head_seq = 0", [])
        .unwrap();

    let diff = diff_read_model(&store, project).unwrap();

    let head = diff
        .divergences
        .iter()
        .find(|d| d.field == "head_seq")
        .expect("the head_seq divergence");
    assert_eq!(head.persisted, "0");
    assert_eq!(head.rebuilt, "1");
}

#[test]
fn a_story_with_events_but_no_row_is_reported_as_missing() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    create_story(&store, project, "First", "2026-01-01T00:00:00Z");

    raw(&store).execute("DELETE FROM stories", []).unwrap();

    let diff = diff_read_model(&store, project).unwrap();

    assert_eq!(diff.missing_rows, vec![StoryNo::new(1)]);
    assert!(!diff.is_clean());
    assert!(diff.describe().contains("no read-model row"));
}

#[test]
fn a_row_with_no_events_behind_it_is_reported_as_extra() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    create_story(&store, project, "First", "2026-01-01T00:00:00Z");
    // A second story row written without events — the shape a partially-applied
    // import or a hand-edited database leaves behind.
    raw(&store)
        .execute(
            "INSERT INTO stories (project_id, story_no, head_seq, title, state, superstate, \
                 priority, priority_rank, deleted, archived, created_at, updated_at, snapshot) \
             SELECT project_id, 2, 1, title, state, superstate, priority, priority_rank, deleted, \
                 archived, created_at, updated_at, snapshot FROM stories",
            [],
        )
        .unwrap();

    let diff = diff_read_model(&store, project).unwrap();

    assert_eq!(diff.extra_rows, vec![StoryNo::new(2)]);
    assert!(!diff.is_clean());
}

#[test]
fn a_relation_row_deleted_underneath_the_store_is_reported() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    let one = create_story(&store, project, "First", "2026-01-01T00:00:00Z");
    let two = create_story(&store, project, "Second", "2026-01-01T00:00:01Z");
    append_and_fold(
        &store,
        project,
        one,
        ExpectedSeq::Any,
        &[StoryEvent::StoryRelationshipAdded {
            at: "2026-01-01T00:03:00Z".into(),
            other_id: two.to_id("SH"),
            relation: "blocks".into(),
        }],
    )
    .unwrap();

    // Both directions vanish: the mirror trigger fires on the delete too, which
    // is itself worth pinning — the table cannot be left holding half an edge
    // even by a hand-written statement.
    raw(&store)
        .execute("DELETE FROM story_relations WHERE story_no = 1", [])
        .unwrap();
    let remaining: i64 = raw(&store)
        .query_row("SELECT COUNT(*) FROM story_relations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(remaining, 0, "the mirror row must go with it");

    let diff = diff_read_model(&store, project).unwrap();

    let relations = diff
        .divergences
        .iter()
        .find(|d| d.field == "relations" && d.story_no == one)
        .expect("the relations divergence");
    assert_eq!(relations.persisted, "");
    assert_eq!(relations.rebuilt, "blocks:2");
}

#[test]
fn a_label_removed_underneath_the_store_is_reported() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    let one = create_story(&store, project, "First", "2026-01-01T00:00:00Z");
    append_and_fold(
        &store,
        project,
        one,
        ExpectedSeq::Any,
        &[StoryEvent::StoryLabelsSet {
            at: "2026-01-01T00:01:00Z".into(),
            labels: vec!["a".into(), "b".into()],
        }],
    )
    .unwrap();

    raw(&store)
        .execute("DELETE FROM story_labels WHERE label = 'b'", [])
        .unwrap();

    let diff = diff_read_model(&store, project).unwrap();

    let labels = diff
        .divergences
        .iter()
        .find(|d| d.field == "labels")
        .expect("the labels divergence");
    assert_eq!(labels.persisted, "a");
    assert_eq!(labels.rebuilt, "a,b");
}

#[test]
fn a_snapshot_edited_without_its_columns_is_still_caught() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    create_story(&store, project, "First", "2026-01-01T00:00:00Z");

    // A comment lives only in the snapshot — no column carries it. Editing it
    // is invisible to every indexed field, which is exactly the kind of drift
    // the whole-snapshot comparison exists for.
    raw(&store)
        .execute(
            "UPDATE stories SET snapshot = json_set(snapshot, '$.comments', \
             json('[{\"at\":\"2026-01-01T00:00:00Z\",\"text\":\"invented\"}]'))",
            [],
        )
        .unwrap();

    let diff = diff_read_model(&store, project).unwrap();

    assert!(
        diff.divergences.iter().any(|d| d.field == "snapshot"),
        "expected a snapshot divergence, got: {}",
        diff.describe()
    );
}

#[test]
fn a_story_whose_events_do_not_fold_is_reported_not_propagated() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    create_story(&store, project, "Healthy", "2026-01-01T00:00:00Z");
    let broken = create_story(&store, project, "Broken", "2026-01-01T00:00:01Z");
    append_and_fold(
        &store,
        project,
        broken,
        ExpectedSeq::Any,
        &[StoryEvent::StoryStateChanged {
            at: "2026-01-01T00:01:00Z".into(),
            state: "done".into(),
        }],
    )
    .unwrap();
    // Retire the state out from under the story. Its events still name it, so
    // the fold can no longer resolve a superstate.
    store
        .write(|tx| {
            let kept: Vec<_> = store_support::default_states()
                .into_iter()
                .filter(|state| state.slug != "done")
                .collect();
            tx.put_states(project, &kept)
        })
        .unwrap();

    let diff = diff_read_model(&store, project).unwrap();

    assert_eq!(diff.fold_failures.len(), 1);
    assert_eq!(diff.fold_failures[0].0, broken);
    assert!(
        diff.fold_failures[0].1.contains("undefined state"),
        "{}",
        diff.fold_failures[0].1
    );
    // The healthy story was still checked: one unfoldable story must not stop
    // a project-wide report, which is when you most need it.
    assert!(diff.divergences.is_empty());
}

// ---------------------------------------------------------------------------
// Unknown event kinds (SH-54)
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_event_kind_is_retained_reported_and_not_a_divergence() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    let one = create_story(&store, project, "First", "2026-01-01T00:00:00Z");
    // Written directly because this binary cannot construct an event it does
    // not have a variant for — which is precisely the situation a *newer*
    // storyhook puts this one in.
    raw(&store)
        .execute(
            "INSERT INTO events (project_id, story_no, seq, global_seq, kind, at, payload) \
             SELECT ?1, ?2, 2, 99, 'StoryTeleported', '2026-01-01T00:05:00Z', \
                 '{\"kind\":\"StoryTeleported\",\"at\":\"2026-01-01T00:05:00Z\",\"to\":\"mars\"}'",
            rusqlite::params![
                store
                    .read(|tx| tx.project_by_slug("alpha"))
                    .unwrap()
                    .unwrap()
                    .id
                    .get(),
                one.get()
            ],
        )
        .unwrap();

    let events = store.read(|tx| tx.events_for(project, one)).unwrap();
    assert_eq!(events.len(), 2, "the unknown event must still be readable");
    assert_eq!(events[1].kind, "StoryTeleported");
    assert!(
        matches!(
            &events[1].payload,
            storyhook::store::StoredPayload::Unknown { json, .. } if json.contains("mars")
        ),
        "the payload must be retained verbatim"
    );

    let diff = diff_read_model(&store, project).unwrap();

    assert_eq!(diff.unknown_events.len(), 1);
    assert_eq!(diff.unknown_events[0].kind, "StoryTeleported");
    assert_eq!(diff.unknown_events[0].seq, EventSeq::new(2));
    // Reported, but not damage: retaining an event this binary does not
    // understand is correct behaviour. Only `head_seq` moved, because the
    // unknown event still counts as history.
    assert_eq!(
        diff.divergences
            .iter()
            .map(|d| d.field.as_str())
            .collect::<Vec<_>>(),
        vec!["head_seq"]
    );
}

// ---------------------------------------------------------------------------
// Repair
// ---------------------------------------------------------------------------

#[test]
fn repair_rewrites_damaged_rows_and_leaves_the_project_clean() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    let one = create_story(&store, project, "First", "2026-01-01T00:00:00Z");
    let two = create_story(&store, project, "Second", "2026-01-01T00:00:01Z");
    append_and_fold(
        &store,
        project,
        one,
        ExpectedSeq::Any,
        &[
            StoryEvent::StoryLabelsSet {
                at: "2026-01-01T00:01:00Z".into(),
                labels: vec!["keep".into()],
            },
            StoryEvent::StoryRelationshipAdded {
                at: "2026-01-01T00:02:00Z".into(),
                other_id: two.to_id("SH"),
                relation: "parent-of".into(),
            },
        ],
    )
    .unwrap();

    let mut damage = raw(&store);
    let tx = damage.transaction().unwrap();
    tx.execute("UPDATE stories SET title = 'wrong', head_seq = 0", [])
        .unwrap();
    tx.execute("DELETE FROM story_labels", []).unwrap();
    tx.execute("DELETE FROM story_relations", []).unwrap();
    tx.commit().unwrap();
    assert!(!diff_read_model(&store, project).unwrap().is_clean());

    let report = repair_read_model(&store, project).unwrap();

    assert_eq!(report.rewritten, vec![one, two]);
    assert!(report.unrepairable.is_empty());
    assert!(!report.repaired.is_clean(), "it found something to fix");
    let after = diff_read_model(&store, project).unwrap();
    assert!(after.is_clean(), "{}", after.describe());

    let row = store.read(|tx| tx.story(project, one)).unwrap().unwrap();
    assert_eq!(row.title, "First");
    assert_eq!(row.labels, vec!["keep"]);
    assert_eq!(row.head_seq, EventSeq::new(3));
    let edges = store.read(|tx| tx.relations_to(project, two)).unwrap();
    assert_eq!(edges.len(), 1, "the mirror was restored too");
}

#[test]
fn repair_on_a_healthy_project_changes_nothing_observable() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    create_story(&store, project, "First", "2026-01-01T00:00:00Z");
    let before = store
        .read(|tx| tx.stories(project, &StoryQuery::all()))
        .unwrap();

    let report = repair_read_model(&store, project).unwrap();

    assert!(report.repaired.is_clean());
    assert_eq!(
        store
            .read(|tx| tx.stories(project, &StoryQuery::all()))
            .unwrap(),
        before
    );
}

#[test]
fn repair_refuses_to_guess_at_a_story_it_cannot_fold() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    let broken = create_story(&store, project, "Broken", "2026-01-01T00:00:00Z");
    append_and_fold(
        &store,
        project,
        broken,
        ExpectedSeq::Any,
        &[StoryEvent::StoryStateChanged {
            at: "2026-01-01T00:01:00Z".into(),
            state: "done".into(),
        }],
    )
    .unwrap();
    store
        .write(|tx| {
            let kept: Vec<_> = store_support::default_states()
                .into_iter()
                .filter(|state| state.slug != "done")
                .collect();
            tx.put_states(project, &kept)
        })
        .unwrap();

    let report = repair_read_model(&store, project).unwrap();

    assert!(report.rewritten.is_empty());
    assert_eq!(report.unrepairable.len(), 1);
    assert_eq!(report.unrepairable[0].0, broken);
    // The damaged row is still there, unmodified: overwriting it with a guess
    // would destroy the evidence.
    assert!(
        store
            .read(|tx| tx.story(project, broken))
            .unwrap()
            .is_some()
    );
}

#[test]
fn repair_is_scoped_to_one_project() {
    let (_dir, store) = new_store();
    let alpha = seed_project(&store, "alpha", "SH");
    let beta = seed_project(&store, "beta", "SH");
    create_story(&store, alpha, "Alpha one", "2026-01-01T00:00:00Z");
    create_story(&store, beta, "Beta one", "2026-01-01T00:00:00Z");
    raw(&store)
        .execute(
            "UPDATE stories SET title = 'wrong' WHERE project_id = ?1",
            rusqlite::params![beta.get()],
        )
        .unwrap();

    let report = repair_read_model(&store, alpha).unwrap();

    assert_eq!(report.rewritten, vec![StoryNo::new(1)]);
    assert!(
        !diff_read_model(&store, beta).unwrap().is_clean(),
        "repairing alpha must not have touched beta"
    );
    assert_eq!(
        store
            .read(|tx| tx.story(beta, StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .title,
        "wrong"
    );
}

// ---------------------------------------------------------------------------
// Fault injection
// ---------------------------------------------------------------------------

#[test]
fn the_fault_points_compile_and_fire_where_they_are_placed() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    create_story(&store, project, "First", "2026-01-01T00:00:00Z");

    // Each point is reached by a different operation, so each is armed against
    // one that actually passes through it — an assertion that a point "fired"
    // from a call that never reaches it would prove nothing.
    for point in [FaultPoint::BeforeCommit, FaultPoint::AfterCommitBeforeAck] {
        let _fault = arm(point, FaultAction::Fail(point.as_str().to_string()));
        let error = store
            .write(|tx| tx.allocate_story_no(project))
            .expect_err(point.as_str());
        assert!(
            error.to_string().contains(point.as_str()),
            "{point:?} did not fire: {error}"
        );
    }

    let _fault = arm(
        FaultPoint::MidReadModelUpdate,
        FaultAction::Fail(FaultPoint::MidReadModelUpdate.as_str().to_string()),
    );
    let error = append_and_fold(
        &store,
        project,
        StoryNo::new(1),
        ExpectedSeq::Any,
        &[StoryEvent::StoryTitleSet {
            at: "2026-01-01T00:01:00Z".into(),
            title: "Renamed".into(),
        }],
    )
    .expect_err("mid_read_model_update");
    assert!(
        error
            .to_string()
            .contains(FaultPoint::MidReadModelUpdate.as_str()),
        "MidReadModelUpdate did not fire: {error}"
    );
}

#[test]
fn every_named_fault_point_is_disarmed_by_default() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");

    assert!(
        storyhook::store::fault::armed_points().is_empty(),
        "a previous test left a fault armed on this thread"
    );
    for point in FaultPoint::all() {
        assert!(!point.as_str().is_empty());
    }
    // The store is fully usable with the feature compiled in and nothing armed,
    // which is the state every other test in the suite runs under.
    assert_eq!(
        store.write(|tx| tx.allocate_story_no(project)).unwrap(),
        StoryNo::new(1)
    );
}

// ---------------------------------------------------------------------------
// Relation symmetry at the two levels it lives on (SH-60)
// ---------------------------------------------------------------------------

#[test]
fn a_history_that_claims_only_one_side_still_yields_a_symmetric_table() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    let one = create_story(&store, project, "First", "2026-01-01T00:00:00Z");
    let two = create_story(&store, project, "Second", "2026-01-01T00:00:01Z");
    // Only SH-1's history records the edge. Under the old design this produced
    // a permanent `story doctor` violation: SH-2 was blocked by nothing as far
    // as every query could tell.
    append_and_fold(
        &store,
        project,
        one,
        ExpectedSeq::Any,
        &[StoryEvent::StoryRelationshipAdded {
            at: "2026-01-01T00:03:00Z".into(),
            other_id: two.to_id("SH"),
            relation: "blocks".into(),
        }],
    )
    .unwrap();

    let inbound = store.read(|tx| tx.relations_to(project, two)).unwrap();
    assert_eq!(inbound.len(), 1, "the mirror exists regardless");
    let outbound = store.read(|tx| tx.relations_from(project, two)).unwrap();
    assert_eq!(outbound[0].relation, "blocked-by");

    let diff = diff_read_model(&store, project).unwrap();

    // The read model is exactly what the events imply, so a repair has nothing
    // to do…
    assert!(diff.is_clean(), "{}", diff.describe());
    // …but the histories disagree, and only an event can settle that.
    assert!(diff.has_integrity_issues());
    assert_eq!(
        diff.asymmetric_relations,
        vec![(one, "blocks".to_string(), two)]
    );
    assert!(diff.describe().contains("does not claim the inverse"));
}

#[test]
fn repairing_a_one_sided_history_does_not_retract_the_edge() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    let one = create_story(&store, project, "First", "2026-01-01T00:00:00Z");
    let two = create_story(&store, project, "Second", "2026-01-01T00:00:01Z");
    append_and_fold(
        &store,
        project,
        one,
        ExpectedSeq::Any,
        &[StoryEvent::StoryRelationshipAdded {
            at: "2026-01-01T00:03:00Z".into(),
            other_id: two.to_id("SH"),
            relation: "blocks".into(),
        }],
    )
    .unwrap();

    // Rewriting SH-2 from its own history — which never mentions SH-1 — must
    // not delete a row SH-1 still asserts. Getting this wrong makes the result
    // depend on which story is written first, which is how a repair loses data.
    repair_read_model(&store, project).unwrap();

    assert_eq!(
        store
            .read(|tx| tx.relations_to(project, two))
            .unwrap()
            .len(),
        1
    );
    assert!(diff_read_model(&store, project).unwrap().is_clean());
    // Idempotent: a second repair changes nothing either.
    repair_read_model(&store, project).unwrap();
    assert_eq!(
        store
            .read(|tx| tx.relations_from(project, one))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_symmetric_history_reports_no_integrity_issues() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    let one = create_story(&store, project, "First", "2026-01-01T00:00:00Z");
    let two = create_story(&store, project, "Second", "2026-01-01T00:00:01Z");
    for (story, other, relation) in [(one, two, "parent-of"), (two, one, "child-of")] {
        append_and_fold(
            &store,
            project,
            story,
            ExpectedSeq::Any,
            &[StoryEvent::StoryRelationshipAdded {
                at: "2026-01-01T00:03:00Z".into(),
                other_id: other.to_id("SH"),
                relation: relation.into(),
            }],
        )
        .unwrap();
    }

    let diff = diff_read_model(&store, project).unwrap();

    assert!(diff.is_clean(), "{}", diff.describe());
    assert!(
        !diff.has_integrity_issues(),
        "{:?}",
        diff.asymmetric_relations
    );
}

#[test]
fn removing_an_edge_from_both_histories_retracts_it_whichever_side_is_written_first() {
    for write_remover_first in [true, false] {
        let (_dir, store) = new_store();
        let project = seed_project(&store, "alpha", "SH");
        let one = create_story(&store, project, "First", "2026-01-01T00:00:00Z");
        let two = create_story(&store, project, "Second", "2026-01-01T00:00:01Z");
        for (story, other, relation) in [(one, two, "blocks"), (two, one, "blocked-by")] {
            append_and_fold(
                &store,
                project,
                story,
                ExpectedSeq::Any,
                &[StoryEvent::StoryRelationshipAdded {
                    at: "2026-01-01T00:03:00Z".into(),
                    other_id: other.to_id("SH"),
                    relation: relation.into(),
                }],
            )
            .unwrap();
        }

        let order = if write_remover_first {
            [(one, two, "blocks"), (two, one, "blocked-by")]
        } else {
            [(two, one, "blocked-by"), (one, two, "blocks")]
        };
        for (story, other, relation) in order {
            append_and_fold(
                &store,
                project,
                story,
                ExpectedSeq::Any,
                &[StoryEvent::StoryRelationshipRemoved {
                    at: "2026-01-01T00:04:00Z".into(),
                    other_id: other.to_id("SH"),
                    relation: relation.into(),
                }],
            )
            .unwrap();
        }

        let remaining: Vec<_> = store.read(|tx| tx.relations_from(project, one)).unwrap();
        assert!(
            remaining.is_empty(),
            "write_remover_first={write_remover_first}: {remaining:?}"
        );
        assert!(diff_read_model(&store, project).unwrap().is_clean());
    }
}

#[test]
fn a_fault_before_commit_leaves_the_database_untouched() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    let before = store.read(|tx| tx.project(project)).unwrap().unwrap();

    let _fault = arm(
        FaultPoint::BeforeCommit,
        FaultAction::Fail("crash".to_string()),
    );
    let result = store.write(|tx| {
        tx.allocate_story_no(project)?;
        tx.append_events(
            project,
            StoryNo::new(1),
            ExpectedSeq::Exact(EventSeq::ZERO),
            &[StoryEvent::StoryCreated {
                at: "2026-01-01T00:00:00Z".into(),
                title: "Never written".into(),
                state: "todo".into(),
            }],
        )
    });

    assert!(result.is_err());
    drop(_fault);
    let after = store.read(|tx| tx.project(project)).unwrap().unwrap();
    assert_eq!(
        after.next_story_no, before.next_story_no,
        "a rolled-back allocation must return the number"
    );
    assert_eq!(after.next_global_seq, before.next_global_seq);
    assert!(
        store
            .read(|tx| tx.events_for(project, StoryNo::new(1)))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_fault_after_commit_reports_failure_for_a_write_that_did_land() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");

    let fault = arm(
        FaultPoint::AfterCommitBeforeAck,
        FaultAction::Fail("lost the ack".to_string()),
    );
    let result = store.write(|tx| tx.allocate_story_no(project));
    drop(fault);

    assert!(result.is_err(), "the caller is told the write failed");
    // But it did not: the transaction committed before the fault fired. This is
    // the asymmetry a client must never resolve by retrying a mutation, and
    // pinning it here is what makes that rule testable rather than aspirational.
    assert_eq!(
        store
            .read(|tx| tx.project(project))
            .unwrap()
            .unwrap()
            .next_story_no,
        2
    );
}

#[test]
fn a_fault_midway_through_the_read_model_leaves_no_partial_row() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    let one = create_story(&store, project, "First", "2026-01-01T00:00:00Z");

    let fault = arm(
        FaultPoint::MidReadModelUpdate,
        FaultAction::Fail("crash between the row and its labels".to_string()),
    );
    let result = append_and_fold(
        &store,
        project,
        one,
        ExpectedSeq::Any,
        &[StoryEvent::StoryLabelsSet {
            at: "2026-01-01T00:01:00Z".into(),
            labels: vec!["never".into()],
        }],
    );
    drop(fault);

    assert!(result.is_err());
    let row = store.read(|tx| tx.story(project, one)).unwrap().unwrap();
    assert_eq!(row.title, "First");
    assert!(
        row.labels.is_empty(),
        "the half-applied label write must have rolled back"
    );
    assert!(diff_read_model(&store, project).unwrap().is_clean());
}

#[test]
fn a_panic_inside_a_write_rolls_back_and_leaves_the_store_usable() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.write(|tx| -> Result<(), StoreError> {
            tx.allocate_story_no(project)?;
            panic!("something went wrong mid-transaction");
        })
    }));

    assert!(panicked.is_err());
    // The write mutex was poisoned by the panic and is deliberately ignored:
    // the transaction rolled back when it dropped, so nothing a poisoned mutex
    // protects was actually broken. A store that stayed unusable after one
    // panicking request would be the worse failure.
    assert_eq!(
        store
            .read(|tx| tx.project(project))
            .unwrap()
            .unwrap()
            .next_story_no,
        1
    );
    assert_eq!(
        store.write(|tx| tx.allocate_story_no(project)).unwrap(),
        StoryNo::new(1)
    );
}
