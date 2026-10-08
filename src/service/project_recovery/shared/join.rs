//! Explicit coordination of distinct native faults; fingerprints never collapse.
use super::super::{
    DecisionInput, RecoveryState, RecoveryView, RepairCompletion, RepairScope, WorkStatus,
    authority, persistence,
};
use crate::store::{ProjectRecovery, ReadOps, StoreError, StoryNo};

pub(in crate::service::project_recovery) fn leader(state: &RecoveryState) -> Option<&str> {
    state
        .decision
        .as_ref()?
        .input
        .join_recovery
        .as_ref()
        .map(|join| join.recovery.as_str())
}

/// The assessor chooses this relationship. A follower owns no delivery or budget.
pub(in crate::service::project_recovery) fn admit(
    tx: &impl ReadOps,
    view: &RecoveryView,
    input: &DecisionInput,
) -> Result<StoryNo, StoreError> {
    let join = input
        .join_recovery
        .as_ref()
        .ok_or_else(|| invalid("missing join decision"))?;
    if view.state.shared.is_none() || join.recovery == view.record.id {
        return Err(invalid("only distinct native shared faults can coordinate"));
    }
    let owner = persistence::find(tx, view.record.project, &join.recovery)?;
    if !owner.record.active
        || owner.record.revision != join.revision
        || owner.state.shared.is_none()
        || leader(&owner.state).is_some()
        || !super::evidence_current(tx, &owner)?
        || owner
            .state
            .work
            .iter()
            .any(|work| !matches!(work.status, WorkStatus::Pending | WorkStatus::Delivered))
        || owner.state.attempts.iter().any(|attempt| {
            attempt.completion.is_none() || attempt.completion == Some(RepairCompletion::Certified)
        })
        || tx.landing_intents()?.iter().any(|intent| {
            intent.project == view.record.project
                && owner
                    .state
                    .decision
                    .as_ref()
                    .and_then(|decision| decision.repair_story)
                    == Some(intent.story)
        })
    {
        return Err(invalid(
            "repair owner changed, has unsettled effects, or already owns certification",
        ));
    }
    let repair = owner
        .state
        .decision
        .as_ref()
        .filter(|decision| decision.input.scope == RepairScope::SeparateStory)
        .and_then(|decision| decision.repair_story)
        .ok_or_else(|| invalid("join requires a decided separate repair owner"))?;
    let row = tx
        .story(view.record.project, repair)?
        .ok_or_else(|| invalid("repair disappeared"))?;
    if row.superstate != crate::domain::SuperState::Open
        || row.awaiting.is_some()
        || authority::policy_hold(tx, view.record.project, &row.snapshot)?.is_some()
        || super::super::resume::resource_hold(tx, view.record.project, repair)?
    {
        return Err(invalid(
            "repair policy, resources, or manual hold prevents coordination",
        ));
    }
    Ok(repair)
}

/// Validate a one-level star. Inspect the target's join before recursing so a
/// damaged cycle cannot recurse, and never adopt another follower as authority.
pub(in crate::service::project_recovery) fn validate(
    tx: &impl ReadOps,
    record: &ProjectRecovery,
    state: &RecoveryState,
) -> Result<(), StoreError> {
    let Some(join) = state
        .decision
        .as_ref()
        .and_then(|decision| decision.input.join_recovery.as_ref())
    else {
        return Ok(());
    };
    let invalid =
        |message: &str| StoreError::Corrupt(format!("shared recovery coordination: {message}"));
    if state.shared.is_none()
        || join.recovery == record.id
        || !state.work.is_empty()
        || !state.refusals.is_empty()
        || (state.landing.is_none() && !state.attempts.is_empty())
    {
        return Err(invalid(
            "follower has independent effects, attempts, or invalid scope",
        ));
    }
    let owner_record = tx
        .project_recoveries(record.project)?
        .into_iter()
        .find(|owner| owner.id == join.recovery)
        .ok_or_else(|| invalid("owner missing"))?;
    let owner_state = persistence::decode(&owner_record)?;
    if leader(&owner_state).is_some()
        || owner_record.revision < join.revision
        || owner_state.shared.is_none()
    {
        return Err(invalid("owner is stale, foreign, or another follower"));
    }
    let owner = persistence::read_view(tx, owner_record)?;
    if owner.record.active != record.active
        || owner
            .state
            .decision
            .as_ref()
            .and_then(|decision| decision.repair_story)
            != state
                .decision
                .as_ref()
                .and_then(|decision| decision.repair_story)
        || owner.state.landing != state.landing
        || state
            .attempts
            .iter()
            .any(|attempt| !owner.state.attempts.contains(attempt))
    {
        return Err(invalid("repair or release differs from its sole owner"));
    }
    Ok(())
}

/// All followers of this canonical owner, validated before a transactional fanout.
pub(in crate::service::project_recovery) fn followers(
    tx: &impl ReadOps,
    owner: &RecoveryView,
) -> Result<Vec<RecoveryView>, StoreError> {
    let mut followers = Vec::new();
    for record in tx.project_recoveries(owner.record.project)? {
        if record
            .state
            .pointer("/decision/input/join_recovery/recovery")
            .and_then(serde_json::Value::as_str)
            == Some(owner.record.id.as_str())
        {
            followers.push(persistence::read_view(tx, record)?);
        }
    }
    Ok(followers)
}

fn invalid(message: &str) -> StoreError {
    StoreError::Validation(format!("shared recovery coordination: {message}"))
}
