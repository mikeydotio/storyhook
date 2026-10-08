//! Previously accepted recovery data survives; a new return still needs real native proof.
use super::*;
use crate::service::project_fault::{ProjectFault, ReceiptRefusal};
use crate::service::project_recovery::*;
use crate::store::StoryNo;

pub(super) fn retained_lineage(b: &mut Board) -> String {
    crate::service::PrLinkService::new(&b.ctx())
        .link(
            &b.candidate.story_id,
            "https://github.com/acme/widgets/pull/1",
            true,
        )
        .unwrap();
    b.candidate = VerificationQueue::new(&b.store).next().unwrap().unwrap();
    let ctx = b.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let fault = ProjectFault::MissingCertification {
        locus: ".storyhook.toml#verify.gate".into(),
        execution: "/tmp/retained-project-fault.execution".into(),
        execution_status: 0,
        head: "a".repeat(40),
        head_tree: "d".repeat(40),
        base: "b".repeat(40),
        tree: "c".repeat(40),
        gate: "fixture gate".into(),
        log: "/tmp/retained-project-fault.log".into(),
        receipt: ReceiptRefusal::Missing,
        detail: "previously accepted repair lineage".into(),
    };
    let mut view = service
        .observe(&b.candidate, &fault, "historical-observation")
        .unwrap()
        .unwrap();
    // Seed a pre-upgrade accepted decision as data. New observations must not
    // use this fixture path to acquire implementation authority.
    let input = DecisionInput {
        join_recovery: None,
        version: 1,
        revision: view.record.revision,
        project: b.candidate.project,
        generation: view.state.assessment.generation,
        dispatch_identity: view.state.assessment.dispatch_identity.clone(),
        scope: RepairScope::SameStory,
        context: "historical accepted scope".into(),
        question: "repair owner".into(),
        decision: "same story".into(),
        rationale: "retained prior acceptance".into(),
        evidence: vec!["attempt:historical-observation".into()],
        repair: None,
        prerequisite: None,
    };
    view.state.assessment.status = AssessmentStatus::Decided;
    view.state.assessment.hold = None;
    view.state.decision = Some(DecisionReceipt {
        input,
        accepted_at: ctx.now(),
        repair_story: Some(StoryNo::new(1)),
        delivery_identity: Some("historical-delivery".into()),
        owned_edges: vec![],
        dependency_holds: vec![],
        skipped_subjects: vec![],
    });
    view.record.revision += 1;
    view.record.state = serde_json::to_value(&view.state).unwrap();
    b.store
        .write(|tx| tx.update_project_recovery(&view.record, view.record.revision - 1))
        .unwrap();
    service.show(&view.record.id).unwrap();
    StoryService::new(&ctx)
        .set_state(&b.candidate.story_id, "in-progress", None, None, None)
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&b.candidate.story_id, "verifying", None, None, None)
        .unwrap();
    b.candidate = VerificationQueue::new(&b.store).next().unwrap().unwrap();
    view.record.id
}
