//! Administrative failure is not evidence that the submitted change is defective.
use super::*;

#[test]
fn administrative_hold_survives_restart_and_does_not_follow_a_new_generation() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    let id = submitted(&fixture, "uncertain input", Priority::High, PR_ONE);
    let actuator = FakeActuator::new(VerificationOutcome::InvalidSubmission {
        detail: "origin does not match the registered checkout".into(),
    });
    tick_with(fixture.store(), fixture.env(), &actuator, fixture.project()).unwrap();
    assert_eq!(story_row(&fixture, &id).state, "verifying");
    assert!(actuator.notified.lock().unwrap().is_empty());
    let reopened = SqliteStore::open(fixture.store().path()).unwrap();
    assert!(VerificationQueue::new(&reopened).next().unwrap().is_none());
    let records = reopened
        .read(|tx| tx.attributions(fixture.project()))
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].components[0].observed_cause,
        storyhook::service::attribution::FailureCause::Unknown
    );
    assert!(records[0].held);
    assert!(records[0].probes.is_empty());
    let attempts = reopened
        .read(|tx| tx.gate_attempts(fixture.project()))
        .unwrap();
    assert_eq!(records[0].attempt, attempts[0].id);
    assert_eq!(attempts[0].verdict.as_deref(), Some("invalid-submission"));
    let status = VerificationActivity::new().status(&fixture.ctx()).unwrap();
    assert_eq!(
        status.warning, None,
        "a held generation owes no gate progress"
    );
    assert!(status.verifying.is_empty());
    assert!(status.render_human().contains(&records[0].id));
    let status = serde_json::to_value(status).unwrap();
    let mut legacy = status.clone();
    legacy.as_object_mut().unwrap().remove("attribution_holds");
    let legacy: storyhook::daemon::verification::status::VerifierStatus =
        serde_json::from_value(legacy).unwrap();
    assert!(legacy.attribution_holds.is_empty());
    assert_eq!(status["attribution_holds"][0]["evidence_id"], records[0].id);
    assert_eq!(status["attribution_holds"][0]["cause"], "unknown");
    assert!(
        status["attribution_holds"][0]["next_action"]
            .as_str()
            .unwrap()
            .contains("evidence")
    );
    assert_eq!(
        tick_with(&reopened, fixture.env(), &actuator, fixture.project()).unwrap(),
        TickResult::Idle
    );
    assert_eq!(
        reopened
            .read(|tx| tx.gate_attempts(fixture.project()))
            .unwrap(),
        attempts
    );
    StoryService::new(&fixture.ctx())
        .set_state(&id, "in-progress", None, None, None)
        .unwrap();
    StoryService::new(&fixture.ctx())
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    assert!(VerificationQueue::new(&reopened).next().unwrap().is_some());
    assert_eq!(
        reopened
            .read(|tx| tx.attributions(fixture.project()))
            .unwrap(),
        records
    );
    let status =
        serde_json::to_value(VerificationActivity::new().status(&fixture.ctx()).unwrap()).unwrap();
    assert_eq!(status["attribution_holds"], serde_json::json!([]));
}

#[test]
fn an_ambiguous_link_set_is_held_without_assigning_a_repair() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    let id = submitted(&fixture, "ambiguous", Priority::High, PR_ONE);
    PrLinkService::new(&fixture.ctx())
        .link(&id, PR_TWO, true)
        .unwrap();
    let actuator = FakeActuator::new(VerificationOutcome::InvalidSubmission {
        detail: "must not run".into(),
    });
    tick_with(fixture.store(), fixture.env(), &actuator, fixture.project()).unwrap();
    assert_eq!(story_row(&fixture, &id).state, "verifying");
    assert!(actuator.notified.lock().unwrap().is_empty());
    assert!(
        VerificationQueue::new(fixture.store())
            .next()
            .unwrap()
            .is_none()
    );
    let comments = story_row(&fixture, &id).snapshot.comments;
    assert!(
        comments
            .iter()
            .any(|c| c.text.contains(PR_ONE) && c.text.contains(PR_TWO))
    );
}

#[test]
fn held_status_counts_the_allowance_across_retired_attempts() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    let id = submitted(&fixture, "exhausted diagnosis", Priority::High, PR_ONE);
    let actuator = FakeActuator::new(VerificationOutcome::InvalidSubmission {
        detail: "unknown input".into(),
    });
    tick_with(fixture.store(), fixture.env(), &actuator, fixture.project()).unwrap();
    let mut first = fixture
        .store()
        .read(|tx| tx.attributions(fixture.project()))
        .unwrap()
        .remove(0);
    let mut retry = first.clone();
    retry.id = "later-diagnosis".into();
    retry.attempt = "later-attempt".into();
    first.revision += 1;
    first.diagnosis_ms = storyhook::service::attribution::MAX_DIAGNOSIS_MS;
    first.held = false;
    first.retired = Some("later attempt retains the same submission budget".into());
    fixture
        .store()
        .write(|tx| {
            assert!(tx.update_attribution(&first, 0)?);
            tx.insert_attribution(&retry)
        })
        .unwrap();
    let status =
        serde_json::to_value(VerificationActivity::new().status(&fixture.ctx()).unwrap()).unwrap();
    assert_eq!(status["attribution_holds"][0]["story_id"], id);
    assert_eq!(
        status["attribution_holds"][0]["evidence_id"],
        "later-diagnosis"
    );
    assert_eq!(
        status["attribution_holds"][0]["diagnosis"],
        "diagnosis allowance exhausted"
    );
}
