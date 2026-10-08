//! Durable observations before native broker availability. These facts retain
//! original custody and a hold, but cannot pause the host or release a subject.
use super::*;
use crate::{
    domain::StoryEvent,
    service::attribution::FailureComponent,
    store::{ExpectedSeq, HostRecoveryPending, WriteOps},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Retained {
    version: u8,
    subject: Subject,
}

/// One immutable original candidate awaiting a fresh native proof. The worker
/// may inspect it but must use observe_fault/observe_restoration for authority.
pub(crate) struct PendingSubject {
    pub candidate: VerificationCandidate,
    pub attribution: String,
    pub execution: String,
    pub component: String,
}

/// Called only for a completed infrastructure-failure physical gate while its
/// existing verifier owner is applying the result. No new process is started.
pub(crate) fn retain_failed_pressure<S: Store>(
    ctx: &Ctx<'_, S>,
    candidate: &VerificationCandidate,
    attempt_id: &str,
) -> Result<bool, AppError> {
    if candidate.project != ctx.project() {
        return Err(invalid("foreign pending host subject").into());
    }
    let now = ctx.now();
    ctx.write_stories(|tx| {
        let attempts=tx.gate_attempts(candidate.project)?;
        let Some(attempt)=attempts.iter().rev().find(|a|a.submission.matches_story(candidate.project,&candidate.story_id)).filter(|a| a.id==attempt_id && a.finished_at.is_none() && a.submission.generation==candidate.verifying_generation) else {return Ok(false)};
        if let Some(previous)=tx.host_recovery_pending(candidate.project)?.iter().find(|p| decode(p).is_ok_and(|s|s.attribution.attempt==attempt_id)) {
            let previous=decode(previous)?;
            current_with_phase(tx,&previous,CapturePhase::PendingObservation)?;
            previous.archive.verify()?;
            return Ok(true);
        }
        let Some(gate)=attempt.executions.last().filter(|g|g.purpose.is_gate() && g.finished_at.is_some() && !g.estimated && g.journal_bound && g.verdict.as_deref()==Some("infrastructure-failure") && g.diagnostics.is_empty()) else {return Ok(false)};
        // A text error, historical pressure replay, or an unsupported cause is
        // not enough even for this observational host-specific disposition.
        let (_,raw)=match Archive::capture(Path::new(&gate.journal_path)){Ok(value)=>value,Err(_)=>return Ok(false)};
        if derive_request(attempt_id,gate,candidate.verifying_generation.map(GlobalSeq::get),&raw).is_err() {return Ok(false)};
        let id=uuid::Uuid::new_v4().to_string();
        let record=AttributionRecord {
            version:1,id:id.clone(),revision:0,submission:attempt.submission.clone(),attempt:attempt_id.into(),inputs:gate.inputs.clone(),created_at:now.clone(),
            components:vec![FailureComponent{id:"native-host-pressure".into(),check:"native-host-pressure".into(),signature:"native severe-pressure withdrawal".into(),requirement:"fresh native fault and restoration proof under original custody".into(),log:gate.journal_path.clone(),observed_cause:FailureCause::HostExternal}],
            preparation:None,settlement:None,plans:vec![],probes:vec![],assessments:vec![],diagnosis_ms:0,held:true,retired:None,
        };
        tx.insert_attribution(&record)?;
        let subject=capture_with_phase(tx,candidate,&id,&gate.id,"native-host-pressure",CapturePhase::PendingObservation)?;
        let story=record.submission.story_number().ok_or_else(||invalid("pending story identity missing"))?;
        let generation=candidate.verifying_generation.ok_or_else(||invalid("pending host generation missing"))?;
        tx.insert_host_recovery_pending(&HostRecoveryPending{id,project:candidate.project,story,generation,evidence:serde_json::to_value(Retained{version:1,subject}).map_err(|e|invalid(&e.to_string()))?})?;
        crate::service::verification::clear_candidate_retry_incident(tx,candidate)?;
        let row=tx.story(candidate.project,story)?.ok_or_else(||invalid("pending host story disappeared"))?;
        crate::service::append_and_fold(tx,candidate.project,story,&crate::service::project_prefix(tx,candidate.project)?,&tx.state_map(candidate.project)?,ExpectedSeq::Exact(row.head_seq),&[StoryEvent::StoryCommentAdded{at:now.clone(),text:"HOST PRESSURE OBSERVED: this original Verifying submission is retained while the native broker proves exact fault custody and restoration. No author repair, resubmission, host mutation, or certification is inferred from the observation. Unsupported or unavailable proof keeps this hold.".into()}],ctx.provenance())?;
        Ok(true)
    }).map_err(Into::into)
}

pub(crate) fn pending_subjects(
    tx: &impl ReadOps,
    project: crate::store::ProjectId,
) -> Result<Vec<PendingSubject>, StoreError> {
    let records = tx.attributions(project)?;
    let mut result = Vec::new();
    for pending in tx.host_recovery_pending(project)? {
        let subject = decode(&pending)?;
        if records
            .iter()
            .any(|a| a == &subject.attribution && a.held && a.retired.is_none())
            && !super::super::owner::subject_restored(tx, &subject)?
        {
            result.push(PendingSubject {
                candidate: subject.candidate,
                attribution: subject.attribution.id,
                execution: subject.execution.id,
                component: subject.component,
            });
        }
    }
    Ok(result)
}

pub(super) fn validate_retained(tx: &impl ReadOps, subject: &Subject) -> Result<(), StoreError> {
    if let Some(pending) = tx
        .host_recovery_pending(subject.candidate.project)?
        .iter()
        .find(|p| p.id == subject.attribution.id)
    {
        let retained = decode(pending)?;
        if retained != *subject {
            return Err(invalid(
                "native host observation no longer matches pending original custody",
            ));
        }
    }
    Ok(())
}
fn decode(record: &HostRecoveryPending) -> Result<Subject, StoreError> {
    let retained: Retained = serde_json::from_value(record.evidence.clone())
        .map_err(|e| StoreError::Corrupt(format!("pending host {}: {e}", record.id)))?;
    let subject = retained.subject;
    if retained.version != 1
        || record.id != subject.attribution.id
        || record.project != subject.candidate.project
        || record.story
            != subject
                .attribution
                .submission
                .story_number()
                .ok_or_else(|| invalid("pending story missing"))?
        || Some(record.generation) != subject.candidate.verifying_generation
        || !subject.repository_common.as_os_str().is_empty()
        || !subject.request.nonce.is_empty()
        || subject.request.operation != "fault-proof"
    {
        return Err(StoreError::Corrupt(format!(
            "pending host {} changed immutable identity",
            record.id
        )));
    }
    subject.attribution.validate()?;
    Ok(subject)
}
