//! Serialized evidence cannot replace native execution or refund prior allowance.
use super::*;

pub(super) fn records(
    evidence: &Evidence,
    settled: &SettledRustComparison,
    record: &AttributionRecord,
) {
    let conn = rusqlite::Connection::open(evidence.store.path()).unwrap();
    let original = serde_json::to_string(record).unwrap();
    let mutations: &[fn(&mut AttributionRecord)] = &[
        |r| r.inputs.head = Some("e".repeat(40)),
        |r| r.inputs.base = Some("e".repeat(40)),
        |r| r.components[0].check = "foreign-case".into(),
        |r| r.components[0].signature = "foreign-failure".into(),
        |r| r.plans[0].argv.push("--ignored".into()),
        |r| r.plans[0].detector = "forged-detector".into(),
        |r| r.preparation = None,
        |r| r.probes[1].side = ProbeSide::Candidate,
        |r| r.probes[0].id = "unreserved-request".into(),
        |r| r.probes[0].completed.as_mut().unwrap().execution_id = "foreign-execution".into(),
        |r| r.probes[0].completed.as_mut().unwrap().cleanup_complete = false,
        |r| {
            r.probes[0]
                .completed
                .as_mut()
                .unwrap()
                .environment
                .as_mut()
                .unwrap()
                .supported = false
        },
        |r| {
            r.probes[0]
                .completed
                .as_mut()
                .unwrap()
                .environment
                .as_mut()
                .unwrap()
                .toolchain = "forged-toolchain".into()
        },
        |r| r.probes[0].completed.as_mut().unwrap().outcome = ProbeOutcome::Passed,
        |r| {
            r.probes.pop();
        },
        |r| r.diagnosis_ms = MAX_DIAGNOSIS_MS,
        |r| r.diagnosis_ms = 0,
    ];
    for (index, mutate) in mutations.iter().enumerate() {
        let mut changed = record.clone();
        mutate(&mut changed);
        conn.execute(
            "UPDATE verification_attributions SET payload=?1 WHERE id=?2",
            rusqlite::params![serde_json::to_string(&changed).unwrap(), record.id],
        )
        .unwrap();
        assert!(
            evidence
                .store
                .read(|tx| settled.prove(tx, &evidence.candidate, &record.id, "original"))
                .is_err(),
            "forged observation {index} minted a capability"
        );
        conn.execute(
            "UPDATE verification_attributions SET payload=?1 WHERE id=?2",
            rusqlite::params![original, record.id],
        )
        .unwrap();
    }
}

pub(super) fn history(
    evidence: &Evidence,
    settled: &SettledRustComparison,
    record: &AttributionRecord,
) {
    use crate::store::StoreError;
    for change in [
        "exhausted",
        "retired-exhausted",
        "unsettled",
        "contradictory",
    ] {
        let result = evidence.store.write(|tx| {
            let mut prior = record.clone();
            prior.id = "prior-diagnosis".into();
            prior.attempt = "prior-attempt".into();
            prior.revision = 0;
            prior.preparation = None;
            prior.plans.clear();
            prior.probes.clear();
            prior.assessments.clear();
            prior.diagnosis_ms = 0;
            if change == "contradictory" {
                prior.components[0].signature = "different original assertion".into();
            }
            tx.insert_attribution(&prior)?;
            if change != "contradictory" {
                prior.revision = 1;
                if change == "unsettled" {
                    prior.preparation = Some(DiagnosticPreparation {
                        started_at: prior.created_at.clone(),
                        completed: None,
                    });
                } else {
                    prior.diagnosis_ms = MAX_DIAGNOSIS_MS;
                    if change == "retired-exhausted" {
                        prior.held = false;
                        prior.retired = Some("earlier attempt ended".into());
                    }
                }
                assert!(tx.update_attribution(&prior, 0)?);
            }
            assert!(
                settled
                    .prove(tx, &evidence.candidate, &record.id, "original")
                    .is_err(),
                "prior {change} was omitted"
            );
            Err::<(), _>(StoreError::Validation("rollback prior history".into()))
        });
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains("rollback prior history"),
            "{change}: {error}"
        );
    }
    let conn = rusqlite::Connection::open(evidence.store.path()).unwrap();
    let attempt = evidence
        .store
        .read(|tx| tx.gate_attempts(evidence.candidate.project))
        .unwrap()
        .pop()
        .unwrap();
    let mutations: &[fn(&mut GateAttempt)] = &[
        |a| a.control_revision = None,
        |a| a.executions[0].journal_bound = false,
        |a| a.executions[0].failed_cases.clear(),
        |a| a.executions[0].logs.clear(),
        |a| a.executions[0].inputs.tree = Some("e".repeat(40)),
        |a| a.executions[0].finished_at = None,
        |a| a.executions[0].estimated = true,
        |a| a.executions[0].verdict = Some("certified".into()),
        |a| a.executions[1].purpose = GateExecutionPurpose::Gate,
        |a| a.executions[1].finished_at = None,
        |a| a.executions[2].purpose = GateExecutionPurpose::Gate,
        |a| a.executions[2].submissions.clear(),
        |a| a.executions[2].journal_bound = false,
        |a| a.executions[2].verdict = Some("passed".into()),
        |a| a.executions[2].milliseconds = None,
        |a| {
            a.executions.pop();
        },
    ];
    for (index, mutate) in mutations.iter().enumerate() {
        let mut changed = attempt.clone();
        mutate(&mut changed);
        conn.execute(
            "UPDATE gate_attempts SET payload=?1 WHERE id=?2",
            rusqlite::params![serde_json::to_string(&changed).unwrap(), attempt.id],
        )
        .unwrap();
        assert!(
            evidence
                .store
                .read(|tx| settled.prove(tx, &evidence.candidate, &record.id, "original"))
                .is_err(),
            "foreign physical evidence {index} minted a proof"
        );
        conn.execute(
            "UPDATE gate_attempts SET payload=?1 WHERE id=?2",
            rusqlite::params![serde_json::to_string(&attempt).unwrap(), attempt.id],
        )
        .unwrap();
    }
}
