//! Pre-upgrade accepted data. This fixture does not emulate a service or grant
//! production authority: every operation after loading it uses the real service.
use super::*;
use storyhook::domain::StoryEvent;
use storyhook::service::project_recovery::*;

pub(super) fn accepted(f: &ServiceFixture, scope: RepairScope) -> RecoveryView {
    let view = decision::ready(f);
    retain(f, view, scope)
}

pub(super) fn retain(
    f: &ServiceFixture,
    mut view: RecoveryView,
    scope: RepairScope,
) -> RecoveryView {
    let ctx = f.ctx();
    let stories = StoryService::new(&ctx);
    let repair = match scope {
        RepairScope::SameStory => Some(view.state.assessment.story),
        RepairScope::SeparateStory => Some(
            StoryNo::parse_id(
                "SH",
                &stories
                    .create(&NewStoryInput {
                        title: "Previously accepted project repair".into(),
                        story_type: Some("bug".into()),
                        priority: Some("critical".into()),
                        description: Some(
                            "Retained pre-upgrade scope. Acceptance: restore certification.".into(),
                        ),
                        ..Default::default()
                    })
                    .unwrap()
                    .id,
            )
            .unwrap(),
        ),
        RepairScope::External => None,
    };
    let mut request = decision::input(&view, scope);
    request.evidence[0] = format!("attempt:{}", view.observations[0].attempt_id);
    let mut receipt = DecisionReceipt {
        input: request,
        accepted_at: ctx.now(),
        repair_story: repair,
        delivery_identity: repair.map(|_| "retained-repair-delivery".into()),
        owned_edges: vec![],
        dependency_holds: vec![],
        skipped_subjects: vec![],
    };
    for subject in &mut view.state.subjects {
        let id = subject.story.to_id("SH");
        stories
            .set_state(&id, "in-progress", None, None, None)
            .unwrap();
        subject.returned = true;
        subject.state_revision = state_revision(f, subject.story);
        if repair.is_some_and(|r| r != subject.story) {
            storyhook::service::RelationService::new(&ctx)
                .relate(&id, "blocked-by", &repair.unwrap().to_id("SH"), false)
                .unwrap();
            let awaiting = format!(
                "Project recovery {}: wait for certified repair landing of {}",
                view.record.id,
                repair.unwrap().to_id("SH")
            );
            stories.set_awaiting(&id, &awaiting).unwrap();
            let event = f
                .store()
                .read(|tx| {
                    Ok(tx
                        .events_for(f.project(), subject.story)?
                        .into_iter()
                        .rev()
                        .find(|e| matches!(e.known(), Some(StoryEvent::StoryAwaitingSet { .. })))
                        .unwrap()
                        .global_seq)
                })
                .unwrap();
            receipt.owned_edges.push(subject.story);
            receipt.dependency_holds.push(OwnedDependencyHold {
                story: subject.story,
                generation: subject.candidate.verifying_generation.unwrap(),
                awaiting,
                event,
            });
        } else if scope == RepairScope::External {
            let awaiting = format!(
                "Project recovery {}: {}",
                view.record.id,
                receipt.input.prerequisite.as_deref().unwrap()
            );
            stories.set_awaiting(&id, &awaiting).unwrap();
            let event = f
                .store()
                .read(|tx| {
                    Ok(tx
                        .events_for(f.project(), subject.story)?
                        .into_iter()
                        .rev()
                        .find(|e| matches!(e.known(), Some(StoryEvent::StoryAwaitingSet { .. })))
                        .unwrap()
                        .global_seq)
                })
                .unwrap();
            receipt.dependency_holds.push(OwnedDependencyHold {
                story: subject.story,
                generation: subject.candidate.verifying_generation.unwrap(),
                awaiting,
                event,
            });
        }
    }
    view.state.assessment.status = AssessmentStatus::Decided;
    view.state.assessment.hold = None;
    view.state.assessment.epoch = 1;
    view.state.assessment.started_at = Some(ctx.now());
    view.state.assessment.delivered_at = Some(ctx.now());
    view.state.assessment.last_result = Some(AssessmentDelivery::Delivered);
    if let Some(story) = repair {
        let row = f
            .store()
            .read(|tx| tx.story(f.project(), story))
            .unwrap()
            .unwrap();
        view.state.work.push(WorkDelivery {
            id: receipt.delivery_identity.clone().unwrap(),
            story,
            kind: if scope == RepairScope::SameStory {
                WorkKind::SameStoryRepair
            } else {
                WorkKind::SeparateRepair
            },
            source_attempt: None,
            managed_lease: None,
            state: row.state,
            state_revision: state_revision(f, story),
            label_revision: None,
            blocking_revision: None,
            release_event: None,
            status: WorkStatus::Pending,
            hold: None,
            disposition: None,
            epoch: 0,
            failures: 0,
            started_at: None,
            delivered_at: None,
            last_result: None,
            detail: "retained, already accepted delivery".into(),
        });
    }
    view.state.decision = Some(receipt);
    save(f, view)
}

pub(super) fn state_revision(f: &ServiceFixture, story: StoryNo) -> storyhook::store::GlobalSeq {
    f.store()
        .read(|tx| {
            Ok(tx
                .events_for(f.project(), story)?
                .into_iter()
                .rev()
                .find(|e| {
                    matches!(
                        e.known(),
                        Some(
                            StoryEvent::StoryStateChanged { .. } | StoryEvent::StoryCreated { .. }
                        )
                    )
                })
                .unwrap()
                .global_seq)
        })
        .unwrap()
}

pub(super) fn save(f: &ServiceFixture, mut view: RecoveryView) -> RecoveryView {
    let revision = view.record.revision;
    view.record.revision += 1;
    view.record.state = serde_json::to_value(&view.state).unwrap();
    assert!(
        f.store()
            .write(|tx| tx.update_project_recovery(&view.record, revision))
            .unwrap()
    );
    ProjectRecoveryService::new(&f.ctx())
        .show(&view.record.id)
        .unwrap()
}
