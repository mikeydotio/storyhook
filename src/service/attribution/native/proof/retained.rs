//! Bounded raw evidence and full-history checks shared by minting and application.
use super::*;
use crate::store::{GateAttempt, GateExecutionPurpose};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, path::PathBuf};

#[derive(Clone, serde::Serialize)]
pub(in super::super) struct Archive {
    path: PathBuf,
    pub(super) digest: String,
}

impl Archive {
    pub(in super::super) fn probe(output: &Path) -> Result<Vec<Self>, AppError> {
        let mut names = vec![
            "request.json".into(),
            "observation.json".into(),
            "resource.json".into(),
        ];
        for stage in [
            "metadata",
            "build",
            "listing",
            "run",
            "cargo-version",
            "rustc-version",
            "driver",
        ] {
            for stream in ["stdout", "stderr"] {
                names.push(format!("{stage}.{stream}"));
            }
        }
        names
            .into_iter()
            .map(|name| {
                Self::capture(&output.join(name))
                    .map(|(archive, _)| archive)
                    .map_err(AppError::from)
            })
            .collect()
    }

    fn capture(path: &Path) -> Result<(Self, Vec<u8>), StoreError> {
        if !path.is_absolute() {
            return Err(refused("raw evidence path is not absolute"));
        }
        let bytes = pipeline::read(path, 16 * 1024 * 1024).map_err(|e| refused(&e))?;
        Ok((
            Self {
                path: path.into(),
                digest: format!("{:x}", Sha256::digest(&bytes)),
            },
            bytes,
        ))
    }

    pub(super) fn verify(&self) -> Result<(), StoreError> {
        let (current, _) = Self::capture(&self.path)?;
        if self.digest != current.digest {
            return Err(refused(&format!(
                "retained raw evidence changed: {}",
                self.path.display()
            )));
        }
        Ok(())
    }
}

pub(super) fn check(case: &RustCase) -> String {
    let target = match &case.target {
        RustTarget::Integration(target) => target,
        _ => return String::new(),
    };
    format!("rust:{}:{target}:{}", case.package, case.name)
}

pub(super) fn original(
    attempt: &GateAttempt,
    execution: &str,
    record: &AttributionRecord,
    component: &FailureComponent,
    case: &RustCase,
) -> Result<Archive, StoreError> {
    let target = match &case.target {
        RustTarget::Integration(target) => target,
        _ => return Err(refused("unsupported original case")),
    };
    let Some(gate) = attempt.executions.iter().find(|e| e.id == execution) else {
        return Err(refused("original execution missing"));
    };
    if !gate.purpose.is_gate()
        || attempt
            .executions
            .iter()
            .rev()
            .find(|e| e.purpose.is_gate())
            != Some(gate)
        || gate.finished_at.is_none()
        || gate.estimated
        || !gate.journal_bound
        || gate.verdict.as_deref() != Some("tests-failed")
        || gate.inputs != record.inputs
        || !gate.submissions.contains(&record.submission)
        || !gate.logs.contains(&component.log)
        || !gate.diagnostics.is_empty()
        || gate
            .failed_cases
            .iter()
            .filter(|c| {
                c.name.as_ref() == Some(&case.name)
                    && c.target.as_ref() == Some(target)
                    && !c.path.trim().is_empty()
            })
            .count()
            != 1
    {
        return Err(refused(
            "original gate does not bind this exact completed failure",
        ));
    }
    let (archive, bytes) = Archive::capture(Path::new(&component.log))?;
    if case.original_failure(&bytes).map_err(|e| refused(&e))? != component.signature {
        return Err(refused(
            "original assertion differs from the native contrast",
        ));
    }
    Ok(archive)
}

pub(super) fn executions(
    attempt: &GateAttempt,
    record: &AttributionRecord,
    plan: usize,
) -> Result<(), StoreError> {
    if record.preparation.as_ref().is_none_or(|p| p.unsettled()) {
        return Err(refused("preparation was not durably settled"));
    }
    let preparation: Vec<_> = attempt
        .executions
        .iter()
        .filter(|e| {
            e.purpose
                == (GateExecutionPurpose::DiagnosisPreparation {
                    attribution: record.id.clone(),
                })
        })
        .collect();
    let [preparation] = preparation.as_slice() else {
        return Err(refused("one physical preparation execution is required"));
    };
    if preparation.finished_at.is_none()
        || preparation.estimated
        || !preparation.journal_bound
        || preparation.verdict.as_deref() != Some("passed")
        || !preparation.diagnostics.is_empty()
        || preparation.inputs != record.inputs
        || preparation.submissions != vec![record.submission.clone()]
        || preparation.milliseconds.is_none()
    {
        return Err(refused("preparation execution is incomplete or foreign"));
    }
    for probe in record.probes.iter().filter(|p| p.plan == plan) {
        let result = probe
            .completed
            .as_ref()
            .ok_or_else(|| refused("unfinished physical probe"))?;
        let expected = match result.outcome {
            ProbeOutcome::Passed => "passed",
            ProbeOutcome::Failed { .. } => "failed",
            _ => return Err(refused("unavailable native result")),
        };
        let Some(execution) = attempt
            .executions
            .iter()
            .find(|e| e.id == result.execution_id)
        else {
            return Err(refused("physical diagnostic execution missing"));
        };
        if execution.purpose
            != (GateExecutionPurpose::Diagnosis {
                attribution: record.id.clone(),
                probe: probe.id.clone(),
            })
            || execution.finished_at.is_none()
            || execution.estimated
            || !execution.journal_bound
            || execution.verdict.as_deref() != Some(expected)
            || !execution.diagnostics.is_empty()
            || execution
                .milliseconds
                .is_none_or(|ms| ms < result.milliseconds)
            || execution.inputs != record.inputs
            || execution.submissions != vec![record.submission.clone()]
        {
            return Err(refused(
                "physical execution differs from retained native probe",
            ));
        }
    }
    if attempt.executions.iter().any(|e| matches!(&e.purpose, GateExecutionPurpose::Diagnosis { attribution, probe } if attribution == &record.id && !record.probes.iter().any(|p| &p.id == probe && p.completed.as_ref().is_some_and(|r| r.execution_id == e.id)))) {
        return Err(refused("unaccounted diagnostic execution"));
    }
    Ok(())
}

pub(super) fn history(
    history: &[AttributionRecord],
    record: &AttributionRecord,
    component: &FailureComponent,
    native_ms: u64,
) -> Result<(), StoreError> {
    let mut starts = 0usize;
    let mut milliseconds = 0u64;
    let mut ids = BTreeSet::new();
    let mut executions = BTreeSet::new();
    for other in history {
        starts = starts.saturating_add(other.probes.len());
        milliseconds = milliseconds.saturating_add(if other.id == record.id {
            other.diagnosis_ms.max(native_ms)
        } else {
            other.diagnosis_ms
        });
        if other.has_unsettled_diagnosis() || !ids.insert(&other.id) {
            return Err(refused("prior diagnosis is unsettled or duplicated"));
        }
        if other.id != record.id
            && other.components.iter().any(|c| c.check == component.check)
            && (!other.probes.is_empty()
                || other.inputs != record.inputs
                || other
                    .components
                    .iter()
                    .any(|c| c.check == component.check && c.signature != component.signature))
        {
            return Err(refused("earlier evidence for this check cannot be omitted"));
        }
        for probe in &other.probes {
            if let Some(result) = &probe.completed
                && !executions.insert(&result.execution_id)
            {
                return Err(refused(
                    "physical execution reused across diagnosis history",
                ));
            }
        }
    }
    if starts > MAX_PROBES || milliseconds >= MAX_DIAGNOSIS_MS {
        return Err(refused("cumulative diagnosis allowance exhausted"));
    }
    Ok(())
}

pub(super) fn authority(
    tx: &impl ReadOps,
    candidate: &VerificationCandidate,
    attempt_id: &str,
    control: i64,
) -> Result<bool, StoreError> {
    let Some(project) = tx.project(candidate.project)? else {
        return Ok(false);
    };
    let story = crate::store::StoryNo::parse_id(&project.prefix, &candidate.story_id)?;
    let Some(row) = tx.story(candidate.project, story)? else {
        return Ok(false);
    };
    if candidate.verifying_generation.is_none()
        || row.awaiting.is_some()
        || candidate.landing_pending
        || !candidate.blocked_by.is_empty()
        || !tx.verification_enabled(candidate.project)?
        || tx.verification_control_revision(candidate.project)? != control
        || tx
            .landing_intents()?
            .iter()
            .any(|i| i.project == candidate.project && i.story == story)
        || !crate::service::verification::candidate_is_current(tx, &row, candidate)?
        || !crate::service::verification::submission_is_current(tx, &row, candidate)?
    {
        return Ok(false);
    }
    let attempts = tx.gate_attempts(candidate.project)?;
    Ok(attempts
        .iter()
        .rev()
        .find(|a| {
            a.submission
                .matches_story(candidate.project, &candidate.story_id)
        })
        .is_some_and(|a| {
            a.id == attempt_id
                && a.submission.generation == candidate.verifying_generation
                && a.finished_at.is_none()
                && a.control_revision == Some(control)
                && a.mode == crate::domain::landing::VerificationMode::Gated
                && a.verdict.as_deref() == Some("tests-failed")
        }))
}
