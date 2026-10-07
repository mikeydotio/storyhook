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

#[test]
fn prefix_rename_keeps_the_hold_and_current_status_without_rewriting_evidence() {
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    submitted(&fixture, "held across rename", Priority::High, PR_ONE);
    let actuator = FakeActuator::new(VerificationOutcome::InvalidSubmission {
        detail: "unknown input".into(),
    });
    tick_with(fixture.store(), fixture.env(), &actuator, fixture.project()).unwrap();
    let mut first = fixture
        .store()
        .read(|tx| tx.attributions(fixture.project()))
        .unwrap()
        .remove(0);
    first.revision = 1;
    first.diagnosis_ms = storyhook::service::attribution::MAX_DIAGNOSIS_MS;
    fixture
        .store()
        .write(|tx| tx.update_attribution(&first, 0))
        .unwrap();
    storyhook::service::ProjectService::new(fixture.store(), fixture.cwd())
        .set_prefix(
            fixture.project(),
            "NW",
            &fixture.env().maintenance_backups_dir(),
        )
        .unwrap();
    let reopened = SqliteStore::open(fixture.store().path()).unwrap();
    assert!(VerificationQueue::new(&reopened).next().unwrap().is_none());
    let ctx = Ctx::new(
        &reopened,
        fixture.project(),
        fixture.cwd().to_path_buf(),
        fixture.env().clone(),
    )
    .no_hooks(true);
    let status = VerificationActivity::new().status(&ctx).unwrap();
    assert!(status.render_human().contains("NW-1"));
    assert!(status.render_human().contains(&first.id));
    let json = serde_json::to_value(status).unwrap();
    assert_eq!(json["attribution_holds"][0]["story_id"], "NW-1");
    assert_eq!(
        json["attribution_holds"][0]["diagnosis"],
        "diagnosis allowance exhausted"
    );
    assert!(
        json["attribution_holds"][0]["next_action"]
            .as_str()
            .unwrap()
            .contains("NW-1")
    );
    let answer = storyhook::invoke::dispatch(
        &ctx,
        storyhook::cli::Invocation::Verifier {
            action: storyhook::cli::VerifierAction::Evidence {
                story_id: "NW-1".into(),
            },
        },
    )
    .unwrap();
    let storyhook::output::Response::GateEvidence(view) = answer else {
        panic!("expected evidence")
    };
    assert_eq!(view.attributions, [first.clone()]);
    assert_eq!(view.attempts[0].submission.story_id, "SH-1");
    assert_eq!(
        reopened
            .read(|tx| tx.attributions(fixture.project()))
            .unwrap(),
        [first]
    );
    assert!(actuator.notified.lock().unwrap().is_empty());
}

#[test]
fn preparation_or_uncertain_cleanup_is_visible_as_unsettled_diagnosis() {
    use storyhook::service::attribution::{DiagnosticPreparation, PreparationResult};
    let fixture = ServiceFixture::new();
    fixture.github_checkout("https://github.com/acme/widgets");
    submitted(&fixture, "pending preparation", Priority::High, PR_ONE);
    let actuator = FakeActuator::new(VerificationOutcome::InvalidSubmission {
        detail: "unknown input".into(),
    });
    tick_with(fixture.store(), fixture.env(), &actuator, fixture.project()).unwrap();
    let mut record = fixture
        .store()
        .read(|tx| tx.attributions(fixture.project()))
        .unwrap()
        .remove(0);
    record.preparation = Some(DiagnosticPreparation {
        started_at: record.created_at.clone(),
        completed: None,
    });
    for completed in [false, true] {
        if completed {
            record.preparation.as_mut().unwrap().completed = Some(PreparationResult {
                milliseconds: 10,
                log: "/tmp/preparation.log".into(),
                detail: "cleanup cannot be proved".into(),
                cleanup_complete: false,
            });
            record.diagnosis_ms = 10;
        }
        record.revision += 1;
        fixture
            .store()
            .write(|tx| tx.update_attribution(&record, record.revision - 1))
            .unwrap();
        let reopened = SqliteStore::open(fixture.store().path()).unwrap();
        assert!(VerificationQueue::new(&reopened).next().unwrap().is_none());
        let status =
            serde_json::to_value(VerificationActivity::new().status(&fixture.ctx()).unwrap())
                .unwrap();
        assert_eq!(
            status["attribution_holds"][0]["diagnosis"],
            "unsettled execution"
        );
    }
    assert!(actuator.notified.lock().unwrap().is_empty());
}

#[test]
fn sh870_conflict_and_unproved_test_failure_never_assign_repair_or_wait_for_it() {
    for outcome in [
        VerificationOutcome::Conflict {
            detail: "base moved; conflict says nothing about candidate cause".into(),
        },
        VerificationOutcome::TestsFailed {
            tree: "a".repeat(40),
            log: "/tmp/absent-sh870-original.log".into(),
            detail: "test failure without supported causal evidence".into(),
            gate: "gate".into(),
        },
    ] {
        let f = ServiceFixture::new();
        f.github_checkout("https://github.com/acme/widgets");
        let id = submitted(&f, "unproved failure", Priority::High, PR_ONE);
        let actuator = FakeActuator::new(outcome);
        assert_eq!(
            tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
            TickResult::Returned
        );
        assert_eq!(story_row(&f, &id).state, "verifying");
        assert!(actuator.notified.lock().unwrap().is_empty());
        assert!(VerificationQueue::new(f.store()).next().unwrap().is_none());
        let records = f.store().read(|tx| tx.attributions(f.project())).unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].held);
        assert!(records[0].probes.is_empty());
        assert_eq!(
            tick_with(f.store(), f.env(), &actuator, f.project()).unwrap(),
            TickResult::Idle
        );
    }
}

#[test]
fn sh870_unproved_failures_cannot_notify_or_restart_any_agent_state() {
    for conflict in [false, true] {
        for policy in [false, true] {
            for transport in ["live", "absent", "unreachable", "refused-resume"] {
                let f = ServiceFixture::new();
                f.github_checkout("https://github.com/acme/widgets");
                let id = submitted(&f, "unproved failure", Priority::High, PR_ONE);
                if policy {
                    StoryService::new(&f.ctx())
                        .set_labels(&id, &[LABEL_NO_AUTO.into()], &[])
                        .unwrap();
                }
                let outcome = if conflict {
                    VerificationOutcome::Conflict {
                        detail: "both modified src/lib.rs".into(),
                    }
                } else {
                    VerificationOutcome::TestsFailed {
                        tree: "a".repeat(40),
                        log: "/unavailable/original.log".into(),
                        detail: "raw failed gate".into(),
                        gate: GateCommand::DEFAULT.into(),
                    }
                };
                let actuator = FakeActuator::new(outcome);
                let actuator = match transport {
                    "absent" => actuator.with_notify_script([NotifyScript::Absent("pane-dead")]),
                    "unreachable" => {
                        actuator.with_notify_script([NotifyScript::Fail("cannot reach agent")])
                    }
                    "refused-resume" => actuator
                        .with_notify_script([NotifyScript::Absent("pane-dead")])
                        .refusing_redispatch("wrong branch"),
                    _ => actuator,
                };
                let activity = VerificationActivity::new();
                let inflight = InFlight::new(f.env().clone());
                assert_eq!(
                    tick_with_reconciliation(
                        f.store(),
                        f.env(),
                        &actuator,
                        &activity,
                        &inflight,
                        f.project(),
                        |_| panic!("unproved repair must not wait")
                    )
                    .unwrap(),
                    TickResult::Returned
                );
                let row = story_row(&f, &id);
                assert_eq!(row.state, "verifying");
                assert!(row.awaiting.is_none());
                assert!(actuator.notified.lock().unwrap().is_empty());
                assert!(actuator.redispatched.lock().unwrap().is_empty());
                assert!(activity.active_all().is_empty());
                assert!(lifecycle::read_inflight(f.env()).is_empty());
                assert!(VerificationQueue::new(f.store()).next().unwrap().is_none());
            }
        }
    }
}
