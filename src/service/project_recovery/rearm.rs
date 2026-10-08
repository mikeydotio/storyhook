//! Policy release is new coordination authority, never new fault or delivery evidence.
use super::{
    AssessmentDelivery, AssessmentHold, AssessmentStatus, ProjectRecoveryService, RecoveryView,
    WorkDelivery, WorkKind, WorkStatus, authority, persistence,
};
use crate::{
    domain::{StoryEvent, is_reserved_label},
    error::AppError,
    service::verification,
    store::{GlobalSeq, ReadOps, Store, StoreError, StoryNo},
};
use std::collections::BTreeSet;

/// Current policy-release evaluation, independent of immutable delivery evidence.
pub(super) enum PolicyRearm {
    /// Retained authority permits a transactional rearm.
    Ready,
    /// A current policy or authority constraint still prevents rearming.
    Held(AssessmentHold),
    /// Delivery evidence or terminal disposition forbids automatic replay.
    Retained,
}

impl PolicyRearm {
    /// Explain the present constraint without overwriting the retained hold.
    pub(super) fn detail(&self, recovery: &str) -> String {
        let reason = match self {
            Self::Ready => {
                "Reservation and stop policy permit recovery; wait for managed reconciliation."
            }
            Self::Held(hold) => hold.detail(),
            Self::Retained => {
                "Retained delivery evidence or a terminal story hold requires ownership reconciliation; do not retry automatically."
            }
        };
        format!(
            "{reason} Read `story verifier repair show {recovery} --json` for retained evidence."
        )
    }
}

fn removable(hold: Option<AssessmentHold>) -> bool {
    matches!(
        hold,
        Some(AssessmentHold::ReservedLabel | AssessmentHold::OperatorStop)
    )
}

fn undelivered(
    epoch: u32,
    failures: u8,
    delivered: Option<&str>,
    result: Option<&AssessmentDelivery>,
) -> bool {
    delivered.is_none()
        && failures < 3
        && match result {
            None => epoch == 0 && failures == 0,
            Some(AssessmentDelivery::ProvenFailure(_)) => {
                failures > 0 && u32::from(failures) == epoch
            }
            _ => false,
        }
}

/// Only removal of the enrollment reservation can renew initial label authority.
fn reservation_released(
    tx: &impl ReadOps,
    view: &RecoveryView,
    story: StoryNo,
    revision: Option<GlobalSeq>,
) -> Result<bool, StoreError> {
    let events = tx.events_for(view.record.project, story)?;
    let reserved = |labels: &[String]| {
        labels
            .iter()
            .filter(|label| is_reserved_label(label))
            .cloned()
            .collect::<BTreeSet<_>>()
    };
    let mut previous = if let Some(revision) = revision {
        let Some(labels) = events.iter().find_map(|event| {
            if event.global_seq != revision {
                return None;
            }
            match event.known() {
                Some(StoryEvent::StoryLabelsSet { labels, .. }) => Some(labels),
                _ => None,
            }
        }) else {
            return Ok(false);
        };
        reserved(labels)
    } else {
        BTreeSet::new()
    };
    for event in events
        .iter()
        .filter(|event| revision.is_none_or(|seq| event.global_seq > seq))
    {
        if let Some(StoryEvent::StoryLabelsSet { labels, .. }) = event.known() {
            let current = reserved(labels);
            if !current.is_subset(&previous) {
                return Ok(false);
            }
            previous = current;
        }
    }
    Ok(previous.is_empty())
}

/// Evaluate a policy-held assessment without changing its generation or evidence.
pub(super) fn assessment(
    tx: &impl ReadOps,
    view: &RecoveryView,
) -> Result<Option<PolicyRearm>, StoreError> {
    let assessment = &view.state.assessment;
    if !view.record.active
        || assessment.status != AssessmentStatus::Held
        || !removable(assessment.hold)
    {
        return Ok(None);
    }
    let project = view.record.project;
    let subject = view
        .state
        .subjects
        .iter()
        .find(|subject| {
            subject.story == assessment.story
                && subject.candidate.verifying_generation == Some(assessment.generation)
        })
        .ok_or_else(|| {
            StoreError::Corrupt("held assessment has no originating submission".into())
        })?;
    let Some(row) = tx.story(project, subject.story)? else {
        return Ok(Some(PolicyRearm::Held(AssessmentHold::SubjectMissing)));
    };
    if let Some(hold) = authority::policy_hold(tx, project, &row.snapshot)? {
        return Ok(Some(PolicyRearm::Held(hold)));
    }
    if view.state.shared.is_none() && (!subject.returned || view.state.decision.is_none()) {
        return Ok(Some(PolicyRearm::Held(AssessmentHold::CauseUnproved)));
    }
    if !undelivered(
        assessment.epoch,
        assessment.failures,
        assessment.delivered_at.as_deref(),
        assessment.last_result.as_ref(),
    ) || view
        .state
        .holds
        .iter()
        .any(|hold| hold.story == subject.story && hold.generation == assessment.generation)
    {
        return Ok(Some(PolicyRearm::Retained));
    }
    let same_labels = if subject.returned {
        authority::label_revision(tx, project, subject.story)? == subject.label_revision
    } else {
        reservation_released(tx, view, subject.story, subject.label_revision)?
    };
    if !same_labels
        || authority::state_revision(tx, project, subject.story)? != subject.state_revision
        || authority::blocking_revision(tx, project, subject.story)?
            != subject.candidate.blocking_revision
        || verification::verifying_entry(tx, project, subject.story)?
            .map(|(_, generation)| generation)
            != Some(assessment.generation)
        || row.state
            != if subject.returned {
                verification::RETURNED_STATE
            } else {
                verification::VERIFYING_STATE
            }
    {
        return Ok(Some(PolicyRearm::Held(AssessmentHold::AuthorityChanged)));
    }
    if row.awaiting.is_some()
        || super::resume::resource_hold(tx, project, subject.story)?
        || crate::domain::is_blocked(
            &row.snapshot,
            &crate::service::query::story_map(tx, project)?,
        )
    {
        return Ok(Some(PolicyRearm::Held(
            AssessmentHold::ResourceOrDependency,
        )));
    }
    if view.state.shared.is_some() && !super::shared::retained_current(tx, view, subject)? {
        return Ok(Some(PolicyRearm::Held(AssessmentHold::AuthorityChanged)));
    }
    Ok(Some(PolicyRearm::Ready))
}

/// Evaluate a policy-held repair or certified resume under its existing authority.
pub(super) fn work(
    tx: &impl ReadOps,
    view: &RecoveryView,
    work: &WorkDelivery,
) -> Result<Option<PolicyRearm>, StoreError> {
    if (!view.record.active && work.kind != WorkKind::Resume)
        || work.status != WorkStatus::Held
        || !removable(work.hold)
    {
        return Ok(None);
    }
    let Some(row) = tx.story(view.record.project, work.story)? else {
        return Ok(Some(PolicyRearm::Held(AssessmentHold::SubjectMissing)));
    };
    if let Some(hold) = authority::policy_hold(tx, view.record.project, &row.snapshot)? {
        return Ok(Some(PolicyRearm::Held(hold)));
    }
    if work.disposition.is_some()
        || !undelivered(
            work.epoch,
            work.failures,
            work.delivered_at.as_deref(),
            work.last_result.as_ref(),
        )
    {
        return Ok(Some(PolicyRearm::Retained));
    }
    Ok(Some(match super::work::permitted(tx, view, work)? {
        Some(hold) => PolicyRearm::Held(hold),
        None => PolicyRearm::Ready,
    }))
}

fn evaluate(
    tx: &impl ReadOps,
    view: &RecoveryView,
    effect: Option<&str>,
) -> Result<Option<PolicyRearm>, StoreError> {
    if let Some(effect) = effect {
        let target = view
            .state
            .work
            .iter()
            .find(|work| work.id == effect)
            .ok_or_else(|| {
                StoreError::Validation("recovery has no matching policy-held effect".into())
            })?;
        work(tx, view, target)
    } else {
        assessment(tx, view)
    }
}

impl<S: Store> ProjectRecoveryService<'_, S> {
    /// Read whether a policy-held effect can regain authority without replaying delivery.
    pub fn policy_rearm_ready(&self, id: &str, effect: Option<&str>) -> Result<bool, AppError> {
        self.ctx
            .store()
            .read(|tx| {
                let view = persistence::find(tx, self.ctx.project(), id)?;
                Ok(matches!(
                    evaluate(tx, &view, effect)?,
                    Some(PolicyRearm::Ready)
                ))
            })
            .map_err(Into::into)
    }

    /// Recheck and rearm one undelivered effect while its caller holds workspace ownership.
    /// No transport runs here; the ordinary claim must still validate current authority.
    pub fn rearm_policy_hold(&self, id: &str, effect: Option<&str>) -> Result<bool, AppError> {
        let now = self.ctx.now();
        self.ctx
            .write_stories(|tx| {
                let mut view = persistence::find(tx, self.ctx.project(), id)?;
                if !matches!(evaluate(tx, &view, effect)?, Some(PolicyRearm::Ready)) {
                    return Ok(false);
                }
                if let Some(effect) = effect {
                    let work = view
                        .state
                        .work
                        .iter_mut()
                        .find(|work| work.id == effect)
                        .expect("evaluated effect");
                    work.status = WorkStatus::Pending;
                    work.hold = None;
                    work.detail =
                        "policy released; managed delivery pending under retained authority".into();
                } else {
                    view.state.assessment.status = AssessmentStatus::Pending;
                    view.state.assessment.hold = None;
                    view.state.assessment.detail =
                        "policy released; scope assessment pending under retained authority".into();
                }
                persistence::save(tx, &mut view, &now)?;
                Ok(true)
            })
            .map_err(Into::into)
    }
}
