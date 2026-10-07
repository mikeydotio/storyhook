//! The coordinator executes real Rust probes, accounting, store writes and return validation.
use super::*;
use crate::service::attribution::NativeFixture;
use std::{fs, io::Write};

fn original(
    b: &Board,
    owner: &VerificationGuard,
    fixture: &NativeFixture,
    mixed: bool,
) -> RustDiagnosisRequest {
    let mut native = fixture.comparison();
    let log = b.fixture.cwd().join("native-original.log");
    let inputs = GateInputs {
        head: Some(fixture.git(&["rev-parse", "HEAD"])),
        base: Some(fixture.base.clone()),
        tree: Some(fixture.git(&["rev-parse", "HEAD^{tree}"])),
        ..Default::default()
    };
    let mut execution_id = String::new();
    cost::execute(&b.store, &b.env, owner, &b.candidate, GateExecutionPurpose::Gate, inputs.clone(), vec![cost::submission(&b.candidate)],
        |execution| {
            execution_id = execution.id.clone();
            let output = b.fixture.cwd().join("original-run");
            let observed = native.execute(ProbeSide::Candidate, &NativeProbeBinding { project: &b.candidate.project_slug,
                attempt: &owner.active.attempt_id, execution: &execution.id, generation: b.candidate.verifying_generation.unwrap().get(),
                request: "original-gate", journal: std::path::Path::new(&execution.journal_path), output: &output, termination_grace: RECOVERY_WAKE }).unwrap();
            assert!(matches!(observed.outcome, ProbeOutcome::Failed { .. }), "{:?}", observed.outcome);
            let mut raw = b"     Running tests/contract.rs (target/contract)\n".to_vec();
            raw.extend(fs::read(output.join("run.stdout")).unwrap());
            fs::write(&log, raw).unwrap();
            let mut journal = fs::OpenOptions::new().append(true).open(&execution.journal_path).unwrap();
            writeln!(journal, "{}", serde_json::json!({"kind":"case","outcome":"fail","path":"rust-suite","target":"contract","name":"answer"})).unwrap();
            if mixed { writeln!(journal, "{}", serde_json::json!({"kind":"case","outcome":"fail","path":"plugin","target":"unsupported","name":"other"})).unwrap(); }
            VerificationOutcome::TestsFailed { tree: inputs.tree.clone().unwrap(), log: log.display().to_string(), detail: "actual native original failure".into(), gate: "fixture original".into() }
        }, |outcome| Ok(Some(outcome.clone()))).unwrap();
    native.close().unwrap();
    RustDiagnosisRequest {
        execution: execution_id,
        log,
        case: RustCase::new(
            "subject",
            RustTarget::Integration("contract".into()),
            "answer",
        )
        .unwrap(),
        intervention: TreeIntervention::Unchanged,
        fixture: Some(fixture.config.clone()),
    }
}

#[test]
fn behavior_and_fixture_failures_run_through_durable_coordinator_and_return_transaction() {
    for fixture_defect in [false, true] {
        let f = NativeFixture::new(fixture_defect);
        let mut b = Board::new();
        b.fixture.github_checkout_at(
            b.fixture.project(),
            f.directory.path(),
            "https://github.com/acme/widgets",
        );
        b.candidate = VerificationQueue::new(&b.store).next().unwrap().unwrap();
        let owner = b.owner();
        let request = original(&b, &owner, &f, fixture_defect);
        let before = b
            .store
            .read(|tx| tx.gate_attempts(b.candidate.project))
            .unwrap()[0]
            .clone();
        let result = owner
            .diagnose_rust_failure(&b.ctx(), &b.candidate, request)
            .unwrap();
        let proof = match result {
            RustDiagnosisResult::Proven(proof) => proof,
            RustDiagnosisResult::Held { detail, .. } => {
                panic!("real native contrast held: {detail}")
            }
            RustDiagnosisResult::Superseded => panic!("current native contrast superseded"),
        };
        let records = b
            .store
            .read(|tx| tx.attributions(b.candidate.project))
            .unwrap();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.components.len(), if fixture_defect { 2 } else { 1 });
        assert_eq!(record.probes.len(), 4);
        assert_eq!(
            record.probes.iter().map(|p| p.side).collect::<Vec<_>>(),
            [
                ProbeSide::Candidate,
                ProbeSide::Control,
                ProbeSide::Control,
                ProbeSide::Candidate
            ]
        );
        let after = b
            .store
            .read(|tx| tx.gate_attempts(b.candidate.project))
            .unwrap()[0]
            .clone();
        assert_eq!(after.verdict, before.verdict);
        assert_eq!(after.executions[0], before.executions[0]);
        assert_eq!(after.executions.len(), 6);
        assert!(
            after
                .executions
                .iter()
                .all(|e| e.finished_at.is_some() && e.journal_bound)
        );
        assert!(matches!(
            after.executions[1].purpose,
            GateExecutionPurpose::DiagnosisPreparation { .. }
        ));
        for (probe, physical) in record.probes.iter().zip(&after.executions[2..]) {
            assert_eq!(probe.completed.as_ref().unwrap().execution_id, physical.id);
            assert!(
                physical.milliseconds.unwrap() >= probe.completed.as_ref().unwrap().milliseconds
            );
        }
        assert!(
            VerificationQueue::new(&b.store)
                .record_causal_return(&b.ctx(), &b.candidate, &proof)
                .unwrap()
        );
        let returned = b
            .store
            .read(|tx| tx.attributions(b.candidate.project))
            .unwrap();
        assert_eq!(
            returned[0].held, fixture_defect,
            "unproved mixed component must remain held"
        );
        assert_eq!(
            b.store
                .read(|tx| tx.gate_attempts(b.candidate.project))
                .unwrap()[0],
            after
        );
    }
}

#[test]
fn retained_allowance_and_unsettled_work_prevent_native_restarts() {
    for state in ["exhausted", "unfinished", "cleanup"] {
        let f = NativeFixture::new(false);
        let mut b = Board::new();
        b.fixture.github_checkout_at(
            b.fixture.project(),
            f.directory.path(),
            "https://github.com/acme/widgets",
        );
        b.candidate = VerificationQueue::new(&b.store).next().unwrap().unwrap();
        let owner = b.owner();
        let request = original(&b, &owner, &f, false);
        let (gate, _) = record::original(&b.store, &b.candidate, &owner)
            .unwrap()
            .unwrap();
        let mut prior = AttributionRecord {
            version: 1,
            id: "prior-evidence".into(),
            revision: 0,
            submission: cost::submission(&b.candidate),
            attempt: "prior-attempt".into(),
            inputs: gate.inputs,
            created_at: b.ctx().now(),
            components: vec![FailureComponent {
                id: "prior".into(),
                check: "prior unknown".into(),
                signature: "prior failure".into(),
                requirement: "retain earlier diagnosis".into(),
                log: request.log.display().to_string(),
                observed_cause: FailureCause::Unknown,
            }],
            preparation: None,
            plans: vec![],
            probes: vec![],
            assessments: vec![],
            diagnosis_ms: 0,
            held: true,
            retired: None,
        };
        b.store.write(|tx| tx.insert_attribution(&prior)).unwrap();
        if state == "exhausted" {
            prior.diagnosis_ms = MAX_DIAGNOSIS_MS;
        } else {
            prior.preparation = Some(DiagnosticPreparation {
                started_at: b.ctx().now(),
                completed: None,
            });
        }
        record::save(&b.store, &mut prior).unwrap();
        if state == "cleanup" {
            prior.preparation.as_mut().unwrap().completed = Some(PreparationResult {
                milliseconds: 1,
                log: request.log.display().to_string(),
                detail: "cleanup unproved".into(),
                cleanup_complete: false,
            });
            prior.diagnosis_ms = 1;
            record::save(&b.store, &mut prior).unwrap();
        }
        prior.held = false;
        prior.retired =
            Some("retired observation does not refund allowance or settle cleanup".into());
        record::save(&b.store, &mut prior).unwrap();
        let before = b
            .store
            .read(|tx| tx.gate_attempts(b.candidate.project))
            .unwrap();
        assert!(
            matches!(
                owner
                    .diagnose_rust_failure(&b.ctx(), &b.candidate, request)
                    .unwrap(),
                RustDiagnosisResult::Held { .. }
            ),
            "{state}"
        );
        let records = b
            .store
            .read(|tx| tx.attributions(b.candidate.project))
            .unwrap();
        assert_eq!(records[0], prior);
        assert_eq!(records.len(), 2);
        assert!(records[1].preparation.is_none());
        assert!(records[1].probes.is_empty());
        assert_eq!(
            b.store
                .read(|tx| tx.gate_attempts(b.candidate.project))
                .unwrap(),
            before
        );
    }
}
