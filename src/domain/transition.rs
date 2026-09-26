//! Admission of new state changes; historical folding remains unconditional.

use std::collections::BTreeMap;

use super::{
    COMPLETION_STATE_SLUG, DROPPED_STATE_SLUG, StateDef, StoryEvent, StoryIndex, StorySnapshot,
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
                    Some(super::state_rename::historical_slug(state, &map))
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
    if target == story.state || target == DROPPED_STATE_SLUG || target == "blocked" {
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
            "story `{}` cannot advance from `{}` to `{target}` while blocked by {}; move to `dropped` to abandon it",
            story.id,
            story.state,
            blockers.join(", ")
        )));
    }
    Ok(())
}

/// Refuses a creation state that a story filed with `blocked-by` edges may
/// not open in (SH-779).
///
/// `blockers` is every story named at creation; `open_blockers` is the subset
/// whose effective superstate is OPEN, judged against the same computed index
/// readiness uses. Two states are refused:
///
/// * `blocked`, whatever the blockers' states. The reserved state is a hold of
///   its own: it outlives the edges, which retract when their blockers close,
///   so the story would never become ready again without a manual move.
/// * Any state later in catalog order than the default open state, while a
///   blocker is open — the same position rule [`validate_transition`] applies
///   to a move, so a story cannot be filed where it could not have advanced.
///
/// # Errors
///
/// [`AppError::Validation`] naming the refused state, the blockers, and the
/// state to file in instead; or naming `story doctor --fix` when the catalog
/// cannot place the state.
pub fn validate_blocked_creation(
    state: &str,
    states: &[StateDef],
    blockers: &[String],
    open_blockers: &[String],
) -> Result<(), AppError> {
    if blockers.is_empty() {
        return Ok(());
    }
    let default = super::default_open_state(states).ok_or_else(|| {
        AppError::Validation("project has no OPEN-mapped default state".to_string())
    })?;
    if state == "blocked" {
        return Err(AppError::Validation(format!(
            "a story filed with blockers cannot open in `blocked`: that state is a separate hold \
             that stays after {} close, so the story would never become ready again. File it in \
             `{}`; its blocked-by edges already keep it from being claimed",
            blockers.join(", "),
            default.slug
        )));
    }
    if open_blockers.is_empty() {
        return Ok(());
    }
    let position = |slug: &str| {
        states.iter().position(|s| s.slug == slug).ok_or_else(|| {
            AppError::Validation(format!(
                "cannot compare state `{slug}`; repair the state catalog with `story doctor --fix`"
            ))
        })
    };
    if position(state)? > position(&default.slug)? {
        return Err(AppError::Validation(format!(
            "a story cannot be filed in `{state}` while it is blocked by {}; file it in `{}` and \
             move it once its blockers close",
            open_blockers.join(", "),
            default.slug
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(slug: &str, super_state: SuperState) -> StateDef {
        StateDef {
            slug: slug.to_string(),
            super_state,
            role: None,
            description: None,
        }
    }

    fn catalog() -> Vec<StateDef> {
        vec![
            state("todo", SuperState::Open),
            state("in-progress", SuperState::Open),
            state("verifying", SuperState::Open),
            state("blocked", SuperState::Open),
            state("done", SuperState::Closed),
            state("dropped", SuperState::Closed),
        ]
    }

    fn ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_string()).collect()
    }

    #[test]
    fn no_blockers_admits_every_state() {
        for slug in ["todo", "in-progress", "verifying", "blocked"] {
            validate_blocked_creation(slug, &catalog(), &[], &[]).unwrap();
        }
    }

    #[test]
    fn the_default_state_is_always_admitted() {
        validate_blocked_creation("todo", &catalog(), &ids(&["SH-1"]), &ids(&["SH-1"])).unwrap();
    }

    #[test]
    fn an_advanced_state_is_refused_only_while_a_blocker_is_open() {
        for slug in ["in-progress", "verifying"] {
            let error = validate_blocked_creation(
                slug,
                &catalog(),
                &ids(&["SH-1", "SH-2"]),
                &ids(&["SH-2"]),
            )
            .unwrap_err()
            .to_string();
            assert!(error.contains(slug), "{error}");
            assert!(error.contains("SH-2"), "names the open blocker: {error}");
            assert!(
                !error.contains("SH-1"),
                "a closed blocker holds nothing: {error}"
            );
            assert!(error.contains("`todo`"), "names the state to use: {error}");
            validate_blocked_creation(slug, &catalog(), &ids(&["SH-1"]), &[]).unwrap();
        }
    }

    #[test]
    fn the_blocked_state_is_refused_whatever_the_blockers_are() {
        for open in [ids(&["SH-1"]), Vec::new()] {
            let error = validate_blocked_creation("blocked", &catalog(), &ids(&["SH-1"]), &open)
                .unwrap_err()
                .to_string();
            assert!(error.contains("`blocked`"), "{error}");
            assert!(error.contains("`todo`"), "{error}");
        }
    }

    #[test]
    fn the_rule_follows_catalog_order_not_the_default_catalog() {
        // A catalog whose pipeline starts at `backlog`: `todo` is now an
        // advancement, and `backlog` is where a blocked filing belongs.
        let mut states = vec![state("backlog", SuperState::Open)];
        states.extend(catalog());
        let error = validate_blocked_creation("todo", &states, &ids(&["SH-1"]), &ids(&["SH-1"]))
            .unwrap_err()
            .to_string();
        assert!(error.contains("`backlog`"), "{error}");
        validate_blocked_creation("backlog", &states, &ids(&["SH-1"]), &ids(&["SH-1"])).unwrap();
    }

    #[test]
    fn a_state_the_catalog_cannot_place_names_the_repair() {
        let error =
            validate_blocked_creation("limbo", &catalog(), &ids(&["SH-1"]), &ids(&["SH-1"]))
                .unwrap_err()
                .to_string();
        assert!(error.contains("story doctor --fix"), "{error}");
    }
}
