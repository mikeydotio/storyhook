//! A clean native merge releases one original diagnostic, never certifies it.
use super::*;
use crate::{
    service::{
        VerificationCandidate,
        attribution::{AttributionRecord, FailureCause},
    },
    store::{
        GlobalSeq, IntegrationReadmission, ProjectId, ReadOps, Store, StoreError, StoryNo, WriteOps,
    },
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u8,
    candidate: VerificationCandidate,
    original: AttributionRecord,
    retired: AttributionRecord,
    clean: submission::CleanIntegrationEvidence,
    attempt: String,
    control: i64,
    labels: Option<GlobalSeq>,
    released_at: String,
}
fn invalid(detail: &str) -> StoreError {
    StoreError::Validation(format!("clean integration readmission: {detail}"))
}

fn decode(record: &IntegrationReadmission) -> Result<Receipt, StoreError> {
    let receipt: Receipt = serde_json::from_value(record.evidence.clone())
        .map_err(|e| StoreError::Corrupt(format!("clean readmission {}: {e}", record.id)))?;
    let valid = receipt.version == 1
        && receipt.clean.version == 1
        && receipt.candidate.project == record.project
        && receipt.candidate.verifying_generation == Some(record.generation)
        && receipt.original.id == record.id
        && receipt.retired.id == record.id
        && receipt.original.submission.project == record.project
        && receipt.original.submission.story_number() == Some(record.story)
        && receipt.original.submission.generation == Some(record.generation)
        && receipt.original.held
        && receipt.original.retired.is_none()
        && !receipt.retired.held
        && receipt.retired.retired.is_some()
        && receipt.original.revision.checked_add(1) == Some(receipt.retired.revision)
        && receipt.clean.submission.head
            == receipt.original.inputs.head.as_deref().unwrap_or_default()
        && receipt.clean.submission.checkout == receipt.candidate.checkout
        && owner::same_original_pr(&receipt.candidate, &receipt.clean.submission)
        && receipt.original.components.len() == 1
        && receipt.original.components[0].observed_cause == FailureCause::Integration
        && !receipt.attempt.is_empty();
    let mut expected = receipt.original.clone();
    expected.revision = receipt.retired.revision;
    expected.held = false;
    expected.retired = receipt.retired.retired.clone();
    if !valid
        || expected != receipt.retired
        || [
            &receipt.clean.submission.head,
            &receipt.clean.submission.base,
            &receipt.clean.tree,
        ]
        .iter()
        .any(|s| !crate::service::project_fault::is_pinned_oid(s))
        || receipt.clean.policy.len() != 64
        || !receipt.clean.policy.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(StoreError::Corrupt(format!(
            "clean readmission {} changed identity or receipt",
            record.id
        )));
    }
    receipt.original.validate()?;
    receipt.retired.validate()?;
    chrono::DateTime::parse_from_rfc3339(&receipt.released_at)
        .map_err(|e| StoreError::Corrupt(e.to_string()))?;
    Ok(receipt)
}

pub(crate) fn expected_head(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
    generation: Option<GlobalSeq>,
) -> Result<Option<String>, StoreError> {
    let mut expected = None;
    for record in tx
        .integration_readmissions(project)?
        .into_iter()
        .filter(|r| r.story == story && Some(r.generation) == generation)
    {
        let receipt = decode(&record)?;
        let head = receipt.clean.submission.head;
        if expected.as_ref().is_some_and(|old| old != &head) {
            return Err(invalid("receipts disagree on original head"));
        }
        expected = Some(head);
    }
    Ok(expected)
}

pub(crate) fn check_input(
    tx: &impl ReadOps,
    candidate: &VerificationCandidate,
    head: &str,
) -> Result<(), StoreError> {
    let prefix = crate::service::project_prefix(tx, candidate.project)?;
    let story = StoryNo::parse_id(&prefix, &candidate.story_id)?;
    for record in tx
        .integration_readmissions(candidate.project)?
        .into_iter()
        .filter(|r| r.story == story && Some(r.generation) == candidate.verifying_generation)
    {
        let receipt = decode(&record)?;
        let retained = &receipt.candidate;
        if retained.project != candidate.project
            || retained.project_slug != candidate.project_slug
            || retained.story_id != candidate.story_id
            || retained.verifying_generation != candidate.verifying_generation
            || retained.verifying_since != candidate.verifying_since
            || retained.blocking_revision != candidate.blocking_revision
            || retained.human_only_revision != candidate.human_only_revision
            || retained.checkout != candidate.checkout
            || retained.cleanup_lease != candidate.cleanup_lease
            || !owner::same_original_pr(candidate, &receipt.clean.submission)
            || receipt.clean.submission.head != head
            || tx.verification_control_revision(candidate.project)? != receipt.control
            || crate::service::project_recovery::recovery_label_revision(
                tx,
                candidate.project,
                story,
            )? != receipt.labels
            || !tx
                .attributions(candidate.project)?
                .contains(&receipt.retired)
        {
            return Err(invalid(
                "original head, control, labels or diagnostic changed before the fresh gate",
            ));
        }
        owner::check_candidate(tx, candidate)?;
    }
    Ok(())
}

impl<S: Store> IntegrationOwnerService<'_, S> {
    /// The caller retains its exact central slot; the native proof is live in
    /// every transaction boundary. Only the original one-component hold retires.
    pub(crate) fn readmit_clean(
        &self,
        subject: &PendingIntegration,
        proof: &submission::NativeCleanIntegration,
        attempt: &str,
        cancellation: &Cancellation,
    ) -> Result<bool, AppError> {
        proof.check_owner(cancellation)?;
        let now = self.ctx.now();
        self.ctx.store().write(|tx| {
            proof.check_owner(cancellation).map_err(StoreError::from)?;
            let candidate = &subject.candidate;
            if candidate.project != self.ctx.project() { return Err(invalid("foreign project")); }
            let original = pending::required_original(tx, subject)?;
            let story = original.submission.story_number().ok_or_else(||invalid("story missing"))?;
            let control = tx.verification_control_revision(candidate.project)?;
            let attempts = tx.gate_attempts(candidate.project)?;
            if !attempts.iter().any(|a| a.id == attempt && a.submission == original.submission && a.finished_at.is_none() && a.mode == crate::domain::landing::VerificationMode::Gated && a.control_revision == Some(control) && a.executions.is_empty()) {
                return Err(invalid("clean readmission lacks its original live central admission"));
            }
            let old = attempts.iter().find(|a| a.id == original.attempt && a.submission == original.submission).ok_or_else(|| invalid("original gate missing"))?;
            if old.finished_at.is_none() || old.elapsed.estimated || old.verdict.as_deref() != Some("conflict") || old.executions.iter().any(|e| e.finished_at.is_none() || e.estimated || e.verdict.as_deref().is_none_or(|v| matches!(v,"interrupted"|"cleanup-failed"))) {
                return Err(invalid("original gate custody has not settled"));
            }
            if original.components.len() != 1 || original.has_unsettled_diagnosis()
                || tx.attributions(candidate.project)?.iter().any(|a| a.id != original.id && a.submission == original.submission && a.held && a.retired.is_none())
                || tx.integration_recoveries(candidate.project)?.iter().any(|o| o.story == story && o.active)
                || tx.landing_intents()?.iter().any(|i| i.project == candidate.project && i.story == story) {
                return Err(invalid("another component, managed owner, landing or cleanup remains held"));
            }
            let evidence = proof.evidence();
            if evidence.submission.head != subject.retained_head
                || !owner::same_original_pr(candidate, &evidence.submission)
                || evidence.submission.checkout != candidate.checkout {
                return Err(invalid("native clean evidence differs from original submission"));
            }
            let mut retired = original.clone();
            retired.revision = retired.revision.checked_add(1).ok_or_else(||invalid("attribution revision overflow"))?;
            retired.held = false;
            retired.retired = Some(format!("native clean merge {} on current base {}; original head {} requires a fresh central gate", evidence.tree,evidence.submission.base,evidence.submission.head));
            let receipt = Receipt { version:1,candidate:candidate.clone(),original:original.clone(),retired:retired.clone(),clean:evidence.clone(),attempt:attempt.into(),control,labels:crate::service::project_recovery::recovery_label_revision(tx,candidate.project,story)?,released_at:now.clone() };
            let record = IntegrationReadmission { id:original.id.clone(),project:candidate.project,story,generation:candidate.verifying_generation.ok_or_else(||invalid("generation missing"))?,evidence:serde_json::to_value(receipt).map_err(|e|invalid(&e.to_string()))? };
            decode(&record)?;
            tx.insert_integration_readmission(&record)?;
            if !tx.update_attribution(&retired,original.revision)? { return Err(invalid("original diagnostic changed during clean release")); }
            proof.check_owner(cancellation).map_err(StoreError::from)?;
            Ok(true)
        }).map_err(Into::into)
    }
}
