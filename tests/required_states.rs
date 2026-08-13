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

/// A `--fix` run's rendered report, or the error its remaining findings make.
///
/// The two production calls composed — [`IntegrityService::repair`] for what
/// the run did, [`storyhook::service::FixOutcome::verdict`] for what that
/// counts as. Nothing here re-derives the mapping: `verdict` *is* the mapping,
/// and `story doctor --fix` reaches it the same way.
///
/// The `?` is load-bearing and is the reason this is not one call (SH-270). A
/// repair that *blew up* — a story whose events will not fold, met while a
/// repair write was in flight — and a repair that *ran and left findings* are
/// different outcomes that used to arrive as the same `Err(AppError::
/// Integrity)`. Here the first propagates and the second is the verdict, so a
/// test asking for one cannot silently be handed the other.
fn fix(ctx: &Ctx<'_, SqliteStore>) -> Result<String, storyhook::error::AppError> {
    IntegrityService::new(ctx).repair()?.verdict()
}

/// What `story doctor` finds wrong with the project.
///
/// Through [`IntegrityService::examine`], which answers both halves of a
/// doctor read from one fold of the project (SH-267); this file asks about one
/// half at a time, and [`notices`] is the other.
fn findings(ctx: &Ctx<'_, SqliteStore>) -> Vec<storyhook::domain::finding::Finding> {
    IntegrityService::new(ctx)
        .examine()
        .expect("examining")
        .findings
}

/// What `story doctor` remarks on without calling it damage.
fn notices(ctx: &Ctx<'_, SqliteStore>) -> Vec<String> {
    IntegrityService::new(ctx)
        .examine()
        .expect("examining")
        .notices
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
    let issues = findings(&ctx);
    assert_eq!(issues.len(), 1, "{issues:#?}");
    assert!(
        issues[0].message.contains("blocked"),
        "{}",
        issues[0].message
    );
}

#[test]
fn doctor_fix_adds_the_missing_state_and_says_so() {
    let fixture = below_the_floor();
    let ctx = fixture.ctx();
    let message = fix(&ctx).expect("fixing");
    assert!(
        message.contains("added 1 required state"),
        "the repair must be reported, not silent: {message}"
    );
    assert_eq!(slugs(&fixture), ["todo", "in-progress", "blocked", "done"]);
    assert!(findings(&ctx).is_empty());
}

/// A healthy project must not be told it was repaired.
#[test]
fn doctor_fix_adds_nothing_to_a_conforming_project() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let message = fix(&ctx).expect("fixing");
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
    let message = fix(&ctx).expect("fixing");
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
    let error = fix(&ctx).unwrap_err().to_string();
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
    fix(&ctx).expect("fixing");
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

// --- SH-180: `move` names the same repair the catalog-edit refusal does ----

/// `story move` to a state missing only because the project predates the
/// floor gives the same guidance `add_state`'s refusal already gives for the
/// identical underlying condition — not a bare "is not defined" that leaves
/// the caller to already know `doctor --fix` exists.
#[test]
fn moving_to_a_missing_required_state_gives_the_doctor_fix_guidance() {
    let fixture = below_the_floor();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "needs blocking");

    let error = StoryService::new(&ctx)
        .set_state(&id, "blocked", None, None, None)
        .unwrap_err()
        .to_string();

    assert!(error.contains("blocked"), "{error}");
    assert!(error.contains("story doctor --fix"), "{error}");
}

/// A target slug that was simply never defined — a typo, not a gap in the
/// required floor — keeps the plain message: `doctor --fix` cannot invent a
/// state nobody asked for, so it must not be told to.
#[test]
fn moving_to_a_never_defined_state_keeps_the_plain_message() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "needs a real state");

    let error = StoryService::new(&ctx)
        .set_state(&id, "in-review", None, None, None)
        .unwrap_err()
        .to_string();

    assert_eq!(error, "state `in-review` is not defined");
    assert!(!error.contains("doctor --fix"), "{error}");
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
        github_sync: None,
        github_bases: std::collections::BTreeMap::new(),
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

// --- SH-242: a project with no resolvable active state is at the floor, not
// below it, but `is_claimable` silently stops excluding a claimed story ------

/// The floor (SH-125) requires `todo`, `in-progress` and `blocked` all OPEN —
/// three, never two — so `active_state`'s own "exactly two open states"
/// fallback can never fire for a project that clears it. A project sitting
/// exactly at the floor, with no state's role set explicitly, is healthy by
/// `report()` but must still be told.
#[test]
fn doctor_notices_a_project_at_the_floor_with_no_active_role_state() {
    let fixture = ServiceFixture::with_states(&[
        state("todo", SuperState::Open),
        state("in-progress", SuperState::Open),
        state("blocked", SuperState::Open),
        state("done", SuperState::Closed),
    ]);
    let ctx = fixture.ctx();
    assert!(
        findings(&ctx).is_empty(),
        "a project exactly at the floor is healthy"
    );
    let notices = notices(&ctx);
    assert_eq!(notices.len(), 1, "{notices:#?}");
    assert!(notices[0].contains("in-progress"), "{}", notices[0]);
    assert!(notices[0].contains("story state set"), "{}", notices[0]);
}

/// A project that has configured an active role is silent about it.
#[test]
fn doctor_is_silent_about_active_role_once_one_is_configured() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let notices = notices(&ctx);
    assert!(notices.is_empty(), "{notices:#?}");
}

/// A project with more than four OPEN states — no different from the floor
/// case; the notice fires until the project picks one.
#[test]
fn doctor_notices_a_project_with_several_open_states_and_no_active_role() {
    let fixture = ServiceFixture::with_states(&[
        state("todo", SuperState::Open),
        state("in-progress", SuperState::Open),
        state("verifying", SuperState::Open),
        state("blocked", SuperState::Open),
        state("done", SuperState::Closed),
    ]);
    let ctx = fixture.ctx();
    let notices = notices(&ctx);
    assert_eq!(notices.len(), 1, "{notices:#?}");
}

/// The notice never contributes to `--fix`'s verdict: nothing here guesses
/// which state should be active, matching `with_required_states`'s own
/// refusal to award a role during a floor repair.
#[test]
fn doctor_fix_does_not_guess_which_state_should_be_active() {
    let fixture = ServiceFixture::with_states(&[
        state("todo", SuperState::Open),
        state("in-progress", SuperState::Open),
        state("blocked", SuperState::Open),
        state("done", SuperState::Closed),
    ]);
    let ctx = fixture.ctx();
    let message = fix(&ctx).expect("fixing");
    assert!(
        message.starts_with("doctor found nothing to fix"),
        "{message}"
    );
    assert!(message.contains("story state set"), "{message}");
    let states = fixture
        .store()
        .read(|tx| tx.states(fixture.project()))
        .expect("reading states");
    assert!(
        states.iter().all(|def| def.role.is_none()),
        "--fix must not have assigned a role: {states:#?}"
    );
}

// --- SH-205: a story sitting in `blocked` with no `awaiting` reason gets a
// notice, not a health failure — the write-time capture backstop -----------

/// The common case a skipped dashboard prompt or a bare scripted `move`
/// leaves behind: a card in the Blocked column with nothing explaining it.
#[test]
fn doctor_notices_a_story_blocked_with_no_reason() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "stuck");
    StoryService::new(&ctx)
        .set_state(&id, "blocked", None, None, None)
        .expect("blocking");

    assert!(
        findings(&ctx).is_empty(),
        "a reason-less block is not damage"
    );
    let notices = notices(&ctx);
    assert_eq!(notices.len(), 1, "{notices:#?}");
    assert!(notices[0].contains(&id), "{}", notices[0]);
    assert!(notices[0].contains("blocked"), "{}", notices[0]);
}

/// A story blocked with a reason attached — via `move --reason`, atomically
/// (SH-205) — is silent.
#[test]
fn doctor_is_silent_about_a_blocked_story_once_a_reason_is_recorded() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "stuck, but explained");
    StoryService::new(&ctx)
        .set_state(&id, "blocked", None, None, Some("waiting on SH-9"))
        .expect("blocking with a reason");

    let notices = notices(&ctx);
    assert!(notices.is_empty(), "{notices:#?}");
}

/// One notice per reason-less blocked story, not a single aggregate line —
/// each is independently actionable.
#[test]
fn doctor_notices_one_line_per_reasonless_blocked_story() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let a = new_story(&ctx, "stuck a");
    let b = new_story(&ctx, "stuck b");
    service
        .set_state(&a, "blocked", None, None, None)
        .expect("blocking a");
    service
        .set_state(&b, "blocked", None, None, None)
        .expect("blocking b");

    let notices = notices(&ctx);
    assert_eq!(notices.len(), 2, "{notices:#?}");
    assert!(notices.iter().any(|n| n.contains(&a)), "{notices:#?}");
    assert!(notices.iter().any(|n| n.contains(&b)), "{notices:#?}");
}

/// The notice never contributes to `--fix`'s verdict: nothing here guesses
/// or writes a reason on the caller's behalf.
#[test]
fn doctor_fix_does_not_guess_a_blocked_reason() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "stuck");
    StoryService::new(&ctx)
        .set_state(&id, "blocked", None, None, None)
        .expect("blocking");

    let message = fix(&ctx).expect("fixing");
    assert!(
        message.starts_with("doctor found nothing to fix"),
        "{message}"
    );
    let notices = notices(&ctx);
    assert_eq!(notices.len(), 1, "{notices:#?}");
    let awaiting = fixture
        .store()
        .read(|tx| {
            let no = StoryNo::parse_id("SH", &id).expect("a well-formed id");
            Ok(tx
                .story(fixture.project(), no)?
                .expect("the story exists")
                .snapshot
                .awaiting)
        })
        .expect("reading the story");
    assert!(
        awaiting.is_none(),
        "--fix must not have written a reason: {awaiting:?}"
    );
}
