//! Property tests for the store's event-sourcing invariants.
//!
//! Five properties, matching the plan exactly — the list is short on purpose.
//! Property tests are worth their runtime only where the input space is large
//! and the invariant is absolute; CLI parsing, rendering, and routing are
//! neither, and are deliberately excluded.
//!
//! `tests/proptest-regressions/store_properties.txt` is committed. A
//! counterexample proptest finds once becomes a permanent, deterministic
//! regression case — the automated-regression tenet applied to a generator
//! rather than to a bug report. The path is set explicitly rather than left to
//! proptest's default, which looks for a `lib.rs` or `main.rs` beside the test,
//! does not find one in an integration-test crate, and falls back to a
//! differently-named file while warning about it on every failure.

mod store_support;

use proptest::prelude::*;
use store_support::{append_and_fold, default_states, link_atomic, new_store, seed_project};
use storyhook::domain::{Priority, StoryEvent, fold_story};
use storyhook::store::{
    EventSeq, ExpectedSeq, ProjectId, RawEvent, ReadOps, SqliteStore, Store, StoreError, StoryNo,
    StoryQuery, WriteOps, diff_read_model,
};

/// 64 cases per store-backed property: enough to explore the operation space
/// without the suite's wall-clock budget going to database creation. Raise it
/// for a soak with `PROPTEST_CASES=1000 cargo test --test store_properties`.
const STORE_CASES: u32 = 64;

/// The pure properties cost nothing per case, so they get more of them.
const PURE_CASES: u32 = 512;

/// Where a shrunk counterexample is recorded, so it is replayed forever after.
fn persistence() -> Option<Box<dyn proptest::test_runner::FailurePersistence>> {
    Some(Box::new(
        proptest::test_runner::FileFailurePersistence::Direct(
            "tests/proptest-regressions/store_properties.txt",
        ),
    ))
}

// ---------------------------------------------------------------------------
// Generators
// ---------------------------------------------------------------------------

/// The relation vocabulary, generated as one end of an edge.
fn arb_relation() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "relates-to",
        "blocks",
        "blocked-by",
        "parent-of",
        "child-of",
        "duplicate-of",
        "obviates",
        "obviated-by",
    ])
}

fn arb_priority() -> impl Strategy<Value = Priority> {
    prop::sample::select(vec![
        Priority::Critical,
        Priority::High,
        Priority::Medium,
        Priority::Low,
        Priority::None,
    ])
}

/// Text that exercises the encodings a JSON payload has to survive.
fn arb_text() -> impl Strategy<Value = String> {
    prop::string::string_regex("[a-zA-Z0-9 \"'\\\\\n\t✓é]{0,40}").expect("a valid regex")
}

fn arb_timestamp() -> impl Strategy<Value = String> {
    (0u32..24, 0u32..60, 0u32..60).prop_map(|(h, m, s)| format!("2026-01-01T{h:02}:{m:02}:{s:02}Z"))
}

/// Any `StoryEvent`, including ones whose payload a fold would reject.
///
/// Used by the properties that must hold for *every* event, valid or not.
fn arb_event() -> impl Strategy<Value = StoryEvent> {
    let states = prop::sample::select(vec!["todo", "in-progress", "done"]);
    prop_oneof![
        (arb_timestamp(), arb_text(), states.clone()).prop_map(|(at, title, state)| {
            StoryEvent::StoryCreated {
                at,
                title,
                state: state.to_string(),
            }
        }),
        (arb_timestamp(), arb_text())
            .prop_map(|(at, text)| StoryEvent::StoryCommentAdded { at, text }),
        (arb_timestamp(), arb_text())
            .prop_map(|(at, member_id)| StoryEvent::StoryAssigned { at, member_id }),
        (arb_timestamp(), arb_text())
            .prop_map(|(at, awaiting)| StoryEvent::StoryAwaitingSet { at, awaiting }),
        arb_timestamp().prop_map(|at| StoryEvent::StoryAwaitingCleared { at }),
        (arb_timestamp(), states.clone()).prop_map(|(at, state)| StoryEvent::StoryStateChanged {
            at,
            state: state.to_string()
        }),
        (arb_timestamp(), arb_priority())
            .prop_map(|(at, priority)| StoryEvent::StoryPrioritySet { at, priority }),
        (arb_timestamp(), arb_text())
            .prop_map(|(at, story_type)| StoryEvent::StoryTypeSet { at, story_type }),
        (arb_timestamp(), prop::collection::vec(arb_text(), 0..4))
            .prop_map(|(at, labels)| StoryEvent::StoryLabelsSet { at, labels }),
        (arb_timestamp(), arb_text())
            .prop_map(|(at, title)| StoryEvent::StoryTitleSet { at, title }),
        (arb_timestamp(), arb_text())
            .prop_map(|(at, description)| StoryEvent::StoryDescriptionSet { at, description }),
        (arb_timestamp(), arb_text(), arb_relation()).prop_map(|(at, other_id, relation)| {
            StoryEvent::StoryRelationshipAdded {
                at,
                other_id,
                relation: relation.to_string(),
            }
        }),
        (arb_timestamp(), arb_text(), arb_relation()).prop_map(|(at, other_id, relation)| {
            StoryEvent::StoryRelationshipRemoved {
                at,
                other_id,
                relation: relation.to_string(),
            }
        }),
        (arb_timestamp(), states).prop_map(|(at, state)| StoryEvent::StoryClosedAndArchived {
            at,
            state: state.to_string()
        }),
        (arb_timestamp(), arb_text())
            .prop_map(|(at, reason)| StoryEvent::StoryDeleted { at, reason }),
    ]
}

/// One thing a caller might do to a story, before it is bound to a real one.
///
/// Model-driven generation: indices are unbounded here and clamped against the
/// stories that actually exist when the script is applied, so a generated case
/// is always *runnable* rather than mostly rejected.
#[derive(Clone, Debug)]
enum Op {
    Create,
    SetTitle(String),
    SetPriority(Priority),
    SetLabels(Vec<String>),
    SetType(String),
    SetDescription(String),
    Assign(String),
    Await(String),
    ClearAwait,
    Comment(String),
    Move(&'static str),
    Link(usize, usize, &'static str),
    Close,
    Delete,
}

/// An operation paired with the index of the story it targets.
///
/// The index is generated rather than derived: an earlier draft picked the
/// target with an expression that was constant across a script, so every
/// operation landed on one story and the rest were never touched. A generator
/// that quietly explores less than it appears to is worse than no generator.
fn arb_targeted_op() -> impl Strategy<Value = (usize, Op)> {
    (0usize..8, arb_op())
}

fn arb_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        6 => Just(Op::Create),
        2 => arb_text().prop_map(Op::SetTitle),
        2 => arb_priority().prop_map(Op::SetPriority),
        2 => prop::collection::vec(arb_text(), 0..3).prop_map(Op::SetLabels),
        1 => arb_text().prop_map(Op::SetType),
        1 => arb_text().prop_map(Op::SetDescription),
        1 => arb_text().prop_map(Op::Assign),
        1 => arb_text().prop_map(Op::Await),
        1 => Just(Op::ClearAwait),
        1 => arb_text().prop_map(Op::Comment),
        2 => prop::sample::select(vec!["todo", "in-progress", "done"]).prop_map(Op::Move),
        4 => (0usize..8, 0usize..8, arb_relation())
                .prop_map(|(a, b, r)| Op::Link(a, b, r)),
        1 => Just(Op::Close),
        1 => Just(Op::Delete),
    ]
}

/// Runs a script against a real store, returning the stories it created.
///
/// Operations that the schema refuses — a second parent, a self-relation — are
/// *skipped*, not treated as failures: the point of the property is that
/// whatever does land leaves the read model consistent, and a rejected write is
/// the store working.
fn run_script(store: &SqliteStore, project: ProjectId, ops: &[(usize, Op)]) -> Vec<StoryNo> {
    let mut stories: Vec<StoryNo> = Vec::new();
    let mut clock = 0u32;
    let mut at = || {
        clock += 1;
        format!(
            "2026-01-01T{:02}:{:02}:{:02}Z",
            clock / 3600,
            (clock / 60) % 60,
            clock % 60
        )
    };

    for (target, op) in ops {
        if let Op::Create = op {
            let story = store
                .write(|tx| tx.allocate_story_no(project))
                .expect("allocating");
            append_and_fold(
                store,
                project,
                story,
                ExpectedSeq::Exact(EventSeq::ZERO),
                &[StoryEvent::StoryCreated {
                    at: at(),
                    title: format!("Story {story}"),
                    state: "todo".into(),
                }],
            )
            .expect("creating a story");
            stories.push(story);
            continue;
        }
        if stories.is_empty() {
            continue;
        }

        if let Op::Link(from, to, relation) = op {
            let from = stories[from % stories.len()];
            let to = stories[to % stories.len()];
            if from != to {
                // Refusals are expected and are the schema doing its job.
                let _ = link_atomic(store, project, from, relation, to);
            }
            continue;
        }

        let story = stories[target % stories.len()];
        let event = match op {
            Op::Create | Op::Link(..) => unreachable!("handled above"),
            Op::SetTitle(title) => StoryEvent::StoryTitleSet {
                at: at(),
                title: title.clone(),
            },
            Op::SetPriority(priority) => StoryEvent::StoryPrioritySet {
                at: at(),
                priority: priority.clone(),
            },
            Op::SetLabels(labels) => StoryEvent::StoryLabelsSet {
                at: at(),
                labels: labels.clone(),
            },
            Op::SetType(story_type) => StoryEvent::StoryTypeSet {
                at: at(),
                story_type: story_type.clone(),
            },
            Op::SetDescription(description) => StoryEvent::StoryDescriptionSet {
                at: at(),
                description: description.clone(),
            },
            Op::Assign(member_id) => StoryEvent::StoryAssigned {
                at: at(),
                member_id: member_id.clone(),
            },
            Op::Await(awaiting) => StoryEvent::StoryAwaitingSet {
                at: at(),
                awaiting: awaiting.clone(),
            },
            Op::ClearAwait => StoryEvent::StoryAwaitingCleared { at: at() },
            Op::Comment(text) => StoryEvent::StoryCommentAdded {
                at: at(),
                text: text.clone(),
            },
            Op::Move(state) => StoryEvent::StoryStateChanged {
                at: at(),
                state: (*state).to_string(),
            },
            Op::Close => StoryEvent::StoryClosedAndArchived {
                at: at(),
                state: "done".into(),
            },
            Op::Delete => StoryEvent::StoryDeleted {
                at: at(),
                reason: "generated".into(),
            },
        };
        // A move into a CLOSED state is two events, never one. The service's
        // `state_transition_events` always pairs the transition with its close
        // marker, and since SH-130 the schema says so too: `(superstate =
        // 'CLOSED') = archived` refuses a story sitting in a closed state with
        // no close timestamp. A generator that emitted the bare move would be
        // producing histories the product cannot produce, and would fail the
        // rebuild property for a reason that says nothing about the store.
        let mut events = vec![event];
        if let Op::Move(state) = op
            && *state == "done"
        {
            events.push(StoryEvent::StoryClosedAndArchived {
                at: at(),
                state: (*state).to_string(),
            });
        }
        append_and_fold(store, project, story, ExpectedSeq::Any, &events).expect("applying an op");
    }
    stories
}

// ---------------------------------------------------------------------------
// The properties
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: PURE_CASES,
        failure_persistence: persistence(),
        ..ProptestConfig::default()
    })]

    /// The highest-value property in the set. A fold that panics inside a
    /// daemon is an outage, not an exit code — so it must not panic on *any*
    /// sequence, including ones that are nonsense: no `StoryCreated` at all, a
    /// state that does not exist, a deletion before a creation.
    #[test]
    fn folding_any_event_sequence_never_panics(events in prop::collection::vec(arb_event(), 0..12)) {
        let states = default_states()
            .into_iter()
            .map(|state| (state.slug.clone(), state))
            .collect();
        // Result, not unwrap: an *error* is a perfectly good outcome here. The
        // property is only that control returns.
        let _ = fold_story("SH-1", &events, &states);
    }

    /// Folding is a pure function of its inputs. If it were not, a read model
    /// and a rebuild of it could disagree while both were "correct", and the
    /// oracle would be measuring noise.
    #[test]
    fn folding_is_deterministic(events in prop::collection::vec(arb_event(), 0..12)) {
        let states: std::collections::BTreeMap<_, _> = default_states()
            .into_iter()
            .map(|state| (state.slug.clone(), state))
            .collect();
        let first = fold_story("SH-1", &events, &states);
        let second = fold_story("SH-1", &events, &states);
        match (first, second) {
            (Ok(a), Ok(b)) => prop_assert_eq!(a, b),
            (Err(a), Err(b)) => prop_assert_eq!(a.to_string(), b.to_string()),
            (a, b) => prop_assert!(false, "one fold succeeded and the other did not: {:?} {:?}",
                                   a.is_ok(), b.is_ok()),
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: STORE_CASES,
        failure_persistence: persistence(),
        ..ProptestConfig::default()
    })]

    /// Every event survives the store byte-perfectly, whatever is in it.
    #[test]
    fn events_round_trip_through_the_store(events in prop::collection::vec(arb_event(), 1..8)) {
        let (_dir, store) = new_store();
        let project = seed_project(&store, "alpha", "SH");
        let story = store.write(|tx| tx.allocate_story_no(project)).unwrap();

        store
            .write(|tx| tx.append_events(project, story, ExpectedSeq::Exact(EventSeq::ZERO), &events))
            .unwrap();

        let stored = store.read(|tx| tx.events_for(project, story)).unwrap();
        prop_assert_eq!(stored.len(), events.len());
        for (index, (written, read)) in events.iter().zip(&stored).enumerate() {
            prop_assert_eq!(read.known(), Some(written), "event {} changed", index);
            prop_assert_eq!(read.seq, EventSeq::new(index as i64 + 1));
        }
    }

    /// SH-54's contract under generation: a kind this binary has never heard of
    /// is stored, read back verbatim, and reported — never decoded, never lost,
    /// and never a reason for the read to fail.
    #[test]
    fn unknown_event_kinds_are_retained_verbatim(
        kind in "Story[A-Z][a-z]{3,10}",
        detail in arb_text(),
        at in arb_timestamp(),
    ) {
        let (_dir, store) = new_store();
        let project = seed_project(&store, "alpha", "SH");
        let story = store.write(|tx| tx.allocate_story_no(project)).unwrap();
        let payload = serde_json::json!({"kind": kind, "at": at, "detail": detail}).to_string();

        store
            .write(|tx| {
                tx.append_raw_events(
                    project,
                    story,
                    ExpectedSeq::Any,
                    &[RawEvent { kind: kind.clone(), at: at.clone(), payload: payload.clone() }],
                )
            })
            .unwrap();

        let stored = store.read(|tx| tx.events_for(project, story)).unwrap();
        prop_assert_eq!(stored.len(), 1);
        prop_assert_eq!(&stored[0].kind, &kind);
        prop_assert_eq!(&stored[0].at, &at);
        prop_assert!(stored[0].known().is_none(), "an invented kind must not decode");
        match &stored[0].payload {
            storyhook::store::StoredPayload::Unknown { json, .. } => {
                prop_assert_eq!(json, &payload);
            }
            other => prop_assert!(false, "expected an unknown payload, got {:?}", other),
        }
    }

    /// **The event-sourcing correctness property.** Whatever sequence of
    /// operations a caller performs, the persisted read model equals a fresh
    /// rebuild from the events. This is the invariant the whole design rests
    /// on, and the one that split-brain (SH-20) violated.
    #[test]
    fn the_read_model_always_equals_a_rebuild(ops in prop::collection::vec(arb_targeted_op(), 1..24)) {
        let (_dir, store) = new_store();
        let project = seed_project(&store, "alpha", "SH");
        run_script(&store, project, &ops);

        let diff = diff_read_model(&store, project).unwrap();
        prop_assert!(diff.is_clean(), "{}", diff.describe());
    }

    /// The relations table is symmetric under any sequence: every edge has its
    /// mirror, and no story has two parents. Half a relation is the state
    /// SH-60 catalogued fifteen live instances of.
    #[test]
    fn relations_stay_symmetric_under_any_sequence(ops in prop::collection::vec(arb_targeted_op(), 1..24)) {
        let (_dir, store) = new_store();
        let project = seed_project(&store, "alpha", "SH");
        let stories = run_script(&store, project, &ops);

        for story in &stories {
            for edge in store.read(|tx| tx.relations_from(project, *story)).unwrap() {
                let inverse = storyhook::domain::inverse_relation(&edge.relation)
                    .expect("only known relations can be stored");
                let mirror = store
                    .read(|tx| tx.relations_from(project, edge.other_no))
                    .unwrap();
                prop_assert!(
                    mirror
                        .iter()
                        .any(|back| back.relation == inverse && back.other_no == *story),
                    "story {} has `{}` to {} with no `{}` coming back",
                    story, edge.relation, edge.other_no, inverse
                );
            }
            let parents = store
                .read(|tx| tx.relations_from(project, *story))
                .unwrap()
                .into_iter()
                .filter(|edge| edge.relation == "child-of")
                .count();
            prop_assert!(parents <= 1, "story {} has {} parents", story, parents);
        }
    }
}

/// Guards the generator itself.
///
/// A `Link` op that could never be applied — because the script never created
/// two stories, or because every relation was refused — would leave the
/// symmetry property vacuously true. This is the check that the generator
/// really does produce edges.
#[test]
fn the_generator_produces_relations() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    let ops = vec![
        (0, Op::Create),
        (0, Op::Create),
        (0, Op::Create),
        (0, Op::Link(0, 1, "blocks")),
        (0, Op::Link(1, 2, "parent-of")),
        (0, Op::Link(0, 2, "relates-to")),
    ];
    let stories = run_script(&store, project, &ops);

    let edges: usize = stories
        .iter()
        .map(|story| {
            store
                .read(|tx| tx.relations_from(project, *story))
                .unwrap()
                .len()
        })
        .sum();
    assert_eq!(edges, 6, "three edges, both directions of each");
    assert!(diff_read_model(&store, project).unwrap().is_clean());
}

/// The script runner must not swallow a genuine store failure.
///
/// It skips *refused* relations on purpose — a rejected second parent is the
/// schema working — so this pins that a refusal is what is being skipped, and
/// that the refusal really happens.
#[test]
fn a_second_parent_is_refused_and_leaves_no_trace() {
    let (_dir, store) = new_store();
    let project = seed_project(&store, "alpha", "SH");
    let ops = vec![(0, Op::Create), (0, Op::Create), (0, Op::Create)];
    let stories = run_script(&store, project, &ops);

    link_atomic(&store, project, stories[0], "parent-of", stories[2]).unwrap();
    let error = link_atomic(&store, project, stories[1], "parent-of", stories[2])
        .expect_err("a second parent must be refused");
    assert!(matches!(error, StoreError::Invariant(_)), "{error}");

    // Both halves rolled back together: the refused writer's own history must
    // not have gained a relationship event either.
    let events = store.read(|tx| tx.events_for(project, stories[1])).unwrap();
    assert!(
        events
            .iter()
            .all(|e| !matches!(e.known(), Some(StoryEvent::StoryRelationshipAdded { .. }))),
        "the rejected link left an event behind"
    );
    assert!(diff_read_model(&store, project).unwrap().is_clean());
    assert_eq!(
        store
            .read(|tx| tx.stories(project, &StoryQuery::all()))
            .unwrap()
            .len(),
        3
    );
}
