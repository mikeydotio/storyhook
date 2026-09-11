//! Admission of new state changes; historical folding remains unconditional.

use std::collections::BTreeMap;

use super::{
    CLOSED_STATE_SLUG, COMPLETION_STATE_SLUG, StateDef, StoryEvent, StoryIndex, StorySnapshot,
    SuperState, fold_story,
};
use crate::error::AppError;

/// The resolved OPEN dependencies, with the same absent-target semantics as readiness.
pub fn open_blockers(story: &StorySnapshot, index: &impl StoryIndex) -> Vec<String> {
    story
        .relationships
        .iter()
        .filter(|relation| {
            relation.relation == "blocked-by"
                && index
                    .story(&relation.other_id)
                    .is_some_and(|other| other.superstate == SuperState::Open)
        })
        .map(|relation| relation.other_id.clone())
        .collect()
}

/// Validates new events against each preceding fold without revalidating history.
///
/// Catalog order is supplied separately from the fold's slug-keyed map. A move
/// into `blocked` retains the last pipeline position instead of granting access
/// to every column before the board's blocked column.
pub fn validate_append(
    id: &str,
    history: &[StoryEvent],
    proposed: &[StoryEvent],
    states: &[StateDef],
    index: &impl StoryIndex,
) -> Result<(), AppError> {
    let map: BTreeMap<String, StateDef> =
        states.iter().map(|s| (s.slug.clone(), s.clone())).collect();
    let mut events = history.to_vec();
    for event in proposed {
        let before = if events.is_empty() {
            None
        } else {
            Some(fold_story(id, &events, &map)?)
        };
        let pipeline = events
            .iter()
            .rev()
            .find_map(|event| match event {
                StoryEvent::StoryCreated { state, .. }
                | StoryEvent::StoryStateChanged { state, .. }
                | StoryEvent::StoryClosedAndArchived { state, .. }
                    if state != "blocked" =>
                {
                    Some(state.as_str())
                }
                _ => None,
            })
            .or_else(|| {
                states
                    .iter()
                    .find(|s| s.slug != "blocked" && s.super_state == SuperState::Open)
                    .map(|s| s.slug.as_str())
            })
            .map(str::to_owned);
        events.push(event.clone());
        let after = fold_story(id, &events, &map)?;
        if let Some(before) = before {
            validate_transition(&before, &after.state, pipeline.as_deref(), states, index)?;
        }
    }
    Ok(())
}

/// Refuses advancement while dependencies are open, naming the permitted escape.
pub fn validate_transition(
    story: &StorySnapshot,
    target: &str,
    pipeline: Option<&str>,
    states: &[StateDef],
    index: &impl StoryIndex,
) -> Result<(), AppError> {
    if target == story.state || target == CLOSED_STATE_SLUG || target == "blocked" {
        return Ok(());
    }
    let blockers = open_blockers(story, index);
    if blockers.is_empty() {
        return Ok(());
    }
    let source = if story.state == "blocked" {
        pipeline.unwrap_or(&story.state)
    } else {
        &story.state
    };
    let position = |slug: &str| {
        states.iter().position(|s| s.slug == slug).ok_or_else(|| {
        AppError::Validation(format!("cannot compare state `{slug}` for story `{}`; repair the state catalog with `story doctor --fix`", story.id))
    })
    };
    if target == COMPLETION_STATE_SLUG || position(target)? > position(source)? {
        return Err(AppError::Validation(format!(
            "story `{}` cannot advance from `{}` to `{target}` while blocked by {}; move to `closed` to abandon it",
            story.id,
            story.state,
            blockers.join(", ")
        )));
    }
    Ok(())
}
