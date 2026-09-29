//! When a landed recovery stops owing work, so status can stop showing it.
//!
//! The durable record never goes: coordination, resume ownership and
//! `verifier repair show` keep reading it. Only the current-status projection
//! asks what is still outstanding (docs/spec/project-fault-recovery.md,
//! "Resolution").

use super::RecoveryView;
use crate::{
    domain::{StateDef, StoryEvent, SuperState},
    service::verification::VERIFYING_STATE,
    store::{GlobalSeq, ProjectId, ReadOps, StoreError, StoryNo},
};
use std::collections::{BTreeMap, BTreeSet};

/// What a landed recovery still owes, by story.
#[derive(Debug)]
pub(super) struct Outstanding {
    /// Open stories whose current awaiting is still one this recovery wrote.
    pub held: BTreeSet<StoryNo>,
    /// Affected stories, not held, that still owe a fresh verification
    /// generation for the submission this recovery left unjudged.
    pub owed: BTreeSet<StoryNo>,
}

impl Outstanding {
    /// Whether the recovery still has a claim on this story.
    pub fn claims(&self, story: StoryNo) -> bool {
        self.held.contains(&story) || self.owed.contains(&story)
    }
}

/// Reads the held and owed stories of one recovery.
pub(super) fn outstanding(
    tx: &impl ReadOps,
    view: &RecoveryView,
) -> Result<Outstanding, StoreError> {
    let project = view.record.project;
    let state = &view.state;
    // Every awaiting this recovery wrote, by exact event: a replacement with
    // identical text is someone else's hold.
    let owned = state
        .decision
        .iter()
        .flat_map(|d| d.dependency_holds.iter())
        .map(|h| (h.story, &h.awaiting, h.event))
        .chain(state.holds.iter().map(|h| (h.story, &h.awaiting, h.event)))
        .chain(state.work.iter().filter_map(|w| {
            w.disposition
                .as_ref()
                .map(|d| (w.story, &d.awaiting, d.event))
        }));
    let mut held = BTreeSet::new();
    for (story, awaiting, event) in owned {
        if held.contains(&story) {
            continue;
        }
        // An awaiting left on a closed story blocks nothing.
        if let Some(row) = tx.story(project, story)?
            && row.superstate == SuperState::Open
            && row.awaiting.as_ref() == Some(awaiting)
            && super::resume::awaiting_revision(tx, project, story)? == Some(event)
        {
            held.insert(story);
        }
    }
    let states = tx.state_map(project)?;
    let mut owed = BTreeSet::new();
    for subject in &state.subjects {
        if held.contains(&subject.story) || owed.contains(&subject.story) {
            continue;
        }
        let generation = subject
            .candidate
            .verifying_generation
            .ok_or_else(|| StoreError::Corrupt("recovery subject has no generation".into()))?;
        if tx.story(project, subject.story)?.is_some()
            && !discharged(tx, project, subject.story, generation, &states)?
        {
            owed.insert(subject.story);
        }
    }
    Ok(Outstanding { held, owed })
}

/// Whether the story left its unjudged generation for good: a later entry into
/// verification (a fresh generation) or into a closed state. The log is
/// append-only, so a discharge survives a reopen and a resolved recovery never
/// comes back.
fn discharged(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
    generation: GlobalSeq,
    states: &BTreeMap<String, StateDef>,
) -> Result<bool, StoreError> {
    Ok(tx
        .events_for(project, story)?
        .iter()
        .filter(|event| event.global_seq > generation)
        .any(|event| match event.known() {
            Some(StoryEvent::StoryStateChanged { state, .. }) => {
                state == VERIFYING_STATE
                    || states
                        .get(state)
                        .is_some_and(|def| def.super_state == SuperState::Closed)
            }
            Some(StoryEvent::StoryClosedAndArchived { .. } | StoryEvent::StoryDeleted { .. }) => {
                true
            }
            _ => false,
        }))
}
