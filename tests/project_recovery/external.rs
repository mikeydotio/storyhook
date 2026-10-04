//! External-scope recoveries: owned prerequisite holds and their release (SH-849).
use super::*;
use storyhook::service::project_recovery::{RecoveryView, RepairScope};

/// SH-1 faulted and its assessor chose External scope.
pub(super) fn decided(f: &ServiceFixture) -> RecoveryView {
    let ready = decision::ready(f);
    let ctx = f.ctx();
    ProjectRecoveryService::new(&ctx)
        .decide(
            &ready.record.id,
            &decision::input(&ready, RepairScope::External),
        )
        .unwrap()
}

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
fn an_external_decision_owns_its_prerequisite_hold_by_exact_event() {
    let f = fixture();
    let view = decided(&f);
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
fn a_fault_that_joins_an_unsatisfied_external_recovery_gets_an_owned_hold() {
    let f = fixture();
    let view = decided(&f);
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
    let holds = &after.state.decision.as_ref().unwrap().dependency_holds;
    assert_eq!(holds.len(), 2, "{holds:?}");
    let (event, awaiting) = latest_awaiting(&f, StoryNo::new(2));
    assert!(
        holds
            .iter()
            .any(|h| h.story == StoryNo::new(2) && h.event == event && h.awaiting == awaiting)
    );
    assert_eq!(service.show(&view.record.id).unwrap(), after);
}
