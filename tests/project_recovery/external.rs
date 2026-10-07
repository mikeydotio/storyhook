//! External-scope recoveries: owned prerequisite holds and their release (SH-849).
use super::*;
use storyhook::service::project_recovery::{RecoveryView, RepairScope};

/// The latest awaiting write on a story, as `(event, text)`.
fn latest_awaiting(f: &ServiceFixture, story: StoryNo) -> (storyhook::store::GlobalSeq, String) {
    f.store()
        .read(|tx| tx.events_for(f.project(), story))
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|event| match event.known() {
            Some(storyhook::domain::StoryEvent::StoryAwaitingSet { awaiting, .. }) => {
                Some((event.global_seq, awaiting.clone()))
            }
            _ => None,
        })
        .expect("an awaiting write")
}

#[test]
fn sh870_retained_an_external_decision_owns_its_prerequisite_hold_by_exact_event() {
    let f = fixture();
    let view = legacy::accepted(&f, RepairScope::External);
    let receipt = view.state.decision.as_ref().unwrap();
    assert_eq!(receipt.dependency_holds.len(), 1, "{receipt:?}");
    let hold = &receipt.dependency_holds[0];
    let (event, awaiting) = latest_awaiting(&f, StoryNo::new(1));
    assert_eq!(hold.story, StoryNo::new(1));
    assert_eq!(hold.generation, view.state.assessment.generation);
    assert_eq!(hold.event, event);
    assert_eq!(hold.awaiting, awaiting);
    assert!(
        awaiting.contains("restore access to the signing service"),
        "{awaiting}"
    );
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    assert_eq!(service.show(&view.record.id).unwrap(), view);
    // A hold that names another event is not this recovery's.
    f.store()
        .write(|tx| {
            let mut record = tx.project_recoveries(f.project())?.remove(0);
            let revision = record.revision;
            record.state["decision"]["dependency_holds"][0]["event"] =
                serde_json::to_value(hold.generation).unwrap();
            record.revision += 1;
            assert!(tx.update_project_recovery(&record, revision)?);
            Ok(())
        })
        .unwrap();
    assert!(
        service
            .show(&view.record.id)
            .unwrap_err()
            .to_string()
            .contains("event ownership")
    );
}

#[test]
fn sh870_retained_raw_fault_preserves_external_holds_without_enrolling_a_new_subject() {
    let f = fixture();
    let view = legacy::accepted(&f, RepairScope::External);
    let joined = submitted(&f, "same fault after the decision");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let after = service
        .observe(&joined, &fault(), "joined-attempt")
        .unwrap()
        .unwrap();
    assert_eq!(
        after.record.id, view.record.id,
        "an unsatisfied record still owns the fault"
    );
    let receipt = after.state.decision.as_ref().unwrap();
    assert_eq!(
        receipt.dependency_holds,
        view.state.decision.as_ref().unwrap().dependency_holds
    );
    assert_eq!(receipt.skipped_subjects, vec![StoryNo::new(2)]);
    assert!(!after.state.subjects.last().unwrap().returned);
    assert_eq!(awaiting(&f, 2), None);
    assert_eq!(
        f.store()
            .read(|tx| tx.story(f.project(), StoryNo::new(2)))
            .unwrap()
            .unwrap()
            .state,
        "verifying"
    );
    assert_eq!(service.show(&view.record.id).unwrap(), after);
}

// ---- Satisfying the prerequisite (SH-849, council decision D1) ----

use std::sync::atomic::{AtomicBool, AtomicUsize};
use storyhook::cli::{Invocation, VerifierAction, parse_invocation};
use storyhook::daemon::project_recovery::process_one;
use storyhook::daemon::verification::{TickResult, VerificationActivity, tick_with_activity};
use storyhook::invoke::dispatch;
use storyhook::output::render_response;
use storyhook::service::project_recovery::{
    PrerequisiteInput, RecoveryStatus, RepairInput, WorkKind,
};

/// A complete operator statement for the record as it reads now.
fn statement(view: &RecoveryView) -> PrerequisiteInput {
    PrerequisiteInput {
        version: 1,
        revision: view.record.revision,
        context: "The signing service rejected the gate's certificate request.".into(),
        question: "Is the signing service reachable again?".into(),
        decision: "Yes: access to the signing service is restored.".into(),
        rationale: "The owner renewed the credential; a manual signature now succeeds.".into(),
        evidence: vec!["codesign --verify succeeded at 2026-01-01T00:05:00Z".into()],
    }
}

/// The status row of one recovery, or `None` once the snapshot leaves it out.
fn current(f: &ServiceFixture, id: &str) -> Option<RecoveryStatus> {
    let ctx = f.ctx();
    VerificationActivity::new()
        .status(&ctx)
        .unwrap()
        .project_recoveries
        .into_iter()
        .find(|row| row.id == id)
}

fn awaiting(f: &ServiceFixture, story: i64) -> Option<String> {
    f.store()
        .read(|tx| tx.story(f.project(), StoryNo::new(story)))
        .unwrap()
        .unwrap()
        .awaiting
}

fn event_count(f: &ServiceFixture, story: i64) -> usize {
    f.store()
        .read(|tx| tx.events_for(f.project(), StoryNo::new(story)))
        .unwrap()
        .len()
}

/// Run `story verifier repair satisfy <id> --input <file>` through the CLI
/// parser and the shared dispatch, as the daemon does.
fn satisfy_via_cli(
    f: &ServiceFixture,
    id: &str,
    input: &PrerequisiteInput,
) -> Result<serde_json::Value, storyhook::error::AppError> {
    let file = f.cwd().join("satisfied.json");
    std::fs::write(&file, serde_json::to_vec(input).unwrap()).unwrap();
    let args: Vec<String> = [
        "verifier",
        "repair",
        "satisfy",
        id,
        "--input",
        file.to_str().unwrap(),
    ]
    .map(str::to_owned)
    .into();
    let ctx = f.ctx();
    let response = dispatch(&ctx, parse_invocation(&args).unwrap())?;
    Ok(serde_json::from_str(&render_response(&response, true, false)).unwrap())
}

/// Deliver every pending resume effect of a recovery, as the worker does.
fn deliver_resumes(f: &ServiceFixture, id: &str) {
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let effects: Vec<String> = service
        .show(id)
        .unwrap()
        .state
        .work
        .into_iter()
        .filter(|w| w.kind == WorkKind::Resume)
        .map(|w| w.id)
        .collect();
    assert!(!effects.is_empty());
    for effect in effects {
        let claimed = service.claim_work(id, &effect).unwrap().unwrap();
        let epoch = claimed
            .state
            .work
            .iter()
            .find(|w| w.id == effect)
            .unwrap()
            .epoch;
        service
            .settle_work(id, &effect, epoch, AssessmentDelivery::Delivered)
            .unwrap();
    }
}

#[test]
fn sh870_retained_an_unsatisfied_external_recovery_asks_the_operator_and_releases_nothing() {
    let f = fixture();
    let view = legacy::accepted(&f, RepairScope::External);
    let id = view.record.id.clone();
    let row = current(&f, &id).expect("an unsatisfied prerequisite is open work");
    assert_eq!(row.phase, "external-prerequisite");
    for words in [
        "restore access to the signing service",
        "An operator, not an agent",
        &format!("story verifier repair satisfy {id} --input <json-file>"),
        "attestation, not a check",
    ] {
        assert!(row.next_action.contains(words), "{words}: {row:?}");
    }
    // The decided External card names its scope, never an undecided repair.
    assert_eq!(row.scope, Some(RepairScope::External));
    assert_eq!(row.repair_story, None);
    let rendered = VerificationActivity::new()
        .status(&f.ctx())
        .unwrap()
        .render_human();
    assert!(rendered.contains("repair none (external)"), "{rendered}");
    assert!(!rendered.contains("undecided"), "{rendered}");
    let held = awaiting(&f, 1).expect("the prerequisite hold");
    let activity = VerificationActivity::new();
    let stop = AtomicBool::new(false);
    let delivery = worker::helper(&f, r#"{"ok":true}"#);
    assert!(!process_one(f.store(), f.env(), &delivery, &activity, &stop).unwrap());
    assert_eq!(awaiting(&f, 1), Some(held));
    // Clearing the hold by hand and resubmitting is not a satisfaction: the
    // record still governs the fault, and the card still asks the operator.
    let ctx = f.ctx();
    let stories = StoryService::new(&ctx);
    stories.clear_awaiting("SH-1").unwrap();
    stories
        .set_state("SH-1", "verifying", None, None, None)
        .unwrap();
    let after = ProjectRecoveryService::new(&ctx).show(&id).unwrap();
    assert!(after.record.active);
    assert!(after.state.prerequisite.is_none());
    assert_eq!(
        current(&f, &id).map(|row| row.phase).as_deref(),
        Some("external-prerequisite")
    );
}

/// Acceptance 1, through the production verifier tick, the recovery worker
/// and the CLI door.
#[test]
fn sh870_retained_satisfying_the_prerequisite_retires_the_recovery_through_fresh_verification() {
    let f = fixture();
    let original = submitted(&f, "external prerequisite flow");
    let activity = VerificationActivity::new();
    let inflight = storyhook::daemon::lifecycle::InFlight::new(f.env().clone());
    let mut gate = queue::GateEndpoint {
        store: f.store(),
        env: f.env(),
        activity: &activity,
        input: RepairInput {
            base: "a".repeat(40),
            head: "b".repeat(40),
            head_tree: "d".repeat(40),
            tree: "c".repeat(40),
        },
        mismatch: false,
        fail_tests: false,
        project_fault: true,
        executions: AtomicUsize::new(0),
    };
    let tick = |gate: &queue::GateEndpoint<'_>| {
        tick_with_activity(f.store(), f.env(), gate, &activity, &inflight, f.project()).unwrap()
    };
    assert_eq!(tick(&gate), TickResult::Returned);
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let id = f
        .store()
        .read(|tx| tx.project_recoveries(f.project()))
        .unwrap()[0]
        .id
        .clone();
    let delivery = worker::helper(&f, r#"{"ok":true}"#);
    let stop = AtomicBool::new(false);
    let work = || process_one(f.store(), f.env(), &delivery, &activity, &stop).unwrap();
    assert!(!work(), "raw faults cannot deliver an assessment charter");
    let observed = service.show(&id).unwrap();
    assert_eq!(
        observed.state.assessment.hold,
        Some(AssessmentHold::CauseUnproved)
    );
    legacy::retain(&f, observed, RepairScope::External);
    assert_eq!(
        current(&f, &id).map(|row| row.phase).as_deref(),
        Some("external-prerequisite")
    );
    assert!(!work(), "nothing is released before the statement");

    let shown = service.show(&id).unwrap();
    let payload = satisfy_via_cli(&f, &id, &statement(&shown)).unwrap();
    assert_eq!(payload["recovery"]["record"]["active"], false);
    assert!(payload["recovery"]["state"]["prerequisite"].is_object());
    let held = current(&f, &id).expect("its own hold still holds SH-1");
    assert_eq!(held.phase, "resume-held");
    assert!(
        held.next_action.contains("External prerequisite satisfied"),
        "{held:?}"
    );
    assert!(held.next_action.contains("SH-1"), "{held:?}");

    assert!(work(), "the worker releases the hold it owns");
    assert_eq!(awaiting(&f, 1), None);
    assert_eq!(
        current(&f, &id).map(|row| row.phase).as_deref(),
        Some("resume-pending")
    );
    let comments = f
        .store()
        .read(|tx| tx.story(f.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap()
        .snapshot
        .comments;
    assert!(comments.iter().any(|c| {
        c.text
            .contains("an operator declared the external prerequisite satisfied. Managed resume")
    }));
    assert!(work(), "the worker delivers the managed resume");
    let owed = current(&f, &id).expect("SH-1 still owes a fresh generation");
    assert_eq!(owed.phase, "prerequisite-satisfied");
    assert!(owed.next_action.contains("SH-1"), "{owed:?}");
    let checkout = f
        .store()
        .read(|tx| tx.checkout_path(f.project()))
        .unwrap()
        .unwrap();
    let calls = std::fs::read_to_string(checkout.join("recovery-calls")).unwrap();
    assert!(calls.contains("An operator declared the external prerequisite satisfied."));
    assert!(!calls.contains("Certified repair landed"));

    StoryService::new(&ctx)
        .set_state("SH-1", "verifying", None, None, None)
        .unwrap();
    assert_eq!(current(&f, &id), None, "a fresh generation discharges SH-1");
    let refreshed = VerificationQueue::new(f.store()).next().unwrap().unwrap();
    assert_ne!(
        refreshed.verifying_generation,
        original.verifying_generation
    );
    gate.project_fault = false;
    gate.input.head = "e".repeat(40);
    gate.input.tree = "f".repeat(40);
    assert_eq!(tick(&gate), TickResult::Completed);
    assert_eq!(
        f.store()
            .read(|tx| tx.story(f.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .state,
        "done"
    );
    assert!(!work());
    assert_eq!(current(&f, &id), None, "nothing is owed");
    let response = dispatch(
        &ctx,
        Invocation::Verifier {
            action: VerifierAction::RepairShow {
                recovery_id: id.clone(),
            },
        },
    )
    .unwrap();
    let shown: serde_json::Value =
        serde_json::from_str(&render_response(&response, true, false)).unwrap();
    let record = &shown["recovery"];
    assert_eq!(record["record"]["active"], false);
    assert!(record["state"]["prerequisite"].is_object());
    assert!(record["state"].get("landing").is_none_or(|v| v.is_null()));
    let resumes = record["state"]["work"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|w| w["kind"] == "resume")
        .count();
    assert_eq!(resumes, 1);
}

/// SH-1 and SH-2 hit the same fault before the decision.
fn two_affected(f: &ServiceFixture) -> RecoveryView {
    let first = decision::ready(f);
    let second = submitted(f, "second affected submission");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    service
        .observe(&second, &fault(), "second-attempt")
        .unwrap()
        .unwrap();
    let joined = service.show(&first.record.id).unwrap();
    legacy::retain(f, joined, RepairScope::External)
}

#[test]
fn sh870_retained_a_satisfied_recovery_names_only_owed_stories_and_leaves_once_none_owes() {
    for discharge in ["resubmit", "drop"] {
        let f = fixture();
        let view = two_affected(&f);
        let id = view.record.id.clone();
        assert_eq!(
            view.state.decision.as_ref().unwrap().dependency_holds.len(),
            2
        );
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        service.satisfy(&id, &statement(&view)).unwrap();
        let held = current(&f, &id).expect("both holds remain until released");
        assert_eq!(held.phase, "resume-held", "{discharge}");
        assert!(held.next_action.contains("SH-1, SH-2"), "{held:?}");
        service.reconcile_landing(&id).unwrap();
        assert_eq!(
            current(&f, &id).map(|row| row.phase).as_deref(),
            Some("resume-pending")
        );
        deliver_resumes(&f, &id);
        let owed = current(&f, &id).expect("resumed stories still owe a generation");
        assert_eq!(owed.phase, "prerequisite-satisfied");
        assert!(owed.next_action.contains("SH-1, SH-2"), "{owed:?}");
        let stories = StoryService::new(&ctx);
        stories
            .set_state("SH-1", "verifying", None, None, None)
            .unwrap();
        let one = current(&f, &id).expect("SH-2 still owes a generation");
        assert!(one.next_action.contains("SH-2"), "{one:?}");
        assert!(!one.next_action.contains("SH-1"), "{one:?}");
        match discharge {
            "resubmit" => stories
                .set_state("SH-2", "verifying", None, None, None)
                .unwrap(),
            _ => stories
                .set_state("SH-2", "dropped", Some("abandoned"), None, None)
                .unwrap(),
        };
        assert_eq!(current(&f, &id), None, "{discharge}");
        if discharge == "drop" {
            stories.reopen("SH-2").unwrap();
            assert_eq!(current(&f, &id), None, "a reopen cannot revive it");
        }
        assert!(!service.show(&id).unwrap().record.active);
    }
}

/// Acceptance 2.
#[test]
fn sh870_retained_a_fault_after_satisfaction_opens_a_new_recovery_without_the_old_hold() {
    let f = fixture();
    let view = legacy::accepted(&f, RepairScope::External);
    let old = view.record.id.clone();
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let satisfied = service.satisfy(&old, &statement(&view)).unwrap();
    let later = submitted(&f, "the same fault, later");
    let opened = service
        .observe(&later, &fault(), "after-satisfaction")
        .unwrap()
        .unwrap();
    assert_ne!(opened.record.id, old);
    assert!(opened.record.active);
    assert!(opened.state.decision.is_none());
    assert_eq!(opened.state.assessment.status, AssessmentStatus::Held);
    assert_eq!(
        opened.state.assessment.hold,
        Some(AssessmentHold::CauseUnproved)
    );
    assert_eq!(opened.state.supersedes.as_deref(), Some(old.as_str()));
    assert!(
        storyhook::service::project_recovery::assessment_charter(&opened)
            .contains(&format!("retired recovery {old}"))
    );
    assert!(
        awaiting(&f, 2).is_none_or(|text| !text.contains(&old)),
        "the old prerequisite hold is not inherited"
    );
    assert_eq!(
        service.show(&old).unwrap(),
        satisfied,
        "the old record is unchanged"
    );
    assert_eq!(
        f.store()
            .read(|tx| tx.project_recoveries(f.project()))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn sh870_retained_satisfy_accepts_one_exact_statement_and_refuses_the_rest() {
    let refused = |f: &ServiceFixture, id: &str, input: &PrerequisiteInput, words: &str| {
        let ctx = f.ctx();
        let service = ProjectRecoveryService::new(&ctx);
        let before = service.show(id).ok();
        let events = event_count(f, 1);
        let error = service.satisfy(id, input).unwrap_err().to_string();
        assert!(error.contains(words), "{words}: {error}");
        assert_eq!(service.show(id).ok(), before, "no partial write");
        assert_eq!(event_count(f, 1), events, "no partial write");
    };
    // Scopes other than External, and an undecided record.
    for scope in [RepairScope::SameStory, RepairScope::SeparateStory] {
        let f = fixture();
        let ready = decision::ready(&f);
        let view = legacy::retain(&f, ready, scope);
        refused(
            &f,
            &view.record.id,
            &statement(&view),
            "no external-scope decision",
        );
    }
    let f = fixture();
    let ready = decision::ready(&f);
    refused(
        &f,
        &ready.record.id,
        &statement(&ready),
        "no external-scope decision",
    );

    let f = fixture();
    let view = legacy::accepted(&f, RepairScope::External);
    let id = view.record.id.clone();
    refused(&f, "no-such-recovery", &statement(&view), "does not exist");
    let invalid: Vec<PrerequisiteInput> = vec![
        PrerequisiteInput {
            version: 2,
            ..statement(&view)
        },
        PrerequisiteInput {
            revision: -1,
            ..statement(&view)
        },
        PrerequisiteInput {
            context: " ".into(),
            ..statement(&view)
        },
        PrerequisiteInput {
            question: String::new(),
            ..statement(&view)
        },
        PrerequisiteInput {
            decision: "\n".into(),
            ..statement(&view)
        },
        PrerequisiteInput {
            rationale: " ".into(),
            ..statement(&view)
        },
        PrerequisiteInput {
            evidence: Vec::new(),
            ..statement(&view)
        },
        PrerequisiteInput {
            evidence: vec![" ".into()],
            ..statement(&view)
        },
    ];
    for input in &invalid {
        refused(&f, &id, input, "invalid prerequisite statement");
    }
    refused(
        &f,
        &id,
        &PrerequisiteInput {
            revision: view.record.revision - 1,
            ..statement(&view)
        },
        "Read `story verifier repair show",
    );
    // Another affected story joins after the operator read the record.
    let joined = submitted(&f, "joined after the read");
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    service
        .observe(&joined, &fault(), "joined-after-read")
        .unwrap()
        .unwrap();
    refused(&f, &id, &statement(&view), "it changed after it was read");

    let current_view = service.show(&id).unwrap();
    let input = statement(&current_view);
    let accepted = service.satisfy(&id, &input).unwrap();
    assert!(!accepted.record.active);
    let receipt = accepted.state.prerequisite.as_ref().unwrap();
    assert_eq!(receipt.input, input);
    assert_eq!(receipt.story, StoryNo::new(1));
    let events = event_count(&f, 1);
    assert_eq!(
        service.satisfy(&id, &input).unwrap(),
        accepted,
        "exact replay"
    );
    assert_eq!(event_count(&f, 1), events);
    let mut conflicting = input.clone();
    conflicting.rationale.push_str(" Changed.");
    refused(&f, &id, &conflicting, "different prerequisite statement");
}

#[test]
fn sh870_retained_a_dispatched_agent_session_cannot_satisfy_a_prerequisite() {
    use storyhook::api::wire::ProjectSelector;
    use storyhook::invoke::{InvokeRequest, Invoker, StoreInvoker};
    let f = fixture();
    let view = legacy::accepted(&f, RepairScope::External);
    let id = view.record.id.clone();
    let file = f.cwd().join("satisfied.json");
    std::fs::write(&file, serde_json::to_vec(&statement(&view)).unwrap()).unwrap();
    let invocation = parse_invocation(
        &[
            "verifier",
            "repair",
            "satisfy",
            &id,
            "--input",
            file.to_str().unwrap(),
        ]
        .map(str::to_owned),
    )
    .unwrap();
    let request =
        InvokeRequest::new(invocation)
            .no_hooks(true)
            .project(Some(ProjectSelector::Flag {
                slug: "fixture".into(),
            }));
    let invoker = StoreInvoker::new(f.store(), f.cwd(), f.env().clone());
    let error = invoker
        .invoke(request.clone().agent_session(true))
        .unwrap_err()
        .to_string();
    assert!(error.contains("dispatched agent session"), "{error}");
    assert!(
        error.contains("report the prerequisite to the operator"),
        "{error}"
    );
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    assert_eq!(service.show(&id).unwrap(), view, "nothing changed");
    invoker.invoke(request).unwrap();
    assert!(!service.show(&id).unwrap().record.active);
}

#[test]
fn sh870_retained_satisfaction_releases_only_the_holds_the_recovery_still_owns() {
    // A reservation added to SH-2 revokes the recovery's authority to clear
    // its hold; SH-1 is released.
    let f = fixture();
    let view = two_affected(&f);
    let id = view.record.id.clone();
    let ctx = f.ctx();
    StoryService::new(&ctx)
        .set_labels("SH-2", &["no-auto".into()], &[])
        .unwrap();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service.show(&id).unwrap();
    service.satisfy(&id, &statement(&view)).unwrap();
    service.reconcile_landing(&id).unwrap();
    assert_eq!(awaiting(&f, 1), None);
    assert!(awaiting(&f, 2).is_some_and(|text| text.contains(&id)));
    deliver_resumes(&f, &id);
    let row = current(&f, &id).expect("SH-2 is still held by the recovery");
    assert_eq!(row.phase, "resume-held");
    assert!(row.next_action.contains("SH-2"), "{row:?}");

    // An operator's own hold with the same story is never cleared.
    let f = fixture();
    let view = legacy::accepted(&f, RepairScope::External);
    let id = view.record.id.clone();
    let ctx = f.ctx();
    StoryService::new(&ctx)
        .set_awaiting("SH-1", "operator hold")
        .unwrap();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service.show(&id).unwrap();
    service.satisfy(&id, &statement(&view)).unwrap();
    service.reconcile_landing(&id).unwrap();
    assert_eq!(awaiting(&f, 1).as_deref(), Some("operator hold"));
    assert!(
        service
            .show(&id)
            .unwrap()
            .state
            .work
            .iter()
            .all(|w| w.kind != WorkKind::Resume)
    );
    let row = current(&f, &id).expect("SH-1 still owes its generation");
    assert_eq!(row.phase, "prerequisite-satisfied");
}

#[test]
fn sh870_retained_satisfy_records_the_statement_after_the_assessment_story_closed() {
    let f = fixture();
    let view = two_affected(&f);
    let id = view.record.id.clone();
    let ctx = f.ctx();
    StoryService::new(&ctx)
        .set_state("SH-1", "dropped", Some("abandoned"), None, None)
        .unwrap();
    let service = ProjectRecoveryService::new(&ctx);
    let view = service.show(&id).unwrap();
    let satisfied = service.satisfy(&id, &statement(&view)).unwrap();
    assert_eq!(
        satisfied.state.prerequisite.as_ref().unwrap().story,
        StoryNo::new(1)
    );
    service.reconcile_landing(&id).unwrap();
    assert_eq!(awaiting(&f, 2), None, "SH-2 is released");
}

#[test]
fn sh870_retained_satisfaction_survives_restart_and_releases_each_hold_once() {
    let f = fixture();
    let view = two_affected(&f);
    let id = view.record.id.clone();
    {
        let ctx = f.ctx();
        ProjectRecoveryService::new(&ctx)
            .satisfy(&id, &statement(&view))
            .unwrap();
    }
    let reopened = storyhook::store::SqliteStore::open(f.store().path()).unwrap();
    let ctx = storyhook::service::Ctx::new(&reopened, f.project(), f.cwd(), f.env().clone());
    let service = ProjectRecoveryService::new(&ctx);
    assert!(service.landing_release_ready(&id).unwrap());
    let first = service.reconcile_landing(&id).unwrap();
    assert!(!service.landing_release_ready(&id).unwrap());
    assert_eq!(service.reconcile_landing(&id).unwrap(), first);
    let resumes: Vec<_> = first
        .state
        .work
        .iter()
        .filter(|w| w.kind == WorkKind::Resume)
        .map(|w| w.story)
        .collect();
    assert_eq!(resumes, vec![StoryNo::new(1), StoryNo::new(2)]);
}

/// Rewrite one retained record and expect strict reads to refuse it.
fn assert_corrupt(
    f: &ServiceFixture,
    id: &str,
    edit: impl Fn(&mut storyhook::store::ProjectRecovery),
) {
    f.store()
        .write(|tx| {
            let mut record = tx
                .project_recoveries(f.project())?
                .into_iter()
                .find(|r| r.id == id)
                .unwrap();
            let revision = record.revision;
            record.revision += 1;
            edit(&mut record);
            assert!(tx.update_project_recovery(&record, revision)?);
            Ok(())
        })
        .unwrap();
    let ctx = f.ctx();
    let error = ProjectRecoveryService::new(&ctx)
        .show(id)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("damaged")
            || error.contains("inconsistent")
            || error.contains("disagrees")
            || error.contains("supersedes"),
        "{error}"
    );
    assert_eq!(
        current(f, id).map(|row| row.phase).as_deref(),
        Some("invalid")
    );
}

#[test]
fn sh870_retained_inconsistent_prerequisite_statements_read_as_damaged() {
    let satisfied = || {
        let f = fixture();
        let view = legacy::accepted(&f, RepairScope::External);
        let ctx = f.ctx();
        let view = ProjectRecoveryService::new(&ctx)
            .satisfy(&view.record.id, &statement(&view))
            .unwrap();
        (f, view)
    };
    // The statement must follow every hold the decision wrote.
    let (f, view) = satisfied();
    let hold = view.state.decision.as_ref().unwrap().dependency_holds[0].event;
    assert_corrupt(&f, &view.record.id, |r| {
        r.state["prerequisite"]["event"] = serde_json::to_value(hold).unwrap();
    });
    // A satisfied record cannot be active.
    let (f, view) = satisfied();
    assert_corrupt(&f, &view.record.id, |r| r.active = true);
    // The statement must precede the revision that recorded it.
    let (f, view) = satisfied();
    assert_corrupt(&f, &view.record.id, |r| {
        r.state["prerequisite"]["input"]["revision"] = serde_json::json!(r.revision);
    });
    // The statement lives on the assessment story.
    let (f, view) = satisfied();
    assert_corrupt(&f, &view.record.id, |r| {
        r.state["prerequisite"]["story"] = serde_json::json!(2);
    });
    // Its text must match the recorded comment.
    let (f, view) = satisfied();
    assert_corrupt(&f, &view.record.id, |r| {
        r.state["prerequisite"]["input"]["evidence"] = serde_json::json!(["something else"]);
    });
    // Only External scope has a prerequisite to satisfy.
    let (f, view) = satisfied();
    assert_corrupt(&f, &view.record.id, |r| {
        r.state["decision"]["input"]["scope"] = serde_json::json!("same-story");
    });
    // A retired record without a release authority is damage, too.
    let f = fixture();
    let view = legacy::accepted(&f, RepairScope::External);
    assert_corrupt(&f, &view.record.id, |r| r.active = false);
    // A release must follow the statement.
    let (f, view) = satisfied();
    let ctx = f.ctx();
    let released = ProjectRecoveryService::new(&ctx)
        .reconcile_landing(&view.record.id)
        .unwrap();
    let anchor = released.state.prerequisite.as_ref().unwrap().event;
    assert_corrupt(&f, &view.record.id, |r| {
        let work = r.state["work"].as_array_mut().unwrap();
        let resume = work.iter_mut().find(|w| w["kind"] == "resume").unwrap();
        resume["release_event"] = serde_json::to_value(anchor).unwrap();
    });
}

#[test]
fn sh870_retained_a_supersedes_link_must_name_a_retired_recovery_with_the_same_fault() {
    let f = fixture();
    let view = legacy::accepted(&f, RepairScope::External);
    let old = view.record.id.clone();
    let ctx = f.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    service.satisfy(&old, &statement(&view)).unwrap();
    let later = submitted(&f, "recurrence");
    let opened = service
        .observe(&later, &fault(), "recurrence")
        .unwrap()
        .unwrap();
    let new = opened.record.id.clone();
    assert_eq!(opened.state.supersedes.as_deref(), Some(old.as_str()));
    assert_eq!(service.show(&new).unwrap(), opened);
    assert_corrupt(&f, &new, |r| {
        r.state["supersedes"] = serde_json::json!("no-such-recovery");
    });
    let f = fixture();
    let view = legacy::accepted(&f, RepairScope::External);
    let id = view.record.id.clone();
    assert_corrupt(&f, &id, |r| {
        r.state["supersedes"] = serde_json::json!(r.id.clone());
    });
}

#[test]
fn the_satisfy_door_parses_strictly() {
    for suffix in [
        vec!["satisfy", "id"],
        vec!["satisfy", "id", "--input"],
        vec!["satisfy", "id", "--input", "x", "extra"],
        vec!["satisfy", "id", "--file", "x"],
        vec!["satisfy", "--input", "x"],
    ] {
        let args = [vec!["verifier", "repair"], suffix.clone()]
            .concat()
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert!(parse_invocation(&args).is_err(), "{suffix:?}");
    }
    let args: Vec<String> = ["verifier", "repair", "satisfy", "r-1", "--input", "s.json"]
        .map(str::to_owned)
        .into();
    assert_eq!(
        parse_invocation(&args).unwrap(),
        Invocation::Verifier {
            action: VerifierAction::RepairSatisfy {
                recovery_id: "r-1".into(),
                input: "s.json".into(),
            },
        }
    );
}
