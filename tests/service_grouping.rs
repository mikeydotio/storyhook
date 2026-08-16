//! Invariants of the grouping service.
//!
//! Phases and epics are conventions over labels and relations rather than
//! things in their own right, so what these tests pin is the conventions: one
//! phase per story, a phase's title coming from the story that names it, an
//! epic membership being exactly a `parent-of` edge, and the writes landing
//! atomically like every other service write.

use storyhook::domain::{StoryRelation, StorySnapshot};
use storyhook::error::AppError;
use storyhook::service::{
    Ctx, GroupingService, NewStoryInput, PhaseCleared, RelationService, StoryService,
};
use storyhook::store::{ReadOps, SqliteStore, Store, StoryNo, WriteOps};
use storyhook_test_support::ServiceFixture;

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

fn snapshot(fixture: &ServiceFixture, id: &str) -> StorySnapshot {
    let no = StoryNo::parse_id("SH", id).expect("a well-formed id");
    fixture
        .store()
        .read(|tx| tx.story(fixture.project(), no))
        .expect("reading the story")
        .expect("the story exists")
        .snapshot
}

fn relations(story: &StorySnapshot) -> Vec<(String, String)> {
    story
        .relationships
        .iter()
        .map(|StoryRelation { relation, other_id }| (relation.clone(), other_id.clone()))
        .collect()
}

// --- phases ----------------------------------------------------------------

#[test]
fn a_story_belongs_to_at_most_one_phase() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "moves around");
    let service = GroupingService::new(&ctx);

    service.assign_phase(&id, "1").expect("assigning");
    service.assign_phase(&id, "2").expect("reassigning");
    assert_eq!(snapshot(&fixture, &id).labels, ["phase:2"]);
}

#[test]
fn assigning_a_phase_leaves_a_storys_other_labels_alone() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "labelled");
    StoryService::new(&ctx)
        .set_labels(&id, &["urgent".into(), "ui".into()], &[])
        .expect("labelling");

    GroupingService::new(&ctx)
        .assign_phase(&id, "3")
        .expect("assigning");
    assert_eq!(snapshot(&fixture, &id).labels, ["phase:3", "ui", "urgent"]);

    GroupingService::new(&ctx)
        .clear_phase(&id)
        .expect("clearing");
    assert_eq!(snapshot(&fixture, &id).labels, ["ui", "urgent"]);
}

#[test]
fn clearing_a_phase_a_story_never_had_writes_nothing() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "unphased");
    let before = fixture
        .store()
        .read(|tx| tx.head_seq(fixture.project(), StoryNo::parse_id("SH", &id).unwrap()))
        .expect("reading the head");

    let outcome = GroupingService::new(&ctx)
        .clear_phase(&id)
        .expect("clearing");
    assert_eq!(outcome, PhaseCleared::NoAssignment);

    let after = fixture
        .store()
        .read(|tx| tx.head_seq(fixture.project(), StoryNo::parse_id("SH", &id).unwrap()))
        .expect("reading the head");
    assert_eq!(before, after, "a no-op still appended an event");
}

#[test]
fn a_closed_story_cannot_be_phased() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "done already");
    StoryService::new(&ctx)
        .set_state(&id, "done", None, None, None)
        .expect("closing");

    let service = GroupingService::new(&ctx);
    let error = service.assign_phase(&id, "1").unwrap_err();
    assert!(matches!(error, AppError::Validation(_)), "{error:?}");
    // A phase is scope, so this stays on the edit side of SH-261's line — and
    // the whole refusal is asserted rather than a fragment of it, because a
    // fragment is what let this assertion survive the sentence changing.
    assert!(
        error.to_string().contains(
            "story `SH-1` is closed; reopen it with `story reopen SH-1` to change it \
             — a comment needs no reopen"
        ),
        "{error}"
    );
    assert!(service.clear_phase(&id).is_err());
}

#[test]
fn phasing_an_unknown_story_is_not_found() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let error = GroupingService::new(&ctx)
        .assign_phase("SH-99", "1")
        .unwrap_err();
    assert!(matches!(error, AppError::NotFound(_)), "{error:?}");
}

#[test]
fn creating_a_phase_makes_a_story_that_names_and_carries_it() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = GroupingService::new(&ctx);
    let titled = service
        .create_phase("1", Some("the migration"))
        .expect("creating");
    let bare = service.create_phase("2", None).expect("creating");

    assert_eq!(titled.title, "Phase 1: the migration");
    assert_eq!(titled.labels, ["phase:1"]);
    assert_eq!(bare.title, "Phase 2");
    assert_eq!(bare.labels, ["phase:2"]);

    let phases = service.phases().expect("listing phases");
    assert_eq!(
        phases.iter().map(|p| p.phase.as_str()).collect::<Vec<_>>(),
        ["1", "2"]
    );
    assert_eq!(phases[0].title.as_deref(), Some("the migration"));
    assert_eq!(
        phases[1].title, None,
        "a story called `Phase 2` belongs to the phase without naming it"
    );
}

#[test]
fn a_phase_rollup_sorts_every_story_into_exactly_one_bucket() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = GroupingService::new(&ctx);
    let ids: Vec<String> = (0..5)
        .map(|i| new_story(&ctx, &format!("story {i}")))
        .collect();
    for id in &ids {
        service.assign_phase(id, "1").expect("assigning");
    }
    StoryService::new(&ctx)
        .set_state(&ids[1], "in-progress", None, None, None)
        .expect("starting");
    StoryService::new(&ctx)
        .set_state(&ids[2], "done", None, None, None)
        .expect("closing");
    RelationService::new(&ctx)
        .relate(&ids[4], "blocks", &ids[3], false)
        .expect("blocking");

    let phases = service.phases().expect("listing");
    assert_eq!(phases.len(), 1);
    let phase = &phases[0];
    assert_eq!(phase.total, 5);
    assert_eq!(phase.done, 1);
    assert_eq!(phase.in_progress, 1);
    assert_eq!(phase.blocked, 1);
    assert_eq!(phase.todo, 2);
    assert_eq!(
        phase.done + phase.in_progress + phase.blocked + phase.todo,
        phase.total,
        "the buckets must partition the phase"
    );
    assert_eq!(phase.story_ids.len(), 5);
}

/// Regression test for SH-126 (council verdict, recorded on that story): a
/// story parked
/// in the literal `blocked` state, with no unmet `blocked-by` edge and no
/// `awaiting` reason, used to fall through to the `in_progress` bucket here
/// — `rollup` buckets a story as *blocked* purely via `!is_ready(...)`, and
/// `is_ready` never inspected `story.state`. The sibling defect predates
/// SH-126 and is fixed for free by the same `is_ready` correction.
#[test]
fn a_story_in_the_blocked_state_rolls_up_as_blocked_even_with_no_unmet_dependency() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = GroupingService::new(&ctx);
    let id = new_story(&ctx, "manually blocked");
    service.assign_phase(&id, "1").expect("assigning");
    StoryService::new(&ctx)
        .set_state(&id, "blocked", None, None, None)
        .expect("blocking");

    let phases = service.phases().expect("listing");
    let phase = &phases[0];
    assert_eq!(
        phase.blocked, 1,
        "a state=blocked story must roll up as blocked"
    );
    assert_eq!(phase.in_progress, 0);
    assert_eq!(phase.todo, 0);
}

#[test]
fn a_phases_stories_come_back_in_story_number_order() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = GroupingService::new(&ctx);
    let ids: Vec<String> = (0..12)
        .map(|i| new_story(&ctx, &format!("story {i}")))
        .collect();
    for id in &ids {
        service.assign_phase(id, "1").expect("assigning");
    }

    let listed: Vec<String> = service
        .phase_stories("1")
        .expect("listing")
        .into_iter()
        .map(|view| view.story.id)
        .collect();
    assert_eq!(listed, ids, "SH-10 must not sort between SH-1 and SH-2");
}

/// The sibling SH-64 names in the same family as the story-id ordering split
/// it fixes: `phase list` used to sort by label text, so phase `10` came
/// before phase `2`.
#[test]
fn phases_list_in_numeric_order_not_label_text_order() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let service = GroupingService::new(&ctx);
    let ten = new_story(&ctx, "phase ten story");
    let two = new_story(&ctx, "phase two story");
    service.assign_phase(&ten, "10").expect("assigning");
    service.assign_phase(&two, "2").expect("assigning");

    let phases: Vec<String> = service
        .phases()
        .expect("listing")
        .into_iter()
        .map(|phase| phase.phase)
        .collect();
    assert_eq!(phases, ["2", "10"], "`10` must not sort before `2`");
}

// --- epics -----------------------------------------------------------------

#[test]
fn an_epic_is_a_story_typed_epic() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    fixture
        .store()
        .write(|tx| {
            let mut types = tx.types(fixture.project())?;
            types.push(storyhook::domain::TypeDef {
                slug: "epic".into(),
                description: None,
                emoji: None,
            });
            tx.put_types(fixture.project(), &types)
        })
        .expect("adding the epic type");

    let service = GroupingService::new(&ctx);
    let epic = service.create_epic("the big one").expect("creating");
    assert_eq!(epic.story_type.as_deref(), Some("epic"));

    new_story(&ctx, "not an epic");
    let listed: Vec<String> = service
        .epics()
        .expect("listing")
        .into_iter()
        .map(|view| view.story.id)
        .collect();
    assert_eq!(listed, [epic.id]);
}

#[test]
fn creating_an_epic_without_the_type_says_how_to_add_it() {
    // The fixture's catalog has no `epic` type, which is the situation this
    // message exists for.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let error = GroupingService::new(&ctx)
        .create_epic("doomed")
        .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("type `epic` is not defined"), "{message}");
    assert!(message.contains("story type add epic"), "{message}");
}

#[test]
fn adding_to_an_epic_writes_the_edge_from_both_ends() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let parent = new_story(&ctx, "the epic");
    let child = new_story(&ctx, "the child");

    GroupingService::new(&ctx)
        .add_to_epic(&parent, &child)
        .expect("adding");
    assert_eq!(
        relations(&snapshot(&fixture, &parent)),
        [("parent-of".to_string(), child.clone())]
    );
    assert_eq!(
        relations(&snapshot(&fixture, &child)),
        [("child-of".to_string(), parent.clone())]
    );
    fixture.assert_no_drift();
}

#[test]
fn adding_the_same_story_to_an_epic_twice_changes_nothing() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let parent = new_story(&ctx, "the epic");
    let child = new_story(&ctx, "the child");
    let service = GroupingService::new(&ctx);
    service.add_to_epic(&parent, &child).expect("adding");
    let after_first = snapshot(&fixture, &parent);
    service.add_to_epic(&parent, &child).expect("adding again");
    assert_eq!(snapshot(&fixture, &parent), after_first);
}

#[test]
fn a_story_cannot_be_given_a_second_epic() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let first = new_story(&ctx, "first epic");
    let second = new_story(&ctx, "second epic");
    let child = new_story(&ctx, "the child");
    let service = GroupingService::new(&ctx);

    service.add_to_epic(&first, &child).expect("adding");
    let error = service.add_to_epic(&second, &child).unwrap_err();
    assert!(error.to_string().contains("already has a different parent"));
    assert_eq!(
        relations(&snapshot(&fixture, &child)),
        [("child-of".to_string(), first)]
    );
    assert!(relations(&snapshot(&fixture, &second)).is_empty());
}

#[test]
fn an_epic_cannot_contain_itself() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = new_story(&ctx, "the epic");
    let error = GroupingService::new(&ctx)
        .add_to_epic(&id, &id)
        .unwrap_err();
    assert!(error.to_string().contains("cannot relate to themselves"));
}

// --- hooks -----------------------------------------------------------------

#[test]
fn the_grouping_commands_that_never_fired_hooks_still_do_not() {
    // `story phase create`, `story epic create` and `story epic add` fire no
    // hooks, where `story new` and `story relate` do. Delegating to those
    // services would have started firing them; the hook-suppressed context is
    // what stops it, and this is what proves the suppression is real.
    let fixture = ServiceFixture::new();
    let marker = fixture.cwd().join("fired.txt");
    fixture.write_hooks_toml(&format!(
        "[on_create]\ncommand = \"touch {}\"\n[on_relationship_change]\ncommand = \"touch {}\"\n",
        marker.display(),
        marker.display()
    ));

    let ctx = fixture.ctx();
    let service = GroupingService::new(&ctx);
    service.create_phase("1", None).expect("creating a phase");
    let parent = service
        .create_phase("2", None)
        .expect("creating another phase");
    let child = service.create_phase("3", None).expect("and another");
    service
        .add_to_epic(&parent.id, &child.id)
        .expect("adding to an epic");

    assert!(
        !marker.exists(),
        "a grouping command fired a hook the legacy path does not"
    );

    // And the control: the command that *does* fire one still does.
    StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "an ordinary story".into(),
            ..NewStoryInput::default()
        })
        .expect("creating");
    assert!(marker.exists(), "the create hook stopped firing");
}

#[test]
fn assigning_a_phase_fires_the_label_change_hook() {
    let fixture = ServiceFixture::new();
    let marker = fixture.cwd().join("labelled.txt");
    fixture.write_hooks_toml(&format!(
        "[on_label_change]\ncommand = \"touch {}\"\n",
        marker.display()
    ));

    let ctx = fixture.ctx();
    let id = new_story(&ctx, "needs a phase");
    GroupingService::new(&ctx)
        .assign_phase(&id, "1")
        .expect("assigning");
    assert!(marker.exists(), "phase add must fire the label-change hook");

    std::fs::remove_file(&marker).expect("removing the marker");
    GroupingService::new(&ctx)
        .clear_phase(&id)
        .expect("clearing");
    assert!(
        !marker.exists(),
        "phase remove fires no hook in the legacy path"
    );
}
