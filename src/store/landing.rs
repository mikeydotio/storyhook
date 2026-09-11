//! Durable merge authority, retained across uncertain external outcomes.

use super::{GlobalSeq, ProjectId, ReadOps, StoreError, StoryNo, StoryQuery};
use crate::domain::landing::VerifiedSubmission;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A single immutable authorization whose absence is required for conflicting writes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandingIntent {
    /// Unique attempt identity, including retries of the same submission.
    pub id: String,
    /// Owning project identity.
    pub project: ProjectId,
    /// Owning story number.
    pub story: StoryNo,
    /// Public story identity at admission.
    pub story_id: String,
    /// Project slug at admission.
    pub project_slug: String,
    /// Submitted generation, never a mutable row timestamp.
    pub generation: GlobalSeq,
    /// Exact close-on-merge pull request URL.
    pub pull_request: String,
    /// Registered checkout used for the operation.
    pub checkout: PathBuf,
    /// Evidence the merge must continue to match.
    pub certification: VerifiedSubmission,
    /// When the intent was durably admitted.
    pub created_at: String,
}

/// Checks all unresolved authority against the transaction's final read model.
///
/// The store calls this before COMMIT, including for raw imports and catalog
/// repairs. It consumes domain projections without reinterpreting event history.
pub(crate) fn validate_pending(tx: &impl ReadOps) -> Result<(), StoreError> {
    for intent in tx.landing_intents()? {
        validate_intent(tx, &intent)?;
    }
    Ok(())
}

/// Validates one immutable intent before acquisition, commit, or resolution.
pub(crate) fn validate_intent(tx: &impl ReadOps, intent: &LandingIntent) -> Result<(), StoreError> {
    use crate::domain::{
        StoryEvent, SuperState, VERIFYING_STATE_SLUG, apply_computed_epic_states, is_epic,
    };
    let refuse = |detail: &str| {
        StoreError::Validation(format!(
            "story `{}` has unresolved landing `{}`: {detail}; reconcile its merge outcome before changing submission authority or blockers",
            intent.story_id, intent.id,
        ))
    };
    intent.certification.validate()?;
    if tx.story_resets(intent.project)?.contains_key(&intent.story) {
        return Err(refuse("workspace reset is reserved"));
    }
    let project = tx
        .project(intent.project)?
        .ok_or_else(|| refuse("project cannot be removed"))?;
    if project.slug != intent.project_slug
        || intent.story.to_id(&project.prefix) != intent.story_id
        || tx.checkout_path(intent.project)?.as_ref() != Some(&intent.checkout)
    {
        return Err(refuse("project identity or checkout changed"));
    }
    let states = tx.states(intent.project)?;
    if !states
        .iter()
        .any(|state| state.slug == VERIFYING_STATE_SLUG && state.super_state == SuperState::Open)
        || crate::domain::completion_state(&states).is_none()
    {
        return Err(refuse("required submission or completion state changed"));
    }
    let pr = crate::domain::pr_url::parse_pr_url(&intent.pull_request)?;
    let remote_present = tx.project_remotes(intent.project)?.iter().any(|remote| {
        crate::domain::github_remote::parse_github_url(&remote.raw).is_some_and(|repo| {
            repo.host.eq_ignore_ascii_case(&pr.host)
                && repo.owner.eq_ignore_ascii_case(&pr.owner)
                && repo.repo.eq_ignore_ascii_case(&pr.repo)
        })
    });
    if !remote_present {
        return Err(refuse("registered pull request repository changed"));
    }
    let mut index: std::collections::BTreeMap<_, _> = tx
        .stories(intent.project, &StoryQuery::all())?
        .into_iter()
        .map(|r| (r.snapshot.id.clone(), r.snapshot))
        .collect();
    apply_computed_epic_states(&mut index, &states);
    let story = index
        .get(&intent.story_id)
        .ok_or_else(|| refuse("story cannot be removed"))?;
    if story.state != VERIFYING_STATE_SLUG || story.superstate != SuperState::Open || is_epic(story)
    {
        return Err(refuse("submitted state changed"));
    }
    let events = tx.events_for(intent.project, intent.story)?;
    let generation = events.iter().rev().find_map(|event| match event.known() {
        Some(StoryEvent::StoryStateChanged { .. }) => Some(event.global_seq),
        _ => None,
    });
    if generation != Some(intent.generation) {
        return Err(refuse("submission generation changed"));
    }
    let links: Vec<_> = tx
        .open_pr_links_for_story(intent.project, intent.story)?
        .into_iter()
        .filter(|p| p.close_on_merge)
        .collect();
    if links.len() != 1 || links[0].url != intent.pull_request {
        return Err(refuse("linked pull request changed"));
    }
    let blockers = crate::domain::transition::open_blockers(story, &index);
    if !blockers.is_empty() {
        return Err(refuse(&format!(
            "open blockers introduced: {}",
            blockers.join(", ")
        )));
    }
    Ok(())
}
