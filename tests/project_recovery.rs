//! Current generation authority, durable assessment ownership, and bounded delivery.

#[path = "project_recovery/attempts.rs"]
mod attempts;
#[path = "project_recovery/callback.rs"]
mod callback;
#[path = "project_recovery/decision.rs"]
mod decision;
#[path = "project_recovery/landing.rs"]
mod landing;
#[path = "project_recovery/queue.rs"]
mod queue;
#[path = "project_recovery/refusal.rs"]
mod refusal;
#[path = "project_recovery/resume.rs"]
mod resume;
#[path = "project_recovery/work.rs"]
mod work;

use storyhook::service::project_fault::{ProjectFault, ReceiptRefusal};
use storyhook::service::project_recovery::{
    AssessmentDelivery, AssessmentHold, AssessmentStatus, ProjectRecoveryService,
};
use storyhook::service::{
    NewStoryInput, PrLinkService, StoryService, VerificationCandidate, VerificationQueue,
};
use storyhook::store::{ReadOps, Store, StoryNo, WriteOps};
use storyhook_test_support::ServiceFixture;

fn submitted(f: &ServiceFixture, title: &str) -> VerificationCandidate {
    let ctx = f.ctx();
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: title.into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    PrLinkService::new(&ctx)
        .link(&id, "https://github.com/acme/widgets/pull/1", true)
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    VerificationQueue::new(f.store())
        .ordered_for(f.project())
        .unwrap()
        .into_iter()
        .find(|c| c.story_id == id)
        .unwrap()
}

#[test]
fn confirmed_assessment_expires_without_a_competing_dispatch() {
    let mut f = fixture();
    let candidate = submitted(&f, "response deadline");
    let delivered = {
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        let view = service
            .observe(&candidate, &fault(), "attempt")
            .unwrap()
            .unwrap();
        let claimed = service.claim_assessment(&view.record.id).unwrap().unwrap();
        service
            .settle_assessment(
                &view.record.id,
                &claimed.state.assessment.dispatch_identity,
                claimed.state.assessment.epoch,
                AssessmentDelivery::Delivered,
            )
            .unwrap()
    };
    for (time, expected) in [
        ("2026-01-01T00:29:59Z", None),
        (
            "2026-01-01T00:30:00Z",
            Some(AssessmentHold::ResponseExpired),
        ),
    ] {
        f.set_clock(storyhook::service::Clock::Fixed(time.into()));
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        assert!(
            service
                .claim_assessment(&delivered.record.id)
                .unwrap()
                .is_none()
        );
        let view = service.show(&delivered.record.id).unwrap();
        assert_eq!(view.state.assessment.hold, expected);
        assert_eq!(view.state.assessment.epoch, 1);
        assert_eq!(view.state.assessment.failures, 0);
        let row = f
            .store()
            .read(|tx| tx.story(f.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap();
        assert_eq!(row.awaiting.is_some(), expected.is_some());
        if expected.is_some() {
            assert_eq!(view.state.holds.len(), 1);
            let owned = &view.state.holds[0];
            assert_eq!(owned.awaiting, row.awaiting.unwrap());
            let recorded = f
                .store()
                .read(|tx| tx.events_for(f.project(), StoryNo::new(1)))
                .unwrap();
            assert!(recorded.iter().any(|event| event.global_seq == owned.event && matches!(event.known(), Some(storyhook::domain::StoryEvent::StoryAwaitingSet { awaiting, .. }) if awaiting == &owned.awaiting)));
            assert!(
                f.store()
                    .read(|tx| tx.block_deliveries(f.project()))
                    .unwrap()
                    .iter()
                    .any(|delivery| delivery.story == owned.story
                        && delivery.action == storyhook::store::BlockAction::Interrupt)
            );
        }
    }
}

#[test]
fn restart_retains_in_flight_identity_and_manual_stop_does_not_erase_delivery_evidence() {
    let f = fixture();
    let candidate = submitted(&f, "restart boundary");
    let claimed = {
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        let view = service
            .observe(&candidate, &fault(), "attempt")
            .unwrap()
            .unwrap();
        service.claim_assessment(&view.record.id).unwrap().unwrap()
    };
    let reopened = storyhook::store::SqliteStore::open(f.store().path()).unwrap();
    let ctx = storyhook::service::Ctx::new(
        &reopened,
        f.project(),
        candidate.checkout.clone(),
        f.env().clone(),
    );
    let service = ProjectRecoveryService::new(&ctx);
    assert_eq!(service.show(&claimed.record.id).unwrap(), claimed);
    assert!(
        service
            .claim_assessment(&claimed.record.id)
            .unwrap()
            .is_none()
    );
    reopened
        .write(|tx| tx.put_verification_enabled(f.project(), false))
        .unwrap();
    let held = service
        .settle_assessment(
            &claimed.record.id,
            &claimed.state.assessment.dispatch_identity,
            claimed.state.assessment.epoch,
            AssessmentDelivery::Delivered,
        )
        .unwrap();
    assert_eq!(
        held.state.assessment.hold,
        Some(AssessmentHold::OperatorStop)
    );
    assert_eq!(
        held.state.assessment.last_result,
        Some(AssessmentDelivery::Delivered)
    );
    assert!(held.state.assessment.delivered_at.is_some());
    assert!(
        service
            .claim_assessment(&claimed.record.id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn uncertain_ownership_holds_without_spending_proven_failure_budget() {
    let f = fixture();
    let candidate = submitted(&f, "ambiguous managed owner");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service
        .observe(&candidate, &fault(), "attempt")
        .unwrap()
        .unwrap();
    let claimed = service.claim_assessment(&view.record.id).unwrap().unwrap();
    let held = service
        .settle_assessment(
            &view.record.id,
            &claimed.state.assessment.dispatch_identity,
            claimed.state.assessment.epoch,
            AssessmentDelivery::Uncertain("provider identity cannot be verified".into()),
        )
        .unwrap();
    assert_eq!(
        held.state.assessment.hold,
        Some(AssessmentHold::OwnershipUncertain)
    );
    assert_eq!(held.state.assessment.failures, 0);
    assert!(
        f.store()
            .read(|tx| tx.story(f.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .awaiting
            .is_some()
    );
    assert!(service.claim_assessment(&view.record.id).unwrap().is_none());
}

#[test]
fn malformed_persisted_observations_cannot_grant_assessment_authority() {
    let f = fixture();
    let candidate = submitted(&f, "corrupt evidence refuses recovery");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service
        .observe(&candidate, &fault(), "attempt")
        .unwrap()
        .unwrap();
    let mut malformed = view.observations[0].clone();
    malformed.attempt_id = "malformed-observation".into();
    malformed.evidence["fault"]["execution_status"] = 127.into();
    f.store()
        .write(|tx| tx.insert_project_recovery_observation(&malformed))
        .unwrap();
    assert!(service.show(&view.record.id).is_err());
    assert!(service.claim_assessment(&view.record.id).is_err());
}

fn fault() -> ProjectFault {
    ProjectFault::MissingCertification {
        locus: ".storyhook.toml#verify.gate".into(),
        tree: "a".repeat(40),
        base: "b".repeat(40),
        head: "c".repeat(40),
        head_tree: "d".repeat(40),
        gate: "make test".into(),
        log: "/retained/gate.log".into(),
        execution: "/retained/execution.json".into(),
        execution_status: 0,
        receipt: ReceiptRefusal::Missing,
        detail: "successful gate omitted certification".into(),
    }
}

fn fixture() -> ServiceFixture {
    let f = ServiceFixture::new();
    f.github_checkout("https://github.com/acme/widgets");
    f
}

#[test]
fn terminal_assessment_preserves_independent_awaiting_and_replays_without_more_events() {
    let f = fixture();
    let first = submitted(&f, "assessment owner");
    let second = submitted(&f, "independent hold");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service.observe(&first, &fault(), "first").unwrap().unwrap();
    service
        .observe(&second, &fault(), "second")
        .unwrap()
        .unwrap();
    let claimed = service.claim_assessment(&view.record.id).unwrap().unwrap();
    StoryService::new(&ctx)
        .set_awaiting(&second.story_id, "operator prerequisite")
        .unwrap();
    let before = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(2)))
        .unwrap();
    let held = service
        .settle_assessment(
            &view.record.id,
            &claimed.state.assessment.dispatch_identity,
            claimed.state.assessment.epoch,
            AssessmentDelivery::Uncertain("pane owner is ambiguous".into()),
        )
        .unwrap();
    assert_eq!(held.state.holds.len(), 1);
    assert_eq!(held.state.holds[0].story, StoryNo::new(1));
    assert_eq!(
        f.store()
            .read(|tx| tx.story(f.project(), StoryNo::new(2)))
            .unwrap(),
        before
    );
    assert_eq!(
        service
            .settle_assessment(
                &view.record.id,
                &claimed.state.assessment.dispatch_identity,
                claimed.state.assessment.epoch,
                AssessmentDelivery::Uncertain("pane owner is ambiguous".into())
            )
            .unwrap(),
        held
    );
    assert!(service.claim_assessment(&view.record.id).unwrap().is_none());
}

#[test]
fn late_subject_inherits_terminal_hold_without_restoring_a_cleared_operator_hold() {
    let f = fixture();
    let first = submitted(&f, "original held owner");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service.observe(&first, &fault(), "first").unwrap().unwrap();
    let claimed = service.claim_assessment(&view.record.id).unwrap().unwrap();
    service
        .settle_assessment(
            &view.record.id,
            &claimed.state.assessment.dispatch_identity,
            claimed.state.assessment.epoch,
            AssessmentDelivery::Uncertain("ambiguous owner".into()),
        )
        .unwrap();
    StoryService::new(&ctx)
        .clear_awaiting(&first.story_id)
        .unwrap();
    let second = submitted(&f, "late affected story");
    let joined = service
        .observe(&second, &fault(), "second")
        .unwrap()
        .unwrap();
    assert_eq!(joined.state.holds.len(), 2);
    assert!(
        f.store()
            .read(|tx| tx.story(f.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .awaiting
            .is_none()
    );
    assert!(
        f.store()
            .read(|tx| tx.story(f.project(), StoryNo::new(2)))
            .unwrap()
            .unwrap()
            .awaiting
            .is_some()
    );
}

#[test]
fn unresolved_landing_cannot_be_returned_for_fault_assessment() {
    let f = fixture();
    let mut candidate = submitted(&f, "uncertain merge authority");
    candidate.landing_pending = true;
    let ctx = f.ctx();
    assert!(
        ProjectRecoveryService::new(&ctx)
            .observe(&candidate, &fault(), "attempt")
            .unwrap()
            .is_none()
    );
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "verifying");
    assert!(
        f.store()
            .read(|tx| tx.project_recoveries(f.project()))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn observation_returns_only_the_current_submission_and_replays_without_new_work() {
    let f = fixture();
    let candidate = submitted(&f, "unjudged tree");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let first = service
        .observe(&candidate, &fault(), "attempt-1")
        .unwrap()
        .unwrap();
    assert_eq!(first.state.assessment.status, AssessmentStatus::Pending);
    assert_eq!(first.observations.len(), 1);
    assert_eq!(
        first.observations[0].evidence["fault"]["tree"],
        "a".repeat(40)
    );
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "in-progress");
    assert_eq!(row.snapshot.comments.len(), 1);
    assert!(row.snapshot.comments[0].text.contains("scope"));
    assert!(
        row.snapshot.comments[0]
            .text
            .contains(&first.state.assessment.dispatch_identity)
    );
    assert!(
        f.store()
            .read(|tx| tx.verification_incident(f.project()))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        service
            .observe(&candidate, &fault(), "attempt-1")
            .unwrap()
            .unwrap(),
        first
    );
    let mut conflict = fault();
    if let ProjectFault::MissingCertification { head, .. } = &mut conflict {
        *head = "d".repeat(40);
    }
    assert!(service.observe(&candidate, &conflict, "attempt-1").is_err());
    assert_eq!(service.show(&first.record.id).unwrap(), first);
}

#[test]
fn repeated_faults_coalesce_without_replacing_an_in_flight_assessor() {
    let f = fixture();
    let first_candidate = submitted(&f, "first affected");
    let second_candidate = submitted(&f, "second affected");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let first = service
        .observe(&first_candidate, &fault(), "first-attempt")
        .unwrap()
        .unwrap();
    let claimed = service.claim_assessment(&first.record.id).unwrap().unwrap();
    assert!(
        service
            .claim_assessment(&first.record.id)
            .unwrap()
            .is_none()
    );
    let joined = service
        .observe(&second_candidate, &fault(), "second-attempt")
        .unwrap()
        .unwrap();
    assert_eq!(joined.record.id, first.record.id);
    assert_eq!(joined.state.subjects.len(), 2);
    assert_eq!(joined.observations.len(), 2);
    assert_eq!(joined.state.assessment, claimed.state.assessment);
    let delivered = service
        .settle_assessment(
            &first.record.id,
            &claimed.state.assessment.dispatch_identity,
            claimed.state.assessment.epoch,
            AssessmentDelivery::Delivered,
        )
        .unwrap();
    assert_eq!(
        delivered.state.assessment.status,
        AssessmentStatus::Delivered
    );
    assert_eq!(delivered.state.subjects.len(), 2);
    assert_eq!(
        service
            .settle_assessment(
                &first.record.id,
                &claimed.state.assessment.dispatch_identity,
                claimed.state.assessment.epoch,
                AssessmentDelivery::Delivered
            )
            .unwrap(),
        delivered
    );
}

#[test]
fn newer_submissions_and_existing_holds_cannot_be_overwritten_by_enrollment() {
    let f = fixture();
    let candidate = submitted(&f, "new generation wins");
    let ctx = f.ctx();
    StoryService::new(&ctx)
        .set_state(&candidate.story_id, "in-progress", None, None, None)
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&candidate.story_id, "verifying", None, None, None)
        .unwrap();
    let service = ProjectRecoveryService::new(&ctx);
    assert!(
        service
            .observe(&candidate, &fault(), "stale")
            .unwrap()
            .is_none()
    );
    assert!(
        f.store()
            .read(|tx| tx.project_recoveries(f.project()))
            .unwrap()
            .is_empty()
    );
    let current = VerificationQueue::new(f.store()).next().unwrap().unwrap();
    StoryService::new(&ctx)
        .set_awaiting(&candidate.story_id, "operator investigation")
        .unwrap();
    assert!(
        service
            .observe(&current, &fault(), "held")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.store()
            .read(|tx| tx.story(f.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .awaiting
            .as_deref(),
        Some("operator investigation")
    );
}

#[test]
fn stopped_or_no_auto_projects_retain_faults_without_dispatch_or_state_changes() {
    for no_auto in [false, true] {
        let f = fixture();
        let candidate = submitted(&f, "policy hold");
        let ctx = f.ctx();
        if no_auto {
            StoryService::new(&ctx)
                .set_labels(&candidate.story_id, &["no-auto".into()], &[])
                .unwrap();
        } else {
            f.store()
                .write(|tx| tx.put_verification_enabled(f.project(), false))
                .unwrap();
        }
        let service = ProjectRecoveryService::new(&ctx);
        let view = service
            .observe(&candidate, &fault(), "policy-attempt")
            .unwrap()
            .unwrap();
        assert_eq!(view.state.assessment.status, AssessmentStatus::Held);
        assert!(!view.state.subjects[0].returned);
        assert!(service.claim_assessment(&view.record.id).unwrap().is_none());
        assert_eq!(
            f.store()
                .read(|tx| tx.story(f.project(), StoryNo::new(1)))
                .unwrap()
                .unwrap()
                .state,
            "verifying"
        );
    }
}

#[test]
fn transient_reserved_labels_revoke_the_old_assessment_authority() {
    for label in ["human-only", "no-auto"] {
        let f = fixture();
        let candidate = submitted(&f, "reserved for a person");
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        let view = service
            .observe(&candidate, &fault(), "attempt")
            .unwrap()
            .unwrap();
        StoryService::new(&ctx)
            .set_labels(&candidate.story_id, &[label.into()], &[])
            .unwrap();
        StoryService::new(&ctx)
            .set_labels(&candidate.story_id, &[], &[label.into()])
            .unwrap();
        assert!(service.claim_assessment(&view.record.id).unwrap().is_none());
        assert_eq!(
            service
                .show(&view.record.id)
                .unwrap()
                .state
                .assessment
                .status,
            AssessmentStatus::Held
        );
    }
}

#[test]
fn proven_delivery_failures_are_bounded_and_stale_completions_are_rejected() {
    let f = fixture();
    let candidate = submitted(&f, "bounded delivery");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service
        .observe(&candidate, &fault(), "attempt")
        .unwrap()
        .unwrap();
    for attempt in 1..=3 {
        let claimed = service.claim_assessment(&view.record.id).unwrap().unwrap();
        assert_eq!(claimed.state.assessment.epoch, attempt);
        let result = service
            .settle_assessment(
                &view.record.id,
                &claimed.state.assessment.dispatch_identity,
                attempt,
                AssessmentDelivery::ProvenFailure("managed dispatch proved absent".into()),
            )
            .unwrap();
        assert_eq!(result.state.assessment.failures, attempt as u8);
        assert_eq!(
            result.state.assessment.status,
            if attempt == 3 {
                AssessmentStatus::Held
            } else {
                AssessmentStatus::Pending
            }
        );
        assert!(
            service
                .settle_assessment(
                    &view.record.id,
                    &claimed.state.assessment.dispatch_identity,
                    attempt,
                    AssessmentDelivery::Delivered
                )
                .is_err()
        );
    }
    assert!(service.claim_assessment(&view.record.id).unwrap().is_none());
}
