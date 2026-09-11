//! The production engine wait, driven by a real subscriber mailbox.

use super::*;

#[test]
fn project_and_catalog_changes_wake_a_pass() {
    for change in [Change::Project("new-project".into()), Change::Catalog] {
        let bus = ChangeBus::new();
        let subscription = bus.subscribe();
        bus.publish(change);
        assert!(wait_for_reconcile(
            &subscription,
            Instant::now() + Duration::from_secs(1),
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        ));
        assert_eq!(subscription.recv(Duration::ZERO), None);
    }
}

#[test]
fn stop_and_drain_take_precedence_over_queued_work_and_expired_deadlines() {
    for (stop, draining) in [(true, false), (false, true), (true, true)] {
        let bus = ChangeBus::new();
        let subscription = bus.subscribe();
        bus.publish(Change::Project("work".into()));
        assert!(!wait_for_reconcile(
            &subscription,
            Instant::now(),
            &AtomicBool::new(stop),
            &AtomicBool::new(draining),
        ));
        assert_eq!(
            subscription.recv(Duration::ZERO),
            Some(Change::Project("work".into()))
        );
    }
}

#[test]
fn pings_do_not_shorten_the_deadline() {
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    bus.publish(Change::Ping);
    let deadline = Instant::now() + Duration::from_millis(20);
    assert!(wait_for_reconcile(
        &subscription,
        deadline,
        &AtomicBool::new(false),
        &AtomicBool::new(false),
    ));
    assert!(Instant::now() >= deadline);
}
