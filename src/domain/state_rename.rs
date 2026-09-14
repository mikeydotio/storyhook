//! Compatibility for the SH-663 abandonment-state rename.

use std::collections::BTreeMap;

use super::{DROPPED_STATE_SLUG, StateDef, SuperState};
use crate::error::AppError;

/// Normalizes an imported catalog without reclassifying or merging user states.
pub(crate) fn normalize_catalog(states: &[StateDef]) -> Result<Vec<StateDef>, AppError> {
    let legacy = states
        .iter()
        .position(|s| s.slug == "closed" && s.super_state == SuperState::Closed);
    let dropped = states.iter().find(|s| s.slug == DROPPED_STATE_SLUG);
    if dropped.is_some_and(|s| s.super_state != SuperState::Closed)
        || (legacy.is_some() && dropped.is_some())
    {
        return Err(AppError::Validation(
            "conflicting state `dropped`: it must be CLOSED and cannot coexist with legacy `closed`/CLOSED; resolve the conflicting catalog with the previous binary before retrying".into(),
        ));
    }
    let mut normalized = states.to_vec();
    if let Some(index) = legacy {
        normalized[index].slug = DROPPED_STATE_SLUG.into();
    }
    Ok(normalized)
}

/// Resolves historical spellings only after the catalog has been migrated.
/// An existing OPEN `closed` state retains its exact historical meaning.
pub(crate) fn historical_slug<'a>(slug: &'a str, states: &BTreeMap<String, StateDef>) -> &'a str {
    if slug == "closed"
        && !states.contains_key("closed")
        && states
            .get(DROPPED_STATE_SLUG)
            .is_some_and(|s| s.super_state == SuperState::Closed)
    {
        DROPPED_STATE_SLUG
    } else {
        slug
    }
}
