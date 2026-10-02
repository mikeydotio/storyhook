//! One read projection for CLI and dashboard recovery diagnostics.
use super::{
    AssessmentStatus, RecoveryView, RepairScope, WorkDelivery, WorkKind, WorkStatus, persistence,
    resolution,
};
use crate::store::{ProjectId, ReadOps, StoreError, StoryNo};
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

/// Current recoveries only: every row still owes work. A resolved recovery
/// is left out, while its durable record stays for coordination, resume
/// ownership and `verifier repair show`.
pub(crate) fn snapshot(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<Vec<RecoveryStatus>, StoreError> {
    let metadata = tx
        .project(project)?
        .ok_or_else(|| StoreError::Corrupt("recovery project missing".into()))?;
    let mut current = Vec::new();
    for record in tx.project_recoveries(project)? {
        let view = persistence::read_view(tx, record)?;
        let repair = view.state.decision.as_ref().and_then(|d| d.repair_story);
        let repair_story = repair.map(|story| story.to_id(&metadata.prefix));
        let Some((phase, next_action)) =
            phase(tx, &view, repair_story.as_deref(), &metadata.prefix)?
        else {
            continue;
        };
        let repair_link = if let Some(story) = repair {
            tx.open_pr_links_for_story(project, story)?
                .into_iter()
                .find(|link| link.close_on_merge)
                .map(|link| link.url)
        } else {
            None
        };
        current.push(RecoveryStatus {
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
        });
    }
    Ok(current)
}

/// The phase and next action, or `None` once the recovery is resolved.
fn phase(
    tx: &impl ReadOps,
    view: &RecoveryView,
    repair: Option<&str>,
    prefix: &str,
) -> Result<Option<(&'static str, String)>, StoreError> {
    let state = &view.state;
    if view.record.active
        && let Some(hold) = state.assessment.hold
    {
        return Ok(Some((
            "held",
            match super::rearm::assessment(tx, view)? {
                Some(policy) => policy.detail(&view.record.id),
                None => hold.detail().into(),
            },
        )));
    }
    if view.record.active
        && state.assessment.status != AssessmentStatus::Decided
        && let Some(hold) = super::authority::assessment_hold(tx, view)?
    {
        return Ok(Some(("held", hold.detail().into())));
    }
    // Only a landed recovery can resolve, so only it pays for the story reads.
    let outstanding = if state.landing.is_some() {
        Some(resolution::outstanding(tx, view)?)
    } else {
        None
    };
    // A retired record keeps only resume effects, and one of those matters
    // while its story is still held or owed; after that it is moot.
    let claimed = |work: &WorkDelivery| {
        view.record.active
            || (work.kind == WorkKind::Resume
                && outstanding.as_ref().is_none_or(|o| o.claims(work.story)))
    };
    if let Some(work) = state
        .work
        .iter()
        .rev()
        .find(|w| w.status == WorkStatus::Held && claimed(w))
    {
        return Ok(Some((
            "held",
            match super::rearm::work(tx, view, work)? {
                Some(policy) => policy.detail(&view.record.id),
                None => work.detail.clone(),
            },
        )));
    }
    if let Some(decision) = &state.decision {
        if decision.input.scope == RepairScope::External {
            return Ok(Some((
                "external-prerequisite",
                decision.input.prerequisite.clone().unwrap_or_default(),
            )));
        }
        // An outstanding external call always shows, even when moot.
        if let Some(work) = state.work.iter().find(|w| match w.status {
            WorkStatus::InFlight => view.record.active || w.kind == WorkKind::Resume,
            WorkStatus::Pending => claimed(w),
            WorkStatus::Delivered | WorkStatus::Held => false,
        }) {
            if let Some(reason) = super::work::permitted(tx, view, work)? {
                return Ok(Some(("held", reason.detail().into())));
            }
            return Ok(Some((
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
            )));
        }
        if let Some(outstanding) = &outstanding {
            return Ok(landed(outstanding, prefix));
        }
        if let Some(story) = decision.repair_story {
            let row = tx.story(view.record.project, story)?;
            if let Some(row) = &row {
                if let Some(hold) =
                    super::authority::policy_hold(tx, view.record.project, &row.snapshot)?
                {
                    return Ok(Some(("held", hold.detail().into())));
                }
                if row.awaiting.is_some()
                    || super::resume::resource_hold(tx, view.record.project, story)?
                {
                    return Ok(Some((
                        "held",
                        row.awaiting
                            .clone()
                            .unwrap_or_else(|| "Repair resources require reconciliation.".into()),
                    )));
                }
            }
            let phase = if row.as_ref().is_some_and(|r| r.state == "verifying") {
                "repair-verifying"
            } else {
                "repair-active"
            };
            return Ok(Some((
                phase,
                format!(
                    "{} must pass central verification and land before affected work can resume.",
                    repair.unwrap_or("Repair")
                ),
            )));
        }
    }
    Ok(Some(match state.assessment.status {
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
    }))
}

/// After landing: name exactly the stories that still owe something, or
/// resolve when none does. Never instructs work no story owes.
fn landed(outstanding: &resolution::Outstanding, prefix: &str) -> Option<(&'static str, String)> {
    let names = |stories: &std::collections::BTreeSet<StoryNo>| {
        stories
            .iter()
            .map(|story| story.to_id(prefix))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut next = vec!["Repair landed.".to_string()];
    if !outstanding.held.is_empty() {
        next.push(format!(
            "Reconcile the recovery holds on {}; preserve operator controls and unrelated blockers.",
            names(&outstanding.held)
        ));
    }
    if !outstanding.owed.is_empty() {
        next.push(format!(
            "{} must refresh source and gate configuration from the current base and resubmit for a fresh central verification generation.",
            names(&outstanding.owed)
        ));
    }
    let phase = if !outstanding.held.is_empty() {
        "resume-held"
    } else if !outstanding.owed.is_empty() {
        "landed"
    } else {
        return None;
    };
    Some((phase, next.join(" ")))
}
