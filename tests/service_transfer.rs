//! `TransferService` — export, the batch importer, and the project restore.
//!
//! The round trip is the headline: a project exported and re-imported into an
//! empty store must produce a byte-identical document. That is the same
//! property `tests/story_export.rs` asserts of the legacy path, and it is what
//! makes the flip a two-way door.

use storyhook::cli::Invocation;
use storyhook::domain::remote::RemoteUrl;
use storyhook::domain::{ImportRelationship, ImportStory};
use storyhook::error::AppError;
use storyhook::invoke::dispatch;
use storyhook::output::Response;
use storyhook::service::transfer::ProjectExport;
use storyhook::service::{Clock, NewStoryInput, StoryService, TransferService, transfer};
use storyhook::store::{ReadOps, SqliteStore, Store, StoryNo, StoryQuery, WriteOps};
use storyhook_test_support::{ServiceFixture, scratch_dir};

/// A description with nothing but a title.
fn described(title: &str) -> ImportStory {
    ImportStory {
        title: title.to_string(),
        priority: None,
        labels: None,
        assignee: None,
        relationships: None,
        description: None,
        state: None,
        story_type: None,
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

fn export(fixture: &ServiceFixture) -> ProjectExport {
    TransferService::new(&fixture.ctx())
        .export()
        .expect("exporting")
}

fn document(export: &ProjectExport) -> String {
    serde_json::to_string_pretty(export).expect("serializing the export")
}

/// The project a restore left at `root`, found the way the CLI finds it.
///
/// Through the committed pointer file rather than the directory, because that
/// is the only thing identifying an imported checkout: the recorded-path index
/// is gone (SH-119), and the export document carries no uuid to look up.
fn restored_project(
    store: &SqliteStore,
    root: &std::path::Path,
) -> storyhook::store::ProjectRecord {
    let pointer = storyhook::service::project::read_pointer(root)
        .expect("reading the pointer file")
        .expect("a restore must leave a pointer file behind");
    store
        .read(|tx| tx.project_by_uuid(&pointer.uuid))
        .expect("reading the store")
        .expect("the pointer must name the imported project")
}

// --- export ----------------------------------------------------------------

#[test]
fn an_export_carries_the_catalog_and_every_story() {
    let fixture = ServiceFixture::new();
    fixture.add_member("ada", "Ada Lovelace", Some("ada"));
    let id = create(&fixture, "Only story");

    let export = export(&fixture);
    assert_eq!(export.schema, 1);
    assert_eq!(
        export.prefix, None,
        "the default prefix is emitted as an absent field, as the legacy \
         document did"
    );
    assert_eq!(export.states.len(), 4);
    assert_eq!(export.types.len(), 2);
    assert_eq!(export.members.len(), 1);
    assert_eq!(export.stories.len(), 1);
    assert_eq!(export.stories[0].id, id);
    assert!(!export.stories[0].archived);
    assert_eq!(export.stories[0].events.len(), 1, "just the creation event");
}

#[test]
fn an_export_puts_open_stories_first_and_sorts_each_group_as_text() {
    // Lexicographic, so `SH-10` precedes `SH-2` — the legacy exporter's order,
    // inherited on purpose so the document stays byte-comparable.
    let fixture = ServiceFixture::new();
    for index in 1..=11 {
        create(&fixture, &format!("Story {index}"));
    }
    StoryService::new(&fixture.ctx())
        .set_state("SH-3", "done", None, None)
        .expect("closing SH-3");

    let export = export(&fixture);
    let ids: Vec<&str> = export.stories.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "SH-1", "SH-10", "SH-11", "SH-2", "SH-4", "SH-5", "SH-6", "SH-7", "SH-8", "SH-9",
            "SH-3",
        ],
        "open stories sorted as text, then the archived ones"
    );
    assert!(export.stories.last().unwrap().archived);
}

#[test]
fn a_non_default_prefix_is_carried_in_the_document() {
    let fixture = ServiceFixture::new();
    let (store, dir) = empty_store();
    let mut exported = export(&fixture);
    exported.prefix = Some("API".to_string());
    transfer::import_project(&store, dir.path(), &Clock::System, &exported, false)
        .expect("importing");

    let project = restored_project(&store, dir.path());
    let ctx = storyhook::service::Ctx::new(
        &store,
        project.id,
        dir.path(),
        storyhook::env::Environment::at(dir.path()),
    );
    assert_eq!(
        TransferService::new(&ctx)
            .export()
            .expect("re-exporting")
            .prefix
            .as_deref(),
        Some("API")
    );
}

#[test]
fn an_empty_project_exports_an_empty_story_list() {
    let fixture = ServiceFixture::new();
    let export = export(&fixture);
    assert!(export.stories.is_empty());
    assert!(export.members.is_empty());
}

// --- a project's settings (SH-133) ------------------------------------------

/// Writes both settings a user can write, straight into the store.
fn set_settings(fixture: &ServiceFixture, auto_transition: bool, threshold: &str) {
    fixture
        .store()
        .write(|tx| {
            let mut row = tx.settings(fixture.project())?;
            row.sync_auto_transition = Some(auto_transition);
            row.doctor_stale_threshold = Some(threshold.to_string());
            tx.put_settings(fixture.project(), &row)
        })
        .expect("writing settings");
}

/// One project's settings as `(auto_transition, stale_threshold, has_github_sync)`.
fn settings_of(
    store: &SqliteStore,
    project: storyhook::store::ProjectId,
) -> (Option<bool>, Option<String>, bool) {
    let row = store
        .read(|tx| tx.settings(project))
        .expect("reading settings");
    (
        row.sync_auto_transition,
        row.doctor_stale_threshold,
        row.github_sync.is_some(),
    )
}

#[test]
fn a_restore_keeps_the_settings_the_document_carries() {
    // The defect, at the level it actually bites: `sync.auto_transition` is read
    // as `.unwrap_or(true)`, so a restore that drops the column does not merely
    // forget a preference — it turns automatic transitions back **on** for the
    // user who deliberately turned them off.
    let fixture = ServiceFixture::new();
    create(&fixture, "One");
    set_settings(&fixture, false, "21d");

    let exported = export(&fixture);
    let (store, dir) = empty_store();
    transfer::import_project(&store, dir.path(), &Clock::System, &exported, false)
        .expect("importing");

    let (auto, threshold, _) = settings_of(&store, restored_project(&store, dir.path()).id);
    assert_eq!(
        auto,
        Some(false),
        "the one setting whose purpose is stopping commit-sync must not come back unset"
    );
    assert_eq!(threshold.as_deref(), Some("21d"));
}

#[test]
fn a_project_with_no_settings_writes_no_settings_key_at_all() {
    // "Nothing is set" must have exactly one encoding, or the two exporters can
    // disagree by a byte and the second lap of the round trip fails on a
    // difference that is not one.
    let fixture = ServiceFixture::new();
    create(&fixture, "One");

    let json = document(&export(&fixture));
    assert!(
        !json.contains("\"settings\""),
        "an unset table is an absent one, never an emitted `{{}}`: {json}"
    );
    let parsed: ProjectExport = serde_json::from_str(&json).expect("re-parsing");
    assert!(parsed.settings.is_none());
}

#[test]
fn one_setting_travels_without_dragging_the_other_along() {
    let fixture = ServiceFixture::new();
    create(&fixture, "One");
    fixture
        .store()
        .write(|tx| {
            let mut row = tx.settings(fixture.project())?;
            row.sync_auto_transition = Some(false);
            tx.put_settings(fixture.project(), &row)
        })
        .expect("writing one setting");

    let exported = export(&fixture);
    let settings = exported.settings.as_ref().expect("a settings table");
    assert_eq!(settings.auto_transition(), Some(false));
    assert_eq!(
        settings.stale_threshold(),
        None,
        "an unset sibling must not be invented as a default"
    );

    let (store, dir) = empty_store();
    transfer::import_project(&store, dir.path(), &Clock::System, &exported, false)
        .expect("importing");
    let (auto, threshold, _) = settings_of(&store, restored_project(&store, dir.path()).id);
    assert_eq!(auto, Some(false));
    assert_eq!(threshold, None);
}

#[test]
fn a_restore_does_not_blank_a_setting_the_document_does_not_carry() {
    // `put_settings` writes every column, and a restore can land in a project
    // that already exists — the adopt-a-checkout branch. Building a fresh row
    // from the document and writing it would blank `github_sync`, which the
    // document deliberately never carries: the SH-49 shape, one layer up.
    // An empty document first, so the directory holds a project that a second
    // restore will *adopt* rather than create — the branch this is about. A
    // restore into a project that already holds stories is refused outright, so
    // the adopt branch is only ever reached with an empty one.
    let empty = ServiceFixture::new();
    let (store, dir) = empty_store();
    transfer::import_project(&store, dir.path(), &Clock::System, &export(&empty), false)
        .expect("creating the project to be adopted");
    let project = restored_project(&store, dir.path()).id;

    // It acquires a github-sync document, the way a real one would by running
    // `story github-sync`.
    store
        .write(|tx| {
            let mut row = tx.settings(project)?;
            row.github_sync = Some(serde_json::json!({"owner": "ada", "repo": "engine"}));
            tx.put_settings(project, &row)
        })
        .expect("configuring github-sync");

    // Now restore a document that carries the two settings it does carry.
    let fixture = ServiceFixture::new();
    create(&fixture, "One");
    set_settings(&fixture, false, "21d");
    transfer::import_project(&store, dir.path(), &Clock::System, &export(&fixture), false)
        .expect("restoring over the adopted project");

    let (auto, _, has_github) = settings_of(&store, project);
    assert_eq!(auto, Some(false));
    assert!(
        has_github,
        "a column the document does not carry must survive a restore that writes the ones it does"
    );
}

// --- a project's registered origins (SH-138) --------------------------------

/// Registers `raw_url` against `fixture`'s project, straight into the store.
fn set_remote(fixture: &ServiceFixture, raw_url: &str) {
    let url = RemoteUrl::normalize(raw_url).expect("the fixture url should normalize");
    fixture
        .store()
        .write(|tx| tx.link_remote(fixture.project(), &url, "2026-01-01T00:00:00Z"))
        .expect("registering the origin");
}

/// Every origin registered against `project`, normalized keys only.
fn remotes_of(store: &SqliteStore, project: storyhook::store::ProjectId) -> Vec<String> {
    store
        .read(|tx| tx.project_remotes(project))
        .expect("reading remotes")
        .into_iter()
        .map(|record| record.normalized)
        .collect()
}

/// `fixture`'s project's own slug, read back from the store.
fn slug_of(fixture: &ServiceFixture) -> String {
    fixture
        .store()
        .read(|tx| {
            Ok(tx
                .projects()?
                .into_iter()
                .find(|p| p.id == fixture.project()))
        })
        .expect("reading")
        .expect("the fixture's project exists")
        .slug
}

#[test]
fn an_export_carries_a_projects_registered_remotes() {
    let fixture = ServiceFixture::new();
    create(&fixture, "One");
    set_remote(&fixture, "https://github.com/acme/widgets.git");

    let exported = export(&fixture);
    assert_eq!(exported.remotes.len(), 1);
    let remote = &exported.remotes[0];
    assert_eq!(remote.normalized, "github.com/acme/widgets");
    assert_eq!(remote.raw, "https://github.com/acme/widgets.git");
    assert_eq!(remote.registered_at, "2026-01-01T00:00:00Z");
}

#[test]
fn a_project_with_no_remotes_writes_no_remotes_key_at_all() {
    // "Nothing is registered" must have exactly one encoding, the same rule
    // `ExportedSettings` already follows — otherwise the second lap of the
    // round trip (and the golden-document comparison) fails on a difference
    // that carries no information.
    let fixture = ServiceFixture::new();
    create(&fixture, "One");

    let json = document(&export(&fixture));
    assert!(
        !json.contains("\"remotes\""),
        "no registrations is an absent key, never an emitted `[]`: {json}"
    );
    let parsed: ProjectExport = serde_json::from_str(&json).expect("re-parsing");
    assert!(parsed.remotes.is_empty());
}

#[test]
fn a_restore_registers_the_remotes_the_document_carries() {
    let fixture = ServiceFixture::new();
    create(&fixture, "One");
    set_remote(&fixture, "https://github.com/acme/widgets.git");
    set_remote(&fixture, "https://github.com/acme/widgets-docs.git");

    let exported = export(&fixture);
    let (store, dir) = empty_store();
    let outcome = transfer::import_project(&store, dir.path(), &Clock::System, &exported, false)
        .expect("importing");
    assert!(outcome.skipped_remotes.is_empty());

    let project = restored_project(&store, dir.path()).id;
    let mut normalized = remotes_of(&store, project);
    normalized.sort();
    assert_eq!(
        normalized,
        ["github.com/acme/widgets", "github.com/acme/widgets-docs"]
    );
}

#[test]
fn a_restore_skips_a_remote_already_held_by_another_project_but_still_restores_the_stories() {
    // The origin is an auxiliary fact riding beside the payload a restore
    // exists to protect: a URL reclaimed by another project between backup
    // and restore must not sink an otherwise-clean restore of every story.
    let holder = ServiceFixture::new();
    set_remote(&holder, "https://github.com/acme/widgets.git");
    let holder_slug = slug_of(&holder);

    let restoring = ServiceFixture::new();
    let id = create(&restoring, "One");
    set_remote(&restoring, "https://github.com/acme/widgets.git");
    set_remote(&restoring, "https://github.com/acme/other.git");
    let exported = export(&restoring);
    assert_eq!(exported.remotes.len(), 2, "both registrations travel");

    let dir = scratch_dir();
    let outcome =
        transfer::import_project(holder.store(), dir.path(), &Clock::System, &exported, false)
            .expect("the restore must still succeed");

    assert_eq!(
        outcome.skipped_remotes.len(),
        1,
        "exactly the colliding remote is skipped: {:?}",
        outcome.skipped_remotes
    );
    assert_eq!(
        outcome.skipped_remotes[0].url,
        "https://github.com/acme/widgets.git"
    );
    assert_eq!(outcome.skipped_remotes[0].holder, holder_slug);

    // The story that has nothing to do with the collision still restored.
    let project = restored_project(holder.store(), dir.path()).id;
    assert_eq!(
        holder
            .store()
            .read(|tx| tx.stories(project, &StoryQuery::all()))
            .expect("reading stories")
            .into_iter()
            .map(|row| row.snapshot.id)
            .collect::<Vec<_>>(),
        vec![id]
    );

    // The non-colliding remote registered normally...
    assert_eq!(
        remotes_of(holder.store(), project),
        ["github.com/acme/other"]
    );
    // ...and the collided one still belongs to its original holder, untouched.
    assert_eq!(
        remotes_of(holder.store(), holder.project()),
        ["github.com/acme/widgets"]
    );
}

#[test]
fn an_unparseable_remote_in_the_document_is_rejected_whole() {
    let (store, dir) = empty_store();
    let mut exported = export(&ServiceFixture::new());
    exported
        .remotes
        .push(storyhook::service::transfer::ExportedRemote {
            normalized: "garbage".to_string(),
            raw: String::new(),
            registered_at: "2026-01-01T00:00:00Z".to_string(),
        });

    let error = transfer::import_project(&store, dir.path(), &Clock::System, &exported, false)
        .expect_err("an empty raw URL cannot normalize");
    assert!(
        matches!(error, AppError::Validation(_)),
        "a corrupt document is a validation error, not a partial write: {error:?}"
    );
    assert_eq!(
        store.read(|tx| tx.projects()).expect("listing").len(),
        0,
        "the whole transaction must roll back, including the project row it would have created"
    );
}

#[test]
fn the_import_project_arm_reports_a_skipped_remote_as_a_structured_warning() {
    // The dispatch layer, not just the service function: `invoke.rs` must
    // route a skip into `Response::MessageWithWarnings`'s `warnings`, not fold
    // it into `message`'s prose where a scripted `--json` caller would never
    // see it.
    let holder = ServiceFixture::new();
    set_remote(&holder, "https://github.com/acme/widgets.git");
    let holder_slug = slug_of(&holder);

    let restoring = ServiceFixture::new();
    create(&restoring, "One");
    set_remote(&restoring, "https://github.com/acme/widgets.git");
    let json = document(&export(&restoring));

    let dir = scratch_dir();
    std::fs::write(dir.path().join("backup.json"), &json).expect("writing the document");
    let ctx = storyhook::service::Ctx::new(
        holder.store(),
        holder.project(),
        dir.path(),
        storyhook::env::Environment::at(dir.path()),
    );

    let response = dispatch(
        &ctx,
        Invocation::ImportProject {
            file: "backup.json".to_string(),
            legacy_links: false,
        },
    )
    .expect("importing");
    let Response::MessageWithWarnings(message, warnings) = response else {
        panic!("a restore that skips a remote must answer with warnings, not a bare message");
    };
    assert!(message.contains("1 stories"), "{message}");
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].contains("https://github.com/acme/widgets.git")
            && warnings[0].contains(&holder_slug),
        "the warning must name both the URL and the project that already holds it: {warnings:?}"
    );
}

// --- an event kind this build does not understand (SH-67) -------------------

/// A payload no storyhook has ever written, with its keys deliberately out of
/// alphabetical order so that a re-serialization would be visible.
const FROM_THE_FUTURE: &str =
    r#"{"kind":"StoryPinned","at":"2030-01-01T00:00:00Z","z":1,"by":"ada"}"#;

/// Appends `FROM_THE_FUTURE` to story 1 as its second event, then repairs the
/// read model the raw append deliberately leaves behind.
fn pin_from_the_future(fixture: &ServiceFixture) {
    fixture
        .store()
        .write(|tx| {
            tx.append_raw_events(
                fixture.project(),
                StoryNo::new(1),
                storyhook::store::ExpectedSeq::Any,
                &[storyhook::store::RawEvent {
                    kind: "StoryPinned".to_string(),
                    at: "2030-01-01T00:00:00Z".to_string(),
                    payload: FROM_THE_FUTURE.to_string(),
                }],
                storyhook::store::LinkSource::Replayed,
            )?;
            Ok(())
        })
        .expect("writing an event from the future");
    // A raw append leaves the read model's head behind by design; the fixture's
    // drift guard on the way out would otherwise fail for a reason no test here
    // is about.
    storyhook::store::repair_read_model(fixture.store(), fixture.project())
        .expect("repairing the read model");
}

/// Every event of story 1 in `store`, as `(seq, kind, payload)`.
///
/// Deliberately not `global_seq`: that is a project-wide feed position
/// allocated fresh on every import and was never preserved by anything.
fn triples(
    store: &SqliteStore,
    project: storyhook::store::ProjectId,
) -> Vec<(i64, String, String)> {
    store
        .read(|tx| tx.events_for(project, StoryNo::new(1)))
        .expect("reading events")
        .into_iter()
        .map(|event| {
            let payload = match &event.payload {
                storyhook::store::StoredPayload::Known(decoded) => {
                    serde_json::to_string(decoded).expect("a known event serializes")
                }
                storyhook::store::StoredPayload::Unknown { json, .. } => json.clone(),
            };
            (event.seq.get(), event.kind.clone(), payload)
        })
        .collect()
}

#[test]
fn an_event_kind_this_build_cannot_decode_is_exported_verbatim() {
    let fixture = ServiceFixture::new();
    create(&fixture, "Pinned by a newer storyhook");
    pin_from_the_future(&fixture);

    let json = document(&export(&fixture));
    assert!(
        json.contains(FROM_THE_FUTURE),
        "the payload must reach the document byte for byte — key order included, and not even \
         re-indented by the pretty-printer, because it is written as raw JSON rather than as a \
         re-serialized value: {json}"
    );

    // And in position, rather than appended somewhere the fold happened to put
    // it: the document carries order, so a slot that moved is data that moved.
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("re-parsing");
    assert_eq!(
        parsed["stories"][0]["events"][1],
        serde_json::from_str::<serde_json::Value>(FROM_THE_FUTURE).unwrap(),
        "the unknown event must be the second event of the story it belongs to"
    );
}

#[test]
fn a_restored_document_holds_the_same_events_at_the_same_positions() {
    let fixture = ServiceFixture::new();
    let id = create(&fixture, "Pinned by a newer storyhook");
    // The unknown lands at position 2 of 4: before SH-67 it was dropped, which
    // renumbered every event after it as well as losing that one.
    pin_from_the_future(&fixture);
    StoryService::new(&fixture.ctx())
        .comment(&id, "After the future.")
        .expect("commenting");
    StoryService::new(&fixture.ctx())
        .set_state(&id, "done", None, None)
        .expect("closing");

    let before = triples(fixture.store(), fixture.project());
    assert_eq!(
        before
            .iter()
            .map(|(_, kind, _)| kind.as_str())
            .collect::<Vec<_>>(),
        [
            "StoryCreated",
            "StoryPinned",
            "StoryCommentAdded",
            "StoryStateChanged",
            "StoryClosedAndArchived",
        ],
        "the unknown event sits in the middle of the history, not at an edge"
    );

    let exported = export(&fixture);
    let (store, dir) = empty_store();
    transfer::import_project(&store, dir.path(), &Clock::System, &exported, false)
        .expect("importing");
    let after = triples(&store, restored_project(&store, dir.path()).id);

    assert_eq!(
        after, before,
        "a restore must reproduce every event at its own sequence number"
    );
}

#[test]
fn a_document_carrying_an_unknown_kind_re_exports_byte_for_byte() {
    let fixture = ServiceFixture::new();
    create(&fixture, "Pinned by a newer storyhook");
    pin_from_the_future(&fixture);

    let first = document(&export(&fixture));
    let (store, dir) = empty_store();
    transfer::import_project(
        &store,
        dir.path(),
        &Clock::Fixed("2026-01-01T00:00:00Z".to_string()),
        &serde_json::from_str(&first).expect("parsing the document"),
        false,
    )
    .expect("importing");
    let project = restored_project(&store, dir.path()).id;
    let ctx = storyhook::service::Ctx::new(
        &store,
        project,
        dir.path(),
        storyhook::env::Environment::at(dir.path()),
    );
    let second = document(&TransferService::new(&ctx).export().expect("re-exporting"));

    assert_eq!(
        first, second,
        "the two-way door must hold for a document this build cannot fully read"
    );
}

#[test]
fn an_event_the_store_cannot_index_is_refused_by_the_document_reader() {
    for (element, missing) in [
        (r#"{"at":"2030-01-01T00:00:00Z","note":"x"}"#, "kind"),
        (r#"{"kind":"StoryPinned","note":"x"}"#, "at"),
        (r#"{"kind":42,"at":"2030-01-01T00:00:00Z"}"#, "kind"),
    ] {
        let json = format!(
            r#"{{"schema":1,"prefix":null,"states":[],"types":[],"members":[],
                "stories":[{{"id":"SH-1","events":[{element}],"archived":false}}]}}"#
        );
        let error = serde_json::from_str::<ProjectExport>(&json)
            .expect_err("an event storyhook cannot index by kind and time is corrupt, not unknown");
        assert!(
            error.to_string().contains(missing),
            "a refusal must name the field that is missing: {error}"
        );
    }
}

#[test]
fn a_restore_still_does_not_claim_a_git_comment_as_a_link() {
    // The default path (`legacy_links: false`) must stay exactly what it was
    // before SH-70: a restore that does not assert its document predates kind
    // #18 leaves every `[git]`-shaped comment as prose. See
    // `a_legacy_links_restore_projects_a_pre_18_comment_into_a_link` and
    // `a_legacy_links_restore_does_not_promote_a_live_comment_sharing_the_shape`
    // for the flag's own behaviour.
    let fixture = ServiceFixture::new();
    let id = create(&fixture, "Synced before kind #18");
    StoryService::new(&fixture.ctx())
        .comment(&id, "[git] a04a8c4: an old link record")
        .expect("commenting");

    let exported = export(&fixture);
    let (store, dir) = empty_store();
    transfer::import_project(&store, dir.path(), &Clock::System, &exported, false)
        .expect("importing");
    let project = restored_project(&store, dir.path()).id;

    assert!(
        !store
            .read(|tx| tx.commit_linked(project, StoryNo::new(1), "a04a8c4"))
            .expect("reading the link table"),
        "a `[git]` comment restored through import-project is prose, not a link record"
    );
}

// --- `--legacy-links` (SH-70) ------------------------------------------------

#[test]
fn a_legacy_links_restore_projects_a_pre_18_comment_into_a_link() {
    let fixture = ServiceFixture::new();
    let id = create(&fixture, "Synced before kind #18");
    StoryService::new(&fixture.ctx())
        .comment(&id, "[git] a04a8c4: an old link record")
        .expect("commenting");

    let exported = export(&fixture);
    let (store, dir) = empty_store();
    transfer::import_project(&store, dir.path(), &Clock::System, &exported, true)
        .expect("importing");
    let project = restored_project(&store, dir.path()).id;

    assert!(
        store
            .read(|tx| tx.commit_linked(project, StoryNo::new(1), "a04a8c4"))
            .expect("reading the link table"),
        "the operator's `--legacy-links` assertion must project the comment into a link record"
    );
}

/// The mixed-provenance case the council's decision named directly: one
/// document carrying two `[git]`-shaped comments, one meant to represent
/// genuine pre-#18 history and one meant to represent a live-era comment that
/// merely matches the shape. `--legacy-links` cannot tell them apart — nothing
/// can, from the document alone — so both are promoted, and both must surface
/// through `unbacked_commit_links` (`story doctor`'s `legacy_link_advice`),
/// since a `--legacy-links` restore is exactly the case that advisory exists
/// to catch when the operator's assertion was wrong for one comment out of
/// several.
#[test]
fn a_legacy_links_restore_promotes_and_surfaces_both_a_genuine_and_a_lookalike_comment() {
    let fixture = ServiceFixture::new();
    let genuine = create(&fixture, "Synced before kind #18");
    StoryService::new(&fixture.ctx())
        .comment(&genuine, "[git] a04a8c4: an old link record")
        .expect("commenting the genuine legacy link");
    let lookalike = create(&fixture, "Created after kind #18 existed");
    StoryService::new(&fixture.ctx())
        .comment(&lookalike, "[git] b13e9f2: pretend a user typed this")
        .expect("commenting the live-era lookalike");

    let exported = export(&fixture);
    let (store, dir) = empty_store();
    transfer::import_project(&store, dir.path(), &Clock::System, &exported, true)
        .expect("importing");
    let project = restored_project(&store, dir.path()).id;

    assert!(
        store
            .read(|tx| tx.commit_linked(project, StoryNo::new(1), "a04a8c4"))
            .expect("reading the link table"),
        "the genuine legacy comment must be promoted"
    );
    assert!(
        store
            .read(|tx| tx.commit_linked(project, StoryNo::new(2), "b13e9f2"))
            .expect("reading the link table"),
        "the lookalike is promoted too — the flag is document-wide, not per-comment"
    );

    let unbacked = store
        .read(|tx| tx.unbacked_commit_links(project))
        .expect("reading unbacked commit links");
    assert_eq!(
        unbacked,
        vec![
            (StoryNo::new(1), "a04a8c4".to_string()),
            (StoryNo::new(2), "b13e9f2".to_string()),
        ],
        "both rows have no backing `StoryCommitLinked` event, so doctor's advisory must \
         surface both regardless of which one was actually genuine"
    );

    let ctx = storyhook::service::Ctx::new(
        &store,
        project,
        dir.path(),
        storyhook::env::Environment::at(dir.path()),
    );
    let Response::Issues(issues) =
        dispatch(&ctx, Invocation::Doctor { fix: false }).expect("running doctor")
    else {
        panic!("no other integrity issue exists here; doctor must answer with advisory issues");
    };
    let report = issues.join("\n");
    assert!(
        report.contains("a04a8c4") && report.contains("b13e9f2"),
        "the advisory must name both commits doctor cannot vouch for: {report}"
    );
}

// --- the batch importer -----------------------------------------------------

#[test]
fn importing_descriptions_creates_one_story_each_in_order() {
    let fixture = ServiceFixture::new();
    let batch = TransferService::new(&fixture.ctx())
        .import(&[described("First"), described("Second")])
        .expect("importing");

    let ids: Vec<&str> = batch.views.iter().map(|v| v.story.id.as_str()).collect();
    assert_eq!(ids, ["SH-1", "SH-2"]);
    assert_eq!(batch.views[0].story.title, "First");
    assert!(batch.relationship_lines.is_empty());
    fixture.assert_no_drift();
}

/// SH-164: a bulk-import document is exactly as untrusted as a REST body —
/// nothing guarantees its labels were split on comma before it got here.
#[test]
fn importing_splits_a_comma_bearing_label() {
    let fixture = ServiceFixture::new();
    let story = ImportStory {
        labels: Some(vec!["a,b".to_string()]),
        ..described("Comma in import")
    };
    let batch = TransferService::new(&fixture.ctx())
        .import(&[story])
        .expect("importing");
    assert_eq!(batch.views[0].story.labels, ["a", "b"]);
    fixture.assert_no_drift();
}

#[test]
fn an_unparseable_priority_is_dropped_rather_than_rejected() {
    // `story new --priority urgent` is an error; `story import` has always
    // ignored the field instead, and scripts depend on it.
    let fixture = ServiceFixture::new();
    let story = ImportStory {
        priority: Some("urgent".to_string()),
        ..described("Lenient")
    };
    let batch = TransferService::new(&fixture.ctx())
        .import(&[story])
        .expect("importing");
    assert_eq!(
        batch.views[0].story.priority,
        storyhook::domain::Priority::None
    );

    let rejected = StoryService::new(&fixture.ctx()).create(&NewStoryInput {
        title: "Strict".to_string(),
        priority: Some("urgent".to_string()),
        ..NewStoryInput::default()
    });
    assert!(
        matches!(rejected, Err(AppError::Validation(_))),
        "`story new` must still reject what `story import` tolerates: {rejected:?}"
    );
}

#[test]
fn one_unknown_type_anywhere_in_the_batch_creates_no_stories_at_all() {
    let fixture = ServiceFixture::new();
    let good = ImportStory {
        story_type: Some("bug".to_string()),
        ..described("Fine")
    };
    let bad = ImportStory {
        story_type: Some("nonsense".to_string()),
        ..described("Broken")
    };
    let error = TransferService::new(&fixture.ctx())
        .import(&[good, bad])
        .expect_err("an unknown type must reject the batch");
    assert!(
        error.to_string().contains("unknown types: nonsense")
            && error.to_string().contains("Available types:"),
        "{error}"
    );

    let stored = fixture
        .store()
        .read(|tx| tx.stories(fixture.project(), &StoryQuery::all()))
        .expect("reading back");
    assert!(stored.is_empty(), "the whole batch must have rolled back");
}

#[test]
fn a_rejected_batch_returns_the_story_numbers_it_had_allocated() {
    let fixture = ServiceFixture::new();
    let bad = ImportStory {
        story_type: Some("nonsense".to_string()),
        ..described("Broken")
    };
    let _ = TransferService::new(&fixture.ctx()).import(&[described("Fine"), bad]);
    assert_eq!(
        create(&fixture, "After the failure"),
        "SH-1",
        "a rolled-back allocation must not burn a story number"
    );
}

#[test]
fn relationships_are_resolved_by_index_within_the_batch() {
    let fixture = ServiceFixture::new();
    let child = ImportStory {
        relationships: Some(vec![ImportRelationship {
            relation: "child-of".to_string(),
            ref_index: Some(0),
            other_id: None,
        }]),
        ..described("Child")
    };
    let batch = TransferService::new(&fixture.ctx())
        .import(&[described("Parent"), child])
        .expect("importing");

    assert_eq!(batch.relationship_lines, ["SH-2 child-of SH-1"]);
    let parent = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .expect("reading")
        .expect("SH-1");
    assert!(
        parent
            .snapshot
            .relationships
            .iter()
            .any(|r| r.relation == "parent-of" && r.other_id == "SH-2"),
        "both ends must claim the edge: {:?}",
        parent.snapshot.relationships
    );
    fixture.assert_no_drift();
}

#[test]
fn a_relationship_naming_a_story_outside_the_batch_links_the_existing_one() {
    let fixture = ServiceFixture::new();
    let existing = create(&fixture, "Already here");
    let story = ImportStory {
        relationships: Some(vec![ImportRelationship {
            relation: "blocks".to_string(),
            ref_index: None,
            other_id: Some(existing.clone()),
        }]),
        ..described("New")
    };
    TransferService::new(&fixture.ctx())
        .import(&[story])
        .expect("importing");

    let blocked = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .expect("reading")
        .expect("SH-1");
    assert!(
        blocked
            .snapshot
            .relationships
            .iter()
            .any(|r| r.relation == "blocked-by" && r.other_id == "SH-2")
    );
    fixture.assert_no_drift();
}

#[test]
fn an_out_of_range_ref_index_is_a_validation_error() {
    let fixture = ServiceFixture::new();
    let story = ImportStory {
        relationships: Some(vec![ImportRelationship {
            relation: "blocks".to_string(),
            ref_index: Some(9),
            other_id: None,
        }]),
        ..described("Dangling")
    };
    let error = TransferService::new(&fixture.ctx())
        .import(&[story])
        .expect_err("an out-of-range index must be rejected");
    assert!(
        error.to_string().contains("ref_index 9 out of bounds"),
        "{error}"
    );
}

#[test]
fn a_relationship_with_neither_an_index_nor_an_id_is_rejected() {
    let fixture = ServiceFixture::new();
    let story = ImportStory {
        relationships: Some(vec![ImportRelationship {
            relation: "blocks".to_string(),
            ref_index: None,
            other_id: None,
        }]),
        ..described("Underspecified")
    };
    let error = TransferService::new(&fixture.ctx())
        .import(&[story])
        .expect_err("an unaddressed relationship must be rejected");
    assert!(
        error
            .to_string()
            .contains("relationship must have ref_index or other_id"),
        "{error}"
    );
}

#[test]
fn a_relation_to_a_story_that_does_not_exist_is_refused_rather_than_half_written() {
    // The legacy importer wrote one end and skipped the other, leaving the
    // dangling edge that is SH-60. The read model's foreign key makes it
    // unrepresentable.
    let fixture = ServiceFixture::new();
    let story = ImportStory {
        relationships: Some(vec![ImportRelationship {
            relation: "blocks".to_string(),
            ref_index: None,
            other_id: Some("SH-404".to_string()),
        }]),
        ..described("Points at nothing")
    };
    let error = TransferService::new(&fixture.ctx())
        .import(&[story])
        .expect_err("a dangling relation must be refused");
    assert!(
        matches!(error, AppError::Validation(_) | AppError::Integrity(_)),
        "{error}"
    );
    let stored = fixture
        .store()
        .read(|tx| tx.stories(fixture.project(), &StoryQuery::all()))
        .expect("reading back");
    assert!(stored.is_empty(), "nothing may survive the rejection");
}

// --- import-project ---------------------------------------------------------

/// An empty store with nothing in it, and the directory it is anchored to.
fn empty_store() -> (SqliteStore, tempfile::TempDir) {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).expect("opening the store");
    store.migrate().expect("migrating");
    (store, dir)
}

#[test]
fn a_project_round_trips_through_export_and_import_byte_for_byte() {
    let fixture = ServiceFixture::new();
    fixture.add_member("ada", "Ada Lovelace", Some("ada"));
    let parent = create(&fixture, "Parent");
    let child = create(&fixture, "Child");
    create(&fixture, "Doomed");
    storyhook::service::RelationService::new(&fixture.ctx())
        .relate(&parent, "parent-of", &child, false)
        .expect("relating");
    StoryService::new(&fixture.ctx())
        .comment(&parent, "A remark.")
        .expect("commenting");
    StoryService::new(&fixture.ctx())
        .set_state("SH-3", "done", None, None)
        .expect("closing SH-3");

    let first = document(&export(&fixture));

    let (store, dir) = empty_store();
    let outcome = transfer::import_project(
        &store,
        dir.path(),
        &Clock::Fixed("2026-01-01T00:00:00Z".to_string()),
        &serde_json::from_str(&first).expect("parsing the document"),
        false,
    )
    .expect("importing");
    assert_eq!(outcome.stories, 3);

    let project = restored_project(&store, dir.path());
    let ctx = storyhook::service::Ctx::new(
        &store,
        project.id,
        dir.path(),
        storyhook::env::Environment::at(dir.path()),
    );
    let second = document(&TransferService::new(&ctx).export().expect("re-exporting"));

    assert_eq!(first, second, "the round trip must be byte-identical");
}

#[test]
fn an_imported_project_continues_numbering_after_its_highest_story() {
    let fixture = ServiceFixture::new();
    for index in 1..=4 {
        create(&fixture, &format!("Story {index}"));
    }
    let exported = export(&fixture);

    let (store, dir) = empty_store();
    transfer::import_project(&store, dir.path(), &Clock::System, &exported, false)
        .expect("importing");
    let project = restored_project(&store, dir.path()).id;
    let ctx = storyhook::service::Ctx::new(
        &store,
        project,
        dir.path(),
        storyhook::env::Environment::at(dir.path()),
    );
    let next = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "After the import".to_string(),
            ..NewStoryInput::default()
        })
        .expect("creating");
    assert_eq!(
        next.id, "SH-5",
        "the counter must resume above the imported ids"
    );
}

#[test]
fn importing_into_a_project_that_already_holds_stories_is_refused() {
    let fixture = ServiceFixture::new();
    create(&fixture, "In the way");
    let exported = export(&fixture);

    let (store, dir) = empty_store();
    transfer::import_project(&store, dir.path(), &Clock::System, &exported, false)
        .expect("the first restore into an empty project");
    let error = transfer::import_project(&store, dir.path(), &Clock::System, &exported, false)
        .expect_err("a restore must not half-overwrite a live project");
    assert!(
        error.to_string().contains("already holds stories"),
        "{error}"
    );
}

#[test]
fn a_restore_leaves_the_directory_carrying_the_project_it_restored() {
    // The whole of what identifies an imported checkout. Nothing else does:
    // the export document has no uuid, the directory is not a key any more,
    // and the restore registers no origin.
    let fixture = ServiceFixture::new();
    create(&fixture, "Restored");
    let exported = export(&fixture);

    let (store, dir) = empty_store();
    transfer::import_project(&store, dir.path(), &Clock::System, &exported, false)
        .expect("importing");

    let pointer = storyhook::service::project::read_pointer(dir.path())
        .expect("reading the pointer file")
        .expect("the restore must write one");
    let project = store
        .read(|tx| tx.project_by_uuid(&pointer.uuid))
        .expect("reading the store")
        .expect("the pointer must name a project this store holds");
    assert_eq!(
        project.prefix, "SH",
        "the pointer carries the prefix the document was imported under"
    );
}

#[test]
fn a_restore_into_a_directory_that_already_names_a_project_never_mints_a_second() {
    // The other half of the pointer: `importing_into_a_project_that_already
    // _holds_stories_is_refused` above can only refuse because the second run
    // recognizes the first one's directory, and the pointer file is now the
    // only thing that lets it.
    let fixture = ServiceFixture::new();
    let exported = export(&fixture);

    let (store, dir) = empty_store();
    transfer::import_project(&store, dir.path(), &Clock::System, &exported, false)
        .expect("the first restore");
    transfer::import_project(&store, dir.path(), &Clock::System, &exported, false)
        .expect("a second restore into an empty project is idempotent, not a second project");

    assert_eq!(
        store.read(|tx| tx.projects()).expect("listing").len(),
        1,
        "two restores into one directory must not produce two projects"
    );
}

#[test]
fn a_restore_into_a_second_empty_store_leaves_a_resolvable_pointer() {
    // SH-190: the directory restored into already carries a pointer file
    // (written by the first restore) naming a uuid the *second* store has
    // never heard of. `import_project`'s create branch used to mint a fresh
    // project and its own uuid in the second store, while the pointer is
    // "never overwritten when one already exists" -- so the file on disk
    // kept naming the first store's uuid, and every command run from this
    // directory afterward could not find the project it just restored. The
    // fix adopts the pointer's own uuid instead of minting a new one.
    let fixture = ServiceFixture::new();
    create(&fixture, "Restored");
    let exported = export(&fixture);

    let (first_store, dir) = empty_store();
    transfer::import_project(&first_store, dir.path(), &Clock::System, &exported, false)
        .expect("the first restore, into the first store");
    let original_pointer = storyhook::service::project::read_pointer(dir.path())
        .expect("reading the pointer file")
        .expect("the first restore must leave a pointer file behind");

    let (second_store, _unused_dir) = empty_store();
    transfer::import_project(&second_store, dir.path(), &Clock::System, &exported, false)
        .expect("restoring the same backup into a second, empty store");

    let pointer = storyhook::service::project::read_pointer(dir.path())
        .expect("reading the pointer file")
        .expect("a restore must leave a pointer file behind");
    assert_eq!(
        pointer.uuid, original_pointer.uuid,
        "the pointer file is never rewritten -- it already named the right identity"
    );
    let project = second_store
        .read(|tx| tx.project_by_uuid(&pointer.uuid))
        .expect("reading the store")
        .expect("the pointer must name a project the second store holds");
    assert_eq!(
        project.uuid, original_pointer.uuid,
        "the second store's project must be created under the identity the pointer already names"
    );
    assert_eq!(project.prefix, "SH");
}

#[test]
fn a_pointer_naming_an_unparseable_uuid_is_rejected_rather_than_adopted() {
    // SH-190's fix adopts a stale pointer's uuid verbatim; a hand-edited or
    // corrupted pointer must not have that string written into `projects.uuid`
    // unvalidated.
    let fixture = ServiceFixture::new();
    create(&fixture, "Restored");
    let exported = export(&fixture);

    let (store, dir) = empty_store();
    storyhook::service::project::write_pointer(
        dir.path(),
        &storyhook::service::project::ProjectPointer::new(
            "not-a-real-uuid".to_string(),
            "SH".to_string(),
        ),
    )
    .expect("writing a hand-crafted pointer");

    let error = transfer::import_project(&store, dir.path(), &Clock::System, &exported, false)
        .expect_err("a malformed pointer uuid must be refused, not adopted");
    assert!(error.to_string().contains("not a valid uuid"), "{error}");
    assert!(
        store.read(|tx| tx.projects()).expect("listing").is_empty(),
        "a rejected restore must leave no project behind"
    );
}

#[test]
fn a_second_checkout_with_the_same_stale_pointer_cannot_restore_into_the_same_store() {
    // Two checkouts committed the same pointer file (e.g. two clones of one
    // repository) before their common store was lost. Restoring the backup
    // into the first checkout adopts the shared uuid (SH-190); restoring the
    // *same* backup into the second checkout, against the same store, must
    // not silently mint a second project under a colliding identity -- it
    // lands in the ordinary "already holds stories" refusal once the first
    // restore has claimed that uuid.
    let fixture = ServiceFixture::new();
    create(&fixture, "Restored");
    let exported = export(&fixture);

    let (store, first_dir) = empty_store();
    let second_dir = scratch_dir();
    let shared_pointer = storyhook::service::project::ProjectPointer::new(
        uuid::Uuid::new_v4().to_string(),
        "SH".to_string(),
    );
    storyhook::service::project::write_pointer(first_dir.path(), &shared_pointer)
        .expect("writing the first checkout's pointer");
    storyhook::service::project::write_pointer(second_dir.path(), &shared_pointer)
        .expect("writing the second checkout's pointer");

    transfer::import_project(&store, first_dir.path(), &Clock::System, &exported, false)
        .expect("the first checkout's restore");
    let error =
        transfer::import_project(&store, second_dir.path(), &Clock::System, &exported, false)
            .expect_err("a second checkout claiming the same identity must not mint a duplicate");
    assert!(
        error.to_string().contains("already holds stories"),
        "{error}"
    );
    assert_eq!(
        store.read(|tx| tx.projects()).expect("listing").len(),
        1,
        "only the first checkout's restore may produce a project"
    );
}

#[test]
fn a_document_whose_ids_do_not_match_its_prefix_is_rejected_whole() {
    let (store, dir) = empty_store();
    let mut export = ProjectExport {
        schema: 1,
        prefix: Some("AB".to_string()),
        states: storyhook_test_support::default_states(),
        types: storyhook_test_support::default_types(),
        members: Vec::new(),
        settings: None,
        remotes: Vec::new(),
        github_sync: None,
        github_bases: std::collections::BTreeMap::new(),
        stories: Vec::new(),
    };
    export
        .stories
        .push(storyhook::service::transfer::ExportedStory {
            id: "ZZ-1".to_string(),
            events: vec![storyhook::service::transfer::ExportedEvent::Known(
                storyhook::domain::StoryEvent::StoryCreated {
                    at: "2026-01-01T00:00:00Z".to_string(),
                    title: "Foreign".to_string(),
                    state: "todo".to_string(),
                },
            )],
            archived: false,
        });

    let error = transfer::import_project(&store, dir.path(), &Clock::System, &export, false)
        .expect_err("a foreign prefix must be rejected");
    assert!(
        error.to_string().contains("does not belong to a project"),
        "{error}"
    );
    assert!(
        store.read(|tx| tx.projects()).expect("reading").is_empty(),
        "a rejected import must leave no project behind"
    );
}

// --- github-sync carry (SH-189) ---------------------------------------------

/// A story's read-model snapshot, usable as a stand-in github-sync merge
/// base — the base's own contents are not this test's subject, only whether
/// it survives the round trip intact.
fn story_snapshot(
    fixture: &ServiceFixture,
    id: &str,
) -> (StoryNo, storyhook::domain::StorySnapshot) {
    let story_no = StoryNo::parse_id("SH", id).expect("parsing the story id");
    let snapshot = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), story_no))
        .expect("reading the story")
        .expect("the story exists")
        .snapshot;
    (story_no, snapshot)
}

#[test]
fn github_sync_and_its_bases_round_trip_through_export_and_import() {
    let fixture = ServiceFixture::new();
    let with_base = create(&fixture, "Has synced before");
    let without_base = create(&fixture, "Never synced");

    let blob = serde_json::json!({
        "github": {"owner": "acme", "repo": "widgets"},
        "sync": {"mode": "manual"},
        "mappings": [
            {"story_id": with_base, "issue_number": 42, "last_synced_at": "2026-01-01T00:00:00Z"},
            {"story_id": without_base, "issue_number": 43, "last_synced_at": "2026-01-01T00:00:00Z"},
        ],
    });
    fixture
        .store()
        .write(|tx| {
            let mut settings = tx.settings(fixture.project())?;
            settings.github_sync = Some(blob.clone());
            tx.put_settings(fixture.project(), &settings)
        })
        .expect("configuring github-sync");

    let (base_story_no, base_snapshot) = story_snapshot(&fixture, &with_base);
    fixture
        .store()
        .write(|tx| tx.put_github_base(fixture.project(), base_story_no, &base_snapshot))
        .expect("writing a merge base");

    let exported = export(&fixture);
    assert_eq!(exported.github_sync.as_ref(), Some(&blob));
    assert_eq!(
        exported.github_bases.len(),
        1,
        "only the story that actually has a base is carried: {:?}",
        exported.github_bases.keys().collect::<Vec<_>>()
    );
    assert!(exported.github_bases.contains_key(&with_base));
    assert!(!exported.github_bases.contains_key(&without_base));

    let (store, dir) = empty_store();
    transfer::import_project(&store, dir.path(), &Clock::System, &exported, false)
        .expect("importing");
    let project = restored_project(&store, dir.path());

    let restored_settings = store
        .read(|tx| tx.settings(project.id))
        .expect("reading the restored settings");
    assert_eq!(
        restored_settings.github_sync,
        Some(blob),
        "the configuration blob must survive verbatim"
    );

    let restored_base = store
        .read(|tx| tx.github_base(project.id, base_story_no))
        .expect("reading the restored base")
        .expect("the base that was carried must be there");
    assert_eq!(restored_base, base_snapshot);

    let without_base_no = StoryNo::parse_id("SH", &without_base).expect("parsing");
    let missing = store
        .read(|tx| tx.github_base(project.id, without_base_no))
        .expect("reading");
    assert!(
        missing.is_none(),
        "a story with no base before export must not be backfilled with one by the restore"
    );
}

#[test]
fn a_document_with_no_github_sync_configured_leaves_an_adopted_projects_settings_untouched() {
    // Mirrors `apply_settings`'s existing rule for `sync`/`doctor`: a document
    // carrying no github-sync blob at all (any backup taken before this
    // existed, or of a project that never configured it) must not clear a
    // configuration already present in the project being restored into.
    //
    // Two restores into the same directory, because that is the only way to
    // reach the "adopt an existing project" branch of `import_project`: the
    // first establishes a project with github-sync already configured and no
    // stories yet; the second, a document that carries stories but no
    // github-sync blob at all, adopts that same (still story-less) project.
    let configured = ServiceFixture::new();
    configured
        .store()
        .write(|tx| {
            let mut settings = tx.settings(configured.project())?;
            settings.github_sync = Some(serde_json::json!({"already": "configured"}));
            tx.put_settings(configured.project(), &settings)
        })
        .expect("configuring github-sync");
    let first_document = export(&configured);
    assert!(first_document.stories.is_empty());

    let (store, dir) = empty_store();
    transfer::import_project(&store, dir.path(), &Clock::System, &first_document, false)
        .expect("the first restore");

    let unconfigured = ServiceFixture::new();
    create(&unconfigured, "Adopted story");
    let mut second_document = export(&unconfigured);
    second_document.github_sync = None;
    assert!(!second_document.stories.is_empty());

    transfer::import_project(&store, dir.path(), &Clock::System, &second_document, false)
        .expect("the second restore, adopting the same project");

    let project = restored_project(&store, dir.path());
    let settings = store.read(|tx| tx.settings(project.id)).expect("reading");
    assert_eq!(
        settings.github_sync,
        Some(serde_json::json!({"already": "configured"})),
        "a document with no github-sync must not blank what the adopted project already has"
    );
}

#[test]
fn an_orphan_github_base_rejects_the_whole_restore() {
    let fixture = ServiceFixture::new();
    let id = create(&fixture, "Only story");
    let (_story_no, snapshot) = story_snapshot(&fixture, &id);

    let mut exported = export(&fixture);
    exported.github_bases.insert("SH-999".to_string(), snapshot);

    let (store, dir) = empty_store();
    let error = transfer::import_project(&store, dir.path(), &Clock::System, &exported, false)
        .expect_err("a base naming a story absent from the document must reject the restore");
    assert!(
        error.to_string().contains("SH-999"),
        "the error must name the offending id: {error}"
    );

    assert!(
        storyhook::service::project::read_pointer(dir.path())
            .expect("reading the pointer file")
            .is_none(),
        "a rejected restore must leave nothing behind, not even the pointer file"
    );
    assert_eq!(
        store.read(|tx| tx.projects()).expect("listing").len(),
        0,
        "a rejected restore must not create a project either"
    );
}

// --- through dispatch -------------------------------------------------------

#[test]
fn the_export_arm_answers_with_the_raw_document() {
    let fixture = ServiceFixture::new();
    create(&fixture, "Exported");
    let response = dispatch(&fixture.ctx(), Invocation::Export).expect("exporting");
    let Response::RawJson(json) = response else {
        panic!("`story export` must answer with RawJson, not a wrapped message");
    };
    let parsed: ProjectExport = serde_json::from_str(&json).expect("the document must re-parse");
    assert_eq!(parsed.stories.len(), 1);
}

#[test]
fn the_decompose_arm_summarizes_the_stories_and_relationships_it_created() {
    let fixture = ServiceFixture::new();
    let dir = scratch_dir();
    let spec = dir.path().join("plan.md");
    std::fs::write(
        &spec,
        "# Plan\n\n## Phase 1: Setup\n\n- Set up the database\n- Wire the client\n",
    )
    .expect("writing the spec");

    let response = dispatch(
        &fixture.ctx(),
        Invocation::Decompose {
            file: Some(spec.to_string_lossy().into_owned()),
            stdin: false,
            dry_run: false,
        },
    )
    .expect("decomposing");
    let Response::Stories(views, Some(summary)) = response else {
        panic!("`story decompose` must answer with stories and a summary");
    };
    assert!(!views.is_empty());
    assert!(summary.starts_with("Created "), "{summary}");
    fixture.assert_no_drift();
}

#[test]
fn a_dry_run_decompose_writes_nothing() {
    let fixture = ServiceFixture::new();
    let dir = scratch_dir();
    let spec = dir.path().join("plan.md");
    std::fs::write(&spec, "# Plan\n\n- Do the thing\n").expect("writing the spec");

    dispatch(
        &fixture.ctx(),
        Invocation::Decompose {
            file: Some(spec.to_string_lossy().into_owned()),
            stdin: false,
            dry_run: true,
        },
    )
    .expect("decomposing");
    let stored = fixture
        .store()
        .read(|tx| tx.stories(fixture.project(), &StoryQuery::all()))
        .expect("reading back");
    assert!(stored.is_empty(), "a dry run must create nothing");
}

#[test]
fn decompose_without_a_file_or_stdin_explains_the_usage() {
    let fixture = ServiceFixture::new();
    let error = dispatch(
        &fixture.ctx(),
        Invocation::Decompose {
            file: None,
            stdin: false,
            dry_run: false,
        },
    )
    .expect_err("decompose needs an input");
    assert!(matches!(error, AppError::Usage(_)), "{error}");
    assert!(error.to_string().contains("story decompose"), "{error}");
}

#[test]
fn importing_an_empty_document_says_so_without_writing() {
    let fixture = ServiceFixture::new();
    let dir = scratch_dir();
    let file = dir.path().join("stories.json");
    std::fs::write(&file, "[]").expect("writing");
    let response = dispatch(
        &fixture.ctx(),
        Invocation::Import {
            file: Some(file.to_string_lossy().into_owned()),
        },
    )
    .expect("importing");
    assert!(matches!(response, Response::Message(ref m) if m == "no stories to import"));
}
