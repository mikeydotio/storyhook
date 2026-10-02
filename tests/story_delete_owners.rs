//! `story delete` names the durable owner that keeps a story (SH-848).
//!
//! A pending landing, a native `story reset` reservation and an in-flight
//! block delivery each own the story they name. The store already refused to
//! purge such a story, but by a raw foreign-key or ownership failure, which
//! reads as store damage (exit 5). The delete and its preview must refuse
//! first, as an ordinary validation error that names the owner, and leave the
//! story and its owner exactly as they were.

use storyhook::error::AppError;
use storyhook::service::landing::{LandingAdmission, VerifiedSubmission};
use storyhook::service::{NewStoryInput, PrLinkService, StoryService, VerificationQueue};
use storyhook::store::{
    BlockAction, DeliveryStatus, LandingIntent, ReadOps, Store, StoryNo, WriteOps,
};
use storyhook_test_support::ServiceFixture;

const PR: &str = "https://github.com/acme/widgets/pull/1";

fn created(f: &ServiceFixture) -> String {
    StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "owned story".into(),
            ..Default::default()
        })
        .unwrap()
        .id
}

/// A submitted story whose certified landing has been admitted.
fn landing(f: &ServiceFixture) -> (String, LandingIntent) {
    f.github_checkout("https://github.com/acme/widgets");
    let ctx = f.ctx();
    let id = created(f);
    PrLinkService::new(&ctx).link(&id, PR, true).unwrap();
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    let queue = VerificationQueue::new(f.store());
    let candidate = queue.next().unwrap().unwrap();
    let certification = VerifiedSubmission {
        head: "a".repeat(40),
        tree: "b".repeat(40),
        gate: "make test".into(),
    };
    let LandingAdmission::Admitted(intent) = queue
        .begin_landing(&ctx, &candidate, &certification)
        .unwrap()
    else {
        panic!("landing admission refused")
    };
    (id, intent)
}

/// Both doors refuse with a validation error carrying every expected phrase,
/// and the story keeps its row and every event.
fn assert_refused(f: &ServiceFixture, id: &str, expected: &[&str]) {
    let story = StoryNo::new(id.trim_start_matches("SH-").parse().unwrap());
    let before = f.store().read(|tx| tx.story(f.project(), story)).unwrap();
    let events = f
        .store()
        .read(|tx| tx.events_for(f.project(), story))
        .unwrap()
        .len();
    let ctx = f.ctx();
    let stories = StoryService::new(&ctx);
    for (door, error) in [
        (
            "delete_plan",
            stories.delete_plan(id).map(|_| ()).unwrap_err(),
        ),
        ("delete", stories.delete(id).map(|_| ()).unwrap_err()),
    ] {
        let AppError::Validation(message) = &error else {
            panic!("{door} {id}: expected a validation refusal, got {error:?}");
        };
        for phrase in expected {
            assert!(message.contains(phrase), "{door} {id}: {message}");
        }
    }
    assert!(before.is_some());
    assert_eq!(
        f.store().read(|tx| tx.story(f.project(), story)).unwrap(),
        before
    );
    assert_eq!(
        f.store()
            .read(|tx| tx.events_for(f.project(), story))
            .unwrap()
            .len(),
        events
    );
}

#[test]
fn a_pending_landing_refuses_the_delete_by_name() {
    let f = ServiceFixture::new();
    let (id, intent) = landing(&f);
    assert_refused(&f, &id, &["landing", &intent.id, PR]);
    assert_eq!(f.store().read(|tx| tx.landing_intents()).unwrap(), [intent]);
}

#[test]
fn a_native_reset_reservation_refuses_the_delete_by_name() {
    let f = ServiceFixture::new();
    let id = created(&f);
    f.store()
        .write(|tx| {
            tx.put_legacy_story_reset(
                f.project(),
                StoryNo::new(1),
                Some(r#"{"operation":"native-owner"}"#),
            )
        })
        .unwrap();
    assert_refused(&f, &id, &[&format!("story reset {id}")]);
    assert!(
        f.store()
            .read(|tx| tx.story_resets(f.project()))
            .unwrap()
            .contains_key(&StoryNo::new(1))
    );
}

#[test]
fn an_in_flight_block_delivery_refuses_the_delete_by_name() {
    let f = ServiceFixture::new();
    let id = created(&f);
    f.store()
        .write(|tx| tx.enqueue_block_delivery(f.project(), StoryNo::new(1), BlockAction::Interrupt))
        .unwrap();
    let mut delivery = f
        .store()
        .read(|tx| Ok(tx.block_deliveries(f.project())?.remove(0)))
        .unwrap();
    delivery.status = DeliveryStatus::Attempting;
    delivery.target = Some("acknowledged-session".into());
    assert!(
        f.store()
            .write(|tx| tx.update_block_delivery(&delivery, DeliveryStatus::Pending))
            .unwrap()
    );
    assert_refused(&f, &id, &["block delivery", &delivery.id.to_string()]);
    assert_eq!(
        f.store()
            .read(|tx| tx.block_deliveries(f.project()))
            .unwrap(),
        [delivery]
    );
}

#[test]
fn a_pending_block_delivery_does_not_refuse_the_delete() {
    let f = ServiceFixture::new();
    let id = created(&f);
    f.store()
        .write(|tx| tx.enqueue_block_delivery(f.project(), StoryNo::new(1), BlockAction::Interrupt))
        .unwrap();
    // Pending is not yet an external operation: it cascades with the story.
    StoryService::new(&f.ctx()).delete(&id).unwrap();
    assert!(
        f.store()
            .read(|tx| tx.block_deliveries(f.project()))
            .unwrap()
            .is_empty()
    );
}
