//! Invariants of the configuration service.
//!
//! Two properties get most of the attention here, because they are the two the
//! previous design could not offer.
//!
//! **Nothing is lost in a round trip.** The legacy path edited configuration
//! by deserializing a whole TOML document, mutating one field of it, and
//! serializing it back — so any field the mutation did not carry forward was
//! silently dropped, which is the shape of SH-49. A state's `role`,
//! `description` and superstate are asserted to survive every edit that did
//! not name them.
//!
//! **An edit that moves stories is one transaction with the moves.** Removing
//! or reclassifying an occupied state migrates its stories; a failure part way
//! through must leave neither the configuration nor the stories changed.
//!
//! Every test builds a [`ServiceFixture`], whose `Drop` re-folds the project
//! from its events and fails if the read model disagrees — so each of these is
//! also a consistency check on whatever it exercised.

use storyhook::cli::MemberInput;
use storyhook::domain::{FieldEdit, StateChanges, StateDef, SuperState, TypeDef};
use storyhook::error::AppError;
use storyhook::service::{
    ConfigService, Ctx, NewStoryInput, StateListing, StoryService, config::state_usage,
};
use storyhook::store::{ReadOps, SqliteStore, Store, StoryNo, WriteOps};
use storyhook_test_support::{FIXTURE_NOW, ServiceFixture, default_states};

// --- helpers ---------------------------------------------------------------

fn new_story(ctx: &Ctx<'_, SqliteStore>, title: &str) -> String {
    StoryService::new(ctx)
        .create(&NewStoryInput {
            title: title.to_string(),
            ..NewStoryInput::default()
        })
        .expect("creating a story")
        .id
}

fn story_in(ctx: &Ctx<'_, SqliteStore>, title: &str, state: &str) -> String {
    let id = new_story(ctx, title);
    StoryService::new(ctx)
        .set_state(&id, state, None, None)
        .expect("moving the story");
    id
}

fn states(fixture: &ServiceFixture) -> Vec<StateDef> {
    fixture
        .store()
        .read(|tx| tx.states(fixture.project()))
        .expect("reading states")
}

fn state(fixture: &ServiceFixture, slug: &str) -> StateDef {
    states(fixture)
        .into_iter()
        .find(|state| state.slug == slug)
        .unwrap_or_else(|| panic!("state `{slug}` is configured"))
}

fn slugs(fixture: &ServiceFixture) -> Vec<String> {
    states(fixture).into_iter().map(|s| s.slug).collect()
}

fn snapshot(fixture: &ServiceFixture, id: &str) -> storyhook::domain::StorySnapshot {
    let no = StoryNo::parse_id("SH", id).expect("a well-formed id");
    fixture
        .store()
        .read(|tx| tx.story(fixture.project(), no))
        .expect("reading the story")
        .expect("the story exists")
        .snapshot
}

fn message(error: AppError) -> String {
    error.to_string()
}

fn listing(fixture: &ServiceFixture, slug: &str) -> StateListing {
    ConfigService::new(&fixture.ctx())
        .list_states()
        .expect("listing states")
        .into_iter()
        .find(|listing| listing.state.slug == slug)
        .unwrap_or_else(|| panic!("state `{slug}` is listed"))
}

/// A fixture with a second CLOSED state, so a superstate can be flipped
/// without leaving the project with no CLOSED state at all.
///
/// Carries the required floor as well: a project below it cannot have its
/// catalog edited, so a fixture that omitted `in-progress` or `blocked` would
/// fail every test here on the floor rather than on its own subject.
fn with_two_closed_states() -> ServiceFixture {
    let mut states = default_states();
    states.push(StateDef {
        slug: "wontfix".into(),
        super_state: SuperState::Closed,
        role: None,
        description: None,
    });
    ServiceFixture::with_states(&states)
}

/// The state a test operates on when its subject is what happens to a state's
/// *occupants*.
const SPARE: &str = "in-review";

/// A fixture with one OPEN state beyond the required floor.
///
/// Removal and reclassification cannot be performed on a required state at any
/// occupancy (SH-125), so the tests about *how* those operations move stories
/// need a state the floor does not protect. Using `todo` for them, as they
/// used to, would now be testing the refusal instead of the migration.
fn with_a_spare_state() -> ServiceFixture {
    let mut states = default_states();
    states.push(StateDef {
        slug: SPARE.into(),
        super_state: SuperState::Open,
        role: None,
        description: None,
    });
    ServiceFixture::with_states(&states)
}

// --- listing ---------------------------------------------------------------

#[test]
fn states_are_listed_in_board_order_not_alphabetical_order() {
    let fixture = ServiceFixture::new();
    let listed: Vec<String> = ConfigService::new(&fixture.ctx())
        .list_states()
        .expect("listing")
        .into_iter()
        .map(|listing| listing.state.slug)
        .collect();
    assert_eq!(listed, ["todo", "in-progress", "blocked", "done"]);
}

#[test]
fn a_states_listing_counts_open_and_archived_occupants_separately() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "one");
    new_story(&ctx, "two");
    story_in(&ctx, "three", "done");

    assert_eq!(listing(&fixture, "todo").usage.open, 2);
    assert_eq!(listing(&fixture, "todo").usage.archived, 0);
    assert_eq!(listing(&fixture, "done").usage.open, 0);
    assert_eq!(listing(&fixture, "done").usage.archived, 1);
    assert_eq!(listing(&fixture, "in-progress").usage.open, 0);
}

#[test]
fn a_deleted_story_occupies_no_state() {
    // `fold_story` forces a deleted story to CLOSED without consulting the
    // state map, so it neither blocks removing a state nor needs migrating.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "doomed");
    StoryService::new(&ctx)
        .delete(&id, "not needed")
        .expect("deleting");
    assert_eq!(listing(&fixture, "todo").usage.open, 0);
    assert_eq!(listing(&fixture, "todo").usage.archived, 0);
}

#[test]
fn every_configured_state_appears_in_the_usage_map_even_when_empty() {
    let fixture = ServiceFixture::new();
    let usage = fixture
        .store()
        .read(|tx| Ok(state_usage(tx, fixture.project())?))
        .expect("reading usage");
    let mut slugs: Vec<&str> = usage.keys().map(String::as_str).collect();
    slugs.sort_unstable();
    assert_eq!(slugs, ["blocked", "done", "in-progress", "todo"]);
}

// --- adding states ---------------------------------------------------------

#[test]
fn a_new_state_is_appended_to_the_board_order() {
    let fixture = ServiceFixture::new();
    let added = ConfigService::new(&fixture.ctx())
        .add_state(
            "in-review",
            SuperState::Open,
            None,
            Some("Waiting on a reviewer".into()),
        )
        .expect("adding a state");
    assert_eq!(added.slug, "in-review");
    assert_eq!(
        slugs(&fixture),
        ["todo", "in-progress", "blocked", "done", "in-review"]
    );
    assert_eq!(
        state(&fixture, "in-review").description.as_deref(),
        Some("Waiting on a reviewer")
    );
}

#[test]
fn adding_a_state_leaves_every_other_states_fields_intact() {
    // The SH-49 shape: a read-modify-write of a whole document drops what the
    // writer did not carry forward. `todo` has a description and `in-progress`
    // has a role; both must survive an edit that mentions neither.
    let fixture = ServiceFixture::new();
    ConfigService::new(&fixture.ctx())
        .add_state("in-review", SuperState::Open, None, None)
        .expect("adding a state");
    assert_eq!(
        state(&fixture, "todo").description.as_deref(),
        Some("Not started")
    );
    assert_eq!(
        state(&fixture, "in-progress").role.as_deref(),
        Some("active")
    );
    assert_eq!(state(&fixture, "done").super_state, SuperState::Closed);
}

#[test]
fn a_duplicate_state_slug_is_rejected() {
    let fixture = ServiceFixture::new();
    let error = ConfigService::new(&fixture.ctx())
        .add_state("todo", SuperState::Open, None, None)
        .unwrap_err();
    assert!(message(error).contains("state `todo` already exists"));
    assert_eq!(slugs(&fixture).len(), 4);
}

#[test]
fn an_unaddressable_state_slug_is_rejected() {
    let fixture = ServiceFixture::new();
    for bad in ["In Review", "in_review", "in--review", "-review", "review-"] {
        let error = ConfigService::new(&fixture.ctx())
            .add_state(bad, SuperState::Open, None, None)
            .unwrap_err();
        assert!(
            message(error).contains("invalid state slug"),
            "`{bad}` was accepted"
        );
    }
    assert_eq!(slugs(&fixture).len(), 4);
}

#[test]
fn a_second_active_state_is_rejected() {
    let fixture = ServiceFixture::new();
    let error = ConfigService::new(&fixture.ctx())
        .add_state("doing", SuperState::Open, Some("active".into()), None)
        .unwrap_err();
    let message = message(error);
    assert!(
        message.contains("only one state may have role `active`"),
        "{message}"
    );
    assert!(message.contains("in-progress"), "{message}");
    assert!(message.contains("doing"), "{message}");
}

#[test]
fn an_unknown_state_role_is_rejected() {
    let fixture = ServiceFixture::new();
    let error = ConfigService::new(&fixture.ctx())
        .add_state("triage", SuperState::Open, Some("triage".into()), None)
        .unwrap_err();
    assert!(message(error).contains("unknown role `triage`"));
}

// --- editing states --------------------------------------------------------

#[test]
fn an_edit_sets_and_clears_only_the_fields_it_names() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = ConfigService::new(&ctx);

    service
        .update_state(
            "todo",
            &StateChanges {
                super_state: None,
                role: FieldEdit::Set("active".into()),
                description: FieldEdit::Keep,
            },
            None,
        )
        .expect_err("`in-progress` already holds the active role");

    service
        .update_state(
            "todo",
            &StateChanges {
                super_state: None,
                role: FieldEdit::Keep,
                description: FieldEdit::Set("Queued".into()),
            },
            None,
        )
        .expect("setting a description");
    let todo = state(&fixture, "todo");
    assert_eq!(todo.description.as_deref(), Some("Queued"));
    assert_eq!(todo.super_state, SuperState::Open);
    assert_eq!(todo.role, None);

    service
        .update_state(
            "todo",
            &StateChanges {
                super_state: None,
                role: FieldEdit::Keep,
                description: FieldEdit::Clear,
            },
            None,
        )
        .expect("clearing the description");
    assert_eq!(state(&fixture, "todo").description, None);
}

#[test]
fn editing_one_field_never_drops_another() {
    // Every field of `in-progress` set at once, then a single unrelated edit;
    // the other two must read back unchanged. This is the regression shape of
    // SH-49 stated as a property.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = ConfigService::new(&ctx);
    service
        .update_state(
            "in-progress",
            &StateChanges {
                super_state: None,
                role: FieldEdit::Keep,
                description: FieldEdit::Set("Being worked on".into()),
            },
            None,
        )
        .expect("describing the state");

    service
        .update_state(
            "in-progress",
            &StateChanges {
                super_state: None,
                role: FieldEdit::Clear,
                description: FieldEdit::Keep,
            },
            None,
        )
        .expect("clearing the role");

    let edited = state(&fixture, "in-progress");
    assert_eq!(edited.role, None, "the edit named the role");
    assert_eq!(
        edited.description.as_deref(),
        Some("Being worked on"),
        "the edit did not name the description"
    );
    assert_eq!(edited.super_state, SuperState::Open);
    assert_eq!(
        slugs(&fixture),
        ["todo", "in-progress", "blocked", "done"],
        "an edit must not disturb the board order"
    );
}

#[test]
fn an_empty_change_set_is_a_usage_error() {
    let fixture = ServiceFixture::new();
    let error = ConfigService::new(&fixture.ctx())
        .update_state(
            "todo",
            &StateChanges {
                super_state: None,
                role: FieldEdit::Keep,
                description: FieldEdit::Keep,
            },
            None,
        )
        .unwrap_err();
    assert!(matches!(error, AppError::Usage(_)), "{error:?}");
    assert!(message(error).contains("nothing to change on state `todo`"));
}

#[test]
fn editing_an_unknown_state_is_not_found() {
    let fixture = ServiceFixture::new();
    let error = ConfigService::new(&fixture.ctx())
        .update_state(
            "limbo",
            &StateChanges {
                super_state: Some(SuperState::Open),
                role: FieldEdit::Keep,
                description: FieldEdit::Keep,
            },
            None,
        )
        .unwrap_err();
    assert!(matches!(error, AppError::NotFound(_)), "{error:?}");
}

/// The required floor reaches this case first, and that is the point.
///
/// This test used to walk a project down to zero OPEN states one flip at a
/// time, and the "at least one OPEN state" rule caught it on the last one.
/// `todo`, `in-progress` and `blocked` are now all required to be OPEN, so the
/// first flip is refused and the walk never starts — the older rule is
/// unreachable from here rather than gone, and `domain`'s
/// `requires_open_and_closed_states` still covers it directly.
#[test]
fn a_required_state_cannot_be_reclassified() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let error = ConfigService::new(&ctx)
        .update_state(
            "todo",
            &StateChanges {
                super_state: Some(SuperState::Closed),
                role: FieldEdit::Keep,
                description: FieldEdit::Keep,
            },
            None,
        )
        .unwrap_err();
    let message = message(error);
    assert!(message.contains("`todo` must be OPEN"), "{message}");
    assert_eq!(state(&fixture, "todo").super_state, SuperState::Open);
}

#[test]
fn reclassifying_an_occupied_state_requires_a_destination() {
    let fixture = with_a_spare_state();
    let ctx = fixture.ctx();
    let id = story_in(&ctx, "sitting in review", SPARE);

    let error = ConfigService::new(&ctx)
        .update_state(
            SPARE,
            &StateChanges {
                super_state: Some(SuperState::Closed),
                role: FieldEdit::Keep,
                description: FieldEdit::Keep,
            },
            None,
        )
        .unwrap_err();
    let message = message(error);
    assert!(message.contains("holds 1 open story"), "{message}");
    assert!(message.contains("todo, in-progress, blocked"), "{message}");

    assert_eq!(state(&fixture, SPARE).super_state, SuperState::Open);
    assert_eq!(snapshot(&fixture, &id).state, SPARE);
}

#[test]
fn a_metadata_edit_needs_no_destination_however_occupied_the_state_is() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "sitting in todo");
    ConfigService::new(&ctx)
        .update_state(
            "todo",
            &StateChanges {
                super_state: None,
                role: FieldEdit::Keep,
                description: FieldEdit::Set("Queued".into()),
            },
            None,
        )
        .expect("a description edit reclassifies nothing");
    assert_eq!(listing(&fixture, "todo").usage.open, 1);
}

#[test]
fn reclassifying_migrates_its_occupants_and_reports_how_many() {
    let fixture = with_a_spare_state();
    let ctx = fixture.ctx();
    let ids = [
        story_in(&ctx, "one", SPARE),
        story_in(&ctx, "two", SPARE),
        story_in(&ctx, "three", SPARE),
    ];

    let edit = ConfigService::new(&ctx)
        .update_state(
            SPARE,
            &StateChanges {
                super_state: Some(SuperState::Closed),
                role: FieldEdit::Keep,
                description: FieldEdit::Keep,
            },
            Some("in-progress"),
        )
        .expect("migrating the occupants");
    assert_eq!(edit.moved, 3);
    assert_eq!(edit.state.super_state, SuperState::Closed);

    for id in &ids {
        let story = snapshot(&fixture, id);
        assert_eq!(story.state, "in-progress");
        assert_eq!(story.superstate, SuperState::Open);
        assert_eq!(
            story.comments.last().map(|c| c.text.as_str()),
            Some("[states] moved from `in-review` to `in-progress`"),
            "the move must be traceable to the edit that caused it"
        );
    }
}

#[test]
fn migrating_into_a_closed_state_closes_and_archives() {
    let fixture = with_a_spare_state();
    let ctx = fixture.ctx();
    let id = story_in(&ctx, "on its way out", SPARE);

    ConfigService::new(&ctx)
        .remove_state(SPARE, Some("done"))
        .expect("removing an occupied state");

    let story = snapshot(&fixture, &id);
    assert_eq!(story.state, "done");
    assert_eq!(story.superstate, SuperState::Closed);
    assert_eq!(story.closed_at.as_deref(), Some(FIXTURE_NOW));
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::parse_id("SH", &id).unwrap()))
        .expect("reading the row")
        .expect("the story exists");
    assert!(row.archived, "a closed story is archived");
}

#[test]
fn an_unknown_destination_is_rejected_before_anything_moves() {
    let fixture = with_a_spare_state();
    let ctx = fixture.ctx();
    let id = story_in(&ctx, "staying put", SPARE);
    let error = ConfigService::new(&ctx)
        .remove_state(SPARE, Some("nowhere"))
        .unwrap_err();
    assert!(matches!(error, AppError::NotFound(_)), "{error:?}");
    assert_eq!(snapshot(&fixture, &id).state, SPARE);
    assert_eq!(slugs(&fixture).len(), 5);
}

#[test]
fn stories_cannot_be_migrated_into_the_state_being_edited() {
    let fixture = with_a_spare_state();
    let ctx = fixture.ctx();
    story_in(&ctx, "sitting in review", SPARE);
    let error = ConfigService::new(&ctx)
        .remove_state(SPARE, Some(SPARE))
        .unwrap_err();
    assert!(message(error).contains("cannot be moved into `in-review` itself"));
}

// --- the definition change that re-derives rows ----------------------------

#[test]
fn flipping_a_superstate_re_derives_the_rows_of_the_stories_left_in_it() {
    // Archived stories do not migrate — their history is closed. But a story's
    // superstate is *derived* from the definition of the state it is in, so a
    // definition change has to re-derive it. The alternative is a read model
    // that no longer equals a fold of its own events, which is exactly what
    // `ServiceFixture`'s drop check refuses to accept.
    let fixture = with_two_closed_states();
    let ctx = fixture.ctx();
    let id = story_in(&ctx, "shelved", "wontfix");
    assert_eq!(snapshot(&fixture, &id).superstate, SuperState::Closed);

    ConfigService::new(&ctx)
        .update_state(
            "wontfix",
            &StateChanges {
                super_state: Some(SuperState::Open),
                role: FieldEdit::Keep,
                description: FieldEdit::Keep,
            },
            None,
        )
        .expect("no open stories occupy `wontfix`");

    let story = snapshot(&fixture, &id);
    assert_eq!(story.state, "wontfix");
    assert_eq!(
        story.superstate,
        SuperState::Open,
        "the row still reports the old superstate"
    );
    fixture.assert_no_drift();
}

// --- removing states -------------------------------------------------------

#[test]
fn an_empty_state_is_removed_without_ceremony() {
    let fixture = with_a_spare_state();
    let moved = ConfigService::new(&fixture.ctx())
        .remove_state(SPARE, None)
        .expect("removing an empty state");
    assert_eq!(moved, 0);
    assert_eq!(slugs(&fixture), ["todo", "in-progress", "blocked", "done"]);
}

#[test]
fn removing_an_unknown_state_is_not_found() {
    let fixture = ServiceFixture::new();
    let error = ConfigService::new(&fixture.ctx())
        .remove_state("limbo", None)
        .unwrap_err();
    assert!(matches!(error, AppError::NotFound(_)), "{error:?}");
}

/// As with reclassification, the floor is what refuses now: `done` is the
/// CLOSED state every project must have, so "the last CLOSED state" is not a
/// state a conforming project can be talked out of.
#[test]
fn a_required_state_cannot_be_removed() {
    let fixture = ServiceFixture::new();
    for slug in ["todo", "in-progress", "blocked", "done"] {
        let error = ConfigService::new(&fixture.ctx())
            .remove_state(slug, None)
            .unwrap_err();
        let message = message(error);
        assert!(message.contains(slug), "removing `{slug}`: {message}");
        assert!(
            message.contains("story doctor --fix"),
            "the refusal must name the repair: {message}"
        );
    }
    assert_eq!(slugs(&fixture).len(), 4, "nothing was removed");
}

#[test]
fn a_state_with_archived_history_cannot_be_removed() {
    let fixture = with_two_closed_states();
    let ctx = fixture.ctx();
    story_in(&ctx, "shelved", "wontfix");

    let error = ConfigService::new(&ctx)
        .remove_state("wontfix", None)
        .unwrap_err();
    let message = message(error);
    assert!(message.contains("1 archived story"), "{message}");
    assert!(
        message.contains("fold against a state that no longer exists"),
        "{message}"
    );
    assert_eq!(slugs(&fixture).len(), 5);
}

#[test]
fn a_deleted_story_does_not_hold_a_state_open() {
    let fixture = with_a_spare_state();
    let ctx = fixture.ctx();
    let id = story_in(&ctx, "doomed", SPARE);
    StoryService::new(&ctx)
        .delete(&id, "gone")
        .expect("deleting");

    ConfigService::new(&ctx)
        .remove_state(SPARE, None)
        .expect("a deleted occupant neither blocks nor migrates");
    assert_eq!(slugs(&fixture), ["todo", "in-progress", "blocked", "done"]);
    // SH-130 makes this stronger than it was. It used to assert the deleted
    // story kept `SPARE` — "the slug its history records" — which meant the
    // removal above left a story naming a state the catalog no longer defined.
    // Deletion now lands the story in `done`, so removing `SPARE` is safe for a
    // better reason than "nobody was looking": nothing is in it.
    //
    // Worth stating plainly, because the old fold comment claimed the
    // superstate override protected against exactly this. It did not: it kept
    // the *superstate* right while leaving the slug dangling, and `state_usage`
    // counts only undeleted stories, so `remove_state` never saw the occupant
    // at all.
    assert_eq!(
        snapshot(&fixture, &id).state,
        "done",
        "a deleted story rests in the required CLOSED state, not in a slug that \
         can be removed out from under it"
    );
}

// --- atomicity -------------------------------------------------------------

#[cfg(feature = "fault-injection")]
#[test]
fn a_migration_that_fails_part_way_moves_nothing_at_all() {
    use storyhook::store::fault::{FaultAction, FaultPoint, arm};

    let fixture = with_a_spare_state();
    let ctx = fixture.ctx();
    let ids = [
        story_in(&ctx, "one", SPARE),
        story_in(&ctx, "two", SPARE),
        story_in(&ctx, "three", SPARE),
    ];
    let before: Vec<_> = ids.iter().map(|id| snapshot(&fixture, id)).collect();

    let error = {
        let _fault = arm(
            FaultPoint::BeforeCommit,
            FaultAction::Fail("interrupted".to_string()),
        );
        ConfigService::new(&ctx)
            .remove_state(SPARE, Some("in-progress"))
            .expect_err("the injected fault must fail the removal")
    };
    assert!(error.to_string().contains("interrupted"), "{error}");

    assert_eq!(
        slugs(&fixture),
        ["todo", "in-progress", "blocked", "done", SPARE],
        "the configuration change survived a rollback"
    );
    for (id, was) in ids.iter().zip(&before) {
        let now = snapshot(&fixture, id);
        assert_eq!(&now, was, "story `{id}` moved despite the rollback");
    }
    assert_eq!(listing(&fixture, SPARE).usage.open, 3);
}

#[cfg(feature = "fault-injection")]
#[test]
fn a_failed_migration_leaves_no_stray_comments_behind() {
    // The comment that records the move is appended before the story is
    // folded, so it is the first thing a non-transactional migration would
    // leak. Its absence is the sharpest evidence the rollback was complete.
    use storyhook::store::fault::{FaultAction, FaultPoint, arm};

    let fixture = with_a_spare_state();
    let ctx = fixture.ctx();
    let id = story_in(&ctx, "one", SPARE);
    {
        let _fault = arm(
            FaultPoint::MidReadModelUpdate,
            FaultAction::Fail("interrupted".to_string()),
        );
        ConfigService::new(&ctx)
            .remove_state(SPARE, Some("in-progress"))
            .expect_err("the injected fault must fail the removal");
    }

    let story = snapshot(&fixture, &id);
    assert_eq!(story.state, SPARE);
    assert!(
        story.comments.is_empty(),
        "a rolled-back migration left a comment: {:?}",
        story.comments
    );
}

// --- reordering ------------------------------------------------------------

#[test]
fn reordering_rewrites_the_board_order() {
    let fixture = ServiceFixture::new();
    let order = vec![
        "done".to_string(),
        "todo".to_string(),
        "blocked".to_string(),
        "in-progress".to_string(),
    ];
    let reordered = ConfigService::new(&fixture.ctx())
        .reorder_states(&order)
        .expect("reordering");
    assert_eq!(
        reordered
            .iter()
            .map(|s| s.slug.as_str())
            .collect::<Vec<_>>(),
        ["done", "todo", "blocked", "in-progress"]
    );
    assert_eq!(slugs(&fixture), ["done", "todo", "blocked", "in-progress"]);
}

#[test]
fn reordering_carries_every_states_fields_across() {
    let fixture = ServiceFixture::new();
    let order = vec![
        "in-progress".to_string(),
        "todo".to_string(),
        "blocked".to_string(),
        "done".to_string(),
    ];
    ConfigService::new(&fixture.ctx())
        .reorder_states(&order)
        .expect("reordering");
    assert_eq!(
        state(&fixture, "todo").description.as_deref(),
        Some("Not started")
    );
    assert_eq!(
        state(&fixture, "in-progress").role.as_deref(),
        Some("active")
    );
    assert_eq!(state(&fixture, "done").super_state, SuperState::Closed);
}

#[test]
fn a_partial_order_is_rejected() {
    let fixture = ServiceFixture::new();
    let error = ConfigService::new(&fixture.ctx())
        .reorder_states(&["todo".to_string(), "done".to_string()])
        .unwrap_err();
    let message = message(error);
    assert!(message.contains("must list every state"), "{message}");
    assert!(message.contains("in-progress"), "{message}");
    assert_eq!(slugs(&fixture), ["todo", "in-progress", "blocked", "done"]);
}

#[test]
fn a_repeated_or_unknown_slug_is_rejected() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = ConfigService::new(&ctx);
    let error = service
        .reorder_states(&[
            "todo".to_string(),
            "todo".to_string(),
            "in-progress".to_string(),
        ])
        .unwrap_err();
    assert!(message(error).contains("listed more than once"));

    let error = service
        .reorder_states(&[
            "todo".to_string(),
            "in-progress".to_string(),
            "limbo".to_string(),
        ])
        .unwrap_err();
    assert!(matches!(error, AppError::NotFound(_)), "{error:?}");
    assert_eq!(slugs(&fixture), ["todo", "in-progress", "blocked", "done"]);
}

#[test]
fn reordering_changes_which_state_a_new_story_opens_in() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    ConfigService::new(&ctx)
        .reorder_states(&[
            "in-progress".to_string(),
            "todo".to_string(),
            "blocked".to_string(),
            "done".to_string(),
        ])
        .expect("reordering");
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "after the reorder".into(),
            ..NewStoryInput::default()
        })
        .expect("creating");
    assert_eq!(story.state, "in-progress");
}

#[test]
fn concurrent_reorders_leave_one_state_per_position() {
    // The board order is stored as a position per state, and two writers
    // racing on it is the classic way to end up with a duplicated or missing
    // position. The store serializes writers; what this asserts is that the
    // *outcome* is always a permutation of the configured set, whichever
    // writer went last.
    let fixture = ServiceFixture::new();
    let orders = [
        vec![
            "done".to_string(),
            "todo".to_string(),
            "blocked".to_string(),
            "in-progress".to_string(),
        ],
        vec![
            "in-progress".to_string(),
            "done".to_string(),
            "blocked".to_string(),
            "todo".to_string(),
        ],
    ];
    std::thread::scope(|scope| {
        for order in &orders {
            scope.spawn(|| {
                for _ in 0..20 {
                    ConfigService::new(&fixture.ctx())
                        .reorder_states(order)
                        .expect("reordering");
                }
            });
        }
    });

    let final_slugs = slugs(&fixture);
    assert_eq!(final_slugs.len(), 4, "a state was lost or duplicated");
    let unique: std::collections::BTreeSet<&String> = final_slugs.iter().collect();
    assert_eq!(unique.len(), 4);
    assert!(orders.contains(&final_slugs), "{final_slugs:?}");
}

// --- types -----------------------------------------------------------------

#[test]
fn types_are_listed_in_configured_order() {
    let fixture = ServiceFixture::new();
    let listed: Vec<String> = ConfigService::new(&fixture.ctx())
        .list_types()
        .expect("listing")
        .into_iter()
        .map(|t| t.slug)
        .collect();
    assert_eq!(listed, ["feature", "bug"]);
}

#[test]
fn a_new_type_is_appended_and_keeps_its_description() {
    let fixture = ServiceFixture::new();
    let added = ConfigService::new(&fixture.ctx())
        .add_type("spike", Some("A timeboxed investigation"), None)
        .expect("adding a type");
    assert_eq!(added.slug, "spike");
    let types = ConfigService::new(&fixture.ctx())
        .list_types()
        .expect("listing");
    assert_eq!(
        types.iter().map(|t| t.slug.as_str()).collect::<Vec<_>>(),
        ["feature", "bug", "spike"]
    );
    assert_eq!(
        types[1].description.as_deref(),
        Some("Something is broken"),
        "an existing type's description survived the write"
    );
    assert_eq!(
        types[2].description.as_deref(),
        Some("A timeboxed investigation")
    );
}

#[test]
fn the_slugs_that_mean_no_type_are_reserved() {
    let fixture = ServiceFixture::new();
    for reserved in ["none", "NONE", "default", "Default"] {
        let error = ConfigService::new(&fixture.ctx())
            .add_type(reserved, None, None)
            .unwrap_err();
        assert!(
            message(error).contains("is reserved and cannot be used"),
            "`{reserved}` was accepted"
        );
    }
}

/// SH-134: a type slug is an address, so `add_type` refuses one nothing can
/// address — the same rule `add_state` has always applied to a state slug.
///
/// These are the eight shapes measured reaching the store before the fix. The
/// last one is the reason SH-62's flag gate could not stand in for this check:
/// `story type add -- --typo` is a legitimate use of the argument terminator,
/// so the token arrives at the service as ordinary data and only a domain
/// invariant can refuse it.
#[test]
fn add_type_rejects_unaddressable_slugs() {
    let fixture = ServiceFixture::new();
    for bad in [
        "",
        "in review",
        "a b",
        "Bug",
        "spike/two",
        "-lead",
        "double--dash",
        "café",
        "--typo",
    ] {
        let error = ConfigService::new(&fixture.ctx())
            .add_type(bad, None, None)
            .unwrap_err();
        assert!(
            message(error).contains("invalid type slug"),
            "`{bad}` was accepted"
        );
    }

    let types = ConfigService::new(&fixture.ctx())
        .list_types()
        .expect("listing");
    assert_eq!(
        types.iter().map(|t| t.slug.as_str()).collect::<Vec<_>>(),
        ["feature", "bug"],
        "a refused slug must leave the catalog exactly as it was"
    );
}

/// The guard against the fix going too far: a well-formed slug still works.
#[test]
fn add_type_still_accepts_a_well_formed_slug() {
    let fixture = ServiceFixture::new();
    let added = ConfigService::new(&fixture.ctx())
        .add_type("feature-request", None, None)
        .expect("adding a well-formed type");
    assert_eq!(added.slug, "feature-request");
}

/// SH-134's lock-out hazard, pinned in both directions.
///
/// A project can only carry an unaddressable type slug if it predates this
/// rule, so the fixture writes two of them through the store rather than
/// through the service that now refuses them. Neither the cleanup path nor the
/// growth path may consult the rest of the catalog: validating the *resulting
/// set* on removal would mean neither junk slug could ever be removed (taking
/// one away still leaves the other), and validating it on addition would stop
/// the project adding any new type at all until it was clean. Both are worse
/// than the damage they would be policing.
#[test]
fn a_catalog_holding_junk_slugs_can_still_be_cleaned_up_and_grown() {
    let fixture = ServiceFixture::new();
    let project = fixture.project();
    fixture
        .store()
        .write(|tx| {
            tx.put_types(
                project,
                &[
                    TypeDef {
                        slug: "feature".into(),
                        description: None,
                        emoji: None,
                    },
                    TypeDef {
                        slug: "in review".into(),
                        description: None,
                        emoji: None,
                    },
                    TypeDef {
                        slug: "café".into(),
                        description: None,
                        emoji: None,
                    },
                ],
            )
        })
        .expect("seeding a catalog written before the rule existed");

    let ctx = fixture.ctx();
    ConfigService::new(&ctx)
        .remove_type("in review")
        .expect("removing one junk slug while another remains");
    ConfigService::new(&ctx)
        .add_type("spike", None, None)
        .expect("adding a valid type while junk remains");

    let remaining: Vec<String> = ConfigService::new(&ctx)
        .list_types()
        .expect("listing")
        .into_iter()
        .map(|t| t.slug)
        .collect();
    assert_eq!(remaining, ["feature", "café", "spike"]);
}

#[test]
fn a_duplicate_type_slug_is_rejected() {
    let fixture = ServiceFixture::new();
    let error = ConfigService::new(&fixture.ctx())
        .add_type("bug", None, None)
        .unwrap_err();
    assert!(message(error).contains("type `bug` already exists"));
}

#[test]
fn an_unused_type_is_removed() {
    let fixture = ServiceFixture::new();
    ConfigService::new(&fixture.ctx())
        .remove_type("bug")
        .expect("removing an unused type");
    let remaining: Vec<String> = ConfigService::new(&fixture.ctx())
        .list_types()
        .expect("listing")
        .into_iter()
        .map(|t| t.slug)
        .collect();
    assert_eq!(remaining, ["feature"]);
}

#[test]
fn removing_an_unknown_type_is_not_found() {
    let fixture = ServiceFixture::new();
    let error = ConfigService::new(&fixture.ctx())
        .remove_type("spike")
        .unwrap_err();
    assert!(matches!(error, AppError::NotFound(_)), "{error:?}");
}

#[test]
fn a_type_in_use_is_not_removable() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "a bug".into(),
            story_type: Some("bug".into()),
            ..NewStoryInput::default()
        })
        .expect("creating");
    let error = ConfigService::new(&ctx).remove_type("bug").unwrap_err();
    assert!(message(error).contains("still used by an existing story"));
}

#[test]
fn a_closed_storys_type_still_counts_as_in_use() {
    // The legacy check scanned open *and* archived stories, because an
    // archived story's snapshot names its type just as loudly.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "a fixed bug".into(),
            story_type: Some("bug".into()),
            ..NewStoryInput::default()
        })
        .expect("creating");
    StoryService::new(&ctx)
        .set_state(&story.id, "done", None, None)
        .expect("closing");

    let error = ConfigService::new(&ctx).remove_type("bug").unwrap_err();
    assert!(message(error).contains("still used by an existing story"));
}

#[test]
fn the_last_type_cannot_be_removed() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = ConfigService::new(&ctx);
    service
        .remove_type("bug")
        .expect("removing the second type");
    let error = service.remove_type("feature").unwrap_err();
    assert!(message(error).contains("cannot remove the last type"));
}

// --- members ---------------------------------------------------------------

#[test]
fn a_member_identity_is_split_into_a_name_and_an_address() {
    let fixture = ServiceFixture::new();
    let member = ConfigService::new(&fixture.ctx())
        .add_member(&MemberInput::Identity(
            "Ada Lovelace <ada@example.com>".into(),
        ))
        .expect("adding a member");
    assert_eq!(member.id, "ada-lovelace");
    assert_eq!(member.display_name, "Ada Lovelace");
    assert_eq!(member.email.as_deref(), Some("ada@example.com"));
    assert_eq!(member.github, None);
    assert_eq!(member.created_at, FIXTURE_NOW);
}

#[test]
fn an_identity_without_an_address_is_a_display_name() {
    let fixture = ServiceFixture::new();
    let member = ConfigService::new(&fixture.ctx())
        .add_member(&MemberInput::Identity("Grace Hopper".into()))
        .expect("adding a member");
    assert_eq!(member.id, "grace-hopper");
    assert_eq!(member.display_name, "Grace Hopper");
    assert_eq!(member.email, None);
}

#[test]
fn a_github_handle_becomes_both_the_id_and_the_handle() {
    let fixture = ServiceFixture::new();
    let member = ConfigService::new(&fixture.ctx())
        .add_member(&MemberInput::Github("mikeyward".into()))
        .expect("adding a member");
    assert_eq!(member.id, "mikeyward");
    assert_eq!(member.github.as_deref(), Some("mikeyward"));
}

/// SH-134 asked whether `member add` needs the shape guard `add_type` now has.
///
/// It does not, and this is why: a member id is *derived* by `slugify` rather
/// than stored as typed, so it is addressable by construction — where a type
/// slug was stored verbatim, which is the whole defect. The guarantee was never
/// pinned, though, and an unpinned guarantee is what SH-134 was. Asserting it
/// through the type validator rather than restating the charset means the two
/// invariants cannot drift apart: weaken either and this fails.
#[test]
fn a_member_id_cannot_be_claimed_twice() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = ConfigService::new(&ctx);
    service
        .add_member(&MemberInput::Identity(
            "Ada Lovelace <ada@example.com>".into(),
        ))
        .expect("the first Ada");
    let error = service
        .add_member(&MemberInput::Identity("ada lovelace".into()))
        .unwrap_err();
    assert!(message(error).contains("member `ada-lovelace` already exists"));
    assert_eq!(
        service.list_members().expect("listing").len(),
        1,
        "the duplicate must not have overwritten the original"
    );
    assert_eq!(
        service.list_members().expect("listing")[0].email.as_deref(),
        Some("ada@example.com"),
        "the original member's fields survived the rejected add"
    );
}

#[test]
fn a_member_is_assignable_the_moment_it_is_added() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    ConfigService::new(&ctx)
        .add_member(&MemberInput::Github("ada-gh".into()))
        .expect("adding a member");
    let id = new_story(&ctx, "needs an owner");
    let story = StoryService::new(&ctx)
        .assign(&id, "ada-gh")
        .expect("assigning");
    assert_eq!(story.assignee.as_deref(), Some("ada-gh"));
}
