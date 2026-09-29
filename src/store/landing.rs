//! Durable merge authority, retained across uncertain external outcomes.

use super::{BatchId, BatchPhase, GlobalSeq, ProjectId, ReadOps, StoreError, StoryNo, StoryQuery};
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
    /// The verification batch this story lands with, when it is a batch
    /// member (SH-832): `pull_request` stays the member's own, while the
    /// merge requested and observed is the batch pull request's, at the
    /// batch certification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<BatchLanding>,
}

/// What binds a member's landing intent to its batch (SH-832). Every member
/// row of one batch carries the same value; the batch record is the
/// authority for which stories are members.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchLanding {
    /// The verification batch.
    pub id: BatchId,
    /// The one merge attempt every member row shares; it names the landing
    /// marker that proves whether the merge request was sent.
    pub landing: String,
    /// The batch pull request the merge lands.
    pub pull_request: String,
}

impl LandingIntent {
    /// The pull request whose merge this intent requests and observes: the
    /// batch pull request for a batch member, else the story's own.
    #[must_use]
    pub fn landing_pull_request(&self) -> &str {
        self.batch
            .as_ref()
            .map_or(&self.pull_request, |batch| &batch.pull_request)
    }

    /// The identity of the merge attempt, shared by every member of a batch.
    #[must_use]
    pub fn landing_attempt(&self) -> &str {
        self.batch.as_ref().map_or(&self.id, |batch| &batch.landing)
    }
}

/// One batch's landing: the member intents that share a [`BatchLanding`]
/// (spec position B6, "the same guard, many stories").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchLandingIntent {
    /// What binds the rows to their batch.
    pub batch: BatchLanding,
    /// The batch certification every row carries.
    pub certification: VerifiedSubmission,
    /// The members' own intents still pending, in story order.
    pub rows: Vec<LandingIntent>,
}

impl BatchLandingIntent {
    /// Groups the batch members among `intents` by batch, in the order each
    /// batch first appears. Single-story intents are left out.
    #[must_use]
    pub fn collect(intents: &[LandingIntent]) -> Vec<Self> {
        let mut batches: Vec<Self> = Vec::new();
        for intent in intents {
            let Some(batch) = &intent.batch else {
                continue;
            };
            match batches.iter_mut().find(|known| known.batch.id == batch.id) {
                Some(known) => known.rows.push(intent.clone()),
                None => batches.push(Self {
                    batch: batch.clone(),
                    certification: intent.certification.clone(),
                    rows: vec![intent.clone()],
                }),
            }
        }
        batches
    }

    /// The row of `story_id`, if it is still pending.
    #[must_use]
    pub fn row(&self, story_id: &str) -> Option<&LandingIntent> {
        self.rows.iter().find(|row| row.story_id == story_id)
    }
}

/// Checks all unresolved authority against the transaction's final read model.
///
/// The store calls this before COMMIT, including for raw imports and catalog
/// repairs. It consumes domain projections without reinterpreting event history.
pub(crate) fn validate_pending(tx: &impl ReadOps) -> Result<(), StoreError> {
    let intents = tx.landing_intents()?;
    for intent in &intents {
        validate_intent(tx, intent)?;
    }
    for batch in BatchLandingIntent::collect(&intents) {
        validate_batch(tx, &batch)?;
    }
    Ok(())
}

/// Checks one batch's member intents against each other and against the
/// batch record (SH-832). Before the merge is confirmed (`landing`) the rows
/// are exactly the members, so no commit resolves one member alone; once it
/// is (`landed`) they are a subset: a member a person holds keeps its row.
fn validate_batch(tx: &impl ReadOps, batch: &BatchLandingIntent) -> Result<(), StoreError> {
    let refuse = |detail: &str| {
        StoreError::Validation(format!(
            "verification batch {} has unresolved landing `{}`: {detail}; reconcile its merge outcome before changing its members",
            batch.batch.id, batch.batch.landing,
        ))
    };
    let first = &batch.rows[0];
    if batch.rows.iter().any(|row| {
        row.batch.as_ref() != Some(&batch.batch)
            || row.certification != batch.certification
            || row.project != first.project
            || row.checkout != first.checkout
    }) {
        return Err(refuse(
            "its member intents disagree about the batch, certification or checkout",
        ));
    }
    let record = tx
        .verification_batches(first.project)?
        .into_iter()
        .find(|record| record.id == batch.batch.id)
        .ok_or_else(|| refuse("the batch record is missing"))?;
    if record.tip != batch.certification.head
        || record
            .pull_request
            .as_ref()
            .is_none_or(|pull_request| pull_request.url != batch.batch.pull_request)
    {
        return Err(refuse(
            "the certified head or batch pull request is not the batch record's",
        ));
    }
    for row in &batch.rows {
        let member = record
            .members
            .iter()
            .find(|member| member.story == row.story)
            .ok_or_else(|| refuse(&format!("{} is not a member of the batch", row.story_id)))?;
        if member.generation != row.generation || member.pull_request != row.pull_request {
            return Err(refuse(&format!(
                "{}'s generation or pull request is not the one the batch recorded",
                row.story_id
            )));
        }
    }
    match record.phase {
        BatchPhase::Landing if batch.rows.len() == record.members.len() => Ok(()),
        BatchPhase::Landing => Err(refuse(
            "a member's intent was resolved before the batch merge was confirmed",
        )),
        BatchPhase::Landed => Ok(()),
        phase => Err(refuse(&format!(
            "the batch record is {}, not landing",
            phase.as_str()
        ))),
    }
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
    if tx.story_resets(intent.project)?.contains_key(&intent.story)
        || tx
            .story_reset(intent.project, intent.story)?
            .is_some_and(|reset| !reset.completed)
        || tx.engine_reset(intent.project, intent.story)?.is_some()
    {
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
    if story.awaiting.is_some() {
        return Err(refuse(
            "awaiting would hide unresolved merge authority from recovery",
        ));
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
