//! Durable evidence reads use the ordinary parser, dispatcher and wire renderer.
use storyhook::cli::{Invocation, VerifierAction, parse_invocation};
use storyhook::invoke::dispatch;
use storyhook::output::{Response, render_response};
use storyhook::store::{GateAttempt, GateSubmission, GlobalSeq, Store, WriteOps};
use storyhook_test_support::ServiceFixture;

#[test]
fn evidence_requires_exactly_one_story_id() {
    let parsed = parse_invocation(&["verifier".into(), "evidence".into(), "SH-1".into()]).unwrap();
    assert_eq!(
        parsed,
        Invocation::Verifier {
            action: VerifierAction::Evidence {
                story_id: "SH-1".into()
            }
        }
    );
    for words in [
        vec!["verifier", "evidence"],
        vec!["verifier", "evidence", "SH-1", "SH-2"],
    ] {
        assert!(
            parse_invocation(&words.into_iter().map(String::from).collect::<Vec<_>>()).is_err()
        );
    }
}

#[test]
fn evidence_is_readable_without_live_ownership_and_survives_wire_roundtrip() {
    let fixture = ServiceFixture::new();
    let at = "2026-10-03T00:00:00Z";
    let mut attempt = GateAttempt::new(
        "admission".into(),
        GateSubmission {
            project: fixture.project(),
            story_id: "SH-1".into(),
            generation: Some(GlobalSeq::new(1)),
            submitted_at: Some(at.into()),
        },
        at,
    );
    attempt.elapsed.observe(900_000, "2026-10-03T00:15:00Z");
    fixture
        .store()
        .write(|tx| tx.insert_gate_attempt(&attempt))
        .unwrap();
    let answer = dispatch(
        &fixture.ctx(),
        Invocation::Verifier {
            action: VerifierAction::Evidence {
                story_id: "SH-1".into(),
            },
        },
    )
    .unwrap();
    let roundtrip: Response =
        serde_json::from_str(&serde_json::to_string(&answer).unwrap()).unwrap();
    assert_eq!(
        render_response(&answer, true, false),
        render_response(&roundtrip, true, false)
    );
    let json: serde_json::Value =
        serde_json::from_str(&render_response(&answer, true, false)).unwrap();
    assert_eq!(
        json["evidence"]["attempts"][0]["elapsed"]["milliseconds"],
        900_000
    );
    assert_eq!(
        json["evidence"]["submissions"][0]["breaches"][0],
        "admission"
    );
    assert!(render_response(&answer, false, false).contains("process-budget-breach"));
}

#[test]
fn attribution_history_is_scoped_and_survives_the_evidence_wire() {
    use storyhook::service::attribution::AttributionRecord;
    let fixture = ServiceFixture::new();
    for (id, story, generation, retired) in [
        ("old-diagnosis", "SH-1", 1, true),
        ("current-diagnosis", "SH-1", 2, false),
        ("other-diagnosis", "SH-2", 3, false),
    ] {
        let record: AttributionRecord = serde_json::from_value(serde_json::json!({
            "version":1,"id":id,"revision":0,
            "submission":{"project":fixture.project(),"story_id":story,"generation":generation,"submitted_at":"2026-10-03T00:00:00Z"},
            "attempt":format!("attempt-{id}"),"inputs":{},"created_at":"2026-10-03T00:00:00Z",
            "components":[{"id":"check","check":"retained-case","signature":"assertion failed","requirement":"preserve input","log":"/tmp/retained.log","observed_cause":"unknown"}],
            "plans":[],"probes":[],"assessments":[],"diagnosis_ms":0,"held":true,"retired":null,
        })).unwrap();
        fixture
            .store()
            .write(|tx| tx.insert_attribution(&record))
            .unwrap();
        if retired {
            let mut record = record;
            record.revision = 1;
            record.held = false;
            record.retired = Some("superseded by generation 2".into());
            assert!(
                fixture
                    .store()
                    .write(|tx| tx.update_attribution(&record, 0))
                    .unwrap()
            );
        }
    }
    let answer = dispatch(
        &fixture.ctx(),
        Invocation::Verifier {
            action: VerifierAction::Evidence {
                story_id: "SH-1".into(),
            },
        },
    )
    .unwrap();
    let wire = render_response(&answer, true, false);
    let json: serde_json::Value = serde_json::from_str(&wire).unwrap();
    let records = json["evidence"]["attributions"]
        .as_array()
        .expect("retained attribution history");
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["id"], "old-diagnosis");
    assert_eq!(records[0]["held"], false);
    assert_eq!(records[1]["id"], "current-diagnosis");
    assert_eq!(records[1]["held"], true);
    let roundtrip: Response =
        serde_json::from_str(&serde_json::to_string(&answer).unwrap()).unwrap();
    assert_eq!(render_response(&roundtrip, true, false), wire);
    let text = render_response(&roundtrip, false, false);
    assert!(
        text.contains("old-diagnosis")
            && text.contains("current-diagnosis")
            && text.contains("unknown")
    );
    assert!(!text.contains("other-diagnosis"));
}

#[test]
fn legacy_evidence_without_attribution_means_no_retained_diagnosis() {
    use storyhook::service::gate_cost::view::EvidenceView;
    let legacy = serde_json::json!({"version":1,"story_id":"SH-1","attempts":[],"submissions":[]});
    let view: EvidenceView = serde_json::from_value(legacy).unwrap();
    assert_eq!(
        serde_json::to_value(&view).unwrap()["attributions"],
        serde_json::json!([])
    );
    assert!(view.render().contains("No retained attribution evidence"));
}

#[test]
fn prefix_rename_keeps_cost_history_and_groups_the_original_generation() {
    use storyhook::service::ProjectService;
    use storyhook::store::ReadOps;
    let fixture = ServiceFixture::new();
    let at = "2026-10-03T00:00:00Z";
    let mut first = GateAttempt::new(
        "before-rename".into(),
        GateSubmission {
            project: fixture.project(),
            story_id: "SH-1".into(),
            generation: Some(GlobalSeq::new(1)),
            submitted_at: Some(at.into()),
        },
        at,
    );
    first.elapsed.observe(10, at);
    fixture
        .store()
        .write(|tx| tx.insert_gate_attempt(&first))
        .unwrap();
    ProjectService::new(fixture.store(), fixture.cwd())
        .set_prefix(
            fixture.project(),
            "NW",
            &fixture.env().maintenance_backups_dir(),
        )
        .unwrap();
    let mut second = first.clone();
    second.id = "after-rename".into();
    second.submission.story_id = "NW-1".into();
    fixture
        .store()
        .write(|tx| tx.insert_gate_attempt(&second))
        .unwrap();
    let answer = dispatch(
        &fixture.ctx(),
        Invocation::Verifier {
            action: VerifierAction::Evidence {
                story_id: "NW-1".into(),
            },
        },
    )
    .unwrap();
    let Response::GateEvidence(view) = answer else {
        panic!("expected evidence")
    };
    assert_eq!(view.attempts, [first.clone(), second.clone()]);
    assert_eq!(view.submissions.len(), 1);
    assert_eq!(view.submissions[0].admission_milliseconds, Some(20));
    assert_eq!(
        view.submissions[0].attempts,
        ["before-rename", "after-rename"]
    );
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.gate_attempts(fixture.project()))
            .unwrap(),
        [first, second]
    );
}
