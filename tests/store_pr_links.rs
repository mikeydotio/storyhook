//! Schema migration 11 (`story_pr_links`) and its projection — SH-49.
//!
//! Two things are tested here, in the style of `store_migrations.rs`'s
//! migration-10 pair: that the migration itself adds the table without
//! disturbing anything that existed before it, and that the four
//! `StoryPr*` events project into that table the way the module doc on
//! `store::sqlite::write::project_pr_link` promises.

use rusqlite::Connection;
use storyhook::domain::StoryEvent;
use storyhook::store::{
    ExpectedSeq, NewProject, ProjectId, ReadOps, SqliteStore, Store, StoryNo, WriteOps, migrate,
};
use storyhook_test_support::scratch_dir;

/// Seeds one v10 project holding one story, straight to the tables — the same
/// reason `store_migrations.rs::seed_a_v9_story` does: this exercises the
/// migration framework on a schema that predates the table migration 11 adds.
fn seed_a_v10_story(path: &std::path::Path) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "INSERT INTO projects (id, uuid, slug, name, prefix, created_at)
             VALUES (1, 'u-1', 'proj', 'Proj', 'SH', '2026-01-01T00:00:00Z');
         INSERT INTO project_states (project_id, position, slug, superstate)
             VALUES (1, 0, 'todo', 'OPEN'), (1, 1, 'done', 'CLOSED');
         INSERT INTO stories (project_id, story_no, head_seq, title, state, superstate,
                              priority, priority_rank, created_at, updated_at, snapshot)
             VALUES (1, 1, 1, 'A story', 'todo', 'OPEN', 'none', 4,
                     '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z', '{}');",
    )
    .unwrap();
}

#[test]
fn migration_eleven_adds_a_story_pr_links_table_that_starts_empty() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..10]).unwrap();
    seed_a_v10_story(store.path());

    assert!(
        Connection::open(store.path())
            .unwrap()
            .prepare("SELECT 1 FROM story_pr_links")
            .is_err(),
        "a v10 database must not already have `story_pr_links` — otherwise \
         this test proves nothing about the migration that adds it"
    );

    store.migrate().unwrap();

    let count: i64 = Connection::open(store.path())
        .unwrap()
        .query_row("SELECT COUNT(*) FROM story_pr_links", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        count, 0,
        "the migration only creates the table — it backfills nothing, unlike \
         migration 2's `story_commit_links`, because no pre-SH-49 build ever \
         wrote a PR link anywhere the table could recover one from"
    );
}

#[test]
fn migration_eleven_leaves_every_pre_existing_column_and_the_event_log_untouched() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..10]).unwrap();
    seed_a_v10_story(store.path());

    store.migrate().unwrap();

    let (title, hidden_at): (String, Option<String>) = Connection::open(store.path())
        .unwrap()
        .query_row(
            "SELECT title, hidden_at FROM stories WHERE project_id = 1 AND story_no = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(title, "A story");
    assert_eq!(hidden_at, None);

    let events = store
        .read(|tx| tx.events_for(ProjectId::new(1), StoryNo::new(1)))
        .unwrap();
    assert!(
        events.is_empty(),
        "the pre-existing story was seeded with no events; migration 11 must \
         not have added any"
    );
}

/// Builds a fresh, fully-migrated store holding one project.
fn fresh_store(path: &std::path::Path) -> (SqliteStore, ProjectId) {
    let store = SqliteStore::open(path.join("store.db")).unwrap();
    store.migrate().unwrap();
    let project = store
        .write(|tx| {
            tx.create_project(&NewProject {
                uuid: "proj".into(),
                slug: "proj".into(),
                name: "Proj".into(),
                prefix: "SH".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            })
        })
        .unwrap();
    (store, project)
}

const URL: &str = "https://github.com/acme/widgets/pull/7";

#[test]
fn story_pr_linked_inserts_an_open_row() {
    let dir = scratch_dir();
    let (store, project) = fresh_store(dir.path());
    let story = StoryNo::new(1);

    store
        .write(|tx| {
            tx.append_events(
                project,
                story,
                ExpectedSeq::Exact(storyhook::store::EventSeq::ZERO),
                &[StoryEvent::StoryPrLinked {
                    at: "2026-01-01T00:00:00Z".into(),
                    url: URL.into(),
                    owner: "acme".into(),
                    repo: "widgets".into(),
                    number: 7,
                    close_on_merge: true,
                }],
            )
        })
        .unwrap();

    let links = store
        .read(|tx| tx.open_pr_links_for_story(project, story))
        .unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].owner, "acme");
    assert_eq!(links[0].repo, "widgets");
    assert_eq!(links[0].number, 7);
    assert_eq!(links[0].url, URL);
    assert!(links[0].close_on_merge);
    assert_eq!(links[0].status, "open");
}

#[test]
fn a_second_story_pr_linked_for_the_same_pr_upserts_close_on_merge() {
    let dir = scratch_dir();
    let (store, project) = fresh_store(dir.path());
    let story = StoryNo::new(1);

    store
        .write(|tx| {
            tx.append_events(
                project,
                story,
                ExpectedSeq::Exact(storyhook::store::EventSeq::ZERO),
                &[StoryEvent::StoryPrLinked {
                    at: "2026-01-01T00:00:00Z".into(),
                    url: URL.into(),
                    owner: "acme".into(),
                    repo: "widgets".into(),
                    number: 7,
                    close_on_merge: true,
                }],
            )
        })
        .unwrap();
    store
        .write(|tx| {
            tx.append_events(
                project,
                story,
                ExpectedSeq::Any,
                &[StoryEvent::StoryPrLinked {
                    at: "2026-01-02T00:00:00Z".into(),
                    url: URL.into(),
                    owner: "acme".into(),
                    repo: "widgets".into(),
                    number: 7,
                    close_on_merge: false,
                }],
            )
        })
        .unwrap();

    let links = store
        .read(|tx| tx.open_pr_links_for_story(project, story))
        .unwrap();
    assert_eq!(
        links.len(),
        1,
        "re-linking the same (owner, repo, number) is an upsert, not a second row"
    );
    assert!(!links[0].close_on_merge, "the second link's value wins");
    assert_eq!(links[0].linked_at, "2026-01-02T00:00:00Z");
}

#[test]
fn story_pr_unlinked_deletes_the_row() {
    let dir = scratch_dir();
    let (store, project) = fresh_store(dir.path());
    let story = StoryNo::new(1);

    store
        .write(|tx| {
            tx.append_events(
                project,
                story,
                ExpectedSeq::Exact(storyhook::store::EventSeq::ZERO),
                &[
                    StoryEvent::StoryPrLinked {
                        at: "2026-01-01T00:00:00Z".into(),
                        url: URL.into(),
                        owner: "acme".into(),
                        repo: "widgets".into(),
                        number: 7,
                        close_on_merge: true,
                    },
                    StoryEvent::StoryPrUnlinked {
                        at: "2026-01-02T00:00:00Z".into(),
                        url: URL.into(),
                    },
                ],
            )
        })
        .unwrap();

    let links = store
        .read(|tx| tx.open_pr_links_for_story(project, story))
        .unwrap();
    assert!(links.is_empty());
}

#[test]
fn story_pr_merged_updates_status_and_drops_out_of_the_open_read() {
    let dir = scratch_dir();
    let (store, project) = fresh_store(dir.path());
    let story = StoryNo::new(1);

    store
        .write(|tx| {
            tx.append_events(
                project,
                story,
                ExpectedSeq::Exact(storyhook::store::EventSeq::ZERO),
                &[
                    StoryEvent::StoryPrLinked {
                        at: "2026-01-01T00:00:00Z".into(),
                        url: URL.into(),
                        owner: "acme".into(),
                        repo: "widgets".into(),
                        number: 7,
                        close_on_merge: true,
                    },
                    StoryEvent::StoryPrMerged {
                        at: "2026-01-02T00:00:00Z".into(),
                        url: URL.into(),
                    },
                ],
            )
        })
        .unwrap();

    let links = store
        .read(|tx| tx.open_pr_links_for_story(project, story))
        .unwrap();
    assert!(
        links.is_empty(),
        "`open_pr_links_for_story` only ever answers with `open` links"
    );

    let status: String = Connection::open(store.path())
        .unwrap()
        .query_row("SELECT status FROM story_pr_links", [], |row| row.get(0))
        .unwrap();
    assert_eq!(status, "merged");
}

/// `pr_links` (SH-169, `referenced_by.prs`'s read) is the opposite filter
/// from `open_pr_links`: a merged link is exactly what a reader most wants
/// to see under "referenced by", not something to hide.
#[test]
fn pr_links_includes_a_merged_link_that_open_pr_links_excludes() {
    let dir = scratch_dir();
    let (store, project) = fresh_store(dir.path());
    let story = StoryNo::new(1);

    store
        .write(|tx| {
            tx.append_events(
                project,
                story,
                ExpectedSeq::Exact(storyhook::store::EventSeq::ZERO),
                &[
                    StoryEvent::StoryPrLinked {
                        at: "2026-01-01T00:00:00Z".into(),
                        url: URL.into(),
                        owner: "acme".into(),
                        repo: "widgets".into(),
                        number: 7,
                        close_on_merge: true,
                    },
                    StoryEvent::StoryPrMerged {
                        at: "2026-01-02T00:00:00Z".into(),
                        url: URL.into(),
                    },
                ],
            )
        })
        .unwrap();

    assert!(
        store
            .read(|tx| tx.open_pr_links_for_story(project, story))
            .unwrap()
            .is_empty(),
        "sanity: `open_pr_links_for_story` still excludes it"
    );

    let links = store.read(|tx| tx.pr_links(project)).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].0, story);
    assert_eq!(links[0].1.status, "merged");
    assert_eq!(links[0].1.number, 7);
}

/// The project-wide read `story_views` uses to build `referenced_by.prs`
/// (SH-169), across stories and regardless of status — the same shape
/// `open_pr_links_answers_project_wide_across_stories` proves for the
/// open-only variant.
#[test]
fn pr_links_answers_project_wide_across_stories_regardless_of_status() {
    let dir = scratch_dir();
    let (store, project) = fresh_store(dir.path());

    store
        .write(|tx| {
            tx.append_events(
                project,
                StoryNo::new(1),
                ExpectedSeq::Exact(storyhook::store::EventSeq::ZERO),
                &[StoryEvent::StoryPrLinked {
                    at: "2026-01-01T00:00:00Z".into(),
                    url: URL.into(),
                    owner: "acme".into(),
                    repo: "widgets".into(),
                    number: 7,
                    close_on_merge: true,
                }],
            )
        })
        .unwrap();
    store
        .write(|tx| {
            tx.append_events(
                project,
                StoryNo::new(2),
                ExpectedSeq::Exact(storyhook::store::EventSeq::ZERO),
                &[
                    StoryEvent::StoryPrLinked {
                        at: "2026-01-01T00:00:00Z".into(),
                        url: "https://github.com/acme/widgets/pull/9".into(),
                        owner: "acme".into(),
                        repo: "widgets".into(),
                        number: 9,
                        close_on_merge: false,
                    },
                    StoryEvent::StoryPrClosed {
                        at: "2026-01-02T00:00:00Z".into(),
                        url: "https://github.com/acme/widgets/pull/9".into(),
                    },
                ],
            )
        })
        .unwrap();

    let mut links = store.read(|tx| tx.pr_links(project)).unwrap();
    links.sort_by_key(|(no, _)| no.get());
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].0, StoryNo::new(1));
    assert_eq!(links[0].1.status, "open");
    assert_eq!(links[1].0, StoryNo::new(2));
    assert_eq!(links[1].1.status, "closed");
}

#[test]
fn story_pr_closed_without_merging_updates_status_to_closed() {
    let dir = scratch_dir();
    let (store, project) = fresh_store(dir.path());
    let story = StoryNo::new(1);

    store
        .write(|tx| {
            tx.append_events(
                project,
                story,
                ExpectedSeq::Exact(storyhook::store::EventSeq::ZERO),
                &[
                    StoryEvent::StoryPrLinked {
                        at: "2026-01-01T00:00:00Z".into(),
                        url: URL.into(),
                        owner: "acme".into(),
                        repo: "widgets".into(),
                        number: 7,
                        close_on_merge: true,
                    },
                    StoryEvent::StoryPrClosed {
                        at: "2026-01-02T00:00:00Z".into(),
                        url: URL.into(),
                    },
                ],
            )
        })
        .unwrap();

    let status: String = Connection::open(store.path())
        .unwrap()
        .query_row("SELECT status FROM story_pr_links", [], |row| row.get(0))
        .unwrap();
    assert_eq!(status, "closed");
}

#[test]
fn open_pr_links_answers_project_wide_across_stories() {
    let dir = scratch_dir();
    let (store, project) = fresh_store(dir.path());

    store
        .write(|tx| {
            tx.append_events(
                project,
                StoryNo::new(1),
                ExpectedSeq::Exact(storyhook::store::EventSeq::ZERO),
                &[StoryEvent::StoryPrLinked {
                    at: "2026-01-01T00:00:00Z".into(),
                    url: URL.into(),
                    owner: "acme".into(),
                    repo: "widgets".into(),
                    number: 7,
                    close_on_merge: true,
                }],
            )
        })
        .unwrap();
    store
        .write(|tx| {
            tx.append_events(
                project,
                StoryNo::new(2),
                ExpectedSeq::Exact(storyhook::store::EventSeq::ZERO),
                &[StoryEvent::StoryPrLinked {
                    at: "2026-01-01T00:00:00Z".into(),
                    url: "https://github.com/acme/widgets/pull/9".into(),
                    owner: "acme".into(),
                    repo: "widgets".into(),
                    number: 9,
                    close_on_merge: false,
                }],
            )
        })
        .unwrap();

    let mut links = store.read(|tx| tx.open_pr_links(project)).unwrap();
    links.sort_by_key(|(no, _)| no.get());
    assert_eq!(links.len(), 2);
    assert_eq!(links[0].0, StoryNo::new(1));
    assert_eq!(links[1].0, StoryNo::new(2));
}
