//! A project recovery keeps every story it names, so delete refuses them (SH-848).
//!
//! Before the refusal, `story delete` purged a named story's events and the
//! recovery's exact event references stopped resolving: every read of the
//! recovery — verifier status, the queue, repair show — then failed as store
//! corruption and told the operator to restore a snapshot.
use super::*;
use storyhook::daemon::verification::VerificationActivity;
use storyhook::error::AppError;
use storyhook::service::RelationService;
use storyhook::service::project_recovery::RecoveryView;

/// The reads the stranded reference broke all still answer.
fn reads_still_work(f: &ServiceFixture, view: &RecoveryView) {
    let ctx = f.ctx();
    VerificationActivity::new().status(&ctx).unwrap();
    VerificationQueue::new(f.store())
        .ordered_for(f.project())
        .unwrap();
    ProjectRecoveryService::new(&ctx)
        .show(&view.record.id)
        .unwrap();
}

/// Both the preview and the delete itself refuse, name the recovery and the
/// verb that keeps the record, and leave the story exactly as it was.
fn assert_refused(f: &ServiceFixture, story: StoryNo, view: &RecoveryView) {
    let ctx = f.ctx();
    let id = story.to_id("SH");
    let before = f.store().read(|tx| tx.story(f.project(), story)).unwrap();
    let events = f
        .store()
        .read(|tx| tx.events_for(f.project(), story))
        .unwrap()
        .len();
    let stories = StoryService::new(&ctx);
    for (door, error) in [
        (
            "delete_plan",
            stories.delete_plan(&id).map(|_| ()).unwrap_err(),
        ),
        ("delete", stories.delete(&id).map(|_| ()).unwrap_err()),
    ] {
        let AppError::Validation(message) = &error else {
            panic!("{door} {id}: expected a validation refusal, got {error:?}");
        };
        assert!(
            message.contains(&format!("project recovery {}", view.record.id)),
            "{door} {id}: {message}"
        );
        assert!(
            message.contains(&format!("story close {id}")),
            "{door} {id}: {message}"
        );
        assert!(!message.contains("damaged"), "{door} {id}: {message}");
    }
    assert_eq!(
        f.store().read(|tx| tx.story(f.project(), story)).unwrap(),
        before,
        "{id} must be untouched"
    );
    assert_eq!(
        f.store()
            .read(|tx| tx.events_for(f.project(), story))
            .unwrap()
            .len(),
        events,
        "{id} must keep every event"
    );
    f.assert_no_drift();
}

#[test]
fn delete_refuses_every_story_a_decided_recovery_names() {
    let f = fixture();
    let view = resume::decided(&f);
    // SH-1: subject, assessor, dependency hold and owned edge.
    // SH-2: the separate repair story and its managed work target.
    for story in [StoryNo::new(1), StoryNo::new(2)] {
        assert_refused(&f, story, &view);
    }
    let edge = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap()
        .snapshot
        .relationships;
    assert!(
        edge.iter().any(|r| r.other_id == "SH-2"),
        "the recovery-owned edge must survive the refusal: {edge:?}"
    );
    reads_still_work(&f, &view);
    // The original repro ended with an unrelated submission panicking on its
    // queue read.
    submitted(&f, "unrelated after the refusal");
}

#[test]
fn delete_refuses_the_subject_of_an_undecided_recovery() {
    let f = fixture();
    let candidate = submitted(&f, "observed only");
    let view = ProjectRecoveryService::new(&f.ctx())
        .observe(&candidate, &fault(), "attempt")
        .unwrap()
        .unwrap();
    assert_refused(&f, StoryNo::new(1), &view);
    reads_still_work(&f, &view);
}

#[test]
fn delete_refuses_stories_a_retired_recovery_still_names() {
    let f = fixture();
    let view = resume::decided(&f);
    resume::land(&f, &view);
    assert!(
        !ProjectRecoveryService::new(&f.ctx())
            .show(&view.record.id)
            .unwrap()
            .record
            .active
    );
    for story in [StoryNo::new(1), StoryNo::new(2)] {
        assert_refused(&f, story, &view);
    }
    reads_still_work(&f, &view);
}

#[test]
fn a_story_no_recovery_names_still_deletes_and_retracts_its_claims() {
    let f = fixture();
    let view = resume::decided(&f);
    let ctx = f.ctx();
    let unrelated = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "created in error".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    // The retraction lands on SH-1, a story the recovery names: it must not
    // disturb the recovery's retained event authority.
    RelationService::new(&ctx)
        .relate(&unrelated, "relates-to", "SH-1", false)
        .unwrap();
    let message = StoryService::new(&ctx).delete(&unrelated).unwrap();
    assert!(
        message.contains(&format!("retracted SH-1 relates-to {unrelated}")),
        "{message}"
    );
    reads_still_work(&f, &view);
}

#[test]
fn the_refusal_is_scoped_to_the_recovery_project() {
    let f = fixture();
    let view = resume::decided(&f);
    let other = f.add_project("other", "OT");
    let ctx = f.ctx_for(other);
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "same number, other project".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    assert_eq!(id, "OT-1");
    StoryService::new(&ctx).delete(&id).unwrap();
    reads_still_work(&f, &view);
}
