mod store_support;

use serde_json::{Value, json};
use store_support::{new_store, seed_project};
use storyhook::service::gate_cost::view::EvidenceView;
use storyhook::store::{
    GateAttempt, GateExecution, GateSubmission, GlobalSeq, ProjectId, ReadOps, SqliteStore, Store,
    WriteOps,
};

const AT: &str = "2026-10-03T00:00:00Z";

fn purpose() -> Value {
    json!({"kind":"diagnosis", "attribution":"cause-1", "probe":"probe-1"})
}

fn execution(id: &str, purpose: Option<Value>, milliseconds: Option<u64>) -> GateExecution {
    let mut execution = GateExecution::new(id.into(), AT, format!("/tmp/{id}.ndjson"));
    execution.milliseconds = milliseconds;
    execution.finished_at = milliseconds.map(|_| AT.into());
    let mut value = serde_json::to_value(execution).unwrap();
    if let Some(purpose) = purpose {
        value["purpose"] = purpose;
    }
    serde_json::from_value(value).unwrap()
}

fn attempt(project: ProjectId) -> GateAttempt {
    GateAttempt::new(
        "attempt".into(),
        GateSubmission {
            project,
            story_id: "GC-1".into(),
            generation: Some(GlobalSeq::new(7)),
            submitted_at: Some(AT.into()),
        },
        AT,
    )
}

#[test]
fn diagnosis_purpose_survives_reopen_and_cannot_change_while_running() {
    let (root, store) = new_store();
    let project = seed_project(&store, "diagnosis", "GC");
    let mut record = attempt(project);
    record
        .executions
        .push(execution("diagnosis", Some(purpose()), None));
    store.write(|tx| tx.insert_gate_attempt(&record)).unwrap();
    drop(store);
    let store = SqliteStore::open(root.path().join("store.db")).unwrap();
    let retained = store
        .read(|tx| tx.gate_attempts(project))
        .unwrap()
        .remove(0);
    assert_eq!(
        serde_json::to_value(&retained.executions[0]).unwrap()["purpose"],
        purpose()
    );
    for changed in [
        json!({"kind":"gate"}),
        json!({"kind":"diagnosis-preparation", "attribution":"cause-1"}),
        json!({"kind":"diagnosis", "attribution":"another", "probe":"probe-1"}),
        json!({"kind":"diagnosis", "attribution":"cause-1", "probe":"another"}),
    ] {
        let mut value = serde_json::to_value(&retained).unwrap();
        value["revision"] = json!(1);
        value["executions"][0]["purpose"] = changed;
        let changed: GateAttempt = serde_json::from_value(value).unwrap();
        assert!(
            store
                .write(|tx| tx.update_gate_attempt(&changed, 0))
                .is_err()
        );
    }
    assert_eq!(
        store.read(|tx| tx.gate_attempts(project)).unwrap(),
        vec![retained]
    );
}

#[test]
fn diagnosis_purpose_refuses_missing_or_ambiguous_binding() {
    for invalid in [
        json!({"kind":"other"}),
        json!({"kind":"diagnosis", "attribution":"", "probe":"p"}),
        json!({"kind":"diagnosis", "attribution":"a", "probe":""}),
        json!({"kind":"diagnosis", "attribution":"a"}),
        json!({"kind":"diagnosis", "attribution":"a", "probe":"p", "authority":true}),
        json!({"kind":"diagnosis", "attribution":" ", "probe":"p"}),
        json!({"kind":"diagnosis-preparation", "attribution":""}),
        json!({"kind":"diagnosis-preparation", "attribution":"a", "probe":"p"}),
    ] {
        let mut value = serde_json::to_value(attempt(ProjectId::new(1))).unwrap();
        let mut child = serde_json::to_value(execution("e", None, None)).unwrap();
        child["purpose"] = invalid.clone();
        value["executions"] = json!([child]);
        let accepted = serde_json::from_value::<GateAttempt>(value)
            .is_ok_and(|record| record.validate().is_ok());
        assert!(!accepted, "accepted invalid diagnostic binding {invalid}");
    }
}

#[test]
fn legacy_gate_shape_is_unchanged_and_diagnosis_does_not_inflate_gate_cost() {
    let gate = execution("gate", None, Some(1000));
    assert!(
        serde_json::to_value(&gate)
            .unwrap()
            .get("purpose")
            .is_none()
    );
    let mut record = attempt(ProjectId::new(1));
    record.verdict = Some("tests-failed".into());
    record.executions = vec![gate, execution("diagnosis", Some(purpose()), Some(120))];
    let view = EvidenceView::new(record.submission.project, "GC-1", vec![record.clone()]);
    let value = serde_json::to_value(&view.submissions[0]).unwrap();
    assert_eq!(value["execution_milliseconds"], 1000);
    assert_eq!(value["known_execution_milliseconds"], 1000);
    assert_eq!(value["executions"], 1);
    assert_eq!(value["diagnosis_milliseconds"], 120);
    assert_eq!(value["known_diagnosis_milliseconds"], 120);
    assert_eq!(value["diagnosis_executions"], 1);
    assert_eq!(view.attempts[0].verdict.as_deref(), Some("tests-failed"));
    assert!(view.render().contains("diagnosis cost 120 ms"));

    record
        .executions
        .push(execution("unfinished", Some(purpose()), None));
    let view = EvidenceView::new(record.submission.project, "GC-1", vec![record]);
    let value = serde_json::to_value(&view.submissions[0]).unwrap();
    assert_eq!(value["execution_milliseconds"], 1000);
    assert_eq!(value["diagnosis_milliseconds"], Value::Null);
    assert_eq!(value["known_diagnosis_milliseconds"], 120);
    assert_eq!(value["diagnosis_executions"], 2);
    assert!(view.render().contains("diagnosis cost unknown ms"));
    assert!(
        storyhook::service::gate_cost::current::progress(&view, Some(GlobalSeq::new(7)))
            .unwrap()
            .contains("physical gate 1000 ms, diagnosis unknown ms")
    );

    let mut legacy = serde_json::to_value(&view).unwrap();
    let summary = legacy["submissions"][0].as_object_mut().unwrap();
    for field in [
        "diagnosis_milliseconds",
        "known_diagnosis_milliseconds",
        "diagnosis_executions",
    ] {
        summary.remove(field);
    }
    let _: EvidenceView = serde_json::from_value(legacy).unwrap();
}

#[test]
fn diagnosis_preparation_has_its_own_binding_and_contributes_only_diagnostic_cost() {
    let (root, store) = new_store();
    let project = seed_project(&store, "preparation", "GC");
    let mut record = attempt(project);
    let purpose = json!({"kind":"diagnosis-preparation", "attribution":"cause-1"});
    record
        .executions
        .push(execution("preparation", Some(purpose.clone()), Some(200)));
    store.write(|tx| tx.insert_gate_attempt(&record)).unwrap();
    drop(store);
    let store = SqliteStore::open(root.path().join("store.db")).unwrap();
    let rows = store.read(|tx| tx.gate_attempts(project)).unwrap();
    assert_eq!(
        serde_json::to_value(&rows[0].executions[0]).unwrap()["purpose"],
        purpose
    );
    let view = EvidenceView::new(project, "GC-1", rows);
    assert_eq!(view.submissions[0].execution_milliseconds, Some(0));
    assert_eq!(
        serde_json::to_value(&view.submissions[0]).unwrap()["diagnosis_milliseconds"],
        200
    );
}
