//! Original conflict observations retained before native source inspection.
//! Neither a pending record nor its deserialized candidate permits an effect.
use super::*;
use crate::{
    service::{
        VerificationCandidate,
        attribution::{AttributionRecord, FailureCause},
    },
    store::{GateAttempt, GlobalSeq, IntegrationPending, ProjectId, ReadOps, StoreError, WriteOps},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    version: u8,
    candidate: VerificationCandidate,
    attribution: AttributionRecord,
    admitted_at: String,
    control: i64,
    labels: Option<GlobalSeq>,
}

/// The worker may inspect this original candidate but must obtain fresh native
/// policy/source proof and a durable owner before assembly or publication.
pub(crate) struct PendingIntegration {
    pub candidate: VerificationCandidate,
    pub attribution: String,
    pub component: String,
    pub retained_head: String,
}

/// Part of the original conflict-hold transaction. Unsupported/legacy missing
/// input evidence keeps the ordinary hold and gains no automatic recovery path.
pub(crate) fn retain(
    tx: &mut impl WriteOps,
    candidate: &VerificationCandidate,
    attribution: &AttributionRecord,
    attempt: &GateAttempt,
) -> Result<(), StoreError> {
    let Some(generation) = candidate.verifying_generation else {
        return Ok(());
    };
    if [&attribution.inputs.head, &attribution.inputs.base]
        .iter()
        .any(|oid| {
            oid.as_deref().is_none_or(|head| {
                !matches!(head.len(), 40 | 64) || !head.bytes().all(|b| b.is_ascii_hexdigit())
            })
        })
        || !attribution
            .components
            .iter()
            .any(|c| c.observed_cause == FailureCause::Integration)
        || attempt.elapsed.estimated
        || attempt.finished_at.is_some()
        || !attempt.executions.last().is_some_and(|e| {
            e.finished_at.is_some()
                && e.purpose.is_gate()
                && e.journal_bound
                && e.submissions.contains(&attribution.submission)
                && !e.estimated
                && e.verdict.as_deref() == Some("conflict")
                && e.inputs == attribution.inputs
        })
    {
        return Ok(());
    }
    let story = attribution
        .submission
        .story_number()
        .ok_or_else(|| invalid("original story missing"))?;
    let control = tx.verification_control_revision(candidate.project)?;
    if attempt.control_revision != Some(control)
        || attempt.submission != attribution.submission
        || attempt.id != attribution.attempt
        || !crate::service::verification::recovery_cleanup_history_is_current(tx, candidate)?
    {
        return Err(invalid(
            "original conflict custody changed before retention",
        ));
    }
    let observation = Observation {
        version: 1,
        candidate: candidate.clone(),
        attribution: attribution.clone(),
        admitted_at: attempt.admitted_at.clone(),
        control,
        labels: crate::service::project_recovery::recovery_label_revision(
            tx,
            candidate.project,
            story,
        )?,
    };
    let record = IntegrationPending {
        id: attribution.id.clone(),
        project: candidate.project,
        story,
        generation,
        evidence: serde_json::to_value(observation).map_err(|e| invalid(&e.to_string()))?,
    };
    decode(&record)?;
    tx.insert_integration_pending(&record)
}

pub(crate) fn subjects(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<Vec<PendingIntegration>, StoreError> {
    let owners = tx.integration_recoveries(project)?;
    let attributions = tx.attributions(project)?;
    let mut result = Vec::new();
    for record in tx.integration_pending(project)? {
        let observed = decode(&record)?;
        if owners.iter().any(|o| o.active && o.story == record.story)
            || !attributions.contains(&observed.attribution)
        {
            continue;
        }
        match validate_current(tx, &observed) {
            Ok(()) => {}
            // A legitimate stop, label/lease change or new generation revokes
            // only this observation. Preserve its custody and ordinary hold;
            // do not hide independent original subjects in the same project.
            Err(StoreError::Validation(_)) => continue,
            Err(error) => return Err(error),
        }
        for component in &observed.attribution.components {
            if component.observed_cause == FailureCause::Integration {
                result.push(PendingIntegration {
                    candidate: observed.candidate.clone(),
                    attribution: observed.attribution.id.clone(),
                    component: component.id.clone(),
                    retained_head: observed
                        .attribution
                        .inputs
                        .head
                        .clone()
                        .ok_or_else(|| invalid("retained integration head missing"))?,
                });
            }
        }
    }
    Ok(result)
}

/// Mandatory original custody for native clean readmission; absence grants nothing.
pub(super) fn required_original(
    tx: &impl ReadOps,
    subject: &PendingIntegration,
) -> Result<AttributionRecord, StoreError> {
    let record = tx
        .integration_pending(subject.candidate.project)?
        .into_iter()
        .find(|r| r.id == subject.attribution)
        .ok_or_else(|| invalid("original pending custody is missing"))?;
    let observation = decode(&record)?;
    if observation.candidate != subject.candidate
        || observation.attribution.inputs.head.as_deref() != Some(subject.retained_head.as_str())
        || !observation
            .attribution
            .components
            .iter()
            .any(|c| c.id == subject.component && c.observed_cause == FailureCause::Integration)
    {
        return Err(invalid(
            "clean readmission differs from original pending subject",
        ));
    }
    validate_current(tx, &observation)?;
    Ok(observation.attribution)
}

pub(super) fn validate_retained(
    tx: &impl ReadOps,
    candidate: &VerificationCandidate,
    attribution: &AttributionRecord,
) -> Result<(), StoreError> {
    if let Some(record) = tx
        .integration_pending(candidate.project)?
        .iter()
        .find(|r| r.id == attribution.id)
    {
        let observed = decode(record)?;
        if &observed.candidate != candidate || &observed.attribution != attribution {
            return Err(invalid(
                "original pending candidate or attribution was replaced",
            ));
        }
        validate_current(tx, &observed)?;
    }
    Ok(())
}

fn validate_current(tx: &impl ReadOps, observed: &Observation) -> Result<(), StoreError> {
    let candidate = &observed.candidate;
    let story = observed
        .attribution
        .submission
        .story_number()
        .ok_or_else(|| invalid("pending story missing"))?;
    super::owner::check_candidate(tx, candidate)?;
    if tx.verification_control_revision(candidate.project)? != observed.control
        || crate::service::project_recovery::recovery_label_revision(tx, candidate.project, story)?
            != observed.labels
        || !tx
            .attributions(candidate.project)?
            .contains(&observed.attribution)
        || !tx.gate_attempts(candidate.project)?.iter().any(|a| {
            a.id == observed.attribution.attempt
                && a.submission == observed.attribution.submission
                && a.admitted_at == observed.admitted_at
                && a.control_revision == Some(observed.control)
        })
    {
        return Err(invalid(
            "pending original operator, labels or admission custody changed",
        ));
    }
    Ok(())
}

fn decode(record: &IntegrationPending) -> Result<Observation, StoreError> {
    let observed: Observation = serde_json::from_value(record.evidence.clone())
        .map_err(|e| StoreError::Corrupt(format!("pending integration {}: {e}", record.id)))?;
    if observed.version != 1
        || observed.attribution.id != record.id
        || observed.candidate.project != record.project
        || observed.attribution.submission.project != record.project
        || observed.attribution.submission.story_number() != Some(record.story)
        || observed.candidate.verifying_generation != Some(record.generation)
        || observed.attribution.submission.generation != Some(record.generation)
        || !observed.attribution.held
        || observed.attribution.retired.is_some()
    {
        return Err(StoreError::Corrupt(format!(
            "pending integration {} changed identity",
            record.id
        )));
    }
    observed.attribution.validate()?;
    chrono::DateTime::parse_from_rfc3339(&observed.admitted_at)
        .map_err(|e| StoreError::Corrupt(e.to_string()))?;
    Ok(observed)
}

fn invalid(detail: &str) -> StoreError {
    StoreError::Validation(format!("pending integration: {detail}"))
}
