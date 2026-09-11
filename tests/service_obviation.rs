//! SH-673: review evidence follows completion history, never incidental edits.

use serde_json::Value;
use storyhook::domain::StoryEvent;
use storyhook::error::AppError;
use storyhook::service::{Clock, NewStoryInput, PrLinkService, QueryService, StoryService};
use storyhook::store::test_support::inject_events;
use storyhook::store::{ReadOps, Store, StoryNo};
use storyhook_test_support::{FIXTURE_NOW, ServiceFixture};

fn create(f: &ServiceFixture, title: &str) -> String {
    StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: title.into(),
            description: Some(format!("Requirements for {title}")),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id
}

fn at(f: &mut ServiceFixture, instant: &str) {
    f.set_clock(Clock::Fixed(instant.into()));
}

fn move_to(f: &ServiceFixture, id: &str, state: &str) {
    StoryService::new(&f.ctx())
        .set_state(id, state, None, None, None)
        .unwrap();
}

fn document(f: &ServiceFixture, id: &str, json: bool) -> Result<String, AppError> {
    f.store()
        .read(|tx| Ok(QueryService::new(tx, f.project(), FIXTURE_NOW).context_for_story(id, json)))
        .unwrap()
}

fn review(f: &ServiceFixture, id: &str) -> Value {
    serde_json::from_str::<Value>(&document(f, id, true).unwrap()).unwrap()["obviation_review"]
        .clone()
}

#[test]
fn all_active_and_verifying_candidates_are_returned_with_full_evidence_and_no_writes() {
    let f = ServiceFixture::new();
    let target = create(&f, "target");
    move_to(&f, &target, "in-progress");
    for n in 2..=13 {
        let id = create(&f, &format!("candidate {n}"));
        move_to(
            &f,
            &id,
            if n % 2 == 0 {
                "in-progress"
            } else {
                "verifying"
            },
        );
        StoryService::new(&f.ctx())
            .comment(&id, "implementation evidence")
            .unwrap();
    }
    create(&f, "not started");
    PrLinkService::new(&f.ctx())
        .link("SH-2", "https://github.com/acme/widgets/pull/1", true)
        .unwrap();
    let before = f.store().read(|tx| tx.max_global_seq(f.project())).unwrap();
    let r = review(&f, &target);
    let candidates = r["candidates"].as_array().unwrap();
    assert_eq!(r["target"]["story"]["id"], target);
    assert_eq!(candidates.len(), 12);
    for (n, c) in (2..=13).zip(candidates) {
        assert_eq!(c["story"]["id"], format!("SH-{n}"));
        assert_eq!(
            c["reasons"][0],
            if n % 2 == 0 {
                "in-progress"
            } else {
                "verifying"
            }
        );
        assert_eq!(
            c["story"]["description"],
            format!("Requirements for candidate {n}")
        );
        assert_eq!(c["story"]["comments"][0]["text"], "implementation evidence");
    }
    assert_eq!(
        candidates[0]["referenced_by"]["prs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let markdown = document(&f, &target, false).unwrap();
    for needle in [
        "Obviation review",
        "SH-13",
        "implementation evidence",
        "Requirements for candidate 2",
        "https://github.com/acme/widgets/pull/1",
        "story help obviation-review",
    ] {
        assert!(markdown.contains(needle), "missing {needle}: {markdown}");
    }
    assert_eq!(
        before,
        f.store().read(|tx| tx.max_global_seq(f.project())).unwrap()
    );
}

#[test]
fn recent_comments_and_repeated_done_events_do_not_turn_old_completions_into_candidates() {
    let mut f = ServiceFixture::new();
    let old = create(&f, "completed before target");
    move_to(&f, &old, "done");
    at(&mut f, "2026-01-01T01:00:00Z");
    let target = create(&f, "target");
    let equal = create(&f, "completed at target creation");
    move_to(&f, &equal, "done");
    at(&mut f, "2026-01-01T02:00:00Z");
    StoryService::new(&f.ctx())
        .comment(&old, "recent comment")
        .unwrap();
    for id in [&old, &equal] {
        inject_events(
            f.store(),
            f.project(),
            StoryNo::parse_id("SH", id).unwrap(),
            &[StoryEvent::StoryStateChanged {
                at: "2026-01-01T02:00:00Z".into(),
                state: "done".into(),
            }],
        )
        .unwrap();
    }
    let abandoned = create(&f, "abandoned after target");
    move_to(&f, &abandoned, "dropped");
    assert_eq!(review(&f, &target)["candidates"], serde_json::json!([]));
}

#[test]
fn completed_candidates_survive_archive_reopen_and_recompletion() {
    let mut f = ServiceFixture::new();
    let target = create(&f, "target");
    let archived = create(&f, "archived completion");
    let reopened = create(&f, "reopened completion");
    at(&mut f, "2026-01-01T00:01:00Z");
    move_to(&f, &archived, "done");
    StoryService::new(&f.ctx()).hide(&archived).unwrap();
    move_to(&f, &reopened, "done");
    StoryService::new(&f.ctx()).reopen(&reopened).unwrap();
    let r = review(&f, &target);
    assert_eq!(r["candidates"].as_array().unwrap().len(), 2);
    assert_eq!(r["candidates"][1]["story"]["state"], "todo");
    assert_eq!(r["candidates"][1]["completed_at"], "2026-01-01T00:01:00Z");
    at(&mut f, "2026-01-01T00:02:00Z");
    move_to(&f, &reopened, "done");
    inject_events(
        f.store(),
        f.project(),
        StoryNo::parse_id("SH", &reopened).unwrap(),
        &[StoryEvent::StoryStateChanged {
            at: "2026-01-01T00:03:00Z".into(),
            state: "done".into(),
        }],
    )
    .unwrap();
    let r = review(&f, &target);
    assert_eq!(r["candidates"][1]["completed_at"], "2026-01-01T00:02:00Z");
    assert_eq!(
        r["candidates"][1]["reasons"],
        serde_json::json!(["completed-since-creation"])
    );
}

#[test]
fn completion_comparison_uses_instants_with_offsets_and_fractional_seconds() {
    let mut f = ServiceFixture::new();
    at(&mut f, "2026-01-01T01:00:00.1+01:00");
    let target = create(&f, "target");
    let later = create(&f, "later instant sorts earlier as text");
    let equal = create(&f, "same instant another spelling");
    let earlier = create(&f, "earlier instant sorts later as text");
    at(&mut f, "2026-01-01T00:00:00.11Z");
    move_to(&f, &later, "done");
    at(&mut f, "2026-01-01T00:00:00.100Z");
    move_to(&f, &equal, "done");
    at(&mut f, "2026-01-01T02:00:00.01+02:00");
    move_to(&f, &earlier, "done");
    let r = review(&f, &target);
    assert_eq!(r["candidates"].as_array().unwrap().len(), 1);
    assert_eq!(r["candidates"][0]["story"]["id"], later);
}

#[test]
fn missing_target_is_an_error_not_an_empty_review() {
    let f = ServiceFixture::new();
    for id in ["SH-999", "OTHER-1", "not-an-id"] {
        assert!(document(&f, id, true).unwrap_err().to_string().contains(id));
    }
}

#[test]
fn invalid_completion_timestamp_fails_with_the_story_and_field() {
    let f = ServiceFixture::new();
    let target = create(&f, "target");
    let candidate = create(&f, "broken completion");
    inject_events(
        f.store(),
        f.project(),
        StoryNo::parse_id("SH", &candidate).unwrap(),
        &[
            StoryEvent::StoryStateChanged {
                at: "not-a-timestamp".into(),
                state: "done".into(),
            },
            StoryEvent::StoryClosedAndArchived {
                at: "not-a-timestamp".into(),
                state: "done".into(),
            },
        ],
    )
    .unwrap();
    let error = document(&f, &target, true).unwrap_err().to_string();
    for needle in [&candidate, "done-entry", "not-a-timestamp"] {
        assert!(error.contains(needle), "{error}");
    }
}

#[test]
fn missing_history_cannot_masquerade_as_a_clean_review() {
    for broken_target in [false, true] {
        let f = ServiceFixture::new();
        let target = create(&f, "target");
        let candidate = create(&f, "candidate");
        let broken = if broken_target { &target } else { &candidate };
        f.expects_drift();
        storyhook::store::test_support::forget_events(
            f.store(),
            f.project(),
            StoryNo::parse_id("SH", broken).unwrap(),
        )
        .unwrap();
        let error = document(&f, &target, true).unwrap_err().to_string();
        assert!(
            error.contains(broken) && error.contains("history"),
            "{error}"
        );
    }
}

#[test]
fn completion_and_current_activity_are_both_reported_once() {
    let mut f = ServiceFixture::new();
    let target = create(&f, "target");
    let other = create(&f, "replacement reopened for followup");
    at(&mut f, "2026-01-01T00:00:01Z");
    move_to(&f, &other, "done");
    StoryService::new(&f.ctx()).reopen(&other).unwrap();
    move_to(&f, &other, "in-progress");
    let r = review(&f, &target);
    assert_eq!(r["candidates"].as_array().unwrap().len(), 1);
    assert_eq!(
        r["candidates"][0]["reasons"],
        serde_json::json!(["in-progress", "completed-since-creation"])
    );
}

#[test]
fn legacy_deletion_preserves_prior_completion_without_becoming_a_completion() {
    let mut f = ServiceFixture::new();
    let target = create(&f, "target");
    let completed = create(&f, "completed then deleted");
    let abandoned = create(&f, "deleted without completion");
    at(&mut f, "2026-01-01T00:01:00Z");
    move_to(&f, &completed, "done");
    for id in [&completed, &abandoned] {
        inject_events(
            f.store(),
            f.project(),
            StoryNo::parse_id("SH", id).unwrap(),
            &[StoryEvent::StoryDeleted {
                at: "2026-01-01T00:02:00Z".into(),
                reason: "legacy abandonment".into(),
            }],
        )
        .unwrap();
    }
    let r = review(&f, &target);
    let candidates = r["candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["story"]["id"], completed);
    assert_eq!(candidates[0]["story"]["state"], "dropped");
    assert_eq!(candidates[0]["completed_at"], "2026-01-01T00:01:00Z");
}
