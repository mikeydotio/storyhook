//! `StoreSyncStorage` — the storage edges GitHub sync stands on, exercised
//! without a network.
//!
//! The merge engine is untouched by this wave and is covered where it always
//! was; what is new is the eight-method seam underneath it. Each of those
//! methods is a place the flip could silently lose sync state — a base snapshot
//! that does not round-trip means every sync after it reports phantom
//! conflicts — so each gets a test of its own.

#![cfg(feature = "github-sync")]

use std::collections::BTreeMap;

use storyhook::domain::{Priority, StoryEvent};
use storyhook::error::AppError;
use storyhook::github::storage::SyncStorage;
use storyhook::github::sync_state::{
    GithubRepo, GithubSyncConfig, StoryIssueMapping, SyncMode, SyncSettings,
};
use storyhook::service::{Clock, NewStoryInput, StoreSyncStorage, StoryService};
use storyhook::store::{ReadOps, Store, StoryNo, WriteOps, partition_known};
use storyhook_test_support::ServiceFixture;

fn config() -> GithubSyncConfig {
    GithubSyncConfig {
        github: GithubRepo {
            owner: "acme".into(),
            repo: "widgets".into(),
        },
        sync: SyncSettings {
            mode: SyncMode::Auto,
            last_sync_at: Some("2026-03-30T12:00:00Z".into()),
            last_full_sync_at: None,
        },
        etags: BTreeMap::from([("issues".to_string(), "abc123".to_string())]),
        mappings: vec![StoryIssueMapping {
            story_id: "SH-1".into(),
            issue_number: 42,
            last_synced_at: "2026-03-30T12:00:00Z".into(),
            last_local_event_index: Some(5),
        }],
    }
}

fn create(fixture: &ServiceFixture, title: &str) -> String {
    StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: title.to_string(),
            ..NewStoryInput::default()
        })
        .expect("creating a story")
        .id
}

#[test]
fn a_project_that_has_never_synced_has_no_configuration() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    assert!(
        StoreSyncStorage::new(&ctx)
            .load_config()
            .expect("loading")
            .is_none()
    );
}

#[test]
fn the_sync_configuration_round_trips_through_the_settings_row() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);
    storage.save_config(&config()).expect("saving");

    let loaded = storage.load_config().expect("loading").expect("a config");
    assert_eq!(loaded.github.owner, "acme");
    assert_eq!(loaded.sync.mode, SyncMode::Auto);
    assert_eq!(loaded.etags.get("issues").unwrap(), "abc123");
    assert_eq!(loaded.mappings.len(), 1);
    assert_eq!(loaded.mappings[0].issue_number, 42);
    assert_eq!(loaded.mappings[0].last_local_event_index, Some(5));
}

#[test]
fn saving_the_sync_configuration_leaves_the_other_settings_alone() {
    // The settings row is columns, not a document, but a read-modify-write that
    // dropped a sibling field would still be SH-49 all over again.
    let fixture = ServiceFixture::new();
    fixture
        .store()
        .write(|tx| {
            tx.put_settings(
                fixture.project(),
                &storyhook::store::ProjectSettings {
                    sync_auto_transition: Some(false),
                    doctor_stale_threshold: Some("30d".to_string()),
                    github_sync: None,
                },
            )
        })
        .expect("seeding settings");

    let ctx = fixture.ctx();
    StoreSyncStorage::new(&ctx)
        .save_config(&config())
        .expect("saving");

    let settings = fixture
        .store()
        .read(|tx| tx.settings(fixture.project()))
        .expect("reading settings");
    assert_eq!(settings.sync_auto_transition, Some(false));
    assert_eq!(settings.doctor_stale_threshold.as_deref(), Some("30d"));
    assert!(settings.github_sync.is_some());
}

#[test]
fn an_unreadable_stored_configuration_fails_loudly() {
    let fixture = ServiceFixture::new();
    fixture
        .store()
        .write(|tx| {
            tx.put_settings(
                fixture.project(),
                &storyhook::store::ProjectSettings {
                    github_sync: Some(serde_json::json!({"not": "a config"})),
                    ..storyhook::store::ProjectSettings::default()
                },
            )
        })
        .expect("seeding settings");

    let ctx = fixture.ctx();
    let error = StoreSyncStorage::new(&ctx)
        .load_config()
        .expect_err("a malformed document must not read as absent");
    assert!(matches!(error, AppError::Storage(_)), "{error}");
    assert_eq!(error.exit_code(), 5);
}

#[test]
fn a_base_snapshot_round_trips_and_is_absent_until_it_is_written() {
    let fixture = ServiceFixture::new();
    let id = create(&fixture, "Synced");
    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);

    assert!(storage.load_base(&id).expect("loading").is_none());

    let snapshot = storage.story(&id).expect("reading the story");
    storage.save_base(&id, &snapshot).expect("saving the base");
    let base = storage.load_base(&id).expect("loading").expect("a base");
    assert_eq!(base, snapshot, "the merge base must round-trip exactly");
}

#[test]
fn writing_a_base_twice_replaces_it() {
    let fixture = ServiceFixture::new();
    let id = create(&fixture, "Synced");
    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);

    let first = storage.story(&id).expect("reading");
    storage.save_base(&id, &first).expect("saving");
    StoryService::new(&fixture.ctx())
        .set_priority(&id, "high")
        .expect("changing the story");
    let second = storage.story(&id).expect("re-reading");
    storage.save_base(&id, &second).expect("re-saving");

    let base = storage.load_base(&id).expect("loading").expect("a base");
    assert_eq!(base.priority, Priority::High);
}

#[test]
fn a_backup_writes_the_story_log_outside_the_repository() {
    let fixture = ServiceFixture::new();
    let id = create(&fixture, "About to be rewritten");
    StoryService::new(&fixture.ctx())
        .comment(&id, "Something worth keeping.")
        .expect("commenting");

    let ctx = fixture.ctx();
    StoreSyncStorage::new(&ctx).backup(&id).expect("backing up");

    let written = walk(&fixture.env().github_backups_dir());
    assert_eq!(written.len(), 1, "exactly one backup file: {written:?}");
    let contents = std::fs::read_to_string(&written[0]).expect("reading the backup");
    assert_eq!(
        contents.lines().count(),
        2,
        "one line per event: {contents}"
    );
    assert!(contents.contains("Something worth keeping."));
    assert!(
        !fixture.cwd().join(".storyhook").exists(),
        "the backup must not land in the repository"
    );
}

#[test]
fn backing_up_a_story_with_no_events_says_so_rather_than_writing_an_empty_file() {
    let fixture = ServiceFixture::new();
    create(&fixture, "Real");
    let ctx = fixture.ctx();
    let error = StoreSyncStorage::new(&ctx)
        .backup("SH-99")
        .expect_err("a story that does not exist");
    assert!(matches!(error, AppError::NotFound(_)), "{error}");
}

#[test]
fn creating_a_story_from_an_issue_fires_no_create_hook() {
    // Pulling fifty issues would otherwise fire fifty `create` hooks, which the
    // legacy path — a direct `storage::create_story` — never did.
    let fixture = ServiceFixture::new();
    let marker = fixture.cwd().join("hook-fired");
    fixture.write_hooks_toml(&format!(
        "[hooks]\ncreate = \"touch {}\"\n",
        marker.display()
    ));

    let ctx = fixture.ctx();
    let created = StoreSyncStorage::new(&ctx)
        .create_story("From issue #12")
        .expect("creating");
    assert_eq!(created.id, "SH-1");
    assert_eq!(created.state, "todo");
    assert!(
        !marker.exists(),
        "github-sync must not fire the create hook"
    );
}

#[test]
fn writing_events_folds_them_into_the_read_model() {
    let fixture = ServiceFixture::new();
    let id = create(&fixture, "Pulled");
    let ctx = fixture.ctx();
    StoreSyncStorage::new(&ctx)
        .write_events(
            &id,
            &[
                StoryEvent::StoryTitleSet {
                    at: "2026-01-01T00:00:00Z".to_string(),
                    title: "Renamed remotely".to_string(),
                },
                StoryEvent::StoryPrioritySet {
                    at: "2026-01-01T00:00:00Z".to_string(),
                    priority: Priority::Critical,
                },
            ],
        )
        .expect("writing events");

    let story = StoreSyncStorage::new(&ctx).story(&id).expect("re-reading");
    assert_eq!(story.title, "Renamed remotely");
    assert_eq!(story.priority, Priority::Critical);
    fixture.assert_no_drift();
}

#[test]
fn writing_events_to_a_closed_story_is_refused() {
    let fixture = ServiceFixture::new();
    let id = create(&fixture, "Finished");
    StoryService::new(&fixture.ctx())
        .set_state(&id, "done", None, None)
        .expect("closing");

    let ctx = fixture.ctx();
    let error = StoreSyncStorage::new(&ctx)
        .write_events(
            &id,
            &[StoryEvent::StoryTitleSet {
                at: "2026-01-01T00:00:00Z".to_string(),
                title: "Too late".to_string(),
            }],
        )
        .expect_err("a closed story cannot be modified");
    assert!(matches!(error, AppError::Validation(_)), "{error}");
}

#[test]
fn the_catalog_and_the_open_story_list_come_from_the_store() {
    let fixture = ServiceFixture::new();
    fixture.add_member("ada", "Ada Lovelace", Some("ada"));
    let open = create(&fixture, "Open");
    let closed = create(&fixture, "Closed");
    StoryService::new(&fixture.ctx())
        .set_state(&closed, "done", None, None)
        .expect("closing");

    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);
    assert_eq!(storage.prefix().expect("prefix"), "SH");
    assert_eq!(storage.states().expect("states").len(), 4);
    assert_eq!(storage.members().expect("members").len(), 1);
    let stories = storage.open_stories().expect("open stories");
    assert_eq!(stories.len(), 1, "archived stories are excluded");
    assert_eq!(stories[0].id, open);
}

#[test]
fn a_backup_preserves_an_event_kind_this_binary_does_not_understand() {
    let fixture = ServiceFixture::new();
    let id = create(&fixture, "From the future");
    fixture
        .store()
        .write(|tx| {
            tx.append_raw_events(
                fixture.project(),
                StoryNo::new(1),
                storyhook::store::ExpectedSeq::Any,
                &[storyhook::store::RawEvent {
                    kind: "StoryTeleported".to_string(),
                    at: "2030-01-01T00:00:00Z".to_string(),
                    payload: r#"{"kind":"StoryTeleported","at":"2030-01-01T00:00:00Z"}"#
                        .to_string(),
                }],
            )?;
            Ok(())
        })
        .expect("writing a future event");
    // A raw append leaves the read model's head behind by design; repairing it
    // is what the store's own oracle is for, and the fixture's drift guard on
    // the way out would otherwise fail for a reason this test is not about.
    storyhook::store::repair_read_model(fixture.store(), fixture.project())
        .expect("repairing the read model");

    let ctx = fixture.ctx();
    StoreSyncStorage::new(&ctx).backup(&id).expect("backing up");
    let contents =
        std::fs::read_to_string(&walk(&fixture.env().github_backups_dir())[0]).expect("reading");
    assert!(
        contents.contains("StoryTeleported"),
        "an unknown kind must survive the backup: {contents}"
    );

    // And the read model still folds, which is the SH-54 guarantee.
    let stored = fixture
        .store()
        .read(|tx| tx.events_for(fixture.project(), StoryNo::new(1)))
        .expect("reading events");
    assert_eq!(partition_known(StoryNo::new(1), &stored).1.len(), 1);
}

#[test]
fn every_story_backed_up_by_one_sync_lands_in_one_directory() {
    let mut fixture = ServiceFixture::new();
    fixture.set_clock(Clock::Fixed("2026-07-28T14:30:00Z".to_string()));
    let first = create(&fixture, "One");
    let second = create(&fixture, "Two");

    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);
    storage.backup(&first).expect("backing up the first");
    storage.backup(&second).expect("backing up the second");

    let directories: Vec<_> = std::fs::read_dir(fixture.env().github_backups_dir())
        .expect("reading the backups directory")
        .map(|entry| entry.expect("an entry").file_name())
        .collect();
    assert_eq!(
        directories.len(),
        1,
        "one directory per sync: {directories:?}"
    );
    assert_eq!(directories[0], "20260728T143000Z");
}

/// Every file under `root`, recursively.
fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk(&path));
        } else {
            found.push(path);
        }
    }
    found.sort();
    found
}
