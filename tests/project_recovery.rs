//! Current generation authority, durable assessment ownership, and bounded delivery.

#[path = "project_recovery/attempts.rs"]
mod attempts;
#[path = "project_recovery/callback.rs"]
mod callback;
#[path = "project_recovery/decision.rs"]
mod decision;
#[path = "project_recovery/deletion.rs"]
mod deletion;
#[path = "project_recovery/engine.rs"]
mod engine;
#[path = "project_recovery/external.rs"]
mod external;
#[path = "project_recovery/isolation.rs"]
mod isolation;
#[path = "project_recovery/landing.rs"]
mod landing;
#[path = "project_recovery/legacy.rs"]
mod legacy;
#[path = "project_recovery/queue.rs"]
mod queue;
#[path = "project_recovery/rearm.rs"]
mod rearm;
#[path = "project_recovery/rearm_resources.rs"]
mod rearm_resources;
#[path = "project_recovery/rearm_work.rs"]
mod rearm_work;
#[path = "project_recovery/refusal.rs"]
mod refusal;
#[path = "project_recovery/resume.rs"]
mod resume;
#[path = "project_recovery/status.rs"]
mod status;
#[path = "project_recovery/status_invalid.rs"]
mod status_invalid;
#[path = "project_recovery/work.rs"]
mod work;
#[path = "project_recovery/worker.rs"]
mod worker;

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
fn sh870_observation_holds_current_submission_and_replays_without_new_work() {
    let f = fixture();
    let candidate = submitted(&f, "unjudged tree");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let first = service
        .observe(&candidate, &fault(), "attempt-1")
        .unwrap()
        .unwrap();
    assert_eq!(first.state.assessment.status, AssessmentStatus::Held);
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
    assert_eq!(row.state, "verifying");
    assert_eq!(row.snapshot.comments.len(), 1);
    assert!(row.snapshot.comments[0].text.contains("HELD"));
    assert!(row.snapshot.comments[0].text.contains(&first.record.id));
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
fn sh870_repeated_unproved_faults_coalesce_without_starting_an_assessor() {
    let f = fixture();
    let first = submitted(&f, "first affected");
    let second = submitted(&f, "second affected");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service.observe(&first, &fault(), "first").unwrap().unwrap();
    let joined = service
        .observe(&second, &fault(), "second")
        .unwrap()
        .unwrap();
    assert_eq!(view.record.id, joined.record.id);
    assert_eq!(joined.state.subjects.len(), 2);
    assert_eq!(joined.observations.len(), 2);
    assert_eq!(joined.state.assessment, view.state.assessment);
    assert!(joined.state.subjects.iter().all(|s| !s.returned));
    assert!(service.claim_assessment(&view.record.id).unwrap().is_none());
    assert!(joined.state.work.is_empty());
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
fn sh870_fault_observation_and_policy_release_cannot_assign_unproved_repair() {
    for policy in ["none", "no-auto", "stop"] {
        let f = fixture();
        let candidate = submitted(&f, "unproved project fault");
        let ctx = f.ctx();
        let stories = StoryService::new(&ctx);
        if policy == "no-auto" {
            stories
                .set_labels(&candidate.story_id, &["no-auto".into()], &[])
                .unwrap();
        }
        if policy == "stop" {
            f.store()
                .write(|tx| tx.put_verification_enabled(f.project(), false))
                .unwrap();
        }
        let service = ProjectRecoveryService::new(&ctx);
        let view = service
            .observe(&candidate, &fault(), "unproved-fault")
            .unwrap()
            .unwrap();
        let row = f
            .store()
            .read(|tx| tx.story(f.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "verifying", "{policy}");
        assert!(!view.state.subjects[0].returned);
        assert!(view.state.work.is_empty());
        assert_eq!(view.state.assessment.status, AssessmentStatus::Held);
        if policy == "no-auto" {
            stories
                .set_labels(&candidate.story_id, &[], &["no-auto".into()])
                .unwrap();
        }
        if policy == "stop" {
            f.store()
                .write(|tx| tx.put_verification_enabled(f.project(), true))
                .unwrap();
        }
        assert!(!service.policy_rearm_ready(&view.record.id, None).unwrap());
        assert!(!service.rearm_policy_hold(&view.record.id, None).unwrap());
        assert!(service.claim_assessment(&view.record.id).unwrap().is_none());
        assert!(
            service
                .return_failed_repair(&candidate, "unproved-fault", &"a".repeat(40), "raw failure")
                .is_err()
        );
        assert_eq!(
            f.store()
                .read(|tx| tx.story(f.project(), StoryNo::new(1)))
                .unwrap()
                .unwrap()
                .state,
            "verifying"
        );
        assert!(VerificationQueue::new(f.store()).next().unwrap().is_none());
    }
}
