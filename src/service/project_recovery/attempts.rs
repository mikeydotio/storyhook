//! Admission by committed source identity and a bounded repair lineage.

use super::{FaultObservation, ProjectRecoveryService, RecoveryView, authority, persistence};
use crate::{
    error::AppError,
    service::VerificationCandidate,
    store::{GlobalSeq, ReadOps, Store, StoreError, StoryNo},
};
use serde::{Deserialize, Serialize};

/// Pinned Git input reported before inspecting configuration or running the gate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairInput {
    /// Pinned integration commit.
    pub base: String,
    /// Pinned repair branch commit.
    pub head: String,
    /// Committed file content of the repair head, independent of merge-base movement.
    pub head_tree: String,
    /// Proposed merge tree that still requires certification.
    pub tree: String,
}

/// A completed project judgment, distinct from an interrupted or failed inspection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RepairCompletion {
    /// The exact proposed tree was certified; landing remains separate authority.
    Certified,
    /// Required tests completed with a nonzero result.
    TestsFailed,
    /// Structured project-owned gate failure remains in this lineage.
    ProjectFault,
}

/// Durable attempt admitted before any repair gate operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairAttempt {
    /// Original queue authority, including transient reservations and cleanup lease.
    pub candidate: VerificationCandidate,
    /// Latest recovery reservation, including a removed no-auto label.
    #[serde(default)]
    pub label_revision: Option<GlobalSeq>,
    /// Exact verifier attempt identity.
    pub id: String,
    /// The recovery's repair owner.
    pub story: StoryNo,
    /// Current verification generation at admission.
    pub generation: GlobalSeq,
    /// Pinned source and merge inputs.
    pub input: RepairInput,
    /// RFC3339 admission time.
    pub admitted_at: String,
    /// Completed judgment, absent for unjudged interruptions.
    pub completion: Option<RepairCompletion>,
    /// Exact judged evidence; a classification alone cannot consume admission.
    #[serde(default)]
    pub judgment: Option<super::RepairJudgment>,
    /// RFC3339 completed-judgment time.
    pub completed_at: Option<String>,
}

/// A supported reason to withhold another repair gate execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RepairRefusal {
    /// The repair repeats committed file content already judged in this lineage.
    UnchangedInput,
    /// Three changed repair submissions have already completed.
    BudgetExhausted,
    /// Current operator or reserved-label policy withholds automatic recovery.
    PolicyHold,
}

/// Durable refusal awaiting safe story disposition after verifier cleanup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairRefusalRecord {
    /// Original queue authority required for disposition after verifier cleanup.
    pub candidate: VerificationCandidate,
    /// Latest recovery reservation, including a removed no-auto label.
    #[serde(default)]
    pub label_revision: Option<GlobalSeq>,
    /// Exact refused verifier attempt.
    pub id: String,
    /// Original target story.
    pub story: StoryNo,
    /// Refused submission generation.
    pub generation: GlobalSeq,
    /// Input that must not cause another gate run.
    pub input: RepairInput,
    /// Typed refusal cause.
    pub reason: RepairRefusal,
    /// RFC3339 refusal time.
    pub at: String,
    /// Exact applied hold, absent until cleanup has settled.
    #[serde(default)]
    pub disposition: Option<super::RepairRefusalDisposition>,
}

/// Private verifier admission response; it never certifies or merges a tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum RepairAdmission {
    /// Gate inspection may continue under ordinary verifier safeguards.
    Proceed {
        /// Owning recovery, absent for an ordinary submission.
        recovery_id: Option<String>,
    },
    /// Restore and release verifier ownership without executing the gate.
    Deferred {
        /// Recovery retaining the refused input.
        recovery_id: String,
        /// Reason to apply after cleanup settles.
        reason: RepairRefusal,
    },
}

impl<S: Store> ProjectRecoveryService<'_, S> {
    /// Admit pinned repair input under current generation and lineage limits.
    pub fn admit_repair(
        &self,
        candidate: &VerificationCandidate,
        attempt: &str,
        input: &RepairInput,
    ) -> Result<RepairAdmission, AppError> {
        input.validate()?;
        self.validate_attempt_identity(candidate, attempt)?;
        let now = self.ctx.now();
        self.ctx
            .store()
            .write(|tx| {
                let (story, generation) = current(tx, candidate)?;
                let Some(mut view) = owner(tx, candidate.project, story)? else {
                    return Ok(RepairAdmission::Proceed { recovery_id: None });
                };
                let label_revision = authority::label_revision(tx, candidate.project, story)?;
                if let Some(previous) = view
                    .state
                    .refusals
                    .iter()
                    .find(|previous| previous.id == attempt)
                {
                    same_attempt(
                        story,
                        generation,
                        input,
                        previous.story,
                        previous.generation,
                        &previous.input,
                    )?;
                    require_same_authority(candidate, &previous.candidate)?;
                    require_label_revision(label_revision, previous.label_revision)?;
                    return Ok(RepairAdmission::Deferred {
                        recovery_id: view.record.id.clone(),
                        reason: previous.reason,
                    });
                }
                let row = tx
                    .story(candidate.project, story)?
                    .ok_or_else(|| StoreError::Corrupt("repair story disappeared".into()))?;
                let policy_held =
                    authority::policy_hold(tx, candidate.project, &row.snapshot)?.is_some();
                if let Some(previous) = view
                    .state
                    .attempts
                    .iter()
                    .find(|previous| previous.id == attempt)
                {
                    same_attempt(
                        story,
                        generation,
                        input,
                        previous.story,
                        previous.generation,
                        &previous.input,
                    )?;
                    require_same_authority(candidate, &previous.candidate)?;
                    require_label_revision(label_revision, previous.label_revision)?;
                }
                let certified_retry = view.state.attempts.iter().any(|previous| {
                    previous.completion == Some(RepairCompletion::Certified)
                        && previous.generation == generation
                        && previous.input.head == input.head
                        && previous.input.head_tree == input.head_tree
                });
                let completed = view
                    .state
                    .attempts
                    .iter()
                    .filter(|a| a.completion.is_some())
                    .map(|a| &a.input.head_tree)
                    .collect::<std::collections::BTreeSet<_>>();
                let mut unchanged = completed.contains(&input.head_tree);
                for observation in &view.observations {
                    let evidence: FaultObservation =
                        serde_json::from_value(observation.evidence.clone()).map_err(|error| {
                            StoreError::Corrupt(format!("repair baseline evidence: {error}"))
                        })?;
                    unchanged |= evidence.fault.source().1 == input.head_tree;
                }
                let reason = if policy_held {
                    Some(RepairRefusal::PolicyHold)
                } else if completed.len() >= 3 && !certified_retry {
                    Some(RepairRefusal::BudgetExhausted)
                } else if unchanged && !certified_retry {
                    Some(RepairRefusal::UnchangedInput)
                } else {
                    None
                };
                if let Some(reason) = reason {
                    view.state.refusals.push(RepairRefusalRecord {
                        candidate: candidate.clone(),
                        label_revision,
                        id: attempt.into(),
                        story,
                        generation,
                        input: input.clone(),
                        reason,
                        at: now.clone(),
                        disposition: None,
                    });
                    persistence::save(tx, &mut view, &now)?;
                    return Ok(RepairAdmission::Deferred {
                        recovery_id: view.record.id,
                        reason,
                    });
                }
                if view
                    .state
                    .attempts
                    .iter()
                    .any(|previous| previous.id == attempt)
                {
                    return Ok(RepairAdmission::Proceed {
                        recovery_id: Some(view.record.id),
                    });
                }
                view.state.attempts.push(RepairAttempt {
                    candidate: candidate.clone(),
                    label_revision,
                    id: attempt.into(),
                    story,
                    generation,
                    input: input.clone(),
                    admitted_at: now.clone(),
                    completion: None,
                    judgment: None,
                    completed_at: None,
                });
                persistence::save(tx, &mut view, &now)?;
                Ok(RepairAdmission::Proceed {
                    recovery_id: Some(view.record.id),
                })
            })
            .map_err(Into::into)
    }
    /// Account for a completed judgment of one previously admitted exact attempt.
    pub fn complete_repair(
        &self,
        candidate: &VerificationCandidate,
        attempt: &str,
        judgment: &super::RepairJudgment,
    ) -> Result<Option<RecoveryView>, AppError> {
        self.validate_attempt_identity(candidate, attempt)?;
        let result = judgment.classification();
        let now = self.ctx.now();
        self.ctx
            .store()
            .write(|tx| {
                let prefix = crate::service::project_prefix(tx, candidate.project)?;
                let story = StoryNo::parse_id(&prefix, &candidate.story_id)?;
                let Some(mut view) = owner(tx, candidate.project, story)? else {
                    return Ok(None);
                };
                let index = view
                    .state
                    .attempts
                    .iter()
                    .position(|a| {
                        a.id == attempt
                            && a.story == story
                            && Some(a.generation) == candidate.verifying_generation
                    })
                    .ok_or_else(|| {
                        StoreError::Validation(
                            "repair completion has no matching admitted attempt".into(),
                        )
                    })?;
                require_same_authority(candidate, &view.state.attempts[index].candidate)?;
                require_label_revision(
                    authority::label_revision(tx, candidate.project, story)?,
                    view.state.attempts[index].label_revision,
                )?;
                judgment.validate_against(&view.state.attempts[index].input)?;
                if view.state.refusals.iter().any(|r| r.id == attempt) {
                    return Err(StoreError::Validation(
                        "refused repair attempt cannot record a completed gate".into(),
                    ));
                }
                if let Some(previous) = view.state.attempts[index].completion {
                    return if previous == result
                        && view.state.attempts[index].judgment.as_ref() == Some(judgment)
                    {
                        Ok(Some(view))
                    } else {
                        Err(StoreError::Validation(
                            "repair attempt has a different completed outcome".into(),
                        ))
                    };
                }
                current(tx, candidate)?;
                let completed = view
                    .state
                    .attempts
                    .iter()
                    .filter(|a| a.completion.is_some())
                    .map(|a| &a.input.head_tree)
                    .collect::<std::collections::BTreeSet<_>>();
                if completed.len() >= 3
                    && !completed.contains(&view.state.attempts[index].input.head_tree)
                {
                    return Err(StoreError::Validation(
                        "repair completion exceeds the three-input lineage budget".into(),
                    ));
                }
                view.state.attempts[index].completion = Some(result);
                view.state.attempts[index].judgment = Some(judgment.clone());
                view.state.attempts[index].completed_at = Some(now.clone());
                persistence::save(tx, &mut view, &now)?;
                Ok(Some(view))
            })
            .map_err(Into::into)
    }

    fn validate_attempt_identity(
        &self,
        candidate: &VerificationCandidate,
        attempt: &str,
    ) -> Result<(), AppError> {
        if candidate.project != self.ctx.project()
            || candidate.verifying_generation.is_none()
            || candidate.pull_request.is_err()
            || attempt.trim().is_empty()
        {
            return Err(AppError::Validation("repair admission requires the exact project, generation, pull request, and attempt identity".into()));
        }
        Ok(())
    }
}

impl RepairInput {
    /// Require concrete commit and tree IDs; mutable refs cannot grant admission.
    pub fn validate(&self) -> Result<(), AppError> {
        if ![&self.base, &self.head, &self.head_tree, &self.tree]
            .iter()
            .all(|value| crate::service::project_fault::is_pinned_oid(value))
        {
            return Err(AppError::Validation(
                "repair input requires pinned Git commit and tree IDs".into(),
            ));
        }
        Ok(())
    }
}

pub(super) fn owner(
    tx: &impl ReadOps,
    project: crate::store::ProjectId,
    story: StoryNo,
) -> Result<Option<RecoveryView>, StoreError> {
    let mut owner = None;
    for record in tx
        .project_recoveries(project)?
        .into_iter()
        .filter(|r| r.active)
    {
        let view = persistence::read_view(tx, record)?;
        if view.state.decision.as_ref().and_then(|d| d.repair_story) == Some(story) {
            if owner.is_some() {
                return Err(StoreError::Corrupt(
                    "repair story belongs to multiple active recovery lineages".into(),
                ));
            }
            owner = Some(view);
        }
    }
    Ok(owner)
}

pub(super) fn current(
    tx: &impl ReadOps,
    candidate: &VerificationCandidate,
) -> Result<(StoryNo, GlobalSeq), StoreError> {
    let prefix = crate::service::project_prefix(tx, candidate.project)?;
    let (story, row) =
        crate::service::resolve_story(tx, candidate.project, &prefix, &candidate.story_id)?;
    if row.awaiting.is_some()
        || candidate.landing_pending
        || tx
            .landing_intents()?
            .iter()
            .any(|intent| intent.project == candidate.project && intent.story == story)
        || !crate::service::verification::candidate_is_current(tx, &row, candidate)?
    {
        return Err(StoreError::Validation(
            "repair attempt no longer owns the current submission generation".into(),
        ));
    }
    Ok((
        story,
        candidate
            .verifying_generation
            .ok_or_else(|| StoreError::Validation("repair attempt has no generation".into()))?,
    ))
}

pub(super) fn authority_matches(
    current: &VerificationCandidate,
    retained: &VerificationCandidate,
) -> bool {
    current.project == retained.project
        && current.project_slug == retained.project_slug
        && current.story_id == retained.story_id
        && current.verifying_generation == retained.verifying_generation
        && current.human_only_revision == retained.human_only_revision
        && current.blocking_revision == retained.blocking_revision
        && current.checkout == retained.checkout
        && current.cleanup_lease == retained.cleanup_lease
        && current.pull_request.as_ref().ok().map(|pr| &pr.url)
            == retained.pull_request.as_ref().ok().map(|pr| &pr.url)
}

fn require_same_authority(
    current: &VerificationCandidate,
    retained: &VerificationCandidate,
) -> Result<(), StoreError> {
    if !authority_matches(current, retained) {
        return Err(StoreError::Validation("repair attempt authority differs from its retained submission, reservation, or resource identity".into()));
    }
    Ok(())
}
fn same_attempt(
    story: StoryNo,
    generation: GlobalSeq,
    input: &RepairInput,
    previous_story: StoryNo,
    previous_generation: GlobalSeq,
    previous_input: &RepairInput,
) -> Result<(), StoreError> {
    if story != previous_story || generation != previous_generation || input != previous_input {
        return Err(StoreError::Validation(
            "repair attempt identity has conflicting source or generation evidence".into(),
        ));
    }
    Ok(())
}

fn require_label_revision(
    current: Option<GlobalSeq>,
    retained: Option<GlobalSeq>,
) -> Result<(), StoreError> {
    if current != retained {
        return Err(StoreError::Validation(
            "repair reservation changed after admission".into(),
        ));
    }
    Ok(())
}
