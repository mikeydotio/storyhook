//! Upgrade real version-48 rows without inventing assessment events.
mod store_support;
use storyhook::domain::{Complexity, StoryEvent};
use storyhook::store::{ExpectedSeq, ReadOps, SqliteStore, Store, migrate};
use storyhook_test_support::{FIXTURE_NOW, scratch_dir};

#[test]
fn upgrade_preserves_histories_and_defaults_open_and_archived_snapshots() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..48]).unwrap();
    let project = store_support::seed_project(&store, "complexity-upgrade", "UP");
    let open = store_support::create_story(&store, project, "Open", FIXTURE_NOW);
    let closed = store_support::create_story(&store, project, "Archived", FIXTURE_NOW);
    let head = store
        .read(|tx| tx.story(project, closed))
        .unwrap()
        .unwrap()
        .head_seq;
    store_support::append_and_fold(
        &store,
        project,
        closed,
        ExpectedSeq::Exact(head),
        &[StoryEvent::StoryClosedAndArchived {
            at: FIXTURE_NOW.into(),
            state: "done".into(),
        }],
    )
    .unwrap();
    let raw = rusqlite::Connection::open(store.path()).unwrap();
    raw.execute("UPDATE stories SET snapshot = json_remove(snapshot, '$.complexity', '$.complexity_assessed')", []).unwrap();
    let histories = [open, closed].map(|id| store.read(|tx| tx.events_for(project, id)).unwrap());
    let before = histories;
    store.migrate().unwrap();
    assert!(store.migrate().unwrap().is_noop());
    for (index, id) in [open, closed].into_iter().enumerate() {
        let row = store.read(|tx| tx.story(project, id)).unwrap().unwrap();
        assert_eq!(row.snapshot.complexity, Complexity::Medium);
        assert!(!row.snapshot.complexity_assessed);
        let events = store.read(|tx| tx.events_for(project, id)).unwrap();
        assert_eq!(events, before[index]);
    }
    assert!(
        storyhook::store::diff_read_model(&store, project)
            .unwrap()
            .is_clean()
    );
}
