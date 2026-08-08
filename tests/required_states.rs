//! The state set every project must have, and what happens to one that lacks it
//! (SH-125).
//!
//! `todo`, `in-progress` and `blocked` OPEN, `done` CLOSED. A project may hold
//! as many further states as it likes; it may not hold fewer than these.
//!
//! The fixtures below build catalogs the service API refuses to produce, by
//! seeding a project through `put_states` — the store's raw writer, which is
//! deliberately *not* where the floor lives. That is the only way to obtain the
//! subject of these tests: a project written before the floor existed.

use storyhook::domain::{REQUIRED_STATES, StateDef, SuperState};
use storyhook::service::transfer::ProjectExport;
use storyhook::service::{
    Clock, ConfigService, Ctx, IntegrityService, NewStoryInput, StoryService, transfer,
};
use storyhook::store::{ReadOps, SqliteStore, Store, StoryNo};
use storyhook_test_support::{ServiceFixture, scratch_dir};

// --- helpers ---------------------------------------------------------------

fn state(slug: &str, super_state: SuperState) -> StateDef {
    StateDef {
        slug: slug.to_string(),
        super_state,
        role: None,
        description: None,
    }
}

/// The catalog as slugs, in board order.
fn slugs(fixture: &ServiceFixture) -> Vec<String> {
    fixture
        .store()
        .read(|tx| tx.states(fixture.project()))
        .expect("reading states")
        .into_iter()
        .map(|state| state.slug)
        .collect()
}

/// A project whose catalog predates the floor: no `blocked`.
fn below_the_floor() -> ServiceFixture {
    ServiceFixture::with_states(&[
        state("todo", SuperState::Open),
        state("in-progress", SuperState::Open),
        state("done", SuperState::Closed),
    ])
}

fn new_story(ctx: &Ctx<'_, SqliteStore>, title: &str) -> String {
    StoryService::new(ctx)
        .create(&NewStoryInput {
            title: title.to_string(),
            ..NewStoryInput::default()
        })
        .expect("creating a story")
        .id
}

fn state_of(fixture: &ServiceFixture, id: &str) -> String {
    let no = StoryNo::parse_id("SH", id).expect("a well-formed id");
    fixture
        .store()
        .read(|tx| tx.story(fixture.project(), no))
        .expect("reading the story")
        .expect("the story exists")
        .snapshot
        .state
}

// --- what the floor is -----------------------------------------------------

#[test]
fn the_floor_is_four_states_and_their_superstates() {
    let pairs: Vec<(&str, &SuperState)> = REQUIRED_STATES
        .iter()
        .map(|required| (required.slug, &required.super_state))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("todo", &SuperState::Open),
            ("in-progress", &SuperState::Open),
            ("blocked", &SuperState::Open),
            ("done", &SuperState::Closed),
        ]
    );
}

// --- doctor: the repair every refusal names --------------------------------

#[test]
fn doctor_reports_a_project_below_the_floor() {
    let fixture = below_the_floor();
    let ctx = fixture.ctx();
    let issues = IntegrityService::new(&ctx).report().expect("reporting");
    assert_eq!(issues.len(), 1, "{issues:#?}");
    assert!(issues[0].contains("blocked"), "{}", issues[0]);
}

#[test]
fn doctor_fix_adds_the_missing_state_and_says_so() {
    let fixture = below_the_floor();
    let ctx = fixture.ctx();
    let message = IntegrityService::new(&ctx).fix().expect("fixing");
    assert!(
        message.contains("added 1 required state"),
        "the repair must be reported, not silent: {message}"
    );
    assert_eq!(slugs(&fixture), ["todo", "in-progress", "blocked", "done"]);
    assert!(
        IntegrityService::new(&ctx)
            .report()
            .expect("re-reporting")
            .is_empty()
    );
}

/// A healthy project must not be told it was repaired.
#[test]
fn doctor_fix_adds_nothing_to_a_conforming_project() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let message = IntegrityService::new(&ctx).fix().expect("fixing");
    assert_eq!(message, "doctor found nothing to fix");
}

/// The `agentics` shape from the live store: `todo|done`, two states short.
#[test]
fn doctor_fix_repairs_a_two_state_project_in_one_pass() {
    let fixture = ServiceFixture::with_states(&[
        state("todo", SuperState::Open),
        state("done", SuperState::Closed),
    ]);
    let ctx = fixture.ctx();
    let message = IntegrityService::new(&ctx).fix().expect("fixing");
    assert!(message.contains("added 2 required states"), "{message}");
    assert_eq!(slugs(&fixture), ["todo", "in-progress", "blocked", "done"]);
}

/// The repair adds; it never reinterprets. Flipping this project's `done` to
/// CLOSED would reclassify the stories sitting in it, which is a migration and
/// not a repair.
#[test]
fn doctor_will_not_reclassify_a_required_state_that_already_exists() {
    let fixture = ServiceFixture::with_states(&[
        state("todo", SuperState::Open),
        state("in-progress", SuperState::Open),
        state("blocked", SuperState::Open),
        state("done", SuperState::Open),
        state("shipped", SuperState::Closed),
    ]);
    let ctx = fixture.ctx();
    let error = IntegrityService::new(&ctx).fix().unwrap_err().to_string();
    assert!(error.contains("will not reclassify"), "{error}");
    assert_eq!(
        slugs(&fixture),
        ["todo", "in-progress", "blocked", "done", "shipped"],
        "the refused repair changed the catalog"
    );
}

/// A repair must not move the state `story new` puts a story in.
#[test]
fn a_repair_leaves_the_state_new_stories_open_in_alone() {
    let fixture = ServiceFixture::with_states(&[
        state("backlog", SuperState::Open),
        state("todo", SuperState::Open),
        state("in-progress", SuperState::Open),
        state("done", SuperState::Closed),
    ]);
    let ctx = fixture.ctx();
    let before = new_story(&ctx, "before the repair");
    IntegrityService::new(&ctx).fix().expect("fixing");
    let after = new_story(&ctx, "after the repair");

    assert_eq!(state_of(&fixture, &before), "backlog");
    assert_eq!(
        state_of(&fixture, &after),
        "backlog",
        "the repair took position 0: {:?}",
        slugs(&fixture)
    );
    assert_eq!(
        slugs(&fixture),
        ["backlog", "todo", "in-progress", "blocked", "done"],
        "a missing OPEN state joins the end of the OPEN run"
    );
}

// --- a user edit is refused, never repaired --------------------------------

#[test]
fn a_project_below_the_floor_cannot_have_its_catalog_edited() {
    // Not "cannot be read", and not "is silently repaired": the project works,
    // and the one thing it cannot do is drift further from the floor.
    let fixture = below_the_floor();
    let ctx = fixture.ctx();
    let error = ConfigService::new(&ctx)
        .add_state("in-review", SuperState::Open, None, None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("blocked"), "{error}");
    assert!(error.contains("story doctor --fix"), "{error}");

    // …and using it is unaffected.
    let id = new_story(&ctx, "still usable");
    assert_eq!(state_of(&fixture, &id), "todo");
}

// --- import repairs, because a document may predate the floor --------------

#[test]
fn importing_a_document_below_the_floor_repairs_its_catalog() {
    let dir = scratch_dir();
    let store = SqliteStore::open(dir.path().join("store.db")).expect("opening the store");
    store.migrate().expect("migrating");

    let document = ProjectExport {
        schema: 1,
        prefix: Some("OLD".to_string()),
        states: vec![
            state("todo", SuperState::Open),
            state("in-progress", SuperState::Open),
            state("done", SuperState::Closed),
        ],
        types: Vec::new(),
        members: Vec::new(),
        settings: None,
        remotes: Vec::new(),
        stories: Vec::new(),
    };

    let root = scratch_dir();
    transfer::import_project(&store, root.path(), &Clock::System, &document, false)
        .expect("importing");

    let restored: Vec<String> = store
        .read(|tx| {
            let pointer = storyhook::service::project::read_pointer(root.path())
                .expect("reading the pointer the restore wrote")
                .expect("a restore leaves one");
            let project = tx
                .project_by_uuid(&pointer.uuid)?
                .expect("the imported project");
            tx.states(project.id)
        })
        .expect("reading the imported catalog")
        .into_iter()
        .map(|state| state.slug)
        .collect();
    assert_eq!(
        restored,
        ["todo", "in-progress", "blocked", "done"],
        "an import repairs rather than refuses: the document may be older than the floor"
    );
}
