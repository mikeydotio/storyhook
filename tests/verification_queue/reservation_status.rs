//! A verifier held for a story that its own write took out of the queue reads
//! as activity, never as missing evidence (SH-768).

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use storyhook::daemon::bus::ChangeBus;
use storyhook::daemon::verification::status::VerifierStatus;
use storyhook::daemon::verification::{
    ReservationReason, VerifierReservation, wait_for_reconciled_candidate,
};

/// A status read at `now`, as `story verifier status` takes it.
fn status_at(
    fixture: &ServiceFixture,
    activity: &VerificationActivity,
    now: &str,
) -> VerifierStatus {
    activity
        .status(&fixture.ctx().clock(Clock::Fixed(now.into())))
        .unwrap()
}

fn seconds_after(at: &str, seconds: i64) -> String {
    (chrono::DateTime::parse_from_rfc3339(at).unwrap() + chrono::Duration::seconds(seconds))
        .to_rfc3339()
}

/// Ends the real waiter when its companion thread panics, so a failed
/// assertion fails the test instead of leaving the wait to run for ever.
struct StopOnPanic<'a>(&'a AtomicBool);

impl Drop for StopOnPanic<'_> {
    fn drop(&mut self) {
        if thread::panicking() {
            self.0.store(true, Ordering::Relaxed);
        }
    }
}

#[test]
fn a_conflict_reservation_reads_as_activity_for_the_whole_wait() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    let held = submitted(&fixture, "reconciling", Priority::Low, PR_ONE);
    let returned_generation = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap()
        .verifying_generation;
    let activity = VerificationActivity::new();
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(fixture.env().clone());
    let actuator = SequencedActuator {
        outcomes: Mutex::new(VecDeque::from([
            VerificationOutcome::Conflict {
                detail: "both modified src/lib.rs".into(),
            },
            VerificationOutcome::Certified {
                head: "a".repeat(40),
                tree: "b".repeat(40),
                detail: "landed after reconciliation".into(),
                gate: GateCommand::DEFAULT.into(),
            },
        ])),
        verified: Mutex::new(Vec::new()),
        notified: Mutex::new(Vec::new()),
        reaped: Mutex::new(Vec::new()),
    };
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    let stop = AtomicBool::new(false);

    let result = tick_with_reconciliation(
        fixture.store(),
        fixture.env(),
        &actuator,
        &activity,
        &inflight,
        fixture.project(),
        |reserved| {
            let queued = submitted(&fixture, "queued behind", Priority::Critical, PR_TWO);
            let status = status_at(&fixture, &activity, &fixture.env().now());
            let reservation = status
                .reservation
                .clone()
                .unwrap_or_else(|| panic!("the reconcile is not reported: {status:?}"));
            assert_eq!(reservation.story_id, held);
            assert_eq!(reservation.generation, returned_generation);
            assert_eq!(reservation.reason, ReservationReason::Reconcile);
            assert_eq!(reservation.queued_behind, 1);
            assert_eq!(status.verifying, [queued]);
            assert_eq!(
                status.active.as_ref().map(|active| active.generation),
                Some(returned_generation)
            );
            assert_eq!(status.evidence_error, None, "{status:?}");
            assert_eq!(status.warning, None, "{status:?}");

            // Past the publisher interval a reconcile is still activity.
            let later = status_at(
                &fixture,
                &activity,
                &seconds_after(&reservation.reserved_at, 121),
            );
            assert_eq!(
                later.reservation.as_ref().and_then(|r| r.age_seconds),
                Some(121)
            );
            assert_eq!(later.evidence_error, None, "{later:?}");
            assert_eq!(later.warning, None, "{later:?}");
            assert_eq!(later.silence_seconds, None);
            let text = later.render_human();
            assert!(
                text.contains(&format!("{held} reserved for merge-conflict reconcile")),
                "{text}"
            );
            assert!(!text.contains("gate on"), "{text}");

            // The production waiter runs while another reader looks and the
            // agent resubmits.
            thread::scope(|scope| {
                scope.spawn(|| {
                    let _stop = StopOnPanic(&stop);
                    let during = status_at(&fixture, &activity, &fixture.env().now());
                    assert_eq!(
                        during.reservation.map(|r| r.reason),
                        Some(ReservationReason::Reconcile)
                    );
                    assert_eq!(during.evidence_error, None);
                    assert_eq!(during.warning, None);
                    StoryService::new(&fixture.ctx())
                        .set_state(&held, "verifying", None, Some("in-progress"), None)
                        .unwrap();
                });
                wait_for_reconciled_candidate(fixture.store(), &subscription, &stop, reserved)
            })
        },
    )
    .unwrap();

    assert_eq!(result, TickResult::Completed);
    assert_eq!(
        actuator.verified.lock().unwrap().as_slice(),
        [held.as_str(), held.as_str()]
    );
    let after = status_at(&fixture, &activity, &fixture.env().now());
    assert!(after.active.is_none());
    assert!(after.reservation.is_none());
}

#[test]
fn a_reservation_crosses_the_wire_and_its_absence_still_decodes() {
    let fixture = ServiceFixture::new();
    let mut status = VerificationActivity::new().status(&fixture.ctx()).unwrap();
    let absent = serde_json::to_value(&status).unwrap();
    assert!(
        absent.get("reservation").is_none(),
        "an absent reservation is omitted, as older payloads were: {absent}"
    );
    let decoded: VerifierStatus = serde_json::from_value(absent).unwrap();
    assert!(decoded.reservation.is_none());

    status.reservation = Some(VerifierReservation {
        story_id: "SH-1".into(),
        generation: Some(GlobalSeq::new(7)),
        reason: ReservationReason::Reconcile,
        reserved_at: FIXTURE_NOW.into(),
        age_seconds: Some(5),
        queued_behind: 2,
    });
    let present = serde_json::to_value(&status).unwrap();
    assert_eq!(present["reservation"]["reason"], "reconcile");
    assert_eq!(present["reservation"]["reserved_at"], FIXTURE_NOW);
    let decoded: VerifierStatus = serde_json::from_value(present).unwrap();
    assert_eq!(decoded.reservation, status.reservation);
}

#[test]
fn the_verifier_help_topic_names_the_reservation() {
    let topic = storyhook::help_topics::get_help_topic("verifier").unwrap();
    assert!(topic.contains("reservation"), "{topic}");
}
