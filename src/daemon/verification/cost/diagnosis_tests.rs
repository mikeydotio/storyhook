use super::*;
use crate::service::attribution::ProbeOutcome;
use crate::store::GateExecutionPurpose;

fn purpose() -> GateExecutionPurpose {
    GateExecutionPurpose::Diagnosis {
        attribution: "cause-1".into(),
        probe: "probe-1".into(),
    }
}

fn failed_gate(board: &Board, owner: &VerificationGuard) {
    execute(
        &board.store,
        &board.env,
        owner,
        &board.candidate,
        GateExecutionPurpose::Gate,
        GateInputs::default(),
        vec![submission(&board.candidate)],
        |_| VerificationOutcome::TestsFailed {
            tree: "a".repeat(40),
            gate: "fixture gate".into(),
            log: "/retained/gate-output".into(),
            detail: "original gate failed".into(),
        },
        |outcome| Ok(Some(outcome.clone())),
    )
    .unwrap();
}

#[test]
fn diagnosis_is_durable_before_launch_and_preserves_gate_result_and_journal() {
    let board = Board::new();
    let owner = board.admit();
    failed_gate(&board, &owner);
    let gate = board.rows().remove(0);
    let original = std::fs::read(journal_path(&board.env, &board.candidate)).unwrap();
    let result = execute(
        &board.store,
        &board.env,
        &owner,
        &board.candidate,
        purpose(),
        GateInputs::default(),
        vec![submission(&board.candidate)],
        |launch| {
            let durable = board.rows().remove(0);
            assert_eq!(durable.executions.len(), 2);
            assert_eq!(&durable.executions[1], launch);
            assert_eq!(launch.purpose, purpose());
            assert!(launch.finished_at.is_none());
            assert_eq!(durable.verdict, gate.verdict);
            assert_ne!(launch.journal_path, gate.journal_path.clone().unwrap());
            let marker: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&launch.journal_path).unwrap()).unwrap();
            assert_eq!(marker["execution_id"], launch.id);
            assert_eq!(marker["attempt_id"], owner.active.attempt_id);
            ProbeOutcome::Passed
        },
        |outcome| Ok(outcome.clone()),
    )
    .unwrap();
    assert_eq!(result, ProbeOutcome::Passed);
    let completed = board.rows().remove(0);
    assert_eq!(completed.verdict, gate.verdict);
    assert_eq!(completed.executions[0], gate.executions[0]);
    assert_eq!(completed.journal_path, gate.journal_path);
    assert_eq!(
        std::fs::read(journal_path(&board.env, &board.candidate)).unwrap(),
        original
    );
    let diagnostic = &completed.executions[1];
    assert_eq!(diagnostic.verdict.as_deref(), Some("passed"));
    assert!(diagnostic.finished_at.is_some());
    assert!(diagnostic.milliseconds.is_some());
    assert!(diagnostic.journal_bound);
    assert_eq!(diagnostic.purpose, purpose());
    sample(&board.store, &board.activity, board.candidate.project).unwrap();
    assert_eq!(board.rows()[0].executions, completed.executions);
}

#[test]
fn diagnosis_interruption_cannot_replace_a_completed_failed_gate_after_restart() {
    let board = Board::new();
    let owner = board.admit();
    failed_gate(&board, &owner);
    let gate = board.rows()[0].executions[0].clone();
    let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        execute(
            &board.store,
            &board.env,
            &owner,
            &board.candidate,
            purpose(),
            GateInputs::default(),
            vec![submission(&board.candidate)],
            |_| -> ProbeOutcome { panic!("simulated loss after durable diagnostic start") },
            |outcome| Ok(outcome.clone()),
        )
    }));
    assert!(interrupted.is_err());
    drop(owner);
    restart(&board.store, board.candidate.project, &board.env.now()).unwrap();
    let rows = board.rows();
    assert_eq!(rows[0].verdict.as_deref(), Some("tests-failed"));
    assert_eq!(rows[0].executions[0], gate);
    assert_eq!(
        rows[0].executions[1].verdict.as_deref(),
        Some("interrupted")
    );
    assert!(rows[0].executions[1].finished_at.is_none());
    assert!(rows[0].executions[1].milliseconds.is_none());
    restart(&board.store, board.candidate.project, &board.env.now()).unwrap();
    assert_eq!(
        board.rows(),
        rows,
        "restart must not renew or erase diagnosis"
    );
}

#[test]
fn diagnosis_failure_to_record_prevents_launch_and_leaves_gate_evidence_intact() {
    use crate::store::fault::{FaultAction, FaultPoint, arm};
    let board = Board::new();
    let owner = board.admit();
    failed_gate(&board, &owner);
    let before = board.rows();
    let journal = std::fs::read(journal_path(&board.env, &board.candidate)).unwrap();
    let failure = arm(
        FaultPoint::BeforeCommit,
        FaultAction::Fail("diagnostic persistence refused".into()),
    );
    let mut launched = false;
    let result = execute(
        &board.store,
        &board.env,
        &owner,
        &board.candidate,
        purpose(),
        GateInputs::default(),
        vec![submission(&board.candidate)],
        |_| {
            launched = true;
            ProbeOutcome::Passed
        },
        |outcome| Ok(outcome.clone()),
    );
    drop(failure);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("diagnostic persistence refused")
    );
    assert!(!launched);
    assert_eq!(board.rows(), before);
    assert_eq!(
        std::fs::read(journal_path(&board.env, &board.candidate)).unwrap(),
        journal
    );
}

#[test]
fn diagnosis_cannot_report_certification_or_change_the_original_result() {
    let board = Board::new();
    let owner = board.admit();
    failed_gate(&board, &owner);
    let error = execute(
        &board.store,
        &board.env,
        &owner,
        &board.candidate,
        purpose(),
        GateInputs::default(),
        vec![submission(&board.candidate)],
        |_| VerificationOutcome::Certified {
            head: "a".repeat(40),
            tree: "b".repeat(40),
            gate: "fake".into(),
            detail: "not a gate".into(),
        },
        |outcome| Ok(Some(outcome.clone())),
    )
    .unwrap_err();
    assert!(error.to_string().contains("purpose"));
    assert!(owner.is_cancelled());
    let record = board.rows().remove(0);
    assert_eq!(record.verdict.as_deref(), Some("tests-failed"));
    assert_eq!(record.executions[1].verdict.as_deref(), Some("error"));
    assert!(
        record.executions[1]
            .diagnostics
            .iter()
            .any(|d| d.contains("purpose"))
    );
    assert_eq!(record.executions[1].inputs, GateInputs::default());
}
