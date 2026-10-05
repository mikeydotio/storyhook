//! An operator's statement that an external prerequisite is satisfied (SH-849).
//!
//! External scope has no repair to land, so no certified landing can release
//! its holds. This statement is its release authority instead: it retires the
//! record, so a later fault opens a new recovery, and the recovery worker then
//! releases the holds the decision wrote through the same managed resume that
//! follows a certified repair landing. It is an attestation, not a check: it
//! grants no certification, and each affected story still needs a fresh
//! generation that passes central verification.

use super::{ProjectRecoveryService, RecoveryState, RecoveryView, RepairScope, persistence};
use crate::{
    domain::{StoryEvent, provenance::Provenance},
    error::AppError,
    service::{append_and_fold, project_prefix},
    store::{ExpectedSeq, GlobalSeq, ProjectRecovery, ReadOps, Store, StoreError, StoryNo},
};
use serde::{Deserialize, Serialize};

/// Strict, versioned operator statement shared by the CLI and RPC doors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrerequisiteInput {
    /// Request schema version, currently one.
    pub version: u8,
    /// Exact recovery revision the operator read with `repair show`.
    pub revision: i64,
    /// Facts and constraints sufficient to understand the statement.
    pub context: String,
    /// The question the statement answers.
    pub question: String,
    /// The answer: the prerequisite is restored.
    pub decision: String,
    /// Why the operator is sure, including what was weighed.
    pub rationale: String,
    /// What shows that the prerequisite is restored; at least one entry.
    pub evidence: Vec<String>,
}

/// Durable acceptance, retained for exact replay; it retires the recovery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrerequisiteReceipt {
    /// The accepted statement, preserved verbatim as structured input.
    pub input: PrerequisiteInput,
    /// RFC3339 time of acceptance.
    pub accepted_at: String,
    /// The assessment story, whose log records the statement.
    pub story: StoryNo,
    /// The comment event that records the statement; every hold release
    /// follows it.
    pub event: GlobalSeq,
    /// Who made the statement, as the invocation declared it.
    pub provenance: Provenance,
}

impl PrerequisiteInput {
    /// Reject unsupported versions, a negative revision, and blank fields or
    /// evidence.
    pub fn validate(&self) -> Result<(), AppError> {
        let nonblank = |text: &str| !text.trim().is_empty();
        if self.version != 1
            || self.revision < 0
            || [
                &self.context,
                &self.question,
                &self.decision,
                &self.rationale,
            ]
            .iter()
            .any(|text| !nonblank(text))
            || self.evidence.is_empty()
            || self.evidence.iter().any(|item| !nonblank(item))
        {
            return Err(AppError::Validation("invalid prerequisite statement: require version 1, the recovery revision from repair show, nonempty Context/Question/Decision/Rationale, and at least one evidence entry that shows the prerequisite is restored".into()));
        }
        Ok(())
    }
}

impl<S: Store> ProjectRecoveryService<'_, S> {
    /// Accept an operator's statement that an External recovery's
    /// prerequisite is satisfied, once. It retires the record; the recovery
    /// worker then releases the holds the decision wrote. A dispatched agent
    /// session is refused: an agent cannot attest a fact outside the project.
    pub fn satisfy(&self, id: &str, input: &PrerequisiteInput) -> Result<RecoveryView, AppError> {
        if self.ctx.is_agent_session() {
            return Err(AppError::Validation(format!(
                "recovery {id}: refused in a dispatched agent session (STORYHOOK_DISPATCH, STORYHOOK_AUTO or STORYHOOK_FULL_AUTO is set). Only an operator can state that an external prerequisite is restored. Leave the hold in place and report the prerequisite to the operator."
            )));
        }
        input.validate()?;
        let now = self.ctx.now();
        self.ctx
            .write_stories(|tx| {
                let mut view = persistence::find(tx, self.ctx.project(), id)?;
                if let Some(previous) = &view.state.prerequisite {
                    return if &previous.input == input {
                        Ok(view)
                    } else {
                        Err(StoreError::Validation(format!(
                            "recovery {id} already records a different prerequisite statement; nothing changed. Read `story verifier repair show {id} --json`."
                        )))
                    };
                }
                if !view
                    .state
                    .decision
                    .as_ref()
                    .is_some_and(|d| d.input.scope == RepairScope::External)
                {
                    return Err(StoreError::Validation(format!(
                        "recovery {id} has no external-scope decision; only an external prerequisite can be satisfied. Read `story verifier repair show {id} --json` for its scope and repair."
                    )));
                }
                if !view.record.active {
                    return Err(StoreError::Validation(format!(
                        "recovery {id} is retired; nothing changed."
                    )));
                }
                if input.revision != view.record.revision {
                    return Err(StoreError::Validation(format!(
                        "recovery {id} is at revision {}, not {}: it changed after it was read (for example, another affected story joined). Read `story verifier repair show {id} --json` again and retry with its revision.",
                        view.record.revision, input.revision
                    )));
                }
                let project = view.record.project;
                let story = view.state.assessment.story;
                let prefix = project_prefix(tx, project)?;
                let row = tx.story(project, story)?.ok_or_else(|| {
                    StoreError::Validation(format!(
                        "recovery {id}: its assessment story {} no longer exists to record the statement",
                        story.to_id(&prefix)
                    ))
                })?;
                let text = statement(&view.record.id, input);
                append_and_fold(
                    tx,
                    project,
                    story,
                    &prefix,
                    &tx.state_map(project)?,
                    ExpectedSeq::Exact(row.head_seq),
                    &[StoryEvent::StoryCommentAdded {
                        at: now.clone(),
                        text: text.clone(),
                    }],
                    self.ctx.provenance(),
                )?;
                let event = tx
                    .events_for(project, story)?
                    .iter()
                    .rev()
                    .find(|event| {
                        matches!(event.known(), Some(StoryEvent::StoryCommentAdded { text: written, .. }) if written == &text)
                    })
                    .map(|event| event.global_seq)
                    .ok_or_else(|| {
                        StoreError::Corrupt("prerequisite statement comment was not retained".into())
                    })?;
                view.state.prerequisite = Some(PrerequisiteReceipt {
                    input: input.clone(),
                    accepted_at: now.clone(),
                    story,
                    event,
                    provenance: self.ctx.provenance().clone(),
                });
                view.record.active = false;
                persistence::save(tx, &mut view, &now)?;
                Ok(view)
            })
            .map_err(Into::into)
    }
}

/// The comment that records the statement on the assessment story.
fn statement(recovery: &str, input: &PrerequisiteInput) -> String {
    format!(
        "PROJECT RECOVERY {recovery} — EXTERNAL PREREQUISITE SATISFIED\n\nAn operator stated that the external prerequisite is restored. This is an attestation, not a check: it grants no certification, and each affected story still needs a fresh generation that passes central verification. The recovery is retired; the holds it wrote are released and the affected agents are resumed.\n\nContext: {}\nQuestion: {}\nDecision: {}\nRationale: {}\nEvidence: {}",
        input.context,
        input.question,
        input.decision,
        input.rationale,
        input.evidence.join("; ")
    )
}

/// A retained statement must belong to an External decision of a retired
/// record, follow the decision and every hold it wrote, and match its exact
/// comment event.
pub(super) fn validate(
    tx: &impl ReadOps,
    record: &ProjectRecovery,
    state: &RecoveryState,
) -> Result<(), StoreError> {
    let Some(receipt) = &state.prerequisite else {
        return Ok(());
    };
    let corrupt = || {
        StoreError::Corrupt(
            "prerequisite statement has inconsistent decision, revision or event authority".into(),
        )
    };
    let decision = state
        .decision
        .as_ref()
        .filter(|d| d.input.scope == RepairScope::External)
        .ok_or_else(corrupt)?;
    receipt.input.validate().map_err(|error| {
        StoreError::Corrupt(format!("retained prerequisite statement: {error}"))
    })?;
    if record.active
        || state.landing.is_some()
        || receipt.story != state.assessment.story
        || receipt.input.revision <= decision.input.revision
        || receipt.input.revision >= record.revision
        || persistence::timestamp(&receipt.accepted_at)?
            < persistence::timestamp(&decision.accepted_at)?
        || decision
            .dependency_holds
            .iter()
            .any(|hold| hold.event >= receipt.event)
        || !tx
            .events_for(record.project, receipt.story)?
            .iter()
            .any(|event| {
                event.global_seq == receipt.event
                    && matches!(event.known(), Some(StoryEvent::StoryCommentAdded { at, text })
                    if at == &receipt.accepted_at && text == &statement(&record.id, &receipt.input))
            })
    {
        return Err(corrupt());
    }
    Ok(())
}
