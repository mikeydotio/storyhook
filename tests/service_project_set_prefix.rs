//! `ProjectService::set_prefix` — SH-109's item 3.
//!
//! # The corruption this exists to make impossible
//!
//! `projects.prefix` used to have no supported way to change at all. The one
//! time it was changed anyway — a raw `UPDATE` on the `agentics` project,
//! 2026-07-30 — every rendered `id` in the read model kept working (it is
//! derived fresh from the current prefix on every refold), but every
//! relationship's `other_id` did not: it is folded verbatim from the
//! `other_id` field of the event that set it, so it went on naming the *old*
//! prefix no matter how many times the affected stories were refolded. The
//! next ordinary write to any of them failed with `story id `HP-7` does not
//! belong to a project with prefix `AGE``, and stayed broken.
//!
//! [`raw_prefix_swap_without_compensating_events_breaks_relationship_writes`]
//! reproduces that failure directly, with nothing this module wrote — a raw
//! `UPDATE` through a second connection, exactly as it happened by hand. Every
//! other test in this file is the same fixture shape put through
//! [`ProjectService::set_prefix`] instead, proving the compensating events it
//! appends are what the raw path was missing.

mod store_support;

use storyhook::domain::StoryEvent;
use storyhook::service::{Clock, PointerUpdate, ProjectService};
use storyhook::store::{
    ExpectedSeq, ReadOps, SqliteStore, Store, StoreError, WriteOps, diff_read_model,
};
use storyhook_test_support::{FIXTURE_NOW, scratch_dir};

use store_support::{append_and_fold, create_story, link_atomic, new_store, raw, seed_project};

/// A `ProjectService` whose root is never read by `set_prefix` or
/// `set_prefix_plan` — neither consults it, only the store's own
/// `checkout_path` — but the type still asks for one.
fn service(store: &SqliteStore) -> ProjectService<'_, SqliteStore> {
    ProjectService::new(store, "/unused-root").clock(Clock::Fixed(FIXTURE_NOW.into()))
}

/// A scratch directory `set_prefix`'s safety snapshot may write into.
fn backups_dir() -> tempfile::TempDir {
    scratch_dir()
}

// ---------------------------------------------------------------------------
// The failure this command exists to end
// ---------------------------------------------------------------------------

#[test]
fn raw_prefix_swap_without_compensating_events_breaks_relationship_writes() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "agentics", "HP");
    let a = create_story(&store, project, "Depends on b", FIXTURE_NOW);
    let b = create_story(&store, project, "Blocks a", FIXTURE_NOW);
    link_atomic(&store, project, a, "blocks", b).expect("linking a and b");

    // The exact manual mistake: change the column, touch nothing else.
    raw(&store)
        .execute(
            "UPDATE projects SET prefix = 'AGE' WHERE slug = 'agentics'",
            [],
        )
        .expect("the raw rename");

    let error = append_and_fold(
        &store,
        project,
        a,
        ExpectedSeq::Any,
        &[StoryEvent::StoryCommentAdded {
            at: FIXTURE_NOW.to_string(),
            text: "a comment written after the broken rename".to_string(),
        }],
    )
    .expect_err("a's relationship to b still names the old prefix");

    assert!(
        matches!(error, StoreError::Validation(ref msg) if msg.contains("does not belong to a project with prefix")),
        "expected the parse_id refusal, got {error}"
    );
}

// ---------------------------------------------------------------------------
// `set_prefix` itself
// ---------------------------------------------------------------------------

#[test]
fn set_prefix_rewrites_every_relationship_so_ordinary_writes_keep_working() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "agentics", "HP");
    let a = create_story(&store, project, "Depends on b", FIXTURE_NOW);
    let b = create_story(&store, project, "Blocks a", FIXTURE_NOW);
    link_atomic(&store, project, a, "blocks", b).expect("linking a and b");
    let backups = backups_dir();

    let outcome = service(&store)
        .set_prefix(project, "AGE", backups.path())
        .expect("renaming the prefix");

    assert_eq!(outcome.plan.old_prefix, "HP");
    assert_eq!(outcome.plan.new_prefix, "AGE");
    assert_eq!(outcome.plan.stories, 2);
    // Each story owns one relation entry of its own — a's `blocks b` and b's
    // `blocked-by a` — so the pair counts as two, one compensating pair each.
    assert_eq!(outcome.plan.relationships, 2);
    assert!(outcome.backup_path.exists(), "the safety snapshot exists");

    assert!(
        diff_read_model(&store, project).unwrap().is_clean(),
        "the read model must agree with the event log after the rewrite"
    );

    // The failure `raw_prefix_swap_…` reproduces above must not reproduce
    // here: an ordinary write to either story succeeds.
    append_and_fold(
        &store,
        project,
        a,
        ExpectedSeq::Any,
        &[StoryEvent::StoryCommentAdded {
            at: FIXTURE_NOW.to_string(),
            text: "a comment written after the real rename".to_string(),
        }],
    )
    .expect("a's relationship to b now names the current prefix");

    let row_a = store
        .read(|tx| tx.story(project, a))
        .unwrap()
        .expect("a survives");
    assert_eq!(row_a.snapshot.id, "AGE-1");
    assert_eq!(row_a.snapshot.relationships.len(), 1);
    assert_eq!(row_a.snapshot.relationships[0].other_id, "AGE-2");
    assert_eq!(row_a.snapshot.relationships[0].relation, "blocks");

    let row_b = store
        .read(|tx| tx.story(project, b))
        .unwrap()
        .expect("b survives");
    assert_eq!(row_b.snapshot.id, "AGE-2");
    assert_eq!(row_b.snapshot.relationships.len(), 1);
    assert_eq!(row_b.snapshot.relationships[0].other_id, "AGE-1");
    assert_eq!(row_b.snapshot.relationships[0].relation, "blocked-by");
}

#[test]
fn a_story_with_no_relationships_still_gets_refolded() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "agentics", "HP");
    let lonely = create_story(&store, project, "No relations at all", FIXTURE_NOW);
    let backups = backups_dir();

    let outcome = service(&store)
        .set_prefix(project, "AGE", backups.path())
        .expect("renaming the prefix");

    assert_eq!(outcome.plan.stories, 1);
    assert_eq!(outcome.plan.relationships, 0);
    let row = store
        .read(|tx| tx.story(project, lonely))
        .unwrap()
        .expect("the story survives");
    assert_eq!(row.snapshot.id, "AGE-1", "id self-heals on refold alone");
}

#[test]
fn set_prefix_refuses_a_prefix_already_used_by_another_project() {
    let (_dir, store) = new_store();
    let mine = seed_project(&store, "mine", "AAA");
    seed_project(&store, "theirs", "BBB");
    let backups = backups_dir();

    let error = service(&store)
        .set_prefix(mine, "BBB", backups.path())
        .expect_err("BBB already belongs to `theirs`");

    let message = error.to_string();
    assert!(message.contains("BBB"), "{message}");
    assert!(message.contains("theirs"), "{message}");

    // Refused, so nothing moved.
    let record = store.read(|tx| tx.project(mine)).unwrap().unwrap();
    assert_eq!(record.prefix, "AAA");
}

#[test]
fn set_prefix_refuses_a_no_op_rename() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "agentics", "HP");
    let backups = backups_dir();

    let error = service(&store)
        .set_prefix(project, "HP", backups.path())
        .expect_err("HP is already this project's prefix");

    assert!(error.to_string().contains("already has"), "{error}");
}

#[test]
fn set_prefix_refuses_an_invalid_prefix_before_touching_anything() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "agentics", "HP");
    let backups = backups_dir();

    let error = service(&store)
        .set_prefix(project, "not a prefix", backups.path())
        .expect_err("the shared prefix grammar refuses this");

    let record = store.read(|tx| tx.project(project)).unwrap().unwrap();
    assert_eq!(record.prefix, "HP", "a refused rename writes nothing");
    let _ = error;
}

#[test]
fn set_prefix_plan_reports_the_same_counts_the_write_later_produces() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "agentics", "HP");
    let a = create_story(&store, project, "Depends on b", FIXTURE_NOW);
    let b = create_story(&store, project, "Blocks a", FIXTURE_NOW);
    link_atomic(&store, project, a, "blocks", b).expect("linking a and b");

    let plan = service(&store)
        .set_prefix_plan(project, "AGE")
        .expect("planning the rename");
    assert_eq!(plan.stories, 2);
    assert_eq!(plan.relationships, 2);
    assert_eq!(plan.old_prefix, "HP");
    assert_eq!(plan.new_prefix, "AGE");

    // A plan writes nothing.
    let record = store.read(|tx| tx.project(project)).unwrap().unwrap();
    assert_eq!(record.prefix, "HP");

    let backups = backups_dir();
    let outcome = service(&store)
        .set_prefix(project, "AGE", backups.path())
        .expect("renaming the prefix");
    assert_eq!(outcome.plan.stories, plan.stories);
    assert_eq!(outcome.plan.relationships, plan.relationships);
}

#[test]
fn set_prefix_rewrites_github_bases_snapshots_too() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "agentics", "HP");
    let a = create_story(&store, project, "Depends on b", FIXTURE_NOW);
    let b = create_story(&store, project, "Blocks a", FIXTURE_NOW);
    link_atomic(&store, project, a, "blocks", b).expect("linking a and b");
    let base_snapshot = store
        .read(|tx| tx.story(project, a))
        .unwrap()
        .expect("a exists")
        .snapshot;
    store
        .write(|tx| tx.put_github_base(project, a, &base_snapshot))
        .expect("seeding a merge-base snapshot");
    let backups = backups_dir();

    service(&store)
        .set_prefix(project, "AGE", backups.path())
        .expect("renaming the prefix");

    let base = store
        .read(|tx| tx.github_base(project, a))
        .unwrap()
        .expect("the merge base survives");
    assert_eq!(base.id, "AGE-1");
    assert_eq!(base.relationships.len(), 1);
    assert_eq!(base.relationships[0].other_id, "AGE-2");
}

#[test]
fn set_prefix_updates_a_real_checkouts_pointer_file() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "agentics", "HP");
    let checkout = scratch_dir();
    store
        .write(|tx| tx.set_checkout_path(project, Some(checkout.path())))
        .expect("registering the real checkout");
    storyhook::service::project::write_pointer(
        checkout.path(),
        &storyhook::service::project::ProjectPointer::new(
            "uuid-agentics".to_string(),
            "HP".to_string(),
        ),
    )
    .expect("writing the pointer file");
    let backups = backups_dir();

    let outcome = service(&store)
        .set_prefix(project, "AGE", backups.path())
        .expect("renaming the prefix");

    match outcome.pointer_updated {
        PointerUpdate::Updated(path) => {
            assert_eq!(
                path,
                storyhook::service::project::pointer_path(checkout.path())
            );
        }
        other => panic!("expected the pointer to be updated, got {other:?}"),
    }
    let pointer = storyhook::service::project::read_pointer(checkout.path())
        .unwrap()
        .expect("the pointer file still exists");
    assert_eq!(pointer.prefix, "AGE");
}

#[test]
fn set_prefix_reports_no_checkout_rather_than_failing_when_none_is_registered() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "agentics", "HP");
    store
        .write(|tx| tx.set_checkout_path(project, None))
        .expect("clearing the checkout");
    let backups = backups_dir();

    let outcome = service(&store)
        .set_prefix(project, "AGE", backups.path())
        .expect("renaming the prefix");

    assert!(matches!(outcome.pointer_updated, PointerUpdate::NoCheckout));
}

#[test]
fn set_prefix_takes_a_verified_backup_before_writing() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "agentics", "HP");
    let backups = backups_dir();
    assert!(
        std::fs::read_dir(backups.path())
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true),
        "nothing has been backed up yet"
    );

    let outcome = service(&store)
        .set_prefix(project, "AGE", backups.path())
        .expect("renaming the prefix");

    assert!(outcome.backup_path.exists());
    assert!(
        outcome
            .backup_path
            .parent()
            .is_some_and(|dir| dir == backups.path()),
        "the backup lands in the directory this call was given, not the daily one"
    );
    // A verified snapshot opens and answers to the schema it was taken from.
    let copy = SqliteStore::open(&outcome.backup_path).expect("opening the backup");
    let record = copy
        .read(|tx| tx.project(project))
        .unwrap()
        .expect("the backup was taken before the rename committed, so it still reads HP");
    assert_eq!(record.prefix, "HP");
}
