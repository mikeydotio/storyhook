//! Physical execution and explicit settlement, with retained cost at each boundary.
use super::*;
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
};

#[allow(clippy::too_many_arguments)]
pub(super) fn run<S: Store>(
    ctx: &Ctx<'_, S>,
    candidate: &VerificationCandidate,
    owner: &VerificationGuard,
    request: Option<&RustDiagnosisRequest>,
    selection: &Result<(usize, String), String>,
    record: &mut AttributionRecord,
    started: Instant,
    control: i64,
) -> Result<RustDiagnosisResult, AppError> {
    let component = match selection {
        Ok((index, _)) => record.components[*index].id.clone(),
        Err(detail) => {
            record.diagnosis_ms = elapsed(started);
            record::save(ctx.store(), record)?;
            return Ok(held(
                record,
                &format!("original evidence unsupported: {detail}"),
            ));
        }
    };
    let request = request.expect("selected native request");
    let Some(remaining) = record::allowance(ctx.store(), record)? else {
        return Ok(held(
            record,
            "retained diagnosis is unsettled or its allowance is exhausted",
        ));
    };
    let deadline = started + Duration::from_millis(remaining);
    record.preparation = Some(DiagnosticPreparation {
        started_at: ctx.now(),
        completed: None,
    });
    if !record::reserve(ctx.store(), candidate, owner, control, record)? {
        return Ok(RustDiagnosisResult::Superseded);
    }
    let archive =
        journal_path(ctx.env(), candidate).with_file_name(format!("attribution-{}", record.id));
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&archive)?;
    let prepared = cost::execute(
        ctx.store(),
        ctx.env(),
        owner,
        candidate,
        GateExecutionPurpose::DiagnosisPreparation {
            attribution: record.id.clone(),
        },
        record.inputs.clone(),
        vec![record.submission.clone()],
        |_| {
            NativeRustComparison::prepare(
                &candidate.checkout,
                &record.inputs,
                request.case.clone(),
                request.intervention.clone(),
                ctx.env().clone(),
                deadline,
                &owner.cancellation,
            )
        },
        |result| {
            Ok(match result {
                Ok(_) => ProbeOutcome::Passed,
                Err(error) => ProbeOutcome::Unavailable {
                    detail: error.to_string(),
                },
            })
        },
    )?;
    let mut native = match prepared {
        Ok(native) => native,
        Err(error) => {
            // Preparation may have failed during cleanup. Keep the reservation unsettled.
            record.diagnosis_ms = elapsed(started);
            record::save(ctx.store(), record)?;
            return Ok(held(record, &format!("native preparation failed: {error}")));
        }
    };
    #[cfg(test)]
    native.set_fixture(request.fixture.clone());
    let work = (|| -> Result<(), AppError> {
        retain(&archive.join("intervention.patch"), native.patch())?;
        let plan = native.plan(&component);
        retain(
            &archive.join("plan.json"),
            &serde_json::to_vec(&plan)
                .map_err(|e| AppError::Storage(format!("encode native plan: {e}")))?,
        )?;
        record.plans.push(plan);
        record.preparation.as_mut().expect("reserved").completed = Some(PreparationResult {
            milliseconds: elapsed(started),
            log: archive.display().to_string(),
            detail: "Native pinned trees and closed detector prepared; exact intervention archived"
                .into(),
            cleanup_complete: true,
        });
        checkpoint(ctx.store(), record, started)?;
        for side in [
            ProbeSide::Candidate,
            ProbeSide::Control,
            ProbeSide::Control,
            ProbeSide::Candidate,
        ] {
            if Instant::now() >= deadline || owner.is_cancelled() {
                return Err(AppError::Validation(
                    "diagnosis cancelled or active allowance exhausted".into(),
                ));
            }
            checkpoint(ctx.store(), record, started)?;
            let id = uuid::Uuid::new_v4().to_string();
            record.probes.push(DiagnosticProbe {
                id: id.clone(),
                plan: 0,
                side,
                started_at: ctx.now(),
                completed: None,
            });
            if !record::reserve(ctx.store(), candidate, owner, control, record)? {
                return Err(AppError::Validation(
                    "diagnosis authority changed before launch".into(),
                ));
            }
            let output = archive.join(&id);
            let result = cost::execute(
                ctx.store(),
                ctx.env(),
                owner,
                candidate,
                GateExecutionPurpose::Diagnosis {
                    attribution: record.id.clone(),
                    probe: id.clone(),
                },
                record.inputs.clone(),
                vec![record.submission.clone()],
                |execution| {
                    native.execute(
                        side,
                        &NativeProbeBinding {
                            project: &candidate.project_slug,
                            attempt: &record.attempt,
                            execution: &execution.id,
                            generation: candidate.verifying_generation.map_or(0, |g| g.get()),
                            request: &id,
                            journal: std::path::Path::new(&execution.journal_path),
                            output: &output,
                            termination_grace: RECOVERY_WAKE,
                        },
                    )
                },
                |result| {
                    Ok(match result {
                        Ok(result) => result.outcome.clone(),
                        Err(error) => ProbeOutcome::Unavailable {
                            detail: error.to_string(),
                        },
                    })
                },
            )??;
            let complete = result.cleanup_complete
                && result.executions == 1
                && !matches!(result.outcome, ProbeOutcome::Unavailable { .. });
            record.probes.last_mut().expect("reserved").completed = Some(result);
            checkpoint(ctx.store(), record, started)?;
            if !complete {
                return Err(AppError::Validation(
                    "native execution or cleanup is unavailable; inspect retained output".into(),
                ));
            }
        }
        Ok(())
    })();
    // Always consume the owner explicitly, including every failure after preparation.
    let settled = native.settle();
    // A revoked reservation may not have committed the proposed next probe. Charge only
    // the durable record, never the local proposal that the authority fence rejected.
    *record = ctx.store().read(|tx| {
        tx.attributions(candidate.project)?
            .into_iter()
            .find(|a| a.id == record.id)
            .ok_or_else(|| {
                StoreError::Corrupt("diagnosis disappeared during native settlement".into())
            })
    })?;
    record.settlement = Some(DiagnosticSettlement {
        completed_at: ctx.now(),
        milliseconds: elapsed(started),
        cleanup_complete: settled.is_ok(),
        detail: match &settled {
            Ok(_) => "Native comparison resources explicitly settled".into(),
            Err(error) => error.to_string(),
        },
    });
    checkpoint(ctx.store(), record, started)?;
    match (work, settled) {
        (Ok(()), Ok(settled)) => {
            if !ctx.store().read(|tx| {
                CausalReturnEvidence::permits_diagnosis(tx, candidate, &record.attempt, control)
            })? {
                return Ok(RustDiagnosisResult::Superseded);
            }
            match ctx
                .store()
                .read(|tx| settled.prove(tx, candidate, &record.id, &request.execution))
            {
                Ok(proof) => Ok(RustDiagnosisResult::Proven(Box::new(proof))),
                Err(error) => Ok(held(
                    record,
                    &format!("native evidence does not prove candidate cause: {error}"),
                )),
            }
        }
        (work, settled) => {
            let errors: Vec<_> = [work.err(), settled.err()]
                .into_iter()
                .flatten()
                .map(|e| e.to_string())
                .collect();
            Ok(held(record, &errors.join("; ")))
        }
    }
}

fn checkpoint(
    store: &impl Store,
    record: &mut AttributionRecord,
    started: Instant,
) -> Result<(), AppError> {
    let measured = record
        .preparation
        .as_ref()
        .and_then(|p| p.completed.as_ref())
        .map_or(0, |p| p.milliseconds)
        .saturating_add(
            record
                .probes
                .iter()
                .filter_map(|p| p.completed.as_ref())
                .fold(0u64, |total, p| total.saturating_add(p.milliseconds)),
        );
    record.diagnosis_ms = record.diagnosis_ms.max(elapsed(started)).max(measured);
    record::save(store, record)
}

fn retain(path: &std::path::Path, bytes: &[u8]) -> Result<(), AppError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}
