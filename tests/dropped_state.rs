//! SH-663: abandonment is `dropped`; CLOSED remains a superstate.

use std::collections::BTreeMap;

use storyhook::domain::{StateDef, StoryEvent, SuperState, fold_story, with_required_states};
use storyhook_test_support::TestEnv;

fn state(slug: &str, super_state: SuperState) -> StateDef {
    StateDef {
        slug: slug.into(),
        super_state,
        role: None,
        description: None,
    }
}

fn catalog() -> Vec<StateDef> {
    with_required_states(&[]).unwrap()
}

fn history(slug: &str) -> Vec<StoryEvent> {
    vec![
        StoryEvent::StoryCreated {
            at: "2026-01-01T00:00:00Z".into(),
            title: "Abandoned".into(),
            state: "todo".into(),
        },
        StoryEvent::StoryStateChanged {
            at: "2026-01-02T00:00:00Z".into(),
            state: slug.into(),
        },
        StoryEvent::StoryClosedAndArchived {
            at: "2026-01-02T00:00:00Z".into(),
            state: slug.into(),
        },
        StoryEvent::StoryHidden {
            at: "2026-01-03T00:00:00Z".into(),
        },
    ]
}

#[test]
fn required_catalog_names_dropped_and_preserves_superstates() {
    let states = catalog();
    let closed: Vec<_> = states
        .iter()
        .filter(|s| s.super_state == SuperState::Closed)
        .map(|s| s.slug.as_str())
        .collect();
    assert_eq!(closed, ["done", "dropped"]);
}

#[test]
fn old_catalog_is_renamed_in_place_with_metadata_and_is_idempotent() {
    let mut old = vec![
        state("todo", SuperState::Open),
        state("closed", SuperState::Closed),
        state("done", SuperState::Closed),
    ];
    old[1].description = Some("Not proceeding".into());
    let repaired = with_required_states(&old).unwrap();
    assert!(!repaired.iter().any(|s| s.slug == "closed"));
    let dropped = repaired.iter().position(|s| s.slug == "dropped").unwrap();
    assert_eq!(
        repaired[dropped].description.as_deref(),
        Some("Not proceeding")
    );
    assert!(dropped < repaired.iter().position(|s| s.slug == "done").unwrap());
    assert_eq!(with_required_states(&repaired).unwrap(), repaired);
}

#[test]
fn conflicting_catalogs_are_rejected_without_reclassification() {
    for target_superstate in [SuperState::Open, SuperState::Closed] {
        let old = vec![
            state("todo", SuperState::Open),
            state("closed", SuperState::Closed),
            state("dropped", target_superstate),
        ];
        let error = with_required_states(&old).unwrap_err().to_string();
        assert!(error.contains("dropped"), "{error}");
    }
}

#[test]
fn legacy_closed_history_replays_as_dropped_without_rewriting_events() {
    let states = catalog().into_iter().map(|s| (s.slug.clone(), s)).collect();
    let events = history("closed");
    let original = serde_json::to_string(&events).unwrap();
    let snapshot = fold_story("SH-1", &events, &states).unwrap();
    assert_eq!(snapshot.state, "dropped");
    assert_eq!(snapshot.superstate, SuperState::Closed);
    assert_eq!(snapshot.closed_at.as_deref(), Some("2026-01-02T00:00:00Z"));
    assert_eq!(snapshot.hidden_at.as_deref(), Some("2026-01-03T00:00:00Z"));
    assert_eq!(serde_json::to_string(&events).unwrap(), original);
    let mut reopened = events;
    reopened.push(StoryEvent::StoryStateChanged {
        at: "2026-01-04T00:00:00Z".into(),
        state: "todo".into(),
    });
    let snapshot = fold_story("SH-1", &reopened, &states).unwrap();
    assert_eq!(snapshot.state, "todo");
    assert_eq!(snapshot.closed_at, None);
    assert_eq!(snapshot.hidden_at, None);
}

#[test]
fn custom_open_closed_keeps_its_meaning() {
    let states = with_required_states(&[state("closed", SuperState::Open)]).unwrap();
    let states: BTreeMap<_, _> = states.into_iter().map(|s| (s.slug.clone(), s)).collect();
    let snapshot = fold_story("SH-1", &history("closed"), &states).unwrap();
    assert_eq!(snapshot.state, "closed");
    assert_eq!(snapshot.superstate, SuperState::Open);
    assert_eq!(snapshot.closed_at, None);
    assert_eq!(snapshot.hidden_at, None);
}

#[test]
fn cli_close_uses_dropped_and_reopen_preserves_reason() {
    let dir = storyhook_test_support::scratch_dir();
    let env = TestEnv::shared();
    env.story(dir.path())
        .args(["project", "new", "--prefix", "DROP"])
        .assert()
        .success();
    env.story(dir.path())
        .args(["new", "Abandoned work"])
        .assert()
        .success();
    env.story(dir.path())
        .args(["close", "DROP-1", "Superseded"])
        .assert()
        .success();
    let output = env
        .story(dir.path())
        .args(["show", "DROP-1", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["story"]["story"]["state"], "dropped");
    assert_eq!(document["story"]["story"]["superstate"], "CLOSED");
    env.story(dir.path())
        .args(["reopen", "DROP-1"])
        .assert()
        .success();
    let output = env
        .story(dir.path())
        .args(["show", "DROP-1", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["story"]["story"]["state"], "todo");
    assert_eq!(
        document["story"]["story"]["comments"][0]["text"],
        "Superseded"
    );
    env.story(dir.path())
        .args(["state", "add", "closed", "--super", "OPEN"])
        .assert()
        .failure();
}

#[test]
fn catalog_edits_cannot_restore_the_ambiguous_closed_status() {
    let mut states = catalog();
    states.push(state("closed", SuperState::Open));
    storyhook::domain::validate_required_states(&states).unwrap();
    states.last_mut().unwrap().super_state = SuperState::Closed;
    let error = storyhook::domain::validate_required_states(&states)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("dropped") && error.contains("closed"),
        "{error}"
    );
}
