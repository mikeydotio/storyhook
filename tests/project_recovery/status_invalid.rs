//! SH-851: corruption is local to a diagnostic row, never a status outage.
use super::*;
use serde_json::{Value, json};
use storyhook::daemon::verification::VerificationActivity;
use storyhook::service::project_recovery::RecoveryStatus;
use storyhook::service::verification_control::VerificationAction;
use storyhook::store::{ProjectRecovery, VerificationFailureDisposition, VerificationIncident};

fn rows(f: &ServiceFixture) -> Vec<RecoveryStatus> {
    VerificationActivity::new()
        .status(&f.ctx())
        .unwrap()
        .project_recoveries
}

fn assert_invalid(row: &RecoveryStatus, record: &ProjectRecovery, detail: &str) {
    assert_eq!(row.id, record.id);
    assert_eq!(row.fault, record.code);
    assert_eq!(row.locus, record.locus);
    assert_eq!(row.phase, "invalid");
    assert!(row.next_action.contains(&record.id));
    assert!(row.next_action.contains(detail), "{}", row.next_action);
    assert!(
        row.next_action
            .contains(&format!("story verifier repair show {} --json", record.id))
    );
    assert!(row.affected_stories.is_empty());
    assert!(row.assessment_owner.is_empty());
    assert!(row.repair_story.is_none());
    assert!(row.repair_link.is_none());
    assert_eq!((row.completed_attempts, row.attempt_limit), (0, 0));
}

#[test]
fn purged_reference_is_a_read_only_diagnostic_and_repair_show_stays_strict() {
    let f = fixture();
    let view = isolation::stranded(&f);
    let before = f
        .store()
        .read(|tx| {
            Ok((
                tx.project_recoveries(f.project())?,
                tx.project_recovery_observations(f.project(), &view.record.id)?,
                tx.max_global_seq(f.project())?,
            ))
        })
        .unwrap();
    let status = VerificationActivity::new().status(&f.ctx()).unwrap();
    assert_eq!(status.project_recoveries.len(), 1);
    let detail = "recovery dependency hold has inconsistent submission or event ownership";
    assert_invalid(&status.project_recoveries[0], &view.record, detail);
    let error = ProjectRecoveryService::new(&f.ctx())
        .show(&view.record.id)
        .unwrap_err();
    assert!(error.to_string().contains(detail), "{error}");
    let human = status.render_human();
    for expected in [
        &view.record.id,
        &view.record.code,
        &view.record.locus,
        detail,
        "--json",
    ] {
        assert!(human.contains(expected), "{human}");
    }
    for unavailable in [
        "Affected:",
        "assessor",
        "repair undecided",
        "completed attempts",
        "Repair PR:",
    ] {
        assert!(!human.contains(unavailable), "{human}");
    }
    assert_eq!(
        f.store()
            .read(|tx| Ok((
                tx.project_recoveries(f.project())?,
                tx.project_recovery_observations(f.project(), &view.record.id)?,
                tx.max_global_seq(f.project())?,
            )))
            .unwrap(),
        before
    );
}

fn insert_invalid(f: &ServiceFixture, id: &str, state: Value, active: bool) -> ProjectRecovery {
    let mut record = ProjectRecovery {
        id: id.into(),
        project: f.project(),
        code: "missing-certification".into(),
        locus: format!("gate/{id}"),
        revision: 0,
        active: true,
        state,
    };
    assert!(
        f.store()
            .write(|tx| tx.insert_project_recovery(&record))
            .unwrap()
    );
    if !active {
        record.active = false;
        record.revision += 1;
        assert!(
            f.store()
                .write(|tx| tx.update_project_recovery(&record, 0))
                .unwrap()
        );
    }
    record
}

#[test]
fn malformed_version_and_authority_rows_coexist_with_valid_rows_in_either_order() {
    for invalid_first in [true, false] {
        let f = fixture();
        let valid = decision::ready(&f);
        let valid_row = rows(&f)
            .into_iter()
            .find(|r| r.id == valid.record.id)
            .unwrap();
        let early = invalid_first.then(|| {
            // Complete setup before corruption: observation remains strict.
            // Enumeration uses rowid, so put the invalid envelope first.
            let record = insert_invalid(&f, "early", json!({}), false);
            rusqlite::Connection::open(f.store().path())
                .unwrap()
                .execute("UPDATE project_recoveries SET rowid=0 WHERE id='early'", [])
                .unwrap();
            record
        });
        let malformed = insert_invalid(
            &f,
            "malformed",
            json!({"version":"<script>literal</script>"}),
            true,
        );
        let mut state = valid.record.state.clone();
        state["version"] = json!(99);
        let version = insert_invalid(&f, "version", state, false);
        let mut state = valid.record.state.clone();
        state["assessment"]["failures"] = json!(4);
        let authority = insert_invalid(&f, "authority", state, true);
        let before = f
            .store()
            .read(|tx| tx.project_recoveries(f.project()))
            .unwrap();
        let actual = rows(&f);
        assert_eq!(actual.len(), if invalid_first { 5 } else { 4 });
        assert_eq!(
            actual.iter().find(|r| r.id == valid.record.id),
            Some(&valid_row)
        );
        for (record, detail) in [
            (
                &malformed,
                "invalid type: string \"<script>literal</script>\", expected u8",
            ),
            (&version, "unsupported project recovery state version 99"),
            (&authority, "has inconsistent assessment authority"),
        ] {
            assert_invalid(
                actual.iter().find(|r| r.id == record.id).unwrap(),
                record,
                detail,
            );
            assert!(
                ProjectRecoveryService::new(&f.ctx())
                    .show(&record.id)
                    .unwrap_err()
                    .to_string()
                    .contains(detail)
            );
        }
        if let Some(early) = early {
            assert_invalid(&actual[0], &early, "missing field `version`");
        }
        assert_eq!(
            f.store()
                .read(|tx| tx.project_recoveries(f.project()))
                .unwrap(),
            before
        );
    }
}

#[test]
fn invalid_status_preserves_queue_attempt_incident_and_control() {
    let f = fixture();
    let candidate = submitted(&f, "unrelated submission");
    let activity = VerificationActivity::new();
    let _guard = activity.acquire(&candidate, f.ctx().now());
    activity
        .control(f.store(), f.project(), VerificationAction::Drain)
        .unwrap();
    let incident = VerificationIncident {
        incident_id: "independent incident".into(),
        project: f.project(),
        story: StoryNo::new(1),
        generation: candidate.verifying_generation.unwrap(),
        disposition: VerificationFailureDisposition::Permanent,
        halted: true,
        attempts: 1,
        detail: "independent host error".into(),
        first_failed_at: f.ctx().now(),
        last_failed_at: f.ctx().now(),
    };
    f.store()
        .write(|tx| tx.put_verification_incident(&incident))
        .unwrap();
    let before = serde_json::to_value(activity.status(&f.ctx()).unwrap()).unwrap();
    let record = insert_invalid(&f, "invalid", json!({}), true);
    // Diagnostic resilience must not turn malformed ownership into executable
    // queue authority or relax the strict repair reader.
    assert!(
        VerificationQueue::new(f.store())
            .ordered_for(f.project())
            .is_err()
    );
    assert!(
        ProjectRecoveryService::new(&f.ctx())
            .show(&record.id)
            .is_err()
    );
    let mut after = serde_json::to_value(activity.status(&f.ctx()).unwrap()).unwrap();
    assert_eq!(after["project_recoveries"][0]["id"], record.id);
    after["project_recoveries"] = json!([]);
    assert_eq!(after, before);
    assert_eq!(
        f.store()
            .read(|tx| tx.verification_incident(f.project()))
            .unwrap(),
        Some(incident)
    );
}

#[test]
fn invalid_rows_do_not_revive_healthy_resolved_recoveries() {
    let f = fixture();
    let view = resume::decided(&f);
    resume::land(&f, &view);
    StoryService::new(&f.ctx())
        .set_state("SH-1", "verifying", None, None, None)
        .unwrap();
    StoryService::new(&f.ctx()).clear_awaiting("SH-1").unwrap();
    assert!(rows(&f).is_empty());
    let record = insert_invalid(&f, "retained-invalid", json!({}), false);
    let actual = rows(&f);
    assert_eq!(actual.len(), 1);
    assert_invalid(&actual[0], &record, "missing field `version`");
    assert!(
        ProjectRecoveryService::new(&f.ctx())
            .show(&view.record.id)
            .is_ok()
    );
}

fn invoke(
    f: &ServiceFixture,
    activity: &VerificationActivity,
    command: &str,
) -> storyhook::output::Response {
    use storyhook::api::wire::ProjectSelector;
    use storyhook::invoke::{InvokeRequest, Invoker, StoreInvoker};
    let invocation = storyhook::cli::parse_invocation(
        &command
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let request = InvokeRequest::new(invocation).no_hooks(true);
    let request = if matches!(command, "lane-budget" | "help verifier") {
        request
    } else {
        request.project(Some(ProjectSelector::Flag {
            slug: "fixture".into(),
        }))
    };
    StoreInvoker::new(f.store(), f.cwd(), f.env().clone())
        .verification_activity(activity)
        .invoke(request)
        .unwrap()
}

#[test]
fn invalid_recovery_preserves_production_command_results_and_cross_project_census() {
    use storyhook::output::{Response, render_response};
    use storyhook::service::engine::{EngineService, StartRequest};
    use storyhook::store::{EngineAgent, EngineScope};
    use storyhook_test_support::FakeDispatcher;
    let f = fixture();
    isolation::stranded(&f);
    f.add_project("healthy", "HH");
    StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "ready work".into(),
            ..Default::default()
        })
        .unwrap();
    EngineService::new(&f.ctx(), &FakeDispatcher::new([]))
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 1,
            agent: EngineAgent::Codex,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap();
    let activity = VerificationActivity::new();
    let help = render_response(&invoke(&f, &activity, "help verifier"), false, false);
    assert!(help.contains("invalid"));
    assert!(help.contains("unavailable"));
    for command in [
        "verifier status",
        "next",
        "load-context",
        "summary",
        "engine status",
        "lane-budget",
    ] {
        let response = invoke(&f, &activity, command);
        let wire: Value = serde_json::from_str(&render_response(&response, true, false)).unwrap();
        assert!(wire.to_string().contains("invalid"), "{command}");
        if let Response::WithVerifier {
            response: inner,
            verifiers,
            unavailable,
        } = &response
        {
            assert!(unavailable.is_none());
            assert_eq!(
                verifiers.len(),
                if command == "lane-budget" { 2 } else { 1 }
            );
            let affected = verifiers
                .iter()
                .find(|v| !v.project_recoveries.is_empty())
                .unwrap();
            assert_eq!(affected.project_recoveries[0].phase, "invalid");
            if command == "lane-budget" {
                assert_eq!(
                    verifiers
                        .iter()
                        .filter(|v| v.project_recoveries.is_empty())
                        .count(),
                    1
                );
            }
            // Compare the ordinary result with the same production dispatch
            // without the daemon's status attachment.
            let bare = if command == "lane-budget" {
                // The census is read-only but time-dependent; its typed
                // response must survive alongside both project notices.
                assert!(matches!(inner.as_ref(), Response::LaneBudget(_)));
                None
            } else {
                let invocation = storyhook::cli::parse_invocation(
                    &command
                        .split_whitespace()
                        .map(str::to_owned)
                        .collect::<Vec<_>>(),
                )
                .unwrap();
                Some(storyhook::invoke::dispatch(&f.ctx(), invocation).unwrap())
            };
            if let Some(bare) = bare {
                assert_eq!(
                    render_response(inner, true, false),
                    render_response(&bare, true, false),
                    "{command}"
                );
            }
        } else {
            assert_eq!(command, "verifier status");
            assert!(render_response(&response, false, false).contains("invalid"));
        }
    }
}

#[test]
fn status_query_failures_are_not_converted_to_invalid_rows() {
    for table in [
        "projects",
        "project_recoveries",
        "project_recovery_observations",
    ] {
        let f = fixture();
        decision::ready(&f);
        // Break query preparation, not the row data: even a failure inside
        // read_view must propagate when it is not StoreError::Corrupt.
        let connection = rusqlite::Connection::open(f.store().path()).unwrap();
        let original = if table == "projects" {
            "name"
        } else if table == "project_recoveries" {
            "locus"
        } else {
            "evidence"
        };
        connection
            .execute_batch(&format!(
                "ALTER TABLE {table} RENAME COLUMN {original} TO unavailable_for_test"
            ))
            .unwrap();
        let result = VerificationActivity::new().status(&f.ctx());
        connection
            .execute_batch(&format!(
                "ALTER TABLE {table} RENAME COLUMN unavailable_for_test TO {original}"
            ))
            .unwrap();
        assert!(result.is_err(), "{table}: {result:?}");
        assert!(
            result.unwrap_err().to_string().contains(original),
            "{table}"
        );
    }
}
