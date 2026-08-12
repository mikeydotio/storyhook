//! Invariants of the story lifecycle service.
//!
//! Every test here builds a [`ServiceFixture`], whose `Drop` re-folds the whole
//! project from its events and fails if the read model disagrees. So each test
//! asserts two things: the one it was written for, and — for free — that the
//! operation it exercised left the read model consistent with its own history.

use std::collections::BTreeMap;

use storyhook::domain::{Priority, StateDef, StorySnapshot, SuperState};
use storyhook::error::AppError;
use storyhook::service::{Clock, Ctx, FieldEdits, NewStoryInput, StoryService};
use storyhook::store::{ReadOps, SqliteStore, Store, StoryNo, StoryQuery};
use storyhook_test_support::{FIXTURE_NOW, ServiceFixture};

// --- helpers ---------------------------------------------------------------

fn new_story(ctx: &Ctx<'_, SqliteStore>, title: &str) -> StorySnapshot {
    StoryService::new(ctx)
        .create(&NewStoryInput {
            title: title.to_string(),
            ..NewStoryInput::default()
        })
        .expect("creating a story")
}

fn snapshot(fixture: &ServiceFixture, id: &str) -> StorySnapshot {
    let no = StoryNo::parse_id("SH", id).expect("a well-formed id");
    fixture
        .store()
        .read(|tx| tx.story(fixture.project(), no))
        .expect("reading the story")
        .expect("the story exists")
        .snapshot
}

fn event_kinds(fixture: &ServiceFixture, id: &str) -> Vec<String> {
    let no = StoryNo::parse_id("SH", id).expect("a well-formed id");
    fixture
        .store()
        .read(|tx| tx.events_for(fixture.project(), no))
        .expect("reading events")
        .into_iter()
        .map(|event| event.kind)
        .collect()
}

fn validation_message(error: AppError) -> String {
    match error {
        AppError::Validation(message) => message,
        other => panic!("expected a validation error, got {other:?}"),
    }
}

// --- create ----------------------------------------------------------------

#[test]
fn a_new_story_opens_in_the_first_configured_open_state() {
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture.ctx(), "write the thing");
    assert_eq!(story.id, "SH-1");
    assert_eq!(story.state, "todo");
    assert_eq!(story.superstate, SuperState::Open);
    assert_eq!(story.created_at, FIXTURE_NOW);
}

#[test]
fn the_default_open_state_follows_configured_order_not_alphabetical_order() {
    // `in-progress` sorts before `todo`, so a default picked off a slug-keyed
    // map would open every story in the wrong state.
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture.ctx(), "ordering matters");
    assert_eq!(story.state, "todo");
}

#[test]
fn story_numbers_are_allocated_in_sequence() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let ids: Vec<String> = (0..3)
        .map(|i| new_story(&ctx, &format!("story {i}")).id)
        .collect();
    assert_eq!(ids, ["SH-1", "SH-2", "SH-3"]);
}

#[test]
fn a_new_story_can_open_in_any_configured_open_state() {
    let fixture = ServiceFixture::new();
    let story = StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "start hot".into(),
            state: Some("in-progress".into()),
            ..NewStoryInput::default()
        })
        .expect("creating in an open state");
    assert_eq!(story.state, "in-progress");
}

#[test]
fn a_new_story_cannot_open_in_a_closed_state() {
    let fixture = ServiceFixture::new();
    let error = StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "born dead".into(),
            state: Some("done".into()),
            ..NewStoryInput::default()
        })
        .unwrap_err();
    let message = validation_message(error);
    assert!(message.contains("not a valid OPEN state"), "{message}");
    assert!(message.contains("todo, in-progress"), "{message}");
}

#[test]
fn a_new_story_cannot_open_in_an_undefined_state() {
    let fixture = ServiceFixture::new();
    let error = StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "nowhere".into(),
            state: Some("limbo".into()),
            ..NewStoryInput::default()
        })
        .unwrap_err();
    assert!(validation_message(error).contains("limbo"));
}

#[test]
fn enrichment_events_are_written_in_one_batch_in_a_fixed_order() {
    let fixture = ServiceFixture::new();
    fixture.add_member("ada", "Ada Lovelace", Some("ada-gh"));
    let story = StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "everything at once".into(),
            state: None,
            story_type: Some("bug".into()),
            description: Some("a description".into()),
            priority: Some("high".into()),
            labels: Some(vec!["b".into(), "a".into()]),
            assignee: Some("ada".into()),
            draft: false,
        })
        .expect("creating an enriched story");

    assert_eq!(
        event_kinds(&fixture, &story.id),
        [
            "StoryCreated",
            "StoryPrioritySet",
            "StoryLabelsSet",
            "StoryAssigned",
            "StoryDescriptionSet",
            "StoryTypeSet",
        ]
    );
    assert_eq!(story.priority, Priority::High);
    assert_eq!(story.labels, ["a", "b"]);
    assert_eq!(story.assignee.as_deref(), Some("ada"));
    assert_eq!(story.story_type.as_deref(), Some("bug"));
    assert_eq!(story.description.as_deref(), Some("a description"));
}

#[test]
fn new_story_labels_are_sorted_and_deduplicated() {
    let fixture = ServiceFixture::new();
    let story = StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "labels".into(),
            labels: Some(vec!["z".into(), "a".into(), "z".into()]),
            ..NewStoryInput::default()
        })
        .expect("creating with labels");
    assert_eq!(story.labels, ["a", "z"]);
}

#[test]
fn an_empty_label_list_writes_no_labels_event() {
    let fixture = ServiceFixture::new();
    let story = StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "no labels".into(),
            labels: Some(Vec::new()),
            ..NewStoryInput::default()
        })
        .expect("creating with an empty label list");
    assert_eq!(event_kinds(&fixture, &story.id), ["StoryCreated"]);
}

#[test]
fn a_blank_description_writes_no_description_event() {
    let fixture = ServiceFixture::new();
    let story = StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "blank".into(),
            description: Some("   ".into()),
            ..NewStoryInput::default()
        })
        .expect("creating with a blank description");
    assert_eq!(event_kinds(&fixture, &story.id), ["StoryCreated"]);
}

#[test]
fn a_new_story_can_be_assigned_by_github_handle() {
    let fixture = ServiceFixture::new();
    fixture.add_member("ada", "Ada Lovelace", Some("ada-gh"));
    let story = StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "by handle".into(),
            assignee: Some("ada-gh".into()),
            ..NewStoryInput::default()
        })
        .expect("creating with a github handle");
    assert_eq!(story.assignee.as_deref(), Some("ada"));
}

#[test]
fn an_unknown_type_is_rejected_and_names_the_known_ones() {
    let fixture = ServiceFixture::new();
    let error = StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "bad type".into(),
            story_type: Some("chore".into()),
            ..NewStoryInput::default()
        })
        .unwrap_err();
    let message = validation_message(error);
    assert!(message.contains("unknown type `chore`"), "{message}");
    assert!(message.contains("bug, feature"), "{message}");
}

#[test]
fn an_invalid_priority_is_rejected_at_creation() {
    let fixture = ServiceFixture::new();
    let error = StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "bad priority".into(),
            priority: Some("urgent".into()),
            ..NewStoryInput::default()
        })
        .unwrap_err();
    assert!(validation_message(error).contains("invalid priority `urgent`"));
}

#[test]
fn an_unknown_assignee_is_not_found_at_creation() {
    let fixture = ServiceFixture::new();
    let error = StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "ghost".into(),
            assignee: Some("nobody".into()),
            ..NewStoryInput::default()
        })
        .unwrap_err();
    assert!(matches!(error, AppError::NotFound(_)), "{error:?}");
}

#[test]
fn a_rejected_creation_writes_no_story_at_all() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    service
        .create(&NewStoryInput {
            title: "the good one".into(),
            ..NewStoryInput::default()
        })
        .expect("creating a story");
    let _ = service.create(&NewStoryInput {
        title: "the bad one".into(),
        priority: Some("urgent".into()),
        ..NewStoryInput::default()
    });

    let stories = fixture
        .store()
        .read(|tx| tx.stories(fixture.project(), &StoryQuery::all()))
        .expect("listing stories");
    assert_eq!(stories.len(), 1);
    // The rolled-back transaction returns the number it allocated, so the next
    // story is SH-2 rather than SH-3 — a rejected command leaves no gap.
    let next = new_story(&ctx, "the next one");
    assert_eq!(next.id, "SH-2");
}

// --- comment / assign / priority / labels / awaiting ------------------------

#[test]
fn a_comment_is_appended_to_the_story() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "talk to me");
    let after = StoryService::new(&ctx)
        .comment(&story.id, "a remark")
        .expect("commenting");
    assert_eq!(after.comments.len(), 1);
    assert_eq!(after.comments[0].text, "a remark");
}

#[test]
fn commenting_on_a_closed_story_is_refused() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "closing time");
    service
        .set_state(&story.id, "done", None, None, None)
        .expect("closing");
    let error = service.comment(&story.id, "too late").unwrap_err();
    assert_eq!(
        validation_message(error),
        "story `SH-1` is closed and cannot be modified"
    );
}

#[test]
fn commenting_on_a_story_that_does_not_exist_is_not_found() {
    let fixture = ServiceFixture::new();
    let error = StoryService::new(&fixture.ctx())
        .comment("SH-99", "hello")
        .unwrap_err();
    match error {
        AppError::NotFound(message) => assert_eq!(message, "story `SH-99` not found"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn a_story_id_from_another_project_is_not_found_rather_than_invalid() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    new_story(&ctx, "ours");
    for id in ["OTHER-1", "SH-", "SH-x", "SH-007", "nonsense"] {
        let error = StoryService::new(&ctx).comment(id, "hello").unwrap_err();
        assert!(
            matches!(error, AppError::NotFound(_)),
            "`{id}` gave {error:?}"
        );
    }
}

#[test]
fn assignment_accepts_a_member_id_or_a_github_handle() {
    let fixture = ServiceFixture::new();
    fixture.add_member("ada", "Ada Lovelace", Some("ada-gh"));
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let first = new_story(&ctx, "by id");
    let second = new_story(&ctx, "by handle");
    assert_eq!(
        service
            .assign(&first.id, "ada")
            .unwrap()
            .assignee
            .as_deref(),
        Some("ada")
    );
    assert_eq!(
        service
            .assign(&second.id, "ada-gh")
            .unwrap()
            .assignee
            .as_deref(),
        Some("ada")
    );
}

#[test]
fn assigning_to_an_unknown_member_is_not_found() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "unassignable");
    let error = StoryService::new(&ctx)
        .assign(&story.id, "nobody")
        .unwrap_err();
    match error {
        AppError::NotFound(message) => assert_eq!(message, "member `nobody` not found"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn every_priority_slug_round_trips_through_the_service() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    for (slug, expected) in [
        ("critical", Priority::Critical),
        ("high", Priority::High),
        ("medium", Priority::Medium),
        ("low", Priority::Low),
        ("none", Priority::None),
    ] {
        let story = new_story(&ctx, slug);
        let after = service.set_priority(&story.id, slug).expect("setting");
        assert_eq!(after.priority, expected);
    }
}

#[test]
fn an_invalid_priority_names_the_valid_ones() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "priority");
    let error = StoryService::new(&ctx)
        .set_priority(&story.id, "urgent")
        .unwrap_err();
    assert_eq!(
        validation_message(error),
        "priority must be one of: critical, high, medium, low, none"
    );
}

#[test]
fn labels_are_added_removed_and_kept_sorted() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "labelled");

    let after = service
        .set_labels(&story.id, &["zeta".into(), "alpha".into()], &[])
        .expect("adding labels");
    assert_eq!(after.labels, ["alpha", "zeta"]);

    let after = service
        .set_labels(&story.id, &["mid".into()], &["zeta".into()])
        .expect("adding and removing");
    assert_eq!(after.labels, ["alpha", "mid"]);
}

/// SH-164: `set_labels` is what the REST `/labels` route calls directly with
/// a raw JSON array — nothing upstream of it guarantees a comma-bearing value
/// was already split, so it has to normalize `add`/`remove` itself. Also
/// covers the self-heal: a label written before the guard existed (`web,sse`
/// as one label, simulated here rather than through the service, which could
/// no longer produce it) is split on its way back out by any edit that
/// touches the set.
#[test]
fn set_labels_splits_a_comma_bearing_add_and_can_remove_what_it_split() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "comma in add");

    let after = service
        .set_labels(&story.id, &["web,sse".into()], &[])
        .expect("adding a comma-bearing value");
    assert_eq!(after.labels, ["sse", "web"]);

    let after = service
        .set_labels(&story.id, &[], &["sse,web".into()])
        .expect("removing via a comma-bearing value");
    assert!(after.labels.is_empty(), "{:?}", after.labels);
}

#[test]
fn adding_a_label_twice_leaves_one() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "dupes");
    service
        .set_labels(&story.id, &["one".into()], &[])
        .expect("first add");
    let after = service
        .set_labels(&story.id, &["one".into()], &[])
        .expect("second add");
    assert_eq!(after.labels, ["one"]);
}

#[test]
fn removing_a_label_the_story_never_had_changes_nothing() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "nothing to remove");
    let after = StoryService::new(&ctx)
        .set_labels(&story.id, &[], &["absent".into()])
        .expect("removing an absent label");
    assert!(after.labels.is_empty());
}

#[test]
fn awaiting_is_trimmed_and_recorded() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "blocked");
    let after = StoryService::new(&ctx)
        .set_awaiting(&story.id, "  review from Ada  ")
        .expect("setting awaiting");
    assert_eq!(after.awaiting.as_deref(), Some("review from Ada"));
}

#[test]
fn an_empty_awaiting_reason_is_refused() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "blocked on nothing");
    for blank in ["", "   ", "\t\n"] {
        let error = StoryService::new(&ctx)
            .set_awaiting(&story.id, blank)
            .unwrap_err();
        assert_eq!(
            validation_message(error),
            "awaiting reason must not be empty"
        );
    }
}

#[test]
fn clearing_awaiting_on_a_story_that_awaits_nothing_writes_no_event() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "unblocked already");
    let after = StoryService::new(&ctx)
        .clear_awaiting(&story.id)
        .expect("clearing");
    assert_eq!(after.awaiting, None);
    assert_eq!(event_kinds(&fixture, &story.id), ["StoryCreated"]);
}

#[test]
fn clearing_awaiting_removes_it() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "blocked then not");
    service
        .set_awaiting(&story.id, "waiting")
        .expect("setting awaiting");
    let after = service.clear_awaiting(&story.id).expect("clearing");
    assert_eq!(after.awaiting, None);
}

// --- state transitions -----------------------------------------------------

#[test]
fn moving_between_open_states_leaves_the_story_open() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "moving");
    let after = StoryService::new(&ctx)
        .set_state(&story.id, "in-progress", None, None, None)
        .expect("moving");
    assert_eq!(after.state, "in-progress");
    assert_eq!(after.superstate, SuperState::Open);
    assert_eq!(after.closed_at, None);
    assert_eq!(
        event_kinds(&fixture, &story.id),
        ["StoryCreated", "StoryStateChanged"]
    );
}

#[test]
fn moving_into_a_closed_state_archives_the_story_and_stamps_it_closed() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "finishing");
    let after = StoryService::new(&ctx)
        .set_state(&story.id, "done", None, None, None)
        .expect("closing");
    assert_eq!(after.superstate, SuperState::Closed);
    assert_eq!(after.closed_at.as_deref(), Some(FIXTURE_NOW));

    let no = StoryNo::parse_id("SH", &story.id).unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), no))
        .unwrap()
        .unwrap();
    assert!(row.archived, "a closed story is archived");
}

#[test]
fn closing_a_story_that_was_awaiting_something_clears_it_first() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "blocked but done");
    service
        .set_awaiting(&story.id, "review")
        .expect("setting awaiting");
    let after = service
        .set_state(&story.id, "done", None, None, None)
        .expect("closing");

    assert_eq!(after.awaiting, None);
    assert_eq!(
        event_kinds(&fixture, &story.id),
        [
            "StoryCreated",
            "StoryAwaitingSet",
            "StoryStateChanged",
            "StoryAwaitingCleared",
            "StoryClosedAndArchived",
        ]
    );
}

#[test]
fn closing_a_story_that_awaits_nothing_omits_the_awaiting_clear() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "just done");
    StoryService::new(&ctx)
        .set_state(&story.id, "done", None, None, None)
        .expect("closing");
    assert_eq!(
        event_kinds(&fixture, &story.id),
        [
            "StoryCreated",
            "StoryStateChanged",
            "StoryClosedAndArchived"
        ]
    );
}

#[test]
fn a_comment_supplied_with_a_move_lands_between_the_move_and_the_close_markers() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "commented close");
    StoryService::new(&ctx)
        .set_state(&story.id, "done", Some("shipped"), None, None)
        .expect("closing with a comment");
    assert_eq!(
        event_kinds(&fixture, &story.id),
        [
            "StoryCreated",
            "StoryStateChanged",
            "StoryCommentAdded",
            "StoryClosedAndArchived",
        ]
    );
}

#[test]
fn moving_to_an_undefined_state_is_refused() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "nowhere to go");
    let error = StoryService::new(&ctx)
        .set_state(&story.id, "limbo", None, None, None)
        .unwrap_err();
    assert_eq!(validation_message(error), "state `limbo` is not defined");
}

#[test]
fn moving_a_closed_story_is_refused() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "done and dusted");
    service
        .set_state(&story.id, "done", None, None, None)
        .expect("closing");
    let error = service
        .set_state(&story.id, "todo", None, None, None)
        .unwrap_err();
    assert_eq!(
        validation_message(error),
        "story `SH-1` is closed and cannot be modified"
    );
}

// --- compare-and-swap ------------------------------------------------------

#[test]
fn an_if_state_claim_that_matches_succeeds() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "claimable");
    let after = StoryService::new(&ctx)
        .set_state(&story.id, "in-progress", None, Some("todo"), None)
        .expect("claiming");
    assert_eq!(after.state, "in-progress");
}

#[test]
fn an_if_state_claim_that_lost_reports_both_states() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "contended");
    service
        .set_state(&story.id, "in-progress", None, None, None)
        .expect("the winner's move");

    let error = service
        .set_state(&story.id, "in-progress", None, Some("todo"), None)
        .unwrap_err();
    match error {
        AppError::StateConflict(expected, actual) => {
            assert_eq!(expected, "todo");
            assert_eq!(actual, "in-progress");
        }
        other => panic!("expected StateConflict, got {other:?}"),
    }
    assert_eq!(error_exit_code("todo", "in-progress"), 9);
}

fn error_exit_code(expected: &str, actual: &str) -> i32 {
    AppError::StateConflict(expected.to_string(), actual.to_string()).exit_code()
}

#[test]
fn an_if_state_claim_against_a_deleted_story_reports_deleted() {
    // `story delete` leaves the state slug alone, so a stale claim naming the
    // pre-deletion slug would pass a naive comparison.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "deleted mid-claim");
    service.delete(&story.id, "duplicate").expect("deleting");

    let error = service
        .set_state(&story.id, "in-progress", None, Some("todo"), None)
        .unwrap_err();
    match error {
        AppError::StateConflict(expected, actual) => {
            assert_eq!(expected, "todo");
            assert_eq!(actual, "deleted");
        }
        other => panic!("expected StateConflict, got {other:?}"),
    }
}

#[test]
fn an_if_state_claim_against_a_closed_story_is_a_conflict_not_a_validation_error() {
    // The story is closed, so `resolve_open_story` would refuse it — but a
    // caller who claimed `todo` lost a race, and a lost race is a conflict.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "closed under us");
    service
        .set_state(&story.id, "done", None, None, None)
        .expect("closing");

    let error = service
        .set_state(&story.id, "in-progress", None, Some("todo"), None)
        .unwrap_err();
    match error {
        AppError::StateConflict(expected, actual) => {
            assert_eq!(expected, "todo");
            assert_eq!(actual, "done");
        }
        other => panic!("expected StateConflict, got {other:?}"),
    }
}

#[test]
fn two_concurrent_claims_produce_exactly_one_winner() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "one winner only");
    let id = story.id.clone();

    let results = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let id = id.clone();
                let fixture = &fixture;
                scope.spawn(move || {
                    let ctx = fixture.ctx();
                    StoryService::new(&ctx).set_state(&id, "in-progress", None, Some("todo"), None)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("the claim thread"))
            .collect::<Vec<_>>()
    });

    let winners = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(winners, 1, "exactly one claim must win: {results:?}");
    let loser = results
        .into_iter()
        .find_map(Result::err)
        .expect("one claim must lose");
    match loser {
        AppError::StateConflict(expected, actual) => {
            assert_eq!(expected, "todo");
            assert_eq!(actual, "in-progress");
        }
        other => panic!("the loser must report a conflict, got {other:?}"),
    }
    assert_eq!(snapshot(&fixture, &id).state, "in-progress");
}

#[test]
fn a_story_moved_twice_ends_where_the_second_move_put_it() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "back and forth");
    service
        .set_state(&story.id, "in-progress", None, None, None)
        .unwrap();
    let after = service
        .set_state(&story.id, "todo", None, None, None)
        .unwrap();
    assert_eq!(after.state, "todo");
}

// --- set_state's optional `awaiting` reason (SH-205) ------------------------

/// The common case: `story move <id> blocked --reason "..."` lands both the
/// state change and the reason in one call, one transaction.
#[test]
fn set_state_with_awaiting_sets_state_and_reason_atomically() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "stuck");
    let after = StoryService::new(&ctx)
        .set_state(&story.id, "blocked", None, None, Some("waiting on SH-9"))
        .expect("blocking with a reason");

    assert_eq!(after.state, "blocked");
    assert_eq!(after.awaiting.as_deref(), Some("waiting on SH-9"));
    assert_eq!(
        event_kinds(&fixture, &story.id),
        ["StoryCreated", "StoryStateChanged", "StoryAwaitingSet"],
        "state and awaiting land as one batch, not two separate writes"
    );
}

/// Omitting `awaiting` — every unmodified caller, scripts included — behaves
/// exactly as before: no `StoryAwaitingSet` event at all.
#[test]
fn set_state_without_awaiting_is_unchanged() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "stuck, unexplained");
    let after = StoryService::new(&ctx)
        .set_state(&story.id, "blocked", None, None, None)
        .expect("blocking without a reason");

    assert_eq!(after.state, "blocked");
    assert_eq!(after.awaiting, None);
    assert_eq!(
        event_kinds(&fixture, &story.id),
        ["StoryCreated", "StoryStateChanged"]
    );
}

/// A comment and a reason can be set in the same move — both land in the
/// same batch, comment first (matching the order they're passed in).
#[test]
fn set_state_can_carry_a_comment_and_a_reason_together() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "stuck, noted");
    StoryService::new(&ctx)
        .set_state(
            &story.id,
            "blocked",
            Some("filed the upstream ticket"),
            None,
            Some("waiting on SH-9"),
        )
        .expect("blocking with a comment and a reason");

    assert_eq!(
        event_kinds(&fixture, &story.id),
        [
            "StoryCreated",
            "StoryStateChanged",
            "StoryCommentAdded",
            "StoryAwaitingSet"
        ]
    );
    let after = snapshot(&fixture, &story.id);
    assert_eq!(after.awaiting.as_deref(), Some("waiting on SH-9"));
    assert_eq!(
        after.comments.last().unwrap().text,
        "filed the upstream ticket"
    );
}

/// Mirrors `set_awaiting`'s own validation: a blank reason is refused, not
/// silently trimmed to nothing and stored.
#[test]
fn set_state_awaiting_is_trimmed_and_a_blank_one_is_refused() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);

    let story = new_story(&ctx, "trimmed");
    let after = service
        .set_state(
            &story.id,
            "blocked",
            None,
            None,
            Some("  waiting on SH-9  "),
        )
        .expect("blocking with a padded reason");
    assert_eq!(after.awaiting.as_deref(), Some("waiting on SH-9"));

    for blank in ["", "   ", "\t\n"] {
        let other = new_story(&ctx, "blank");
        let error = service
            .set_state(&other.id, "blocked", None, None, Some(blank))
            .unwrap_err();
        assert_eq!(
            validation_message(error),
            "awaiting reason must not be empty"
        );
    }
}

/// Closing already clears `awaiting` unconditionally
/// (`state_transition_events`) — setting a new one in the same breath it
/// gets cleared has no coherent meaning, so it's refused rather than
/// silently discarded or silently overriding the clear.
#[test]
fn set_state_refuses_a_reason_combined_with_a_move_to_a_closed_state() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "not both");
    let error = StoryService::new(&ctx)
        .set_state(&story.id, "done", None, None, Some("waiting on SH-9"))
        .unwrap_err();

    assert_eq!(
        validation_message(error),
        "--reason cannot be combined with a move to a closed state; awaiting is cleared on close"
    );
    // Refused before any write: the story never moved.
    assert_eq!(snapshot(&fixture, &story.id).state, "todo");
    assert_eq!(
        event_kinds(&fixture, &story.id),
        ["StoryCreated"],
        "a refused call must write nothing"
    );
}

// --- set fields ------------------------------------------------------------

#[test]
fn set_fields_reports_every_change_it_made() {
    let fixture = ServiceFixture::new();
    fixture.add_member("ada", "Ada Lovelace", None);
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "before");
    let message = StoryService::new(&ctx)
        .set_fields(
            &story.id,
            &FieldEdits {
                title: Some("after".into()),
                priority: Some("high".into()),
                assignee: Some("ada".into()),
                labels: Some("x, y".into()),
                story_type: Some("bug".into()),
                description: Some("described".into()),
                ..FieldEdits::default()
            },
        )
        .expect("setting fields");

    assert_eq!(
        message,
        "updated SH-1: title -> after, priority -> high, assignee -> ada, labels += x, y, \
         type -> bug, description updated"
    );
    let after = snapshot(&fixture, &story.id);
    assert_eq!(after.title, "after");
    assert_eq!(after.labels, ["x", "y"]);
}

#[test]
fn set_fields_with_nothing_to_do_is_a_usage_error() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "no edits");
    let error = StoryService::new(&ctx)
        .set_fields(&story.id, &FieldEdits::default())
        .unwrap_err();
    match error {
        AppError::Usage(message) => assert_eq!(message, "no fields to update"),
        other => panic!("expected Usage, got {other:?}"),
    }
}

#[test]
fn set_fields_can_close_a_story() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "closing via set");
    StoryService::new(&ctx)
        .set_fields(
            &story.id,
            &FieldEdits {
                state: Some("done".into()),
                ..FieldEdits::default()
            },
        )
        .expect("closing via set");
    let after = snapshot(&fixture, &story.id);
    assert_eq!(after.superstate, SuperState::Closed);
    assert_eq!(
        event_kinds(&fixture, &story.id),
        [
            "StoryCreated",
            "StoryStateChanged",
            "StoryClosedAndArchived"
        ]
    );
}

#[test]
fn set_fields_closing_a_blocked_story_emits_the_same_batch_as_a_move() {
    // The whole point of the single transition function: `story set --state`
    // and `story move` must not be able to disagree.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);

    let via_set = new_story(&ctx, "via set");
    service.set_awaiting(&via_set.id, "review").unwrap();
    service
        .set_fields(
            &via_set.id,
            &FieldEdits {
                state: Some("done".into()),
                ..FieldEdits::default()
            },
        )
        .unwrap();

    let via_move = new_story(&ctx, "via move");
    service.set_awaiting(&via_move.id, "review").unwrap();
    service
        .set_state(&via_move.id, "done", None, None, None)
        .unwrap();

    assert_eq!(
        event_kinds(&fixture, &via_set.id),
        event_kinds(&fixture, &via_move.id)
    );
}

#[test]
fn set_fields_blocks_and_unblocks() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "blocking");
    service
        .set_fields(
            &story.id,
            &FieldEdits {
                blocked: Some("waiting on CI".into()),
                ..FieldEdits::default()
            },
        )
        .unwrap();
    assert_eq!(
        snapshot(&fixture, &story.id).awaiting.as_deref(),
        Some("waiting on CI")
    );

    service
        .set_fields(
            &story.id,
            &FieldEdits {
                unblocked: true,
                ..FieldEdits::default()
            },
        )
        .unwrap();
    assert_eq!(snapshot(&fixture, &story.id).awaiting, None);
}

#[test]
fn set_fields_adds_labels_rather_than_replacing_them() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "additive");
    service
        .set_labels(&story.id, &["kept".into()], &[])
        .unwrap();
    service
        .set_fields(
            &story.id,
            &FieldEdits {
                labels: Some("added".into()),
                ..FieldEdits::default()
            },
        )
        .unwrap();
    assert_eq!(snapshot(&fixture, &story.id).labels, ["added", "kept"]);
}

#[test]
fn set_fields_rejects_an_unknown_assignee_as_invalid_input() {
    // `story assign` says not-found; `story set --assignee` says invalid. Both
    // spellings are pinned by the error contract, so neither may drift.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "assignee");
    let error = StoryService::new(&ctx)
        .set_fields(
            &story.id,
            &FieldEdits {
                assignee: Some("nobody".into()),
                ..FieldEdits::default()
            },
        )
        .unwrap_err();
    assert_eq!(validation_message(error), "member `nobody` not found");
}

#[test]
fn a_rejected_field_in_a_batch_writes_none_of_it() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "atomic edits");
    let _ = StoryService::new(&ctx).set_fields(
        &story.id,
        &FieldEdits {
            title: Some("should not stick".into()),
            priority: Some("urgent".into()),
            ..FieldEdits::default()
        },
    );
    assert_eq!(snapshot(&fixture, &story.id).title, "atomic edits");
    assert_eq!(event_kinds(&fixture, &story.id), ["StoryCreated"]);
}

// --- set fields: the JSON patch --------------------------------------------

#[test]
fn the_json_patch_applies_every_supported_key() {
    let fixture = ServiceFixture::new();
    fixture.add_member("ada", "Ada Lovelace", None);
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "patchable");
    StoryService::new(&ctx)
        .set_fields(
            &story.id,
            &FieldEdits {
                json: Some(
                    r#"{"title":"patched","priority":"low","assignee":"ada",
                        "labels":["p","q"],"blocked":"waiting","story_type":"bug",
                        "description":"new"}"#
                        .into(),
                ),
                ..FieldEdits::default()
            },
        )
        .expect("patching");

    let after = snapshot(&fixture, &story.id);
    assert_eq!(after.title, "patched");
    assert_eq!(after.priority, Priority::Low);
    assert_eq!(after.assignee.as_deref(), Some("ada"));
    assert_eq!(after.labels, ["p", "q"]);
    assert_eq!(after.awaiting.as_deref(), Some("waiting"));
    assert_eq!(after.story_type.as_deref(), Some("bug"));
    assert_eq!(after.description.as_deref(), Some("new"));
}

#[test]
fn the_json_patch_replaces_labels_wholesale() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "replaced");
    service.set_labels(&story.id, &["old".into()], &[]).unwrap();
    service
        .set_fields(
            &story.id,
            &FieldEdits {
                json: Some(r#"{"labels":["new"]}"#.into()),
                ..FieldEdits::default()
            },
        )
        .unwrap();
    assert_eq!(snapshot(&fixture, &story.id).labels, ["new"]);
}

/// SH-164: a JSON array is exactly the shape a REST caller hands in, and
/// nothing guarantees a comma-bearing value inside it was already split.
#[test]
fn the_json_patch_splits_a_comma_bearing_label() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "comma in json");
    service
        .set_fields(
            &story.id,
            &FieldEdits {
                json: Some(r#"{"labels":["web,sse"]}"#.into()),
                ..FieldEdits::default()
            },
        )
        .unwrap();
    assert_eq!(snapshot(&fixture, &story.id).labels, ["sse", "web"]);
}

#[test]
fn the_json_patch_treats_a_null_or_empty_blocked_as_an_unblock() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    for patch in [r#"{"blocked":null}"#, r#"{"blocked":""}"#] {
        let story = new_story(&ctx, "unblocking");
        service.set_awaiting(&story.id, "something").unwrap();
        service
            .set_fields(
                &story.id,
                &FieldEdits {
                    json: Some(patch.into()),
                    ..FieldEdits::default()
                },
            )
            .unwrap();
        assert_eq!(snapshot(&fixture, &story.id).awaiting, None, "{patch}");
    }
}

#[test]
fn the_json_patch_reports_a_cleared_assignee_without_writing_an_event() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "cleared");
    let message = StoryService::new(&ctx)
        .set_fields(
            &story.id,
            &FieldEdits {
                title: Some("kept".into()),
                json: Some(r#"{"assignee":null}"#.into()),
                ..FieldEdits::default()
            },
        )
        .unwrap();
    assert!(message.contains("assignee cleared"), "{message}");
    assert_eq!(
        event_kinds(&fixture, &story.id),
        ["StoryCreated", "StoryTitleSet"]
    );
}

#[test]
fn the_json_patch_ignores_an_empty_title() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "kept");
    let error = StoryService::new(&ctx)
        .set_fields(
            &story.id,
            &FieldEdits {
                json: Some(r#"{"title":""}"#.into()),
                ..FieldEdits::default()
            },
        )
        .unwrap_err();
    assert!(matches!(error, AppError::Usage(_)), "{error:?}");
    assert_eq!(snapshot(&fixture, &story.id).title, "kept");
}

#[test]
fn the_json_patch_rejects_malformed_input() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "malformed");
    let cases = [
        (r#"{"#, "invalid JSON"),
        (r#"[1,2]"#, "JSON must be an object"),
        (r#"{"title":7}"#, "title must be a string"),
        (r#"{"state":7}"#, "state must be a string"),
        (r#"{"priority":"urgent"}"#, "invalid priority `urgent`"),
        (r#"{"assignee":7}"#, "assignee must be a string or null"),
        (r#"{"labels":"x"}"#, "labels must be an array of strings"),
        (r#"{"blocked":7}"#, "blocked must be a string or null"),
        (r#"{"story_type":"chore"}"#, "unknown type `chore`"),
        (r#"{"description":7}"#, "description must be a string"),
        (r#"{"nope":"x"}"#, "unknown field `nope`"),
        (r#"{"state":"limbo"}"#, "state `limbo` is not defined"),
    ];
    for (patch, expected) in cases {
        let error = StoryService::new(&ctx)
            .set_fields(
                &story.id,
                &FieldEdits {
                    json: Some(patch.into()),
                    ..FieldEdits::default()
                },
            )
            .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains(expected),
            "`{patch}` gave `{message}`, expected it to mention `{expected}`"
        );
    }
    assert_eq!(event_kinds(&fixture, &story.id), ["StoryCreated"]);
}

#[test]
fn the_json_patch_can_close_a_story() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "closed by patch");
    StoryService::new(&ctx)
        .set_fields(
            &story.id,
            &FieldEdits {
                json: Some(r#"{"state":"done"}"#.into()),
                ..FieldEdits::default()
            },
        )
        .unwrap();
    assert_eq!(snapshot(&fixture, &story.id).superstate, SuperState::Closed);
}

// --- bulk update -----------------------------------------------------------

#[test]
fn bulk_update_reports_one_line_per_story() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let first = new_story(&ctx, "one");
    let second = new_story(&ctx, "two");
    let message = StoryService::new(&ctx)
        .bulk_update(&[
            (first.id.clone(), "in-progress".into()),
            (second.id.clone(), "done".into()),
        ])
        .expect("bulk updating");
    assert_eq!(message, "SH-1: in-progress\nSH-2: done (archived)");
}

#[test]
fn bulk_update_reports_an_undefined_state_without_touching_the_story() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "untouched");
    let message = StoryService::new(&ctx)
        .bulk_update(&[(story.id.clone(), "limbo".into())])
        .unwrap();
    assert_eq!(message, "SH-1: error — state `limbo` is not defined");
    assert_eq!(event_kinds(&fixture, &story.id), ["StoryCreated"]);
}

#[test]
fn bulk_update_reports_a_missing_or_closed_story_the_same_way() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let closed = new_story(&ctx, "already closed");
    service
        .set_state(&closed.id, "done", None, None, None)
        .unwrap();

    let message = service
        .bulk_update(&[
            ("SH-99".into(), "todo".into()),
            (closed.id.clone(), "todo".into()),
        ])
        .unwrap();
    assert_eq!(
        message,
        "SH-99: error — story not found or not open\nSH-1: error — story not found or not open"
    );
}

#[test]
fn a_failing_item_does_not_stop_the_rest_of_the_batch() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "still moves");
    let message = StoryService::new(&ctx)
        .bulk_update(&[
            ("SH-99".into(), "todo".into()),
            (story.id.clone(), "in-progress".into()),
        ])
        .unwrap();
    assert!(message.ends_with("SH-1: in-progress"), "{message}");
    assert_eq!(snapshot(&fixture, &story.id).state, "in-progress");
}

#[test]
fn each_bulk_item_is_atomic_on_its_own() {
    // A closing item writes its state change, its close marker, its snapshot
    // and its archived flag together or not at all — the three-filesystem-op
    // sequence this replaces could leave any prefix of that behind.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "atomic item");
    service.set_awaiting(&story.id, "review").unwrap();
    service
        .bulk_update(&[(story.id.clone(), "done".into())])
        .unwrap();

    let after = snapshot(&fixture, &story.id);
    assert_eq!(after.superstate, SuperState::Closed);
    assert_eq!(after.awaiting, None);
    assert!(after.closed_at.is_some());
    fixture.assert_no_drift();
}

#[test]
fn an_empty_bulk_update_produces_an_empty_message() {
    let fixture = ServiceFixture::new();
    assert_eq!(
        StoryService::new(&fixture.ctx()).bulk_update(&[]).unwrap(),
        ""
    );
}

// --- delete / reopen -------------------------------------------------------

#[test]
fn deleting_a_story_closes_it_and_records_the_reason() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "mistake");
    let message = StoryService::new(&ctx)
        .delete(&story.id, "created in error")
        .expect("deleting");
    assert_eq!(message, "deleted SH-1: created in error");

    let after = snapshot(&fixture, &story.id);
    assert!(after.deleted);
    assert_eq!(after.deleted_reason.as_deref(), Some("created in error"));
    assert_eq!(after.superstate, SuperState::Closed);
    assert_eq!(after.comments[0].text, "[deleted] created in error");
    // SH-130: the story comes to rest somewhere genuinely CLOSED, rather than
    // keeping an OPEN slug while claiming CLOSED. The truthful record of what
    // it was lives in the event log, which is append-only and cannot lie; the
    // read model says where the story is now.
    assert_eq!(after.state, "done");
}

#[test]
fn deleting_a_story_twice_reports_it_as_missing() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "gone");
    service.delete(&story.id, "first").unwrap();
    let error = service.delete(&story.id, "second").unwrap_err();
    match error {
        AppError::NotFound(message) => assert_eq!(message, "story `SH-1` not found"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn deleting_a_story_that_never_existed_is_not_found() {
    let fixture = ServiceFixture::new();
    let error = StoryService::new(&fixture.ctx())
        .delete("SH-1", "why")
        .unwrap_err();
    assert!(matches!(error, AppError::NotFound(_)), "{error:?}");
}

#[test]
fn reopening_a_closed_story_returns_it_to_the_default_open_state() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "back from the dead");
    service
        .set_state(&story.id, "done", None, None, None)
        .unwrap();

    let reopened = service.reopen(&story.id).expect("reopening");
    assert_eq!(reopened.state, "todo");
    assert_eq!(reopened.superstate, SuperState::Open);
    assert_eq!(reopened.closed_at, None);

    let no = StoryNo::parse_id("SH", &story.id).unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), no))
        .unwrap()
        .unwrap();
    assert!(!row.archived, "a reopened story is not archived");
}

#[test]
fn reopening_preserves_the_whole_event_log() {
    // The legacy path deleted the close markers from the log. This one adds an
    // event instead, so the history of the closure survives the reopen.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "history kept");
    service
        .set_state(&story.id, "done", None, None, None)
        .unwrap();
    service.reopen(&story.id).unwrap();

    assert_eq!(
        event_kinds(&fixture, &story.id),
        [
            "StoryCreated",
            "StoryStateChanged",
            "StoryClosedAndArchived",
            "StoryStateChanged",
        ]
    );
}

#[test]
fn reopening_an_open_story_is_refused() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "already open");
    let error = StoryService::new(&ctx).reopen(&story.id).unwrap_err();
    assert_eq!(validation_message(error), "story `SH-1` is already open");
}

#[test]
fn reopen_plan_of_an_open_story_is_refused() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "already open");
    let error = StoryService::new(&ctx).reopen_plan(&story.id).unwrap_err();
    assert_eq!(validation_message(error), "story `SH-1` is already open");
}

#[test]
fn reopening_a_story_that_does_not_exist_is_not_found() {
    let fixture = ServiceFixture::new();
    let error = StoryService::new(&fixture.ctx())
        .reopen("SH-1")
        .unwrap_err();
    assert!(matches!(error, AppError::NotFound(_)), "{error:?}");
}

/// SH-154: `confirm_undelete` used to answer this from inside `reopen`
/// itself, by reading stdin — which runs inside the daemon and never has a
/// terminal, so it always errored naming `--force` regardless of who was
/// asking or from where. `reopen_plan` is the fix: a plain read that hands
/// back what an undelete would restore, so the question can travel to
/// whichever process — `main.rs` — actually has a terminal to ask it at.
#[test]
fn reopen_plan_of_a_deleted_story_returns_what_it_would_undelete() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "deleted");
    service.delete(&story.id, "an error").unwrap();

    let plan = service
        .reopen_plan(&story.id)
        .expect("reading the plan")
        .expect("a deleted story has a plan");
    assert_eq!(plan.id, "SH-1");
    assert_eq!(plan.title, "deleted");
    assert_eq!(plan.deleted_reason.as_deref(), Some("an error"));
}

/// The sibling of the plan test above: an ordinary closed story (never
/// deleted) needs no confirmation at all, so `reopen_plan` answers `None`
/// rather than a plan nobody should be asked to confirm.
#[test]
fn reopen_plan_of_an_ordinarily_closed_story_is_none() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "just closed");
    service
        .set_state(&story.id, "done", None, None, None)
        .unwrap();

    let plan = service.reopen_plan(&story.id).expect("reading the plan");
    assert!(plan.is_none(), "{plan:?}");
}

#[test]
fn reopening_a_deleted_story_restores_it_and_keeps_the_audit_comment() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "undeleted");
    service.delete(&story.id, "an error").unwrap();

    let reopened = service.reopen(&story.id).expect("reopening");
    assert!(!reopened.deleted);
    assert_eq!(reopened.deleted_reason, None);
    assert_eq!(reopened.superstate, SuperState::Open);
    assert_eq!(reopened.comments[0].text, "[deleted] an error");
}

#[test]
fn a_reopened_story_can_be_edited_again() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "usable again");
    service
        .set_state(&story.id, "done", None, None, None)
        .unwrap();
    service.reopen(&story.id).unwrap();
    let after = service.comment(&story.id, "still here").expect("editing");
    assert_eq!(after.comments.len(), 1);
}

// --- hide / unhide / hide-state (SH-43's "Archive") -------------------------

#[test]
fn hiding_a_closed_story_sets_hidden_at() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "closed, then archived");
    service
        .set_state(&story.id, "done", None, None, None)
        .unwrap();

    let hidden = service.hide(&story.id).expect("archiving");
    assert!(hidden.hidden_at.is_some());
    assert_eq!(hidden.superstate, SuperState::Closed);
}

#[test]
fn hiding_an_open_story_is_refused() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "still open");
    let error = StoryService::new(&ctx).hide(&story.id).unwrap_err();
    assert_eq!(
        validation_message(error),
        "story `SH-1` is open; only a closed story can be archived"
    );
}

#[test]
fn hiding_an_already_hidden_story_is_a_no_op() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "archived twice");
    service
        .set_state(&story.id, "done", None, None, None)
        .unwrap();
    let first = service.hide(&story.id).unwrap();

    let second = service.hide(&story.id).unwrap();
    assert_eq!(second.hidden_at, first.hidden_at);
    // No second `StoryHidden` event: the idempotent call appended nothing.
    assert_eq!(
        event_kinds(&fixture, &story.id),
        [
            "StoryCreated",
            "StoryStateChanged",
            "StoryClosedAndArchived",
            "StoryHidden"
        ]
    );
}

#[test]
fn hiding_a_story_that_does_not_exist_is_not_found() {
    let fixture = ServiceFixture::new();
    let error = StoryService::new(&fixture.ctx()).hide("SH-1").unwrap_err();
    assert!(matches!(error, AppError::NotFound(_)), "{error:?}");
}

#[test]
fn unhiding_clears_hidden_at() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "archived then unarchived");
    service
        .set_state(&story.id, "done", None, None, None)
        .unwrap();
    service.hide(&story.id).unwrap();

    let unhidden = service.unhide(&story.id).expect("unarchiving");
    assert_eq!(unhidden.hidden_at, None);
    // Still closed: unhiding is not the same act as reopening.
    assert_eq!(unhidden.superstate, SuperState::Closed);
}

#[test]
fn unhiding_a_story_that_is_not_hidden_is_a_no_op() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "never archived");
    service
        .set_state(&story.id, "done", None, None, None)
        .unwrap();

    service.unhide(&story.id).unwrap();
    assert_eq!(
        event_kinds(&fixture, &story.id),
        [
            "StoryCreated",
            "StoryStateChanged",
            "StoryClosedAndArchived"
        ]
    );
}

#[test]
fn reopening_a_hidden_story_clears_hidden_at() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "archived then reopened");
    service
        .set_state(&story.id, "done", None, None, None)
        .unwrap();
    service.hide(&story.id).unwrap();

    let reopened = service.reopen(&story.id).expect("reopening");
    assert_eq!(reopened.hidden_at, None);
    assert_eq!(reopened.superstate, SuperState::Open);
}

#[test]
fn hide_state_plan_lists_every_undhidden_story_in_the_state() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let a = new_story(&ctx, "a");
    let b = new_story(&ctx, "b");
    let c = new_story(&ctx, "c");
    service.set_state(&a.id, "done", None, None, None).unwrap();
    service.set_state(&b.id, "done", None, None, None).unwrap();
    service.set_state(&c.id, "done", None, None, None).unwrap();
    service.hide(&c.id).unwrap();

    let plan = service.hide_state_plan("done").expect("previewing");
    assert_eq!(plan.state, "done");
    // `c` is already hidden, so the plan does not name it again.
    assert_eq!(plan.ids, vec!["SH-1".to_string(), "SH-2".to_string()]);
}

#[test]
fn hide_state_plan_of_an_open_state_is_refused() {
    let fixture = ServiceFixture::new();
    let error = StoryService::new(&fixture.ctx())
        .hide_state_plan("todo")
        .unwrap_err();
    assert_eq!(
        validation_message(error),
        "state `todo` is open; only a closed-superstate column can be archived"
    );
}

#[test]
fn hide_state_plan_of_an_undefined_state_is_refused() {
    let fixture = ServiceFixture::new();
    let error = StoryService::new(&fixture.ctx())
        .hide_state_plan("nope")
        .unwrap_err();
    assert_eq!(validation_message(error), "state `nope` is not defined");
}

#[test]
fn hide_state_archives_every_story_currently_in_the_column() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let a = new_story(&ctx, "a");
    let b = new_story(&ctx, "b");
    let open = new_story(&ctx, "still open");
    service.set_state(&a.id, "done", None, None, None).unwrap();
    service.set_state(&b.id, "done", None, None, None).unwrap();

    let message = service.hide_state("done").expect("archiving the column");
    assert_eq!(message, "archived 2 stories from `done`");
    assert!(snapshot(&fixture, &a.id).hidden_at.is_some());
    assert!(snapshot(&fixture, &b.id).hidden_at.is_some());
    assert_eq!(snapshot(&fixture, &open.id).hidden_at, None);
}

#[test]
fn hide_state_does_not_re_hide_an_already_hidden_story() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let a = new_story(&ctx, "a");
    service.set_state(&a.id, "done", None, None, None).unwrap();
    let first_hide = service.hide(&a.id).unwrap();

    service.hide_state("done").unwrap();
    assert_eq!(snapshot(&fixture, &a.id).hidden_at, first_hide.hidden_at);
}

// --- draft / publish (SH-175) -----------------------------------------------

fn new_draft_story(ctx: &Ctx<'_, SqliteStore>, title: &str) -> StorySnapshot {
    StoryService::new(ctx)
        .create(&NewStoryInput {
            title: title.to_string(),
            draft: true,
            ..NewStoryInput::default()
        })
        .expect("creating a draft story")
}

#[test]
fn creating_with_draft_true_sets_draft() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_draft_story(&ctx, "a sketch");
    assert!(story.draft);
    assert_eq!(
        event_kinds(&fixture, &story.id),
        ["StoryCreated", "StoryCreatedAsDraft"]
    );
}

#[test]
fn creating_without_draft_is_live_from_the_start() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "live from the start");
    assert!(!story.draft);
    assert_eq!(event_kinds(&fixture, &story.id), ["StoryCreated"]);
}

#[test]
fn publishing_a_draft_clears_draft() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_draft_story(&ctx, "ready to ship");

    let published = service.publish(&story.id).expect("publishing");
    assert!(!published.draft);
    assert_eq!(
        event_kinds(&fixture, &story.id),
        ["StoryCreated", "StoryCreatedAsDraft", "StoryPublished"]
    );
}

#[test]
fn publishing_an_already_live_story_is_a_no_op() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "never a draft");

    let published = service.publish(&story.id).expect("publishing");
    assert!(!published.draft);
    // No `StoryPublished` event: the idempotent call appended nothing.
    assert_eq!(event_kinds(&fixture, &story.id), ["StoryCreated"]);
}

#[test]
fn publishing_a_draft_twice_appends_only_one_event() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_draft_story(&ctx, "published twice");
    let first = service.publish(&story.id).unwrap();

    let second = service.publish(&story.id).unwrap();
    assert!(!second.draft);
    assert_eq!(second.updated_at, first.updated_at);
    assert_eq!(
        event_kinds(&fixture, &story.id),
        ["StoryCreated", "StoryCreatedAsDraft", "StoryPublished"]
    );
}

#[test]
fn publishing_a_story_that_does_not_exist_is_not_found() {
    let fixture = ServiceFixture::new();
    let error = StoryService::new(&fixture.ctx())
        .publish("SH-1")
        .unwrap_err();
    assert!(matches!(error, AppError::NotFound(_)), "{error:?}");
}

/// The irreversibility contract itself: there is no service method that
/// re-drafts a published story. This test exists to fail loudly — a
/// compile error, not a silent gap — if a future change adds one without a
/// corresponding decision to relax SH-175's "irreversible... we might
/// reconsider this rule in a future version" wording. It does that by
/// exercising the one and only path to `draft: true` (creation) and the
/// one and only path away from it (`publish`), and asserting neither can be
/// reached a second time in the other direction.
#[test]
fn there_is_no_verb_that_turns_a_live_story_back_into_a_draft() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_draft_story(&ctx, "one-way door");
    let published = service.publish(&story.id).expect("publishing");
    assert!(!published.draft);

    // Every other mutation available on an open story — comment, set
    // fields, relate — leaves `draft` alone. Spot-checked here with the one
    // most likely to have been wired wrong: a full `set_fields` patch that
    // touches unrelated fields must not touch `draft` as a side effect.
    service
        .set_fields(
            &story.id,
            &storyhook::service::FieldEdits {
                title: Some("renamed".to_string()),
                ..storyhook::service::FieldEdits::default()
            },
        )
        .expect("editing fields");
    assert!(
        !snapshot(&fixture, &story.id).draft,
        "an unrelated field edit must not re-draft a published story"
    );
}

// --- catalogs other than the default ---------------------------------------

#[test]
fn a_project_whose_first_state_is_closed_still_opens_stories_in_an_open_one() {
    let fixture = ServiceFixture::with_states(&[
        StateDef {
            slug: "archived".into(),
            super_state: SuperState::Closed,
            role: None,
            description: None,
        },
        StateDef {
            slug: "backlog".into(),
            super_state: SuperState::Open,
            role: None,
            description: None,
        },
    ]);
    let story = new_story(&fixture.ctx(), "unusual catalog");
    assert_eq!(story.state, "backlog");
}

#[test]
fn a_project_with_no_open_state_cannot_create_a_story() {
    let fixture = ServiceFixture::with_states(&[StateDef {
        slug: "archived".into(),
        super_state: SuperState::Closed,
        role: None,
        description: None,
    }]);
    let error = StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "nowhere to live".into(),
            ..NewStoryInput::default()
        })
        .unwrap_err();
    assert_eq!(
        validation_message(error),
        "project has no OPEN-mapped default state"
    );
}

// --- clock -----------------------------------------------------------------

#[test]
fn every_event_in_one_call_shares_one_timestamp() {
    let fixture = ServiceFixture::new();
    let ctx = fixture
        .ctx()
        .clock(Clock::Fixed("2026-05-05T05:05:05Z".into()));
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "one instant");
    service.set_awaiting(&story.id, "review").unwrap();
    service
        .set_state(&story.id, "done", Some("ship"), None, None)
        .unwrap();

    let no = StoryNo::parse_id("SH", &story.id).unwrap();
    let stamps: Vec<String> = fixture
        .store()
        .read(|tx| tx.events_for(fixture.project(), no))
        .unwrap()
        .into_iter()
        .map(|event| event.at)
        .collect();
    assert!(
        stamps.iter().all(|at| at == "2026-05-05T05:05:05Z"),
        "{stamps:?}"
    );
}

// --- hooks -----------------------------------------------------------------

/// Records every hook that fired, one line per event, into `hooks.log`.
fn record_all_hooks(fixture: &ServiceFixture) {
    let events = [
        "on_create",
        "on_state_change",
        "on_close",
        "on_comment",
        "on_priority_change",
        "on_label_change",
        "on_relationship_change",
    ];
    let body: String = events
        .iter()
        .map(|event| format!("{event} = {{ command = \"cat >> hooks.log; echo >> hooks.log\" }}\n"))
        .collect();
    fixture.write_hooks_toml(&body);
}

fn fired_hooks(fixture: &ServiceFixture) -> Vec<String> {
    let path = fixture.cwd().join("hooks.log");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            serde_json::from_str::<BTreeMap<String, serde_json::Value>>(line)
                .ok()?
                .get("event_type")?
                .as_str()
                .map(str::to_string)
        })
        .collect()
}

#[test]
fn creating_a_story_fires_the_create_hook() {
    let fixture = ServiceFixture::new();
    record_all_hooks(&fixture);
    new_story(&fixture.ctx(), "hooked");
    assert_eq!(fired_hooks(&fixture), ["create"]);
}

#[test]
fn no_hooks_suppresses_every_hook() {
    let fixture = ServiceFixture::new();
    record_all_hooks(&fixture);
    let ctx = fixture.ctx().no_hooks(true);
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "silent");
    service.comment(&story.id, "quiet").unwrap();
    service
        .set_state(&story.id, "done", None, None, None)
        .unwrap();
    assert!(fired_hooks(&fixture).is_empty());
}

#[test]
fn a_hook_running_inside_a_hook_fires_nothing() {
    // The depth guard, without which a hook that shells out to `story` re-enters
    // the hook that spawned it, forever.
    let fixture = ServiceFixture::new();
    record_all_hooks(&fixture);
    let ctx = fixture.ctx().hook_depth(1);
    new_story(&ctx, "nested");
    assert!(fired_hooks(&fixture).is_empty());
}

#[test]
fn closing_a_story_fires_the_state_change_hook_and_then_the_close_hook() {
    let fixture = ServiceFixture::new();
    record_all_hooks(&fixture);
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "closing");
    StoryService::new(&ctx)
        .set_state(&story.id, "done", None, None, None)
        .unwrap();
    assert_eq!(fired_hooks(&fixture), ["create", "state_change", "close"]);
}

#[test]
fn moving_between_open_states_does_not_fire_the_close_hook() {
    let fixture = ServiceFixture::new();
    record_all_hooks(&fixture);
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "still open");
    StoryService::new(&ctx)
        .set_state(&story.id, "in-progress", None, None, None)
        .unwrap();
    assert_eq!(fired_hooks(&fixture), ["create", "state_change"]);
}

#[test]
fn comment_priority_and_label_edits_each_fire_their_own_hook() {
    let fixture = ServiceFixture::new();
    record_all_hooks(&fixture);
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "noisy");
    service.comment(&story.id, "hi").unwrap();
    service.set_priority(&story.id, "high").unwrap();
    service.set_labels(&story.id, &["x".into()], &[]).unwrap();
    assert_eq!(
        fired_hooks(&fixture),
        ["create", "comment", "priority_change", "label_change"]
    );
}

#[test]
fn assignment_and_awaiting_edits_fire_no_hook() {
    let fixture = ServiceFixture::new();
    fixture.add_member("ada", "Ada Lovelace", None);
    record_all_hooks(&fixture);
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "quiet edits");
    service.assign(&story.id, "ada").unwrap();
    service.set_awaiting(&story.id, "review").unwrap();
    service.clear_awaiting(&story.id).unwrap();
    assert_eq!(fired_hooks(&fixture), ["create"]);
}

#[test]
fn a_rejected_operation_fires_no_hook() {
    let fixture = ServiceFixture::new();
    record_all_hooks(&fixture);
    let ctx = fixture.ctx();
    let story = new_story(&ctx, "rejected");
    let _ = StoryService::new(&ctx).set_state(&story.id, "limbo", None, None, None);
    assert_eq!(fired_hooks(&fixture), ["create"]);
}

#[test]
fn reopening_fires_a_state_change_hook_from_closed() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    let story = new_story(&ctx, "reopened");
    service
        .set_state(&story.id, "done", None, None, None)
        .unwrap();
    record_all_hooks(&fixture);
    service.reopen(&story.id).unwrap();

    let payload: serde_json::Value = serde_json::from_str(
        std::fs::read_to_string(fixture.cwd().join("hooks.log"))
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(payload["from_state"], "closed");
    assert_eq!(payload["to_state"], "todo");
}
