//! New scope proposals do not prove cause; accepted data still replays.
use super::*;
use storyhook::service::project_recovery::{DecisionInput, RecoveryView, RepairScope, RepairSpec};

pub(super) fn ready(f: &ServiceFixture) -> RecoveryView {
    let candidate = submitted(f, "scope assessor");
    ProjectRecoveryService::new(&f.ctx())
        .observe(&candidate, &fault(), "scope-attempt")
        .unwrap()
        .unwrap()
}

pub(super) fn input(view: &RecoveryView, scope: RepairScope) -> DecisionInput {
    DecisionInput {
        join_recovery: None,
        version: 1, revision: view.record.revision, project: view.record.project,
        generation: view.state.assessment.generation,
        dispatch_identity: view.state.assessment.dispatch_identity.clone(), scope,
        context: "The successful project gate omitted its required certificate.".into(),
        question: "Which story owns the gate repair?".into(),
        decision: format!("Use {scope:?} recovery."),
        rationale: "Source inspection shows the gate fault is in the registered project.".into(),
        evidence: vec!["attempt:scope-attempt".into(), "scripts/gate.sh:12".into()],
        repair: (scope == RepairScope::SeparateStory).then(|| RepairSpec {
            title: "Repair gate certification".into(), description: "Restore certification after required tests.".into(),
            acceptance: "Regression fails before repair; required suite produces a valid receipt after repair.".into(),
        }),
        prerequisite: (scope == RepairScope::External).then(|| "The owner must restore access to the signing service.".into()),
    }
}

#[test]
fn sh870_scope_proposals_cannot_turn_raw_faults_or_legacy_assessments_into_repair() {
    for status in [
        AssessmentStatus::Pending,
        AssessmentStatus::InFlight,
        AssessmentStatus::Delivered,
        AssessmentStatus::Held,
    ] {
        for scope in [
            RepairScope::SameStory,
            RepairScope::SeparateStory,
            RepairScope::External,
        ] {
            let f = fixture();
            let mut view = ready(&f);
            // Upgrade input: pending/delivered legacy assessment is not an accepted decision.
            view.state.assessment.status = status.clone();
            view.state.assessment.hold =
                (status == AssessmentStatus::Held).then_some(AssessmentHold::CauseUnproved);
            view.state.assessment.epoch = 1;
            view.state.assessment.started_at = Some(f.ctx().now());
            view.state.assessment.delivered_at =
                (status == AssessmentStatus::Delivered).then(|| f.ctx().now());
            view = legacy::save(&f, view);
            let ctx = f.ctx();
            let service = ProjectRecoveryService::new(&ctx);
            let before = f
                .store()
                .read(|tx| tx.stories(f.project(), &Default::default()))
                .unwrap();
            assert!(
                service
                    .decide(&view.record.id, &input(&view, scope))
                    .is_err()
            );
            assert_eq!(service.show(&view.record.id).unwrap(), view);
            assert_eq!(
                f.store()
                    .read(|tx| tx.stories(f.project(), &Default::default()))
                    .unwrap(),
                before
            );
            assert!(service.claim_assessment(&view.record.id).unwrap().is_none());
            assert!(
                !service
                    .delivery_permitted(&view.record.id, None, 1, false)
                    .unwrap()
            );
        }
    }
}

#[test]
fn sh870_accepted_scope_replays_without_changing_receipt_work_or_dependencies() {
    for scope in [
        RepairScope::SameStory,
        RepairScope::SeparateStory,
        RepairScope::External,
    ] {
        let f = fixture();
        let view = legacy::accepted(&f, scope);
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        let receipt = view.state.decision.as_ref().unwrap();
        assert_eq!(
            service.decide(&view.record.id, &receipt.input).unwrap(),
            view
        );
        let mut changed = receipt.input.clone();
        changed.rationale.push_str(" changed");
        assert!(service.decide(&view.record.id, &changed).is_err());
        assert_eq!(service.show(&view.record.id).unwrap(), view);
    }
}

#[test]
fn sh870_a_new_observation_preserves_accepted_lineage_without_assigning_new_repair() {
    let f = fixture();
    let before = legacy::accepted(&f, RepairScope::SeparateStory);
    let later = submitted(&f, "later raw fault");
    let after = ProjectRecoveryService::new(&f.ctx())
        .observe(&later, &fault(), "later")
        .unwrap()
        .unwrap();
    assert_eq!(after.record.id, before.record.id);
    assert_eq!(after.state.work, before.state.work);
    let mut expected = before.state.decision.clone().unwrap();
    expected.skipped_subjects.push(StoryNo::new(3));
    assert_eq!(after.state.decision.as_ref(), Some(&expected));
    assert!(!after.state.subjects.last().unwrap().returned);
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(3)))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "verifying");
    assert!(row.awaiting.is_none());
    assert!(row.snapshot.relationships.is_empty());
}
