//! `story new --blocked-by` records its edges in the creation transaction
//! (SH-779).
//!
//! Filing a story that must wait used to be two writes: `story new`, then
//! `story relate <id> blocked-by <x>`. The first commit wakes the Full Auto
//! engine at once, and the engine's claim — correct by its own rules — took
//! the story while it was still ready. MT-32 was claimed in the same second
//! it was created and dispatched while blocked.
//!
//! The regression below drives the engine's own claim primitive
//! (`claim_next_filtered` with the engine's `no-auto` filter) immediately after
//! the create, with nothing in between: if any committed state existed in which
//! the new story were ready, the claim would take it.

use std::collections::BTreeMap;

use storyhook::domain::{StorySnapshot, SuperState, is_ready};
use storyhook::error::AppError;
use storyhook::service::{
    ConfigService, Ctx, NewStoryInput, QueryService, ReadyQueueFilters, RelationService,
    StoryService,
};
use storyhook::store::{ReadOps, SqliteStore, Store, StoryNo, WriteOps};
use storyhook_test_support::{FIXTURE_NOW, ServiceFixture};

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

fn new_blocked(
    ctx: &Ctx<'_, SqliteStore>,
    title: &str,
    blocked_by: &[&str],
) -> Result<StorySnapshot, AppError> {
    StoryService::new(ctx).create(&NewStoryInput {
        title: title.to_string(),
        blocked_by: blocked_by.iter().map(|id| (*id).to_string()).collect(),
        ..NewStoryInput::default()
    })
}

fn no(id: &str) -> StoryNo {
    StoryNo::parse_id("SH", id).expect("a well-formed id")
}

fn snapshot(fixture: &ServiceFixture, id: &str) -> StorySnapshot {
    fixture
        .store()
        .read(|tx| tx.story(fixture.project(), no(id)))
        .expect("reading the story")
        .expect("the story exists")
        .snapshot
}

fn exists(fixture: &ServiceFixture, id: &str) -> bool {
    fixture
        .store()
        .read(|tx| tx.story(fixture.project(), no(id)))
        .expect("reading the story")
        .is_some()
}

fn relations(fixture: &ServiceFixture, id: &str) -> Vec<(String, String)> {
    let mut edges: Vec<(String, String)> = snapshot(fixture, id)
        .relationships
        .into_iter()
        .map(|relation| (relation.relation, relation.other_id))
        .collect();
    edges.sort();
    edges
}

fn stored_edges(fixture: &ServiceFixture, id: &str) -> Vec<(String, i64)> {
    let mut edges: Vec<(String, i64)> = fixture
        .store()
        .read(|tx| tx.relations_from(fixture.project(), no(id)))
        .expect("reading edges")
        .into_iter()
        .map(|edge| (edge.relation, edge.other_no.get()))
        .collect();
    edges.sort();
    edges
}

/// Each stored event's kind and sequence number, in history order.
fn events(fixture: &ServiceFixture, id: &str) -> Vec<(String, i64)> {
    fixture
        .store()
        .read(|tx| tx.events_for(fixture.project(), no(id)))
        .expect("reading events")
        .into_iter()
        .map(|event| (event.kind, event.seq.get()))
        .collect()
}

/// The ids `story next` plans, in order. The plan places a dependent after
/// its open blockers, so this is not a readiness oracle on its own; with every
/// blocker already in progress (and so out of the plan), a blocked story must
/// not appear at all.
fn next_ids(fixture: &ServiceFixture) -> Vec<String> {
    fixture
        .store()
        .read(|tx| Ok(QueryService::new(tx, fixture.project(), FIXTURE_NOW).next(50, None)))
        .expect("reading the ready queue")
        .expect("computing the ready queue")
        .into_iter()
        .map(|view| view.story.id)
        .collect()
}

/// Whether `id` is ready under the domain's own predicate, judged against the
/// computed index readiness uses.
fn ready(fixture: &ServiceFixture, id: &str) -> bool {
    let stories: BTreeMap<String, StorySnapshot> = fixture
        .store()
        .read(|tx| {
            Ok(tx
                .stories(fixture.project(), &storyhook::store::StoryQuery::all())?
                .into_iter()
                .map(|row| (row.snapshot.id.clone(), row.snapshot))
                .collect())
        })
        .expect("reading stories");
    let mut stories = stories;
    let states = fixture
        .store()
        .read(|tx| tx.states(fixture.project()))
        .expect("reading states");
    storyhook::domain::apply_computed_epic_states(&mut stories, &states);
    is_ready(&stories[id], &stories)
}

/// The engine's own claim: the `no-auto` filter `fill_idle_lanes` passes.
fn engine_claim(fixture: &ServiceFixture) -> Option<String> {
    StoryService::new(&fixture.ctx())
        .claim_next_filtered(
            ReadyQueueFilters {
                phase: None,
                epic: None,
                exclude_label: Some("no-auto"),
            },
            None,
        )
        .expect("claiming")
        .map(|(_, claimed)| claimed.id)
}

fn validation_message(error: AppError) -> String {
    match error {
        AppError::Validation(message) => message,
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// A blocker that is itself in progress, so the engine has nothing else to
/// take and a claim that returns anything returns the new story.
fn claimed_blocker(fixture: &ServiceFixture) -> String {
    let ctx = fixture.ctx();
    let blocker = new_story(&ctx, "the blocker");
    StoryService::new(&ctx)
        .set_state(&blocker, "in-progress", None, None, None)
        .expect("claiming the blocker by hand");
    blocker
}

// --- the MT-32 regression ----------------------------------------------------

#[test]
fn the_engine_cannot_claim_a_story_filed_with_its_blocker() {
    let fixture = ServiceFixture::new();
    let blocker = claimed_blocker(&fixture);

    let filed = new_blocked(&fixture.ctx(), "waits on the blocker", &[&blocker])
        .expect("filing a blocked story");

    assert_eq!(
        engine_claim(&fixture),
        None,
        "the engine's claim ran straight after the create and must find nothing ready"
    );
    assert_eq!(snapshot(&fixture, &filed.id).state, "todo");
    assert!(!ready(&fixture, &filed.id));
    assert!(!next_ids(&fixture).contains(&filed.id));
}

#[test]
fn the_edges_are_part_of_the_creating_append() {
    let fixture = ServiceFixture::new();
    let blocker = claimed_blocker(&fixture);
    let blocker_events_before = events(&fixture, &blocker).len();

    let filed = new_blocked(&fixture.ctx(), "waits", &[&blocker]).expect("filing");

    let history = events(&fixture, &filed.id);
    assert_eq!(
        history.first().map(|(kind, _)| kind.as_str()),
        Some("StoryCreated")
    );
    assert!(
        history
            .iter()
            .any(|(kind, _)| kind == "StoryRelationshipAdded"),
        "the new story's own history records the edge: {history:?}"
    );
    // One append: the snapshot the create returned already carries the edge,
    // and nothing was appended to the new story after it.
    assert_eq!(
        filed
            .relationships
            .iter()
            .map(|r| (r.relation.as_str(), r.other_id.as_str()))
            .collect::<Vec<_>>(),
        vec![("blocked-by", blocker.as_str())]
    );
    assert_eq!(snapshot(&fixture, &filed.id), filed);

    let blocker_history = events(&fixture, &blocker);
    assert_eq!(blocker_history.len(), blocker_events_before + 1);
    assert_eq!(
        blocker_history.last().map(|(kind, _)| kind.as_str()),
        Some("StoryRelationshipAdded")
    );
    assert_eq!(
        relations(&fixture, &blocker),
        vec![("blocks".to_string(), filed.id.clone())]
    );
    assert_eq!(
        stored_edges(&fixture, &filed.id),
        vec![("blocked-by".to_string(), no(&blocker).get())]
    );
    assert_eq!(
        stored_edges(&fixture, &blocker),
        vec![("blocks".to_string(), no(&filed.id).get())]
    );
    fixture.assert_no_drift();
}

#[test]
fn several_blockers_are_all_recorded_and_duplicates_collapse() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let first = new_story(&ctx, "first");
    let second = new_story(&ctx, "second");

    let filed = new_blocked(&ctx, "waits on both", &[&first, &second, &first]).expect("filing");

    assert_eq!(
        relations(&fixture, &filed.id),
        vec![
            ("blocked-by".to_string(), first.clone()),
            ("blocked-by".to_string(), second.clone()),
        ]
    );
    for blocker in [&first, &second] {
        assert_eq!(
            relations(&fixture, blocker),
            vec![("blocks".to_string(), filed.id.clone())]
        );
    }
    let added = events(&fixture, &filed.id)
        .into_iter()
        .filter(|(kind, _)| kind == "StoryRelationshipAdded")
        .count();
    assert_eq!(added, 2, "a repeated blocker must not double its event");
    assert!(!ready(&fixture, &filed.id));
}

#[test]
fn no_blockers_files_an_ordinary_ready_story() {
    let fixture = ServiceFixture::new();
    let filed = new_blocked(&fixture.ctx(), "free", &[]).expect("filing");
    assert!(filed.relationships.is_empty());
    assert!(ready(&fixture, &filed.id));
}

// --- failure writes nothing ---------------------------------------------------

#[test]
fn an_unknown_blocker_files_nothing_and_burns_no_number() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let blocker = new_story(&ctx, "real");

    let error = new_blocked(&ctx, "never filed", &[&blocker, "SH-99"])
        .expect_err("an unknown blocker must refuse the whole create");
    assert!(
        matches!(&error, AppError::NotFound(message) if message.contains("SH-99")),
        "got {error:?}"
    );

    assert!(!exists(&fixture, "SH-2"), "no story was filed");
    assert!(
        relations(&fixture, &blocker).is_empty(),
        "the real blocker gained no edge"
    );
    assert_eq!(
        new_story(&ctx, "the next one"),
        "SH-2",
        "the refused create burnt no story number"
    );
}

#[test]
fn naming_the_number_about_to_be_allocated_is_not_found() {
    let fixture = ServiceFixture::new();
    let error =
        new_blocked(&fixture.ctx(), "self", &["SH-1"]).expect_err("SH-1 does not exist yet");
    assert!(matches!(error, AppError::NotFound(_)), "got {error:?}");
    assert!(!exists(&fixture, "SH-1"));
}

#[test]
fn a_blocker_held_by_an_unfinished_reset_refuses_the_whole_create() {
    // The store refuses every non-comment append to a story with an
    // unfinished native reset, so the blocker's own `blocks` append fails —
    // after the new story's append in the same transaction. Nothing may
    // survive that, and `story block --on` must refuse the same blocker too.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let blocker = new_story(&ctx, "being reset");
    let other = new_story(&ctx, "an existing story");
    fixture
        .store()
        .write(|tx| {
            tx.put_legacy_story_reset(
                fixture.project(),
                no(&blocker),
                Some(
                    r#"{"operation":"reset-token","lease":null,"force":false,"previous_awaiting":null,"detail":"retained"}"#,
                ),
            )
        })
        .expect("reserving a reset");

    let error = new_blocked(&ctx, "waits", &[&blocker])
        .expect_err("the blocker's own append is refused, so nothing may commit");
    assert!(
        validation_message(error).contains("unfinished reset"),
        "the refusal names the reset"
    );
    assert!(
        !exists(&fixture, "SH-3"),
        "the new story's append rolled back"
    );
    assert!(relations(&fixture, &blocker).is_empty());

    let parity = RelationService::new(&ctx)
        .block_on(&other, std::slice::from_ref(&blocker), None)
        .expect_err("story block --on refuses the same blocker");
    assert!(validation_message(parity).contains("unfinished reset"));
}

// --- blocker states -----------------------------------------------------------

#[test]
fn a_closed_blocker_records_the_edge_and_does_not_block() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let blocker = new_story(&ctx, "already finished");
    StoryService::new(&ctx)
        .set_state(&blocker, "dropped", None, None, None)
        .expect("closing the blocker");

    let filed = new_blocked(&ctx, "after it", &[&blocker]).expect("a closed blocker is allowed");

    assert_eq!(
        relations(&fixture, &filed.id),
        vec![("blocked-by".to_string(), blocker.clone())]
    );
    assert_eq!(
        relations(&fixture, &blocker),
        vec![("blocks".to_string(), filed.id.clone())]
    );
    assert!(
        ready(&fixture, &filed.id),
        "an edge onto a closed story blocks nothing"
    );
    fixture.assert_no_drift();
}

#[test]
fn the_same_edge_as_story_block_on() {
    // `story new --blocked-by X` must equal `story new` + `story block --on X`,
    // without the gap: same edges on both ends.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let blocker = new_story(&ctx, "blocker");

    let two_step = new_story(&ctx, "two steps");
    RelationService::new(&ctx)
        .block_on(&two_step, std::slice::from_ref(&blocker), None)
        .expect("blocking the old way");
    let one_step = new_blocked(&ctx, "one step", &[&blocker]).expect("filing");

    assert_eq!(
        snapshot(&fixture, &two_step).relationships,
        snapshot(&fixture, &one_step.id).relationships
    );
    assert_eq!(snapshot(&fixture, &two_step).awaiting, None);
    assert_eq!(snapshot(&fixture, &one_step.id).awaiting, None);
}

#[test]
fn closing_the_blocker_retracts_the_edge_and_frees_the_story() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let blocker = new_story(&ctx, "blocker");
    let filed = new_blocked(&ctx, "waits", &[&blocker]).expect("filing");
    assert!(!ready(&fixture, &filed.id));

    StoryService::new(&ctx)
        .set_state(&blocker, "dropped", None, None, None)
        .expect("closing the blocker");

    assert!(relations(&fixture, &filed.id).is_empty());
    assert!(ready(&fixture, &filed.id));
    assert!(next_ids(&fixture).contains(&filed.id));
}

#[test]
fn a_draft_filed_blocked_stays_unready_after_publishing() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let blocker = new_story(&ctx, "blocker");
    let draft = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "a draft that waits".into(),
            draft: true,
            blocked_by: vec![blocker.clone()],
            ..NewStoryInput::default()
        })
        .expect("filing a blocked draft");
    assert!(draft.draft);
    assert_eq!(
        relations(&fixture, &draft.id),
        vec![("blocked-by".to_string(), blocker.clone())]
    );

    StoryService::new(&ctx)
        .publish(&draft.id)
        .expect("publishing");
    assert!(
        !ready(&fixture, &draft.id),
        "publishing does not lift the edge"
    );
}

#[test]
fn filing_blocked_enqueues_no_block_delivery() {
    let fixture = ServiceFixture::new();
    let blocker = claimed_blocker(&fixture);
    new_blocked(&fixture.ctx(), "waits", &[&blocker]).expect("filing");
    let deliveries = fixture
        .store()
        .read(|tx| tx.block_deliveries(fixture.project()))
        .expect("reading deliveries");
    assert!(
        deliveries.is_empty(),
        "a new story has no session to interrupt, and the blocker is not blocked: {deliveries:?}"
    );
}

// --- the creation-state rule (D4) ---------------------------------------------

#[test]
fn an_advanced_state_with_an_open_blocker_is_refused_and_writes_nothing() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let blocker = new_story(&ctx, "open blocker");

    for state in ["in-progress", "verifying"] {
        let error = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: format!("born in {state}"),
                state: Some(state.to_string()),
                blocked_by: vec![blocker.clone()],
                ..NewStoryInput::default()
            })
            .expect_err("an advanced state while blocked is refused");
        let message = validation_message(error);
        assert!(message.contains(state), "{message}");
        assert!(message.contains(&blocker), "{message}");
        assert!(
            message.contains("todo"),
            "the refusal names the state to use: {message}"
        );
        assert!(
            !message.contains("dropped"),
            "the move remedy does not apply: {message}"
        );
    }
    assert!(!exists(&fixture, "SH-2"));
    assert!(relations(&fixture, &blocker).is_empty());
}

#[test]
fn the_blocked_state_with_a_blocker_is_refused_even_when_the_blocker_is_closed() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let open = new_story(&ctx, "open blocker");
    let closed = new_story(&ctx, "closed blocker");
    StoryService::new(&ctx)
        .set_state(&closed, "dropped", None, None, None)
        .expect("closing");

    for blocker in [&open, &closed] {
        let error = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: "born blocked twice".into(),
                state: Some("blocked".into()),
                blocked_by: vec![blocker.clone()],
                ..NewStoryInput::default()
            })
            .expect_err("the blocked state would outlive the edge");
        let message = validation_message(error);
        assert!(message.contains("blocked"), "{message}");
    }
    assert!(!exists(&fixture, "SH-3"));
}

#[test]
fn the_default_state_and_an_advanced_state_with_a_closed_blocker_are_allowed() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let open = new_story(&ctx, "open blocker");
    let closed = new_story(&ctx, "closed blocker");
    StoryService::new(&ctx)
        .set_state(&closed, "dropped", None, None, None)
        .expect("closing");

    let todo = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "todo".into(),
            state: Some("todo".into()),
            blocked_by: vec![open.clone()],
            ..NewStoryInput::default()
        })
        .expect("the default state is where a blocked filing belongs");
    assert_eq!(todo.state, "todo");

    let advanced = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "already started".into(),
            state: Some("in-progress".into()),
            blocked_by: vec![closed.clone()],
            ..NewStoryInput::default()
        })
        .expect("a closed blocker holds nothing back");
    assert_eq!(advanced.state, "in-progress");
}

#[test]
fn an_epic_blocker_is_judged_by_its_computed_state() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    ConfigService::new(&ctx)
        .add_type("epic", None, None)
        .expect("adding the epic type");
    let epic = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "an epic".into(),
            story_type: Some("epic".into()),
            ..NewStoryInput::default()
        })
        .expect("creating the epic")
        .id;
    let child = new_story(&ctx, "its only child");
    RelationService::new(&ctx)
        .relate(&epic, "parent-of", &child, false)
        .expect("adding the child");

    // While the child is open the epic computes open, so it blocks.
    let error = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "waits on the epic".into(),
            state: Some("in-progress".into()),
            blocked_by: vec![epic.clone()],
            ..NewStoryInput::default()
        })
        .expect_err("an open epic blocks");
    assert!(validation_message(error).contains(&epic));

    StoryService::new(&ctx)
        .set_state(&child, "dropped", None, None, None)
        .expect("closing the only child");
    let filed = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "after the epic".into(),
            state: Some("in-progress".into()),
            blocked_by: vec![epic.clone()],
            ..NewStoryInput::default()
        })
        .expect("a computed-closed epic holds nothing back");
    assert_eq!(filed.state, "in-progress");
}

// --- hooks ------------------------------------------------------------------------

#[test]
fn filing_blocked_fires_create_then_one_relationship_hook_per_blocker() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let first = new_story(&ctx, "first");
    let second = new_story(&ctx, "second");
    let events = ["on_create", "on_relationship_change"];
    let body: String = events
        .iter()
        .map(|event| format!("{event} = {{ command = \"cat >> hooks.log; echo >> hooks.log\" }}\n"))
        .collect();
    fixture.write_hooks_toml(&body);

    let filed = new_blocked(&fixture.ctx(), "waits", &[&first, &second]).expect("filing");

    let log = std::fs::read_to_string(fixture.cwd().join("hooks.log")).unwrap_or_default();
    let fired: Vec<serde_json::Value> = log
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    let kinds: Vec<&str> = fired
        .iter()
        .filter_map(|hook| hook.get("event_type").and_then(|v| v.as_str()))
        .collect();
    assert_eq!(
        kinds,
        vec!["create", "relationship_change", "relationship_change"]
    );
    for (hook, blocker) in fired[1..].iter().zip([&first, &second]) {
        assert_eq!(hook["story_id"], filed.id.as_str());
        assert_eq!(hook["action"], "added");
        assert_eq!(hook["relation"], "blocked-by");
        assert_eq!(hook["other_id"], blocker.as_str());
    }
}

// --- the helpers are not vacuous -----------------------------------------------

#[test]
fn a_plain_story_reads_ready_so_the_negatives_are_not_vacuous() {
    // Guards the helper above against a vacuous `ready`: a plain ready story
    // must read as ready, or every negative assertion in this file is empty.
    let fixture = ServiceFixture::new();
    let id = new_story(&fixture.ctx(), "plain");
    assert_eq!(snapshot(&fixture, &id).superstate, SuperState::Open);
    assert!(ready(&fixture, &id));
    assert_eq!(engine_claim(&fixture), Some(id));
}
