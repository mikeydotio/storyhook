//! SH-772: `story doctor --fix` healing a drifted row is a change of effective
//! blocking like any other, so it records the delivery edges the change implies.
//!
//! Every reader — `story next`, the dashboard, an agent's own `story show` —
//! reads the read model, not the events. A row that drifted into "blocked" held
//! its agent as surely as a real hold, so healing it back is an unblock the
//! agent must hear about; the reverse is a block that must interrupt.

use storyhook::service::{IntegrityService, NewStoryInput, StoryService};
use storyhook::store::test_support::corrupt_snapshot;
use storyhook::store::{BlockAction, DeliveryStatus, ReadOps, Store, StoryNo};
use storyhook_test_support::ServiceFixture;

fn active(f: &ServiceFixture) -> (String, StoryNo) {
    let ctx = f.ctx();
    let service = StoryService::new(&ctx);
    let id = service
        .create(&NewStoryInput {
            title: "worked".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    service
        .set_state(&id, "in-progress", None, None, None)
        .unwrap();
    let story = StoryNo::parse_id("SH", &id).unwrap();
    (id, story)
}

fn deliveries(f: &ServiceFixture, story: StoryNo) -> Vec<(BlockAction, DeliveryStatus)> {
    f.store()
        .read(|tx| tx.block_deliveries(f.project()))
        .unwrap()
        .into_iter()
        .filter(|delivery| delivery.story == story)
        .map(|delivery| (delivery.action, delivery.status))
        .collect()
}

fn awaiting(f: &ServiceFixture, story: StoryNo) -> Option<String> {
    f.store()
        .read(|tx| tx.story(f.project(), story))
        .unwrap()
        .unwrap()
        .snapshot
        .awaiting
}

fn repair(f: &ServiceFixture) {
    IntegrityService::new(&f.ctx()).repair().unwrap();
}

#[test]
fn healing_a_row_that_drifted_into_a_hold_resumes_its_agent() {
    let f = ServiceFixture::new();
    let (_, story) = active(&f);
    corrupt_snapshot(f.store(), f.project(), story, |snapshot| {
        snapshot["awaiting"] = serde_json::json!("a hold no event recorded");
    })
    .unwrap();
    assert!(deliveries(&f, story).is_empty());

    repair(&f);

    assert_eq!(
        awaiting(&f, story),
        None,
        "the heal restored the events' truth"
    );
    assert_eq!(
        deliveries(&f, story),
        [(BlockAction::Resume, DeliveryStatus::Pending)],
        "the heal unblocked an active story, so its Resume is owed"
    );
}

#[test]
fn healing_a_row_that_drifted_out_of_a_hold_interrupts_its_agent() {
    let f = ServiceFixture::new();
    let (id, story) = active(&f);
    StoryService::new(&f.ctx())
        .set_awaiting(&id, "a recorded hold")
        .unwrap();
    corrupt_snapshot(f.store(), f.project(), story, |snapshot| {
        snapshot["awaiting"] = serde_json::Value::Null;
    })
    .unwrap();

    repair(&f);

    assert_eq!(awaiting(&f, story).as_deref(), Some("a recorded hold"));
    assert_eq!(
        deliveries(&f, story),
        [
            (BlockAction::Interrupt, DeliveryStatus::Superseded),
            (BlockAction::Interrupt, DeliveryStatus::Pending)
        ],
        "the drifted row ended the first episode; the heal starts a new one"
    );
}

#[test]
fn a_heal_that_changes_no_blocking_records_nothing() {
    let f = ServiceFixture::new();
    let (_, story) = active(&f);
    corrupt_snapshot(f.store(), f.project(), story, |snapshot| {
        snapshot["title"] = serde_json::json!("not what the events say");
    })
    .unwrap();

    repair(&f);

    assert!(deliveries(&f, story).is_empty());
}

/// The derivation reads the very rows doctor exists to heal, so it must not
/// refuse the heal because one of them no longer names itself correctly.
#[test]
fn a_row_whose_id_drifted_is_still_healed() {
    let f = ServiceFixture::new();
    let (id, story) = active(&f);
    corrupt_snapshot(f.store(), f.project(), story, |snapshot| {
        snapshot["id"] = serde_json::json!("not-an-id");
    })
    .unwrap();

    repair(&f);

    let row = f
        .store()
        .read(|tx| tx.story(f.project(), story))
        .unwrap()
        .unwrap();
    assert_eq!(row.snapshot.id, id);
}
