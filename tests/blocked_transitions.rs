//! SH-656: live admission, rather than historical replay, owns blocker rules.

use storyhook::domain::StoryEvent;
use storyhook::service::{NewStoryInput, RelationService, StoryService};
use storyhook::store::{ReadOps, Store, StoryNo};
use storyhook_test_support::ServiceFixture;

fn create(fixture: &ServiceFixture, state: &str) -> String {
    StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: format!("story in {state}"),
            state: Some(state.into()),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id
}

fn blocked(fixture: &ServiceFixture, state: &str) -> (String, String) {
    let story = create(fixture, state);
    let blocker = create(fixture, "todo");
    RelationService::new(&fixture.ctx())
        .relate(&story, "blocked-by", &blocker, false)
        .unwrap();
    (story, blocker)
}

#[test]
fn forward_move_refuses_with_context_and_without_appending() {
    let fixture = ServiceFixture::new();
    let (story, blocker) = blocked(&fixture, "todo");
    let no = StoryNo::parse_id("SH", &story).unwrap();
    let before = fixture
        .store()
        .read(|tx| tx.events_for(fixture.project(), no))
        .unwrap();
    let error = StoryService::new(&fixture.ctx())
        .set_state(&story, "in-progress", None, None, None)
        .expect_err("an open blocker must prevent advancement")
        .to_string();
    for expected in [&story, &blocker, "todo", "in-progress", "dropped"] {
        assert!(error.contains(expected), "missing {expected}: {error}");
    }
    let after = fixture
        .store()
        .read(|tx| tx.events_for(fixture.project(), no))
        .unwrap();
    assert_eq!(before.len(), after.len());
}

#[test]
fn abandonment_is_allowed_but_completion_is_not() {
    for target in ["dropped", "done"] {
        let fixture = ServiceFixture::new();
        let (story, _) = blocked(&fixture, "verifying");
        let result = StoryService::new(&fixture.ctx()).set_state(&story, target, None, None, None);
        assert_eq!(
            result.is_ok(),
            target == "dropped",
            "target {target}: {result:?}"
        );
    }
}

#[test]
fn backward_same_state_and_entering_blocked_are_allowed() {
    for target in ["todo", "in-progress", "blocked"] {
        let fixture = ServiceFixture::new();
        let (story, _) = blocked(&fixture, "in-progress");
        StoryService::new(&fixture.ctx())
            .set_state(&story, target, None, None, None)
            .unwrap();
    }
}

#[test]
fn blocked_detour_cannot_manufacture_progress() {
    let fixture = ServiceFixture::new();
    let (story, _) = blocked(&fixture, "todo");
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    service
        .set_state(&story, "blocked", None, None, None)
        .unwrap();
    assert!(
        service
            .set_state(&story, "verifying", None, None, None)
            .is_err()
    );
    service.set_state(&story, "todo", None, None, None).unwrap();
}

#[test]
fn closing_the_blocker_restores_forward_moves() {
    let fixture = ServiceFixture::new();
    let (story, blocker) = blocked(&fixture, "todo");
    let ctx = fixture.ctx();
    let service = StoryService::new(&ctx);
    service
        .set_state(&blocker, "done", None, None, None)
        .unwrap();
    service
        .set_state(&story, "verifying", None, None, None)
        .unwrap();
}

#[test]
fn migrated_abandonment_history_preserves_the_position_before_blocked() {
    use std::collections::BTreeMap;
    use storyhook::domain::transition::validate_append;
    let fixture = ServiceFixture::new();
    let (id, _) = blocked(&fixture, "todo");
    let (mut history, states, index) = fixture
        .store()
        .read(|tx| {
            Ok((
                tx.events_for(fixture.project(), StoryNo::parse_id("SH", &id)?)?
                    .into_iter()
                    .filter_map(|e| e.known().cloned())
                    .collect::<Vec<_>>(),
                tx.states(fixture.project())?,
                tx.stories(fixture.project(), &storyhook::store::StoryQuery::all())?
                    .into_iter()
                    .map(|r| (r.snapshot.id.clone(), r.snapshot))
                    .collect::<BTreeMap<_, _>>(),
            ))
        })
        .unwrap();
    history.extend(
        ["closed", "blocked"].map(|state| StoryEvent::StoryStateChanged {
            at: storyhook_test_support::FIXTURE_NOW.into(),
            state: state.into(),
        }),
    );
    for (target, allowed) in [
        ("todo", true),
        ("verifying", true),
        ("dropped", true),
        ("done", false),
    ] {
        let proposed = [StoryEvent::StoryStateChanged {
            at: storyhook_test_support::FIXTURE_NOW.into(),
            state: target.into(),
        }];
        assert_eq!(
            validate_append(&id, &history, &proposed, &states, &index).is_ok(),
            allowed,
            "{target}"
        );
    }
}

#[test]
fn undo_is_new_input_and_cannot_restore_blocked_progress() {
    let fixture = ServiceFixture::new();
    let story = create(&fixture, "verifying");
    let no = StoryNo::parse_id("SH", &story).unwrap();
    let historical_text = "Don't utilize this historic API.";
    storyhook::store::test_support::inject_events(
        fixture.store(),
        fixture.project(),
        no,
        &[StoryEvent::StoryCommentAdded {
            at: storyhook_test_support::FIXTURE_NOW.into(),
            text: historical_text.into(),
        }],
    )
    .unwrap();
    let history: Vec<StoryEvent> = fixture
        .store()
        .read(|tx| {
            Ok(tx
                .events_for(fixture.project(), no)?
                .into_iter()
                .filter_map(|e| e.known().cloned())
                .collect())
        })
        .unwrap();
    StoryService::new(&fixture.ctx())
        .set_state(&story, "todo", None, None, None)
        .unwrap();
    let blocker = create(&fixture, "todo");
    RelationService::new(&fixture.ctx())
        .relate(&story, "blocked-by", &blocker, false)
        .unwrap();
    storyhook::store::test_support::inject_events(
        fixture.store(),
        fixture.project(),
        no,
        &[StoryEvent::StoryCommentRetracted {
            at: storyhook_test_support::FIXTURE_NOW.into(),
            comment_at: storyhook_test_support::FIXTURE_NOW.into(),
            text: historical_text.into(),
        }],
    )
    .unwrap();
    let before = fixture
        .store()
        .read(|tx| tx.events_for(fixture.project(), no))
        .unwrap();
    let error = storyhook::service::history::restore(&fixture.ctx(), &story, &history)
        .expect_err("historical text exemption must not bypass blocked transitions")
        .to_string();
    assert!(error.contains(&blocker), "{error}");
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.events_for(fixture.project(), no))
            .unwrap(),
        before,
        "a refused undo must not restore even part of the old text"
    );
    StoryService::new(&fixture.ctx())
        .set_state(&blocker, "done", None, None, None)
        .unwrap();
    storyhook::service::history::restore(&fixture.ctx(), &story, &history).unwrap();
    let restored = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), no))
        .unwrap()
        .unwrap();
    assert_eq!(restored.snapshot.state, "verifying");
    assert_eq!(restored.snapshot.comments[0].text, historical_text);
}

#[test]
fn domain_admission_uses_catalog_order_and_exact_abandonment() {
    use std::collections::BTreeMap;
    use storyhook::domain::transition::validate_transition;
    let fixture = ServiceFixture::new();
    let (id, _) = blocked(&fixture, "todo");
    let no = StoryNo::parse_id("SH", &id).unwrap();
    let (mut story, mut states, index) = fixture
        .store()
        .read(|tx| {
            Ok((
                tx.story(fixture.project(), no)?.unwrap().snapshot,
                tx.states(fixture.project())?,
                tx.stories(fixture.project(), &storyhook::store::StoryQuery::all())?
                    .into_iter()
                    .map(|r| (r.snapshot.id.clone(), r.snapshot))
                    .collect::<BTreeMap<_, _>>(),
            ))
        })
        .unwrap();
    // Every rotation and reversal exercises configured order independently of
    // the default layout, including done before the current OPEN state.
    for _ in 0..states.len() {
        for _ in 0..2 {
            for source in &states {
                if source.slug == "blocked" {
                    continue;
                }
                story.state = source.slug.clone();
                story.superstate = source.super_state.clone();
                for target in &states {
                    let forward = states.iter().position(|s| s.slug == target.slug)
                        > states.iter().position(|s| s.slug == source.slug);
                    let allowed = target.slug == source.slug
                        || target.slug == "dropped"
                        || target.slug == "blocked"
                        || (target.slug != "done" && !forward);
                    assert_eq!(
                        validate_transition(&story, &target.slug, None, &states, &index).is_ok(),
                        allowed,
                        "{} -> {} in {:?}",
                        source.slug,
                        target.slug,
                        states
                    );
                }
            }
            states.reverse();
        }
        states.rotate_left(1);
    }
}

#[test]
fn domain_fold_accepts_history_but_admission_rejects_intermediate_advancement() {
    use std::collections::BTreeMap;
    use storyhook::domain::{fold_story, transition::validate_append};
    let fixture = ServiceFixture::new();
    let (id, _) = blocked(&fixture, "todo");
    let no = StoryNo::parse_id("SH", &id).unwrap();
    let (history, states, index) = fixture
        .store()
        .read(|tx| {
            Ok((
                tx.events_for(fixture.project(), no)?
                    .into_iter()
                    .filter_map(|e| e.known().cloned())
                    .collect::<Vec<_>>(),
                tx.states(fixture.project())?,
                tx.stories(fixture.project(), &storyhook::store::StoryQuery::all())?
                    .into_iter()
                    .map(|r| (r.snapshot.id.clone(), r.snapshot))
                    .collect::<BTreeMap<_, _>>(),
            ))
        })
        .unwrap();
    let proposed = ["done", "todo"].map(|state| StoryEvent::StoryStateChanged {
        at: storyhook_test_support::FIXTURE_NOW.into(),
        state: state.into(),
    });
    assert!(validate_append(&id, &history, &proposed, &states, &index).is_err());
    let map = states.iter().map(|s| (s.slug.clone(), s.clone())).collect();
    let accepted_history = [history.as_slice(), &proposed[..1]].concat();
    assert_eq!(
        fold_story(&id, &accepted_history, &map).unwrap().state,
        "done"
    );
    // The same archive marker can carry a state change without a preceding move.
    let marker = StoryEvent::StoryClosedAndArchived {
        at: storyhook_test_support::FIXTURE_NOW.into(),
        state: "done".into(),
    };
    assert!(validate_append(&id, &history, &[marker], &states, &index).is_err());
}

#[test]
fn missing_and_closed_targets_and_obviation_keep_their_existing_semantics() {
    use std::collections::BTreeMap;
    use storyhook::domain::{
        SuperState, is_ready,
        transition::{open_blockers, validate_transition},
    };
    let fixture = ServiceFixture::new();
    let (id, blocker) = blocked(&fixture, "todo");
    let (mut story, states, mut index) = fixture
        .store()
        .read(|tx| {
            Ok((
                tx.story(fixture.project(), StoryNo::parse_id("SH", &id)?)?
                    .unwrap()
                    .snapshot,
                tx.states(fixture.project())?,
                tx.stories(fixture.project(), &storyhook::store::StoryQuery::all())?
                    .into_iter()
                    .map(|r| (r.snapshot.id.clone(), r.snapshot))
                    .collect::<BTreeMap<_, _>>(),
            ))
        })
        .unwrap();
    assert_eq!(
        open_blockers(&story, &index),
        std::slice::from_ref(&blocker)
    );
    index.get_mut(&blocker).unwrap().superstate = SuperState::Closed;
    assert!(validate_transition(&story, "done", None, &states, &index).is_ok());
    index.remove(&blocker);
    assert!(validate_transition(&story, "done", None, &states, &index).is_ok());
    story.relationships[0].relation = "obviated-by".into();
    assert!(!is_ready(&story, &index));
    assert!(validate_transition(&story, "done", None, &states, &index).is_ok());
}

#[test]
fn command_line_move_refuses_a_blocker_and_preserves_the_state() {
    let dir = storyhook_test_support::scratch_dir();
    let env = storyhook_test_support::TestEnv::shared();
    for args in [
        vec!["project", "new", "--prefix", "SH"],
        vec!["new", "dependent"],
        vec!["new", "dependency"],
        vec!["relate", "SH-1", "blocked-by", "SH-2"],
    ] {
        env.story(dir.path()).args(args).assert().success();
    }
    let output = env
        .story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .failure();
    let error = String::from_utf8_lossy(&output.get_output().stderr);
    assert!(
        error.contains("SH-2") && error.contains("dropped"),
        "{error}"
    );
    env.story(dir.path())
        .args(["show", "SH-1", "--json"])
        .assert()
        .success()
        .stdout(predicates::str::contains("\"state\": \"todo\""));
}
