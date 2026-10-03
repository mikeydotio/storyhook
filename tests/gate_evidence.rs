mod store_support;

use store_support::{new_store, seed_project};
use storyhook::store::{
    GateAttempt, GateExecution, GateInputs, GateSubmission, GlobalSeq, ReadOps, SqliteStore, Store,
    WriteOps,
};

const AT: &str = "2026-10-03T00:00:00Z";

fn execution(id: &str, tree: &str) -> GateExecution {
    let mut execution = GateExecution::new(id.into(), AT, format!("/tmp/{id}.ndjson"));
    execution.inputs = GateInputs {
        tree: Some(tree.into()),
        ..Default::default()
    };
    execution
}

#[test]
fn evidence_survives_reopen_and_stale_writes_cannot_erase_a_breach() {
    let (root, store) = new_store();
    let project = seed_project(&store, "cost", "GC");
    let mut record = GateAttempt::new(
        "first".into(),
        GateSubmission {
            project,
            story_id: "GC-1".into(),
            generation: Some(GlobalSeq::new(7)),
            submitted_at: Some(AT.into()),
        },
        AT,
    );
    store.write(|tx| tx.insert_gate_attempt(&record)).unwrap();
    let mut stale = record.clone();
    record.revision = 1;
    record.elapsed.observe(900_000, "2026-10-03T00:15:00Z");
    assert!(
        store
            .write(|tx| tx.update_gate_attempt(&record, 0))
            .unwrap()
    );
    stale.revision = 1;
    assert!(!store.write(|tx| tx.update_gate_attempt(&stale, 0)).unwrap());
    drop(store);
    let reopened = SqliteStore::open(root.path().join("store.db")).unwrap();
    let records = reopened.read(|tx| tx.gate_attempts(project)).unwrap();
    assert_eq!(records, vec![record]);
    assert_eq!(records[0].budget_status(), "process-budget-breach");
}

#[test]
fn finished_attempt_and_known_inputs_are_immutable() {
    let (_root, store) = new_store();
    let project = seed_project(&store, "cost", "GC");
    let mut record = GateAttempt::new(
        "first".into(),
        GateSubmission {
            project,
            story_id: "GC-1".into(),
            generation: None,
            submitted_at: None,
        },
        AT,
    );
    record.executions.push(execution("gate", &"a".repeat(40)));
    store.write(|tx| tx.insert_gate_attempt(&record)).unwrap();
    record.revision = 1;
    record.executions[0].inputs.tree = Some("b".repeat(40));
    assert!(
        store
            .write(|tx| tx.update_gate_attempt(&record, 0))
            .is_err()
    );
    record.executions[0].inputs.tree = Some("a".repeat(40));
    record.finished_at = Some(AT.into());
    record.verdict = Some("certified".into());
    assert!(
        store
            .write(|tx| tx.update_gate_attempt(&record, 0))
            .unwrap()
    );
    record.revision = 2;
    record.verdict = Some("tests-failed".into());
    assert!(
        store
            .write(|tx| tx.update_gate_attempt(&record, 1))
            .is_err(),
        "completed admission verdict is immutable"
    );
    record.verdict = Some("certified".into());
    record.finished_at = None;
    assert!(
        store
            .write(|tx| tx.update_gate_attempt(&record, 1))
            .is_err()
    );
}

#[test]
fn malformed_and_inconsistent_rows_fail_loudly() {
    let (_root, store) = new_store();
    let project = seed_project(&store, "cost", "GC");
    let mut record = GateAttempt::new(
        "broken".into(),
        GateSubmission {
            project,
            story_id: "GC-1".into(),
            generation: None,
            submitted_at: None,
        },
        AT,
    );
    record.elapsed.milliseconds = 900_000;
    assert!(
        store.write(|tx| tx.insert_gate_attempt(&record)).is_err(),
        "a missing breach cannot be stored"
    );
    assert!(
        store
            .read(|tx| tx.gate_attempts(project))
            .unwrap()
            .is_empty()
    );
    record.elapsed.observe(900_000, "2026-10-03T00:15:00Z");
    store.write(|tx| tx.insert_gate_attempt(&record)).unwrap();
    let conn = rusqlite::Connection::open(store.path()).unwrap();
    conn.execute(
        "UPDATE gate_attempts SET payload=json_set(payload, '$.admitted_at', 'invalid')",
        [],
    )
    .unwrap();
    let error = store
        .read(|tx| tx.gate_attempts(project))
        .unwrap_err()
        .to_string();
    assert!(error.contains("invalid gate evidence broken"), "{error}");
}

#[test]
fn retries_and_replacement_submissions_retain_ordered_history() {
    let (_root, store) = new_store();
    let project = seed_project(&store, "cost", "GC");
    for (index, generation) in [7, 7, 8].into_iter().enumerate() {
        let mut record = GateAttempt::new(
            format!("attempt-{index}"),
            GateSubmission {
                project,
                story_id: "GC-1".into(),
                generation: Some(GlobalSeq::new(generation)),
                submitted_at: Some(AT.into()),
            },
            AT,
        );
        record.previous_attempt = index.checked_sub(1).map(|i| format!("attempt-{i}"));
        store.write(|tx| tx.insert_gate_attempt(&record)).unwrap();
    }
    let records = store.read(|tx| tx.gate_attempts(project)).unwrap();
    assert_eq!(records.len(), 3);
    assert_eq!(records[2].previous_attempt.as_deref(), Some("attempt-1"));
    assert_eq!(records[0].submission, records[1].submission);
    assert_ne!(records[1].submission, records[2].submission);
    assert!(
        records
            .iter()
            .all(|r| r.executions.is_empty() && r.verdict.is_none())
    );
}

#[test]
fn distinct_executions_keep_their_trees_and_cannot_replace_finished_evidence() {
    let (_root, store) = new_store();
    let project = seed_project(&store, "cost", "GC");
    let mut record = GateAttempt::new(
        "admission".into(),
        GateSubmission {
            project,
            story_id: "GC-1".into(),
            generation: Some(GlobalSeq::new(1)),
            submitted_at: Some(AT.into()),
        },
        AT,
    );
    record.executions.push(execution("batch", "batch-tree"));
    store.write(|tx| tx.insert_gate_attempt(&record)).unwrap();
    record.revision = 1;
    record.elapsed.observe(900_000, "2026-10-03T00:15:00Z");
    record.executions[0].finished_at = Some("2026-10-03T00:15:00Z".into());
    record.executions[0].milliseconds = Some(900_000);
    record.executions[0].verdict = Some("tests-failed".into());
    assert!(
        store
            .write(|tx| tx.update_gate_attempt(&record, 0))
            .unwrap()
    );
    record.revision = 2;
    record.executions.push(execution("probe", "head-tree"));
    assert!(
        store
            .write(|tx| tx.update_gate_attempt(&record, 1))
            .unwrap()
    );
    let original = record.clone();
    record.revision = 3;
    record.executions[0].verdict = Some("certified".into());
    assert!(
        store
            .write(|tx| tx.update_gate_attempt(&record, 2))
            .is_err()
    );
    let rows = store.read(|tx| tx.gate_attempts(project)).unwrap();
    assert_eq!(rows, vec![original]);
    assert_eq!(rows[0].budget_status(), "process-budget-breach");
    assert_eq!(
        rows[0].executions[1].inputs.tree.as_deref(),
        Some("head-tree")
    );
}
