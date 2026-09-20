use super::*;
use storyhook::service::project_recovery::{
    RecoveryView, RepairAdmission, RepairCompletion, RepairInput, RepairJudgment, RepairRefusal,
    RepairScope,
};

fn prepare(f: &ServiceFixture) -> RecoveryView {
    let view = decision::ready(f);
    let ctx = f.ctx();
    ProjectRecoveryService::new(&ctx)
        .decide(
            &view.record.id,
            &decision::input(&view, RepairScope::SameStory),
        )
        .unwrap()
}
fn resubmit(f: &ServiceFixture) -> VerificationCandidate {
    let ctx = f.ctx();
    let service = StoryService::new(&ctx);
    service
        .set_state("SH-1", "in-progress", None, None, None)
        .unwrap();
    service
        .set_state("SH-1", "verifying", None, None, None)
        .unwrap();
    VerificationQueue::new(f.store())
        .ordered_for(f.project())
        .unwrap()
        .remove(0)
}
fn pins(n: u8) -> RepairInput {
    RepairInput {
        base: "b".repeat(40),
        head: format!("{n:040x}"),
        head_tree: format!("{:040x}", n + 10),
        tree: format!("{:040x}", n + 20),
    }
}

#[test]
fn ordinary_submissions_have_no_repair_budget_but_stale_authority_is_refused() {
    let f = fixture();
    let candidate = submitted(&f, "ordinary submission");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    assert_eq!(
        service
            .admit_repair(&candidate, "ordinary", &pins(1))
            .unwrap(),
        RepairAdmission::Proceed { recovery_id: None }
    );
    StoryService::new(&ctx)
        .set_state("SH-1", "in-progress", None, None, None)
        .unwrap();
    assert!(service.admit_repair(&candidate, "stale", &pins(1)).is_err());
}

#[test]
fn empty_commit_or_changed_base_does_not_make_original_fault_input_new() {
    let f = fixture();
    let view = prepare(&f);
    let candidate = resubmit(&f);
    let ctx = f.ctx();
    let mut input = pins(1);
    input.head_tree = "d".repeat(40);
    input.base = "e".repeat(40);
    let service = ProjectRecoveryService::new(&ctx);
    let refusal = service
        .admit_repair(&candidate, "empty-commit", &input)
        .unwrap();
    assert_eq!(
        refusal,
        RepairAdmission::Deferred {
            recovery_id: view.record.id.clone(),
            reason: RepairRefusal::UnchangedInput
        }
    );
    let held = service.show(&view.record.id).unwrap();
    assert!(held.state.attempts.is_empty());
    assert_eq!(held.state.refusals.len(), 1);
    assert_eq!(
        service
            .admit_repair(&candidate, "empty-commit", &input)
            .unwrap(),
        refusal
    );
    input.head = "f".repeat(40);
    assert!(
        service
            .admit_repair(&candidate, "empty-commit", &input)
            .is_err()
    );
    assert_eq!(
        f.store()
            .read(|tx| tx.story(f.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .state,
        "verifying",
        "ownership must settle before story mutation"
    );
}

#[test]
fn only_three_changed_completed_repair_inputs_are_admitted() {
    let f = fixture();
    let view = prepare(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    for n in 1..=3 {
        let candidate = resubmit(&f);
        let input = pins(n);
        let attempt = format!("repair-{n}");
        assert_eq!(
            service.admit_repair(&candidate, &attempt, &input).unwrap(),
            RepairAdmission::Proceed {
                recovery_id: Some(view.record.id.clone())
            }
        );
        let complete = service
            .complete_repair(
                &candidate,
                &attempt,
                &judgment(&pins(n), RepairCompletion::ProjectFault),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            complete
                .state
                .attempts
                .iter()
                .filter(|a| a.completion.is_some())
                .count(),
            usize::from(n)
        );
        assert_eq!(
            service
                .complete_repair(
                    &candidate,
                    &attempt,
                    &judgment(&pins(n), RepairCompletion::ProjectFault)
                )
                .unwrap()
                .unwrap(),
            complete
        );
        assert!(
            service
                .complete_repair(
                    &candidate,
                    &attempt,
                    &judgment(&pins(n), RepairCompletion::Certified)
                )
                .is_err()
        );
    }
    let candidate = resubmit(&f);
    assert_eq!(
        service
            .admit_repair(&candidate, "fourth", &pins(4))
            .unwrap(),
        RepairAdmission::Deferred {
            recovery_id: view.record.id.clone(),
            reason: RepairRefusal::BudgetExhausted
        }
    );
    assert_eq!(
        service.show(&view.record.id).unwrap().state.attempts.len(),
        3
    );
}

#[test]
fn interrupted_attempts_do_not_consume_completed_budget_or_allow_conflicting_replay() {
    let f = fixture();
    let view = prepare(&f);
    let candidate = resubmit(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    for n in 1..=5 {
        assert!(matches!(
            service
                .admit_repair(&candidate, &format!("interrupted-{n}"), &pins(1))
                .unwrap(),
            RepairAdmission::Proceed {
                recovery_id: Some(_)
            }
        ));
    }
    assert_eq!(
        service
            .show(&view.record.id)
            .unwrap()
            .state
            .attempts
            .iter()
            .filter(|a| a.completion.is_some())
            .count(),
        0
    );
    assert!(
        service
            .admit_repair(&candidate, "interrupted-1", &pins(2))
            .is_err()
    );
    service
        .complete_repair(
            &candidate,
            "interrupted-5",
            &judgment(&pins(1), RepairCompletion::TestsFailed),
        )
        .unwrap()
        .unwrap();
    let newer = resubmit(&f);
    assert!(
        service
            .complete_repair(
                &candidate,
                "interrupted-1",
                &judgment(&pins(1), RepairCompletion::Certified)
            )
            .is_err()
    );
    assert!(matches!(
        service.admit_repair(&newer, "unchanged", &pins(1)).unwrap(),
        RepairAdmission::Deferred {
            reason: RepairRefusal::UnchangedInput,
            ..
        }
    ));
}

#[test]
fn a_repair_with_a_different_fault_keeps_its_original_lineage() {
    let f = fixture();
    let initial = decision::ready(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service
        .decide(
            &initial.record.id,
            &decision::input(&initial, RepairScope::SeparateStory),
        )
        .unwrap();
    PrLinkService::new(&ctx)
        .link("SH-2", "https://github.com/acme/widgets/pull/2", true)
        .unwrap();
    StoryService::new(&ctx)
        .set_state("SH-2", "verifying", None, None, None)
        .unwrap();
    let candidate = VerificationQueue::new(f.store())
        .ordered_for(f.project())
        .unwrap()
        .into_iter()
        .find(|c| c.story_id == "SH-2")
        .unwrap();
    let input = pins(1);
    service
        .admit_repair(&candidate, "recursive", &input)
        .unwrap();
    service
        .complete_repair(
            &candidate,
            "recursive",
            &judgment(&pins(1), RepairCompletion::ProjectFault),
        )
        .unwrap()
        .unwrap();
    let fault = ProjectFault::InvalidGateConfiguration {
        locus: ".storyhook.toml#verify.gate".into(),
        tree: input.tree,
        base: input.base,
        head: input.head,
        head_tree: input.head_tree,
        configuration: "f".repeat(64),
        detail: "repair introduced invalid gate argv".into(),
    };
    let updated = service
        .observe(&candidate, &fault, "recursive")
        .unwrap()
        .unwrap();
    assert_eq!(updated.record.id, view.record.id);
    assert_eq!(updated.state.assessment, view.state.assessment);
    assert_eq!(
        updated.state.decision.as_ref().unwrap().repair_story,
        Some(StoryNo::new(2))
    );
    assert_eq!(updated.observations.len(), 2);
    assert_eq!(
        updated.state.work.len(),
        2,
        "completed recursive fault needs its own delivery receipt"
    );
    assert_eq!(
        f.store()
            .read(|tx| tx.project_recoveries(f.project()))
            .unwrap()
            .len(),
        1
    );
    let row = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(2)))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "in-progress");
    assert!(
        row.snapshot
            .relationships
            .iter()
            .all(|r| r.other_id != "SH-2")
    );
}

#[test]
fn manual_stop_after_admission_revokes_retry_without_granting_completion() {
    let f = fixture();
    let view = prepare(&f);
    let candidate = resubmit(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    service
        .admit_repair(&candidate, "attempt", &pins(1))
        .unwrap();
    f.store()
        .write(|tx| tx.put_verification_enabled(f.project(), false))
        .unwrap();
    assert_eq!(
        service
            .admit_repair(&candidate, "attempt", &pins(1))
            .unwrap(),
        RepairAdmission::Deferred {
            recovery_id: view.record.id,
            reason: RepairRefusal::PolicyHold
        }
    );
    assert!(
        service
            .complete_repair(
                &candidate,
                "attempt",
                &judgment(&pins(1), RepairCompletion::Certified)
            )
            .is_err()
    );
}

#[test]
fn an_old_unfinished_attempt_cannot_bypass_the_completed_budget() {
    let f = fixture();
    let view = prepare(&f);
    let candidate = resubmit(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    service
        .admit_repair(&candidate, "old-unfinished", &pins(4))
        .unwrap();
    for n in 1..=3 {
        let attempt = format!("completed-{n}");
        service
            .admit_repair(&candidate, &attempt, &pins(n))
            .unwrap();
        service
            .complete_repair(
                &candidate,
                &attempt,
                &judgment(&pins(n), RepairCompletion::TestsFailed),
            )
            .unwrap()
            .unwrap();
    }
    assert_eq!(
        service
            .admit_repair(&candidate, "retry-old-unfinished", &pins(4))
            .unwrap(),
        RepairAdmission::Deferred {
            recovery_id: view.record.id,
            reason: RepairRefusal::BudgetExhausted
        }
    );
}

#[test]
fn malformed_attempt_ownership_fails_closed_before_admission() {
    let f = fixture();
    let view = prepare(&f);
    let candidate = resubmit(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    service
        .admit_repair(&candidate, "admitted", &pins(1))
        .unwrap();
    let view = service.show(&view.record.id).unwrap();
    let mut record = view.record.clone();
    record.revision += 1;
    record.state["attempts"][0]["story"] = serde_json::json!(99);
    f.store()
        .write(|tx| tx.update_project_recovery(&record, view.record.revision))
        .unwrap();
    assert!(service.show(&view.record.id).is_err());
    assert!(
        service
            .admit_repair(&candidate, "another", &pins(2))
            .is_err()
    );
}

#[test]
fn certified_same_generation_retry_does_not_spend_another_changed_input_slot() {
    let f = fixture();
    let view = prepare(&f);
    let candidate = resubmit(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    service
        .admit_repair(&candidate, "certified", &pins(1))
        .unwrap();
    service
        .complete_repair(
            &candidate,
            "certified",
            &judgment(&pins(1), RepairCompletion::Certified),
        )
        .unwrap()
        .unwrap();
    assert!(matches!(
        service
            .admit_repair(&candidate, "landing-retry", &pins(1))
            .unwrap(),
        RepairAdmission::Proceed {
            recovery_id: Some(_)
        }
    ));
    service
        .complete_repair(
            &candidate,
            "landing-retry",
            &judgment(&pins(1), RepairCompletion::Certified),
        )
        .unwrap()
        .unwrap();
    for n in [2, 3] {
        let candidate = resubmit(&f);
        let attempt = format!("changed-{n}");
        assert!(matches!(
            service
                .admit_repair(&candidate, &attempt, &pins(n))
                .unwrap(),
            RepairAdmission::Proceed {
                recovery_id: Some(_)
            }
        ));
        service
            .complete_repair(
                &candidate,
                &attempt,
                &judgment(&pins(n), RepairCompletion::TestsFailed),
            )
            .unwrap()
            .unwrap();
    }
    let candidate = resubmit(&f);
    assert_eq!(
        service
            .admit_repair(&candidate, "fourth-input", &pins(4))
            .unwrap(),
        RepairAdmission::Deferred {
            recovery_id: view.record.id,
            reason: RepairRefusal::BudgetExhausted
        }
    );
}

#[test]
fn a_fresh_candidate_cannot_renew_an_old_attempt_after_a_human_reservation() {
    let f = fixture();
    let _view = prepare(&f);
    let original = resubmit(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    service
        .admit_repair(&original, "old-owner", &pins(1))
        .unwrap();
    StoryService::new(&ctx)
        .set_labels("SH-1", &["human-only".into()], &[])
        .unwrap();
    StoryService::new(&ctx)
        .set_labels("SH-1", &[], &["human-only".into()])
        .unwrap();
    let current = VerificationQueue::new(f.store())
        .ordered_for(f.project())
        .unwrap()
        .remove(0);
    assert_eq!(current.verifying_generation, original.verifying_generation);
    assert_ne!(current.human_only_revision, original.human_only_revision);
    assert!(
        service
            .complete_repair(
                &current,
                "old-owner",
                &judgment(&pins(1), RepairCompletion::Certified)
            )
            .is_err()
    );
    assert!(
        service
            .admit_repair(&current, "old-owner", &pins(1))
            .is_err()
    );
}

/// Construct the observed wire judgment for the explicitly supplied test input.
pub(super) fn judgment(input: &RepairInput, completion: RepairCompletion) -> RepairJudgment {
    match completion {
        RepairCompletion::Certified => RepairJudgment::Certified {
            head: input.head.clone(),
            tree: input.tree.clone(),
        },
        RepairCompletion::TestsFailed => RepairJudgment::TestsFailed {
            tree: input.tree.clone(),
        },
        RepairCompletion::ProjectFault => RepairJudgment::ProjectFault {
            fault: ProjectFault::InvalidGateConfiguration {
                locus: ".storyhook.toml#verify.gate".into(),
                tree: input.tree.clone(),
                base: input.base.clone(),
                head: input.head.clone(),
                head_tree: input.head_tree.clone(),
                configuration: "f".repeat(64),
                detail: "repair introduced invalid gate argv".into(),
            },
        },
    }
}

#[test]
fn completion_must_match_pinned_admission_before_it_counts_or_replays() {
    for completion in [
        RepairCompletion::Certified,
        RepairCompletion::TestsFailed,
        RepairCompletion::ProjectFault,
    ] {
        let f = fixture();
        let view = prepare(&f);
        let candidate = resubmit(&f);
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        service
            .admit_repair(&candidate, "judged", &pins(1))
            .unwrap();
        assert!(
            service
                .complete_repair(&candidate, "judged", &judgment(&pins(2), completion))
                .is_err(),
            "mismatched {completion:?}"
        );
        assert!(
            service.show(&view.record.id).unwrap().state.attempts[0]
                .completion
                .is_none()
        );
        for field in match completion {
            RepairCompletion::Certified => vec!["head", "tree"],
            RepairCompletion::TestsFailed => vec!["tree"],
            RepairCompletion::ProjectFault => vec!["head", "head_tree", "base", "tree"],
        } {
            let mut changed = pins(1);
            match field {
                "head" => changed.head = "e".repeat(40),
                "head_tree" => changed.head_tree = "e".repeat(40),
                "base" => changed.base = "e".repeat(40),
                _ => changed.tree = "e".repeat(40),
            }
            assert!(
                service
                    .complete_repair(&candidate, "judged", &judgment(&changed, completion))
                    .is_err(),
                "mismatched {completion:?} {field}"
            );
        }
        let good = judgment(&pins(1), completion);
        let complete = service
            .complete_repair(&candidate, "judged", &good)
            .unwrap()
            .unwrap();
        assert_eq!(complete.state.attempts[0].judgment.as_ref(), Some(&good));
        assert_eq!(
            service
                .complete_repair(&candidate, "judged", &good)
                .unwrap()
                .unwrap(),
            complete
        );
        assert!(
            service
                .complete_repair(&candidate, "judged", &judgment(&pins(2), completion))
                .is_err()
        );
    }
}

#[test]
fn retained_completion_requires_its_matching_judgment() {
    let f = fixture();
    let view = prepare(&f);
    let candidate = resubmit(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    service
        .admit_repair(&candidate, "judged", &pins(1))
        .unwrap();
    service
        .complete_repair(
            &candidate,
            "judged",
            &judgment(&pins(1), RepairCompletion::Certified),
        )
        .unwrap();
    f.store()
        .write(|tx| {
            let mut record = tx.project_recoveries(f.project())?.remove(0);
            let revision = record.revision;
            record.state["attempts"][0]["judgment"]["tree"] = serde_json::json!("f".repeat(40));
            record.revision += 1;
            assert!(tx.update_project_recovery(&record, revision)?);
            Ok(())
        })
        .unwrap();
    assert!(service.show(&view.record.id).is_err());
}

#[test]
fn transient_no_auto_cannot_renew_an_admitted_repair() {
    let f = fixture();
    let _view = prepare(&f);
    let candidate = resubmit(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    service
        .admit_repair(&candidate, "reserved", &pins(1))
        .unwrap();
    StoryService::new(&ctx)
        .set_labels("SH-1", &["no-auto".into()], &[])
        .unwrap();
    StoryService::new(&ctx)
        .set_labels("SH-1", &[], &["no-auto".into()])
        .unwrap();
    assert!(
        service
            .admit_repair(&candidate, "reserved", &pins(1))
            .is_err()
    );
    assert!(
        service
            .complete_repair(
                &candidate,
                "reserved",
                &judgment(&pins(1), RepairCompletion::Certified)
            )
            .is_err()
    );
}

#[test]
fn recursive_repair_return_replays_and_stops_dispatch_at_completed_budget() {
    use storyhook::service::project_recovery::WorkStatus;
    let f = fixture();
    let view = prepare(&f);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let first = &view.state.work[0];
    service
        .claim_work(&view.record.id, &first.id)
        .unwrap()
        .unwrap();
    service
        .settle_work(&view.record.id, &first.id, 1, AssessmentDelivery::Delivered)
        .unwrap();
    for n in 1..=3 {
        let candidate = resubmit(&f);
        let attempt = format!("recursive-{n}");
        let input = pins(n);
        let outcome = judgment(&input, RepairCompletion::ProjectFault);
        service.admit_repair(&candidate, &attempt, &input).unwrap();
        service
            .complete_repair(&candidate, &attempt, &outcome)
            .unwrap();
        let RepairJudgment::ProjectFault { fault } = outcome else {
            panic!("fault")
        };
        let returned = service
            .observe(&candidate, &fault, &attempt)
            .unwrap()
            .unwrap();
        assert_eq!(returned.state.work.len(), usize::from(n) + 1);
        assert_eq!(
            service
                .observe(&candidate, &fault, &attempt)
                .unwrap()
                .unwrap(),
            returned
        );
        let work = returned.state.work.last().unwrap();
        assert_eq!(work.story, StoryNo::new(1));
        if n < 3 {
            assert_eq!(work.status, WorkStatus::Pending);
            service
                .claim_work(&view.record.id, &work.id)
                .unwrap()
                .unwrap();
            service
                .settle_work(&view.record.id, &work.id, 1, AssessmentDelivery::Delivered)
                .unwrap();
        } else {
            assert_eq!(work.status, WorkStatus::Held);
            assert!(
                service
                    .claim_work(&view.record.id, &work.id)
                    .unwrap()
                    .is_none()
            );
            assert!(
                f.store()
                    .read(|tx| tx.story(f.project(), work.story))
                    .unwrap()
                    .unwrap()
                    .awaiting
                    .as_deref()
                    .is_some_and(|s| s.contains("three changed repair submissions"))
            );
        }
    }
}
