//! Retain original components and reserve durable work before each native operation.
use super::*;
use std::{fs, io::Read, os::unix::fs::OpenOptionsExt};

pub(super) fn original(
    store: &impl Store,
    candidate: &VerificationCandidate,
    owner: &VerificationGuard,
) -> Result<Option<(GateExecution, i64)>, AppError> {
    Ok(store.read(|tx| {
        let attempts = tx.gate_attempts(candidate.project)?;
        let Some(attempt) = attempts.iter().find(|a| a.id == owner.active.attempt_id) else {
            return Ok(None);
        };
        let Some(control) = attempt.control_revision else {
            return Ok(None);
        };
        if !CausalReturnEvidence::permits_diagnosis(tx, candidate, &attempt.id, control)? {
            return Ok(None);
        }
        Ok(attempt
            .executions
            .iter()
            .rev()
            .find(|e| e.purpose.is_gate())
            .cloned()
            .map(|e| (e, control)))
    })?)
}

pub(super) fn select(
    original: &GateExecution,
    request: &RustDiagnosisRequest,
) -> Result<(usize, String), String> {
    if original.id != request.execution
        || original.finished_at.is_none()
        || original.estimated
        || !original.journal_bound
        || original.verdict.as_deref() != Some("tests-failed")
        || !original.diagnostics.is_empty()
        || !original.logs.contains(&request.log.display().to_string())
        || request.case.check_identity().is_none()
    {
        return Err("original gate does not bind the proposed complete native failure".into());
    }
    let matches: Vec<_> = original
        .failed_cases
        .iter()
        .enumerate()
        .filter(|(_, c)| request.case.matches_failure(c))
        .collect();
    let [(index, _)] = matches.as_slice() else {
        return Err("original gate lacks one unambiguous selected case".into());
    };
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&request.log)
        .map_err(|e| format!("open original log {}: {e}", request.log.display()))?;
    if !request.log.is_absolute() || !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("original evidence is not an absolute regular file".into());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read original log: {e}"))?;
    Ok((*index, request.case.original_failure(&bytes)?))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn begin<S: Store>(
    ctx: &Ctx<'_, S>,
    candidate: &VerificationCandidate,
    owner: &VerificationGuard,
    control: i64,
    original: &GateExecution,
    request: Option<&RustDiagnosisRequest>,
    selection: &Result<(usize, String), String>,
) -> Result<Option<(AttributionRecord, bool)>, AppError> {
    Ok(ctx.write_stories(|tx| {
        if owner.is_cancelled()
            || !CausalReturnEvidence::permits_diagnosis(
                tx,
                candidate,
                &owner.active.attempt_id,
                control,
            )?
        {
            return Ok(None);
        }
        let prior = tx.attributions(candidate.project)?;
        if let Some(existing) = prior.iter().find(|a| {
            a.attempt == owner.active.attempt_id
                && a.submission.same_generation(&cost::submission(candidate))
        }) {
            if existing.held && existing.retired.is_none() {
                crate::service::verification::clear_candidate_retry_incident(tx, candidate)?;
            }
            return Ok(Some((existing.clone(), false)));
        }
        // A different held attempt is a real hold, not this coordinator's own hold.
        if prior
            .iter()
            .any(|a| a.submission.same_generation(&cost::submission(candidate)) && a.held)
        {
            return Ok(None);
        }
        let mut components: Vec<_> = original
            .failed_cases
            .iter()
            .enumerate()
            .map(|(index, failure)| {
                let selected = selection.as_ref().ok().filter(|(i, _)| *i == index);
                FailureComponent {
                    id: format!("original-{index}"),
                    check: selected
                        .and_then(|_| request.and_then(|request| request.case.check_identity()))
                        .unwrap_or_else(|| {
                            format!(
                                "original:{}:{}:{}",
                                failure.path,
                                failure.target.as_deref().unwrap_or("unknown"),
                                failure.name.as_deref().unwrap_or("unknown")
                            )
                        }),
                    signature: selected
                        .map(|(_, signature)| signature.clone())
                        .unwrap_or_else(|| {
                            format!("uninterpreted original observation {failure:?}")
                        }),
                    requirement:
                        "Establish cause for this original failed check before assigning repair"
                            .into(),
                    log: original
                        .logs
                        .first()
                        .cloned()
                        .unwrap_or_else(|| original.journal_path.clone()),
                    observed_cause: FailureCause::Unknown,
                }
            })
            .collect();
        if let Ok((index, _)) = selection {
            components[*index].log = request.expect("selected request").log.display().to_string();
        }
        for (index, leg) in original.legs.iter().enumerate().filter(|(_, leg)| {
            matches!(leg.status.as_str(), "failed" | "fail" | "error")
                && !original.failed_cases.iter().any(|case| {
                    case.path == leg.path || case.path.starts_with(&format!("{}/", leg.path))
                })
        }) {
            components.push(FailureComponent {
                id: format!("original-leg-{index}"),
                check: format!("original-leg:{}", leg.path),
                signature: format!("original leg {}: {}", leg.path, leg.status),
                requirement: "Identify this failed leg and establish cause before repair".into(),
                log: original
                    .logs
                    .first()
                    .cloned()
                    .unwrap_or_else(|| original.journal_path.clone()),
                observed_cause: FailureCause::Unknown,
            });
        }
        if components.is_empty() {
            components.push(FailureComponent {
                id: "original-unknown".into(),
                check: "unidentified original failure".into(),
                signature: "original gate has no exact failed case".into(),
                requirement: "Identify the original failed check and establish cause".into(),
                log: original
                    .logs
                    .first()
                    .cloned()
                    .unwrap_or_else(|| original.journal_path.clone()),
                observed_cause: FailureCause::Unknown,
            });
        }
        let record = AttributionRecord {
            version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            revision: 0,
            submission: cost::submission(candidate),
            attempt: owner.active.attempt_id.clone(),
            inputs: original.inputs.clone(),
            created_at: ctx.now(),
            components,
            preparation: None,
            settlement: None,
            plans: vec![],
            probes: vec![],
            assessments: vec![],
            diagnosis_ms: 0,
            held: true,
            retired: None,
        };
        tx.insert_attribution(&record)?;
        crate::service::verification::clear_candidate_retry_incident(tx, candidate)?;
        Ok(Some((record, true)))
    })?)
}

pub(super) fn save(store: &impl Store, record: &mut AttributionRecord) -> Result<(), AppError> {
    let revision = record.revision;
    record.revision += 1;
    if !store.write(|tx| tx.update_attribution(record, revision))? {
        return Err(AppError::Storage(format!(
            "diagnosis {} changed at revision {revision}",
            record.id
        )));
    }
    Ok(())
}

pub(super) fn allowance(
    store: &impl Store,
    record: &AttributionRecord,
) -> Result<Option<u64>, AppError> {
    Ok(store.read(|tx| {
        let history = tx.attributions(record.submission.project)?;
        let same: Vec<_> = history
            .iter()
            .filter(|a| a.submission.same_generation(&record.submission))
            .collect();
        if same.iter().any(|a| a.has_unsettled_diagnosis()) {
            return Ok(None);
        }
        let starts = same.iter().map(|a| a.probes.len()).sum::<usize>();
        let spent = same
            .iter()
            .fold(0u64, |total, a| total.saturating_add(a.diagnosis_ms));
        Ok((starts <= MAX_PROBES - 4 && spent < MAX_DIAGNOSIS_MS)
            .then_some(MAX_DIAGNOSIS_MS.saturating_sub(spent)))
    })?)
}

pub(super) fn reserve(
    store: &impl Store,
    candidate: &VerificationCandidate,
    owner: &VerificationGuard,
    control: i64,
    record: &mut AttributionRecord,
) -> Result<bool, AppError> {
    let revision = record.revision;
    record.revision += 1;
    Ok(store.write(|tx| {
        if owner.is_cancelled()
            || !CausalReturnEvidence::permits_diagnosis(
                tx,
                candidate,
                &owner.active.attempt_id,
                control,
            )?
        {
            return Ok(false);
        }
        if !tx.update_attribution(record, revision)? {
            return Err(StoreError::Validation(
                "diagnosis reservation lost its evidence revision".into(),
            ));
        }
        Ok(true)
    })?)
}
