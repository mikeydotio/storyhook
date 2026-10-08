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
    for (fixture_defect, candidate_only) in [(false, false), (true, false), (false, true)] {
        let f = NativeFixture::new_case(fixture_defect, candidate_only);
        let mut b = Board::new();
        b.fixture.github_checkout_at(
            b.fixture.project(),
            f.directory.path(),
            "https://github.com/acme/widgets",
        );
        b.candidate = VerificationQueue::new(&b.store)
            .with_environment(b.env.clone())
            .next()
            .unwrap()
            .unwrap();
        let owner = b.owner();
        let original_request = original(&b, &owner, &f, fixture_defect);
        let (gate, _) = record::original(&b.store, &b.candidate, &owner)
            .unwrap()
            .unwrap();
        let mut request = selection::propose(&gate, &b.candidate.checkout).unwrap();
        assert_eq!(request.execution, original_request.execution);
        assert_eq!(request.log, original_request.log);
        request.fixture = Some(f.config.clone());
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
            matches!(
                record.plans[0].relation,
                DetectorRelation::Transplant { .. }
            ),
            candidate_only
        );
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
        let actuator = Delivery {
            head: Mutex::new(None),
            messages: Mutex::new(Vec::new()),
        };
        for head in [None, Some("f".repeat(40))] {
            *actuator.head.lock().unwrap() = head;
            assert_eq!(
                return_for_repair(
                    &VerificationQueue::new(&b.store).with_environment(b.env.clone()),
                    &b.ctx(),
                    &actuator,
                    &b.candidate,
                    &proof,
                    &owner,
                    ReservationReason::Remediation
                )
                .unwrap(),
                GenerationWrite::Applied(false)
            );
            assert!(actuator.messages.lock().unwrap().is_empty());
            assert_eq!(
                b.store
                    .read(|tx| tx.attributions(b.candidate.project))
                    .unwrap(),
                records
            );
        }
        *actuator.head.lock().unwrap() = Some(proof.submitted_head().into());
        assert_eq!(
            return_for_repair(
                &VerificationQueue::new(&b.store).with_environment(b.env.clone()),
                &b.ctx(),
                &actuator,
                &b.candidate,
                &proof,
                &owner,
                ReservationReason::Remediation
            )
            .unwrap(),
            GenerationWrite::Applied(true)
        );
        assert_eq!(*actuator.messages.lock().unwrap(), [proof.diagnosis()]);
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
        b.candidate = VerificationQueue::new(&b.store)
            .with_environment(b.env.clone())
            .next()
            .unwrap()
            .unwrap();
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
            settlement: None,
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

struct Delivery {
    head: Mutex<Option<String>>,
    messages: Mutex<Vec<String>>,
}
impl VerificationActuator for Delivery {
    fn current_pr_head(&self, _: &VerificationCandidate) -> Result<String, AppError> {
        self.head
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| AppError::Storage("metadata unavailable".into()))
    }
    fn notify(&self, _: &VerificationCandidate, message: &str) -> Result<NotifyDelivery, AppError> {
        self.messages.lock().unwrap().push(message.into());
        Ok(NotifyDelivery::Delivered)
    }
    fn submit(&self, _: &VerificationCandidate) -> Result<SubmissionOutcome, SubmissionFailure> {
        panic!("no submission")
    }
    fn verify(&self, _: &VerificationCandidate, _: &PrLink) -> VerificationOutcome {
        panic!("no recertification")
    }
    fn land(&self, _: &VerificationCandidate, _: &crate::store::LandingIntent) -> LandingOutcome {
        panic!("no landing")
    }
    fn recover_landing(
        &self,
        _: &VerificationCandidate,
        _: &crate::store::LandingIntent,
    ) -> LandingOutcome {
        panic!("no landing recovery")
    }
    fn redispatch(&self, _: &VerificationCandidate, _: &ResumePlan) -> Result<(), AppError> {
        panic!("no redispatch")
    }
    fn reap(&self, _: &VerificationCandidate) -> Result<(), AppError> {
        panic!("no reap")
    }
}

#[test]
fn retained_repair_lineage_requires_the_same_native_capability_for_a_new_return() {
    use crate::service::project_recovery::{
        ProjectRecoveryService, RepairInput, RepairJudgment, WorkStatus,
    };
    let f = NativeFixture::new(false);
    let mut b = Board::new();
    b.fixture.github_checkout_at(
        b.fixture.project(),
        f.directory.path(),
        "https://github.com/acme/widgets",
    );
    b.candidate = VerificationQueue::new(&b.store)
        .with_environment(b.env.clone())
        .next()
        .unwrap()
        .unwrap();
    let recovery = super::recovery::retained_lineage(&mut b);
    let owner = b.owner();
    let ctx = b.ctx();
    let service = ProjectRecoveryService::new(&ctx);
    let input = RepairInput {
        head: f.git(&["rev-parse", "HEAD"]),
        head_tree: f.git(&["rev-parse", "HEAD^{tree}"]),
        base: f.base.clone(),
        tree: f.git(&["rev-parse", "HEAD^{tree}"]),
    };
    service
        .admit_repair(&b.candidate, &owner.active.attempt_id, &input)
        .unwrap();
    let request = original(&b, &owner, &f, false);
    service
        .complete_repair(
            &b.candidate,
            &owner.active.attempt_id,
            &RepairJudgment::TestsFailed {
                tree: input.tree.clone(),
            },
        )
        .unwrap();
    let RustDiagnosisResult::Proven(proof) = owner
        .diagnose_rust_failure(&ctx, &b.candidate, request)
        .unwrap()
    else {
        panic!("real contrast must prove cause");
    };
    let before = service.show(&recovery).unwrap();
    assert!(
        service
            .return_failed_repair(
                &b.candidate,
                &owner.active.attempt_id,
                &input.tree,
                "raw text"
            )
            .is_err()
    );
    assert_eq!(service.show(&recovery).unwrap(), before);
    let actuator = Delivery {
        head: Mutex::new(Some(proof.submitted_head().into())),
        messages: Mutex::new(vec![]),
    };
    assert_eq!(
        return_for_repair(
            &VerificationQueue::new(&b.store).with_environment(b.env.clone()),
            &ctx,
            &actuator,
            &b.candidate,
            &proof,
            &owner,
            ReservationReason::Remediation
        )
        .unwrap(),
        GenerationWrite::Applied(true)
    );
    assert!(
        actuator.messages.lock().unwrap().is_empty(),
        "recovery owns its durable delivery"
    );
    let returned = service.show(&recovery).unwrap();
    assert_eq!(returned.state.decision, before.state.decision);
    assert_eq!(returned.state.attempts, before.state.attempts);
    assert_eq!(returned.state.work.len(), 1);
    assert_eq!(returned.state.work[0].status, WorkStatus::Pending);
    assert_eq!(
        returned.state.work[0].source_attempt.as_deref(),
        Some(owner.active.attempt_id.as_str())
    );
    assert!(returned.state.subjects.last().unwrap().returned);
    assert!(service.return_proven_repair(&b.candidate, &proof).unwrap());
    assert_eq!(
        service.show(&recovery).unwrap(),
        returned,
        "no duplicate effect on replay"
    );
}
