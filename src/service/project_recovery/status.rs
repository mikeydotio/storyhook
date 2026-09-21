//! One read projection for CLI and dashboard recovery diagnostics.
use super::{AssessmentStatus, RecoveryView, RepairScope, WorkKind, WorkStatus, persistence};
use crate::store::{ProjectId, ReadOps, StoreError};
use serde::{Deserialize, Serialize};

/// Durable recovery summary; it never grants new recovery authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryStatus {
    /// Stable coordinator identity accepted by `verifier repair show`.
    pub id: String,
    /// Typed project fault code.
    pub fault: String,
    /// Repository-relative configuration or gate location.
    pub locus: String,
    /// Unique affected story identities, including held unjudged submissions.
    pub affected_stories: Vec<String>,
    /// Story which owns the scope assessment.
    pub assessment_owner: String,
    /// Accepted repair owner, if scope has been decided.
    pub repair_story: Option<String>,
    /// Current repair pull request, when linked.
    pub repair_link: Option<String>,
    /// Current coordination phase, distinct from infrastructure incidents.
    pub phase: String,
    /// Completed, changed-input repair attempts across this lineage.
    pub completed_attempts: usize,
    /// Maximum completed repair attempts.
    pub attempt_limit: usize,
    /// Concrete next action or the constraint that prevents it.
    pub next_action: String,
}

pub(crate) fn snapshot(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<Vec<RecoveryStatus>, StoreError> {
    let metadata = tx
        .project(project)?
        .ok_or_else(|| StoreError::Corrupt("recovery project missing".into()))?;
    tx.project_recoveries(project)?
        .into_iter()
        .map(|record| {
            let view = persistence::read_view(tx, record)?;
            let repair = view.state.decision.as_ref().and_then(|d| d.repair_story);
            let repair_story = repair.map(|story| story.to_id(&metadata.prefix));
            let repair_link = if let Some(story) = repair {
                tx.open_pr_links_for_story(project, story)?
                    .into_iter()
                    .find(|link| link.close_on_merge)
                    .map(|link| link.url)
            } else {
                None
            };
            let (phase, next_action) = phase(tx, &view, repair_story.as_deref())?;
            Ok(RecoveryStatus {
                id: view.record.id,
                fault: view.record.code,
                locus: view.record.locus,
                affected_stories: view
                    .state
                    .subjects
                    .iter()
                    .map(|s| s.story.to_id(&metadata.prefix))
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                assessment_owner: view.state.assessment.story.to_id(&metadata.prefix),
                repair_story,
                repair_link,
                phase: phase.into(),
                next_action,
                completed_attempts: view
                    .state
                    .attempts
                    .iter()
                    .filter(|a| a.completion.is_some())
                    .count(),
                attempt_limit: 3,
            })
        })
        .collect()
}

fn phase(
    tx: &impl ReadOps,
    view: &RecoveryView,
    repair: Option<&str>,
) -> Result<(&'static str, String), StoreError> {
    let state = &view.state;
    if view.record.active
        && let Some(hold) = state.assessment.hold
    {
        return Ok(("held", hold.detail().into()));
    }
    if view.record.active
        && state.assessment.status != AssessmentStatus::Decided
        && let Some(hold) = super::authority::assessment_hold(tx, view)?
    {
        return Ok(("held", hold.detail().into()));
    }
    if let Some(work) = state.work.iter().rev().find(|w| {
        w.status == WorkStatus::Held && (view.record.active || w.kind == WorkKind::Resume)
    }) {
        return Ok(("held", work.detail.clone()));
    }
    if let Some(decision) = &state.decision {
        if decision.input.scope == RepairScope::External {
            return Ok((
                "external-prerequisite",
                decision.input.prerequisite.clone().unwrap_or_default(),
            ));
        }
        if let Some(work) = state.work.iter().find(|w| {
            matches!(w.status, WorkStatus::Pending | WorkStatus::InFlight)
                && (view.record.active || w.kind == WorkKind::Resume)
        }) {
            if let Some(reason) = super::work::permitted(tx, view, work)? {
                return Ok(("held", reason.detail().into()));
            }
            return Ok((
                if work.kind == WorkKind::Resume {
                    "resume-pending"
                } else {
                    "repair-pending"
                },
                format!(
                    "Managed {:?} delivery for {}; wait for its retained receipt.",
                    work.kind,
                    if work.kind == WorkKind::Resume {
                        "affected story"
                    } else {
                        repair.unwrap_or("repair owner")
                    }
                ),
            ));
        }
        if state.landing.is_some() {
            let owed = decision.dependency_holds.iter().any(|hold| {
                !state
                    .work
                    .iter()
                    .any(|work| work.kind == WorkKind::Resume && work.story == hold.story)
            });
            return Ok((
                if owed { "resume-held" } else { "landed" },
                if owed {
                    "Repair landed. Reconcile affected story holds; preserve operator controls and unrelated blockers.".into()
                } else {
                    "Repair landed. Affected agents must submit a fresh generation after refresh."
                        .into()
                },
            ));
        }
        if let Some(story) = decision.repair_story {
            let row = tx.story(view.record.project, story)?;
            if let Some(row) = &row {
                if let Some(hold) =
                    super::authority::policy_hold(tx, view.record.project, &row.snapshot)?
                {
                    return Ok(("held", hold.detail().into()));
                }
                if row.awaiting.is_some()
                    || super::resume::resource_hold(tx, view.record.project, story)?
                {
                    return Ok((
                        "held",
                        row.awaiting
                            .clone()
                            .unwrap_or_else(|| "Repair resources require reconciliation.".into()),
                    ));
                }
            }
            let phase = if row.as_ref().is_some_and(|r| r.state == "verifying") {
                "repair-verifying"
            } else {
                "repair-active"
            };
            return Ok((
                phase,
                format!(
                    "{} must pass central verification and land before affected work can resume.",
                    repair.unwrap_or("Repair")
                ),
            ));
        }
    }
    Ok(match state.assessment.status {
        AssessmentStatus::Pending => (
            "assessment-pending",
            "Wait for managed scope assessment delivery.".into(),
        ),
        AssessmentStatus::InFlight => (
            "assessment-delivering",
            "Reconcile the retained assessment delivery receipt before retry.".into(),
        ),
        AssessmentStatus::Delivered => (
            "assessment-waiting",
            "The assessment owner must submit its scope decision within 30 minutes.".into(),
        ),
        _ => ("held", state.assessment.detail.clone()),
    })
}
