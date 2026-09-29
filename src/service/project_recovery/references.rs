//! Which stories a recovery names, and which recoveries name a story.
//!
//! A recovery keeps story numbers and exact event sequences for as long as its
//! record lives, retired or not. Removing a story it names strands those
//! references, and every later validation of the record then fails.

use super::{
    AffectedSubmission, Assessment, DecisionReceipt, OwnedAssessmentHold, OwnedDependencyHold,
    RecoveryState, RepairAttempt, RepairLanding, RepairRefusalRecord, WorkDelivery, persistence,
};
use crate::store::{ProjectId, ProjectRecovery, ReadOps, StoreError, StoryNo};
use std::collections::BTreeSet;

/// Every story the state names.
///
/// Each struct is destructured without `..`, so a new field does not compile
/// until its author decides whether it names a story. Candidate evidence is
/// left whole: its `story_id` repeats the owning entry's `story`, and its
/// `blocked_by` lists enrollment-time blockers that no validation reads back,
/// so counting them would refuse deleting an unrelated blocker. A managed
/// lease's `story_id` likewise repeats its work target.
pub(super) fn stories(state: &RecoveryState) -> BTreeSet<StoryNo> {
    let RecoveryState {
        version: _,
        created_at: _,
        updated_at: _,
        subjects,
        assessment,
        decision,
        holds,
        work,
        attempts,
        refusals,
        landing,
        legacy_incidents,
    } = state;
    let mut named = BTreeSet::new();
    for AffectedSubmission {
        candidate: _,
        story,
        state_revision: _,
        label_revision: _,
        returned: _,
    } in subjects
    {
        named.insert(*story);
    }
    let Assessment {
        dispatch_identity: _,
        story,
        generation: _,
        status: _,
        hold: _,
        epoch: _,
        failures: _,
        started_at: _,
        delivered_at: _,
        detail: _,
        last_result: _,
    } = assessment;
    named.insert(*story);
    if let Some(DecisionReceipt {
        input: _,
        accepted_at: _,
        repair_story,
        delivery_identity: _,
        owned_edges,
        dependency_holds,
        skipped_subjects,
    }) = decision
    {
        named.extend(*repair_story);
        named.extend(owned_edges.iter().copied());
        named.extend(skipped_subjects.iter().copied());
        for OwnedDependencyHold {
            story,
            generation: _,
            awaiting: _,
            event: _,
        } in dependency_holds
        {
            named.insert(*story);
        }
    }
    for OwnedAssessmentHold {
        story,
        generation: _,
        cause: _,
        awaiting: _,
        event: _,
    } in holds
    {
        named.insert(*story);
    }
    for WorkDelivery {
        id: _,
        story,
        kind: _,
        source_attempt: _,
        managed_lease: _,
        state: _,
        state_revision: _,
        label_revision: _,
        blocking_revision: _,
        release_event: _,
        status: _,
        hold: _,
        disposition: _,
        epoch: _,
        failures: _,
        started_at: _,
        delivered_at: _,
        last_result: _,
        detail: _,
    } in work
    {
        named.insert(*story);
    }
    for RepairAttempt {
        candidate: _,
        label_revision: _,
        id: _,
        story,
        generation: _,
        input: _,
        admitted_at: _,
        completion: _,
        judgment: _,
        completed_at: _,
    } in attempts
    {
        named.insert(*story);
    }
    for RepairRefusalRecord {
        candidate: _,
        label_revision: _,
        id: _,
        story,
        generation: _,
        input: _,
        reason: _,
        at: _,
        disposition: _,
    } in refusals
    {
        named.insert(*story);
    }
    if let Some(RepairLanding {
        intent,
        attempt: _,
        event: _,
        at: _,
    }) = landing
    {
        named.insert(intent.story);
    }
    named.extend(legacy_incidents.iter().map(|incident| incident.story));
    named
}

/// This project's recoveries whose state or observations name the story, in
/// creation order.
///
/// Decodes each record without validating it against other rows: a record
/// that names the story must be kept whole whether or not it is valid, and one
/// that does not name it has no bearing on the story. A record whose state
/// cannot be decoded fails closed, because what it names is unknown.
pub(crate) fn naming(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
) -> Result<Vec<ProjectRecovery>, StoreError> {
    let mut named = Vec::new();
    for record in tx.project_recoveries(project)? {
        if stories(&persistence::decode(&record)?).contains(&story)
            || tx
                .project_recovery_observations(project, &record.id)?
                .iter()
                .any(|observation| observation.story == story)
        {
            named.push(record);
        }
    }
    Ok(named)
}

/// The validated recoveries of this project that name the story.
///
/// A reader that asks about one story validates only these. A record that
/// does not name the story cannot change the answer, so an invalid one must
/// not stop it (SH-848); a record that names it is validated in full and fails
/// closed.
pub(super) fn views_naming(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
) -> Result<Vec<super::RecoveryView>, StoreError> {
    naming(tx, project, story)?
        .into_iter()
        .map(|record| persistence::read_view(tx, record))
        .collect()
}
