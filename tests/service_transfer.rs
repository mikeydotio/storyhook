//! `TransferService` — export, the batch importer, and the project restore.
//!
//! The round trip is the headline: a project exported and re-imported into an
//! empty store must produce a byte-identical document. That is the same
//! property `tests/story_export.rs` asserts of the legacy path, and it is what
//! makes the flip a two-way door.

use storyhook::cli::Invocation;
use storyhook::domain::{ImportRelationship, ImportStory};
use storyhook::error::AppError;
use storyhook::invoke::dispatch;
use storyhook::output::Response;
use storyhook::service::{Clock, NewStoryInput, StoryService, TransferService, transfer};
use storyhook::storage::ProjectExport;
use storyhook::store::{ReadOps, SqliteStore, Store, StoryNo, StoryQuery};
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
    assert_eq!(export.states.len(), 3);
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
    transfer::import_project(&store, dir.path(), &Clock::System, &exported).expect("importing");

    let project = store
        .read(|tx| tx.project_by_path(&dir.path().canonicalize().unwrap()))
        .expect("reading")
        .expect("the imported project");
    let ctx = storyhook::service::Ctx::new(&store, project.id, dir.path());
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
    let count = transfer::import_project(
        &store,
        dir.path(),
        &Clock::Fixed("2026-01-01T00:00:00Z".to_string()),
        &serde_json::from_str(&first).expect("parsing the document"),
    )
    .expect("importing");
    assert_eq!(count, 3);

    let project = store
        .read(|tx| tx.project_by_path(&dir.path().canonicalize().unwrap()))
        .expect("reading")
        .expect("the imported project");
    let ctx = storyhook::service::Ctx::new(&store, project.id, dir.path());
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
    transfer::import_project(&store, dir.path(), &Clock::System, &exported).expect("importing");
    let project = store
        .read(|tx| tx.project_by_path(&dir.path().canonicalize().unwrap()))
        .expect("reading")
        .expect("the imported project")
        .id;
    let ctx = storyhook::service::Ctx::new(&store, project, dir.path());
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
    transfer::import_project(&store, dir.path(), &Clock::System, &exported)
        .expect("the first restore into an empty project");
    let error = transfer::import_project(&store, dir.path(), &Clock::System, &exported)
        .expect_err("a restore must not half-overwrite a live project");
    assert!(
        error.to_string().contains("already holds stories"),
        "{error}"
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
        stories: Vec::new(),
    };
    export.stories.push(storyhook::storage::ExportedStory {
        id: "ZZ-1".to_string(),
        events: vec![storyhook::domain::StoryEvent::StoryCreated {
            at: "2026-01-01T00:00:00Z".to_string(),
            title: "Foreign".to_string(),
            state: "todo".to_string(),
        }],
        archived: false,
    });

    let error = transfer::import_project(&store, dir.path(), &Clock::System, &export)
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
