//! SH-663 upgrade and restore preserve immutable history and replay agreement.

mod store_support;

use rusqlite::Connection;
use storyhook::domain::{StateDef, StoryEvent, SuperState};
use storyhook::service::{Clock, Ctx, TransferService, transfer};
use storyhook::store::{
    ExpectedSeq, ReadOps, SqliteStore, Store, StoryQuery, WriteOps, diff_read_model, migrate,
};
use storyhook_test_support::scratch_dir;

fn old_catalog(store: &SqliteStore, project: storyhook::store::ProjectId, open: bool) {
    store
        .write(|tx| {
            let mut states = tx.states(project)?;
            for state in &mut states {
                if state.slug == "dropped" {
                    state.slug = "closed".into();
                }
                if state.slug == "closed" {
                    state.super_state = if open {
                        SuperState::Open
                    } else {
                        SuperState::Closed
                    };
                    state.description = Some("Original description".into());
                }
            }
            tx.put_states(project, &states)
        })
        .unwrap();
}

#[test]
fn upgrade_preserves_events_metadata_and_rebuild_agreement() {
    for custom_open in [false, true] {
        let dir = scratch_dir();
        let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
        store.migrate_with(&migrate::MIGRATIONS[..35]).unwrap();
        let project = store_support::seed_project(&store, "legacy", "SH");
        old_catalog(&store, project, custom_open);
        for (slug, deleted, reopened) in [
            ("closed", false, false),
            ("done", false, false),
            ("done", true, false),
            ("done", true, true),
        ] {
            let no =
                store_support::create_story(&store, project, "History", "2026-01-01T00:00:00Z");
            let mut events = vec![StoryEvent::StoryStateChanged {
                at: "2026-01-02T00:00:00Z".into(),
                state: slug.into(),
            }];
            if !custom_open || slug != "closed" {
                events.push(StoryEvent::StoryClosedAndArchived {
                    at: "2026-01-02T00:00:00Z".into(),
                    state: slug.into(),
                });
                events.push(StoryEvent::StoryHidden {
                    at: "2026-01-03T00:00:00Z".into(),
                });
            }
            if deleted {
                events.push(StoryEvent::StoryDeleted {
                    at: "2026-01-04T00:00:00Z".into(),
                    reason: "Legacy".into(),
                });
            }
            if reopened {
                events.push(StoryEvent::StoryStateChanged {
                    at: "2026-01-05T00:00:00Z".into(),
                    state: "todo".into(),
                });
            }
            store_support::append_and_fold(&store, project, no, ExpectedSeq::Any, &events).unwrap();
        }
        let before = store
            .read(|tx| tx.stories(project, &StoryQuery::all()))
            .unwrap();
        let history = store
            .read(|tx| tx.events_for(project, before[0].story_no))
            .unwrap();
        let original = format!("{history:?}");
        let report = store.migrate().unwrap();
        assert!(report.to_version > 35);
        assert!(report.backup.is_some());
        let after = store
            .read(|tx| tx.stories(project, &StoryQuery::all()))
            .unwrap();
        assert_eq!(
            after
                .iter()
                .map(|r| r.snapshot.state.as_str())
                .collect::<Vec<_>>(),
            [
                if custom_open { "closed" } else { "dropped" },
                "done",
                "dropped",
                "todo"
            ]
        );
        let mut expected = before;
        for (row, migrated) in expected.iter_mut().zip(&after) {
            row.state = migrated.state.clone();
            row.snapshot.state = migrated.snapshot.state.clone();
        }
        assert_eq!(after, expected, "only the state spelling may change");
        assert_eq!(
            original,
            format!(
                "{:?}",
                store
                    .read(|tx| tx.events_for(project, after[0].story_no))
                    .unwrap()
            )
        );
        assert!(diff_read_model(&store, project).unwrap().is_clean());
        let states = store.read(|tx| tx.states(project)).unwrap();
        if !custom_open {
            assert!(!states.iter().any(|s| s.slug == "closed"));
            assert_eq!(
                states
                    .iter()
                    .find(|s| s.slug == "dropped")
                    .unwrap()
                    .description
                    .as_deref(),
                Some("Original description")
            );
        }
        assert!(store.migrate().unwrap().is_noop());
    }
}

#[test]
fn conflicting_migration_rolls_back_and_names_project() {
    for super_state in [SuperState::Open, SuperState::Closed] {
        let dir = scratch_dir();
        let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
        store.migrate_with(&migrate::MIGRATIONS[..35]).unwrap();
        let project = store_support::seed_project(&store, "collision-project", "SH");
        old_catalog(&store, project, false);
        store
            .write(|tx| {
                let mut states = tx.states(project)?;
                states.push(StateDef {
                    slug: "dropped".into(),
                    super_state: super_state.clone(),
                    role: None,
                    description: None,
                });
                tx.put_states(project, &states)
            })
            .unwrap();
        let error = store.migrate().unwrap_err().to_string();
        assert!(
            error.contains("collision-project") && error.contains("dropped"),
            "{error}"
        );
        let conn = Connection::open(store.path()).unwrap();
        assert_eq!(migrate::schema_version(&conn).unwrap(), 35);
        assert_eq!(
            store
                .read(|tx| tx.states(project))
                .unwrap()
                .iter()
                .filter(|s| s.slug == "closed")
                .count(),
            1
        );
    }
}

#[test]
fn importing_old_export_normalizes_catalog_and_preserves_payloads() {
    let (dir, store) = store_support::new_store();
    let project = store_support::seed_project(&store, "old-export", "SH");
    old_catalog(&store, project, false);
    let no = store_support::create_story(&store, project, "Abandoned", "2026-01-01T00:00:00Z");
    store_support::append_and_fold(
        &store,
        project,
        no,
        ExpectedSeq::Any,
        &[StoryEvent::StoryClosedAndArchived {
            at: "2026-01-02T00:00:00Z".into(),
            state: "closed".into(),
        }],
    )
    .unwrap();
    let clock = Clock::System;
    let ctx = Ctx::new(
        &store,
        project,
        dir.path(),
        storyhook::env::Environment::at(dir.path()),
    );
    let export = TransferService::new(&ctx).export().unwrap();
    let original = serde_json::to_value(&export.stories).unwrap();
    let (destination, restored) = store_support::new_store();
    transfer::import_project(&restored, destination.path(), &clock, &export, false).unwrap();
    let project = restored.read(|tx| tx.projects()).unwrap()[0].id;
    let ctx = Ctx::new(
        &restored,
        project,
        destination.path(),
        storyhook::env::Environment::at(destination.path()),
    );
    let export = TransferService::new(&ctx).export().unwrap();
    assert!(export.states.iter().any(|s| s.slug == "dropped"));
    assert!(!export.states.iter().any(|s| s.slug == "closed"));
    assert_eq!(serde_json::to_value(&export.stories).unwrap(), original);
    let rows = restored
        .read(|tx| tx.stories(project, &StoryQuery::all()))
        .unwrap();
    assert_eq!(rows[0].snapshot.state, "dropped");
    assert!(diff_read_model(&restored, project).unwrap().is_clean());
}

#[test]
fn migration_respects_a_stale_read_models_event_horizon() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).unwrap();
    store.migrate_with(&migrate::MIGRATIONS[..35]).unwrap();
    let project = store_support::seed_project(&store, "stale", "SH");
    old_catalog(&store, project, true);
    let no = store_support::create_story(&store, project, "Legacy", "2026-01-01T00:00:00Z");
    let head = store_support::append_and_fold(
        &store,
        project,
        no,
        ExpectedSeq::Any,
        &[StoryEvent::StoryDeleted {
            at: "2026-01-02T00:00:00Z".into(),
            reason: "Legacy".into(),
        }],
    )
    .unwrap();
    // Simulate a stale projection: the later reopen exists only in the log.
    store
        .write(|tx| {
            tx.append_events(
                project,
                no,
                ExpectedSeq::Exact(head),
                &[StoryEvent::StoryStateChanged {
                    at: "2026-01-03T00:00:00Z".into(),
                    state: "todo".into(),
                }],
                &storyhook::domain::provenance::Provenance::unrecorded(),
            )
        })
        .unwrap();
    store.migrate().unwrap();
    let row = store.read(|tx| tx.story(project, no)).unwrap().unwrap();
    assert_eq!(row.head_seq, head);
    assert_eq!(row.state, "dropped");
    assert_eq!(row.snapshot.state, "dropped");
    assert_eq!(
        row.snapshot.hidden_at.as_deref(),
        Some("2026-01-02T00:00:00Z")
    );
}
