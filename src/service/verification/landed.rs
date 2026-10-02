//! Complete observed ancestry without inventing a gate certification.
use super::*;
use crate::domain::landing::AlreadyLanded;

impl<S: Store> VerificationQueue<'_, S> {
    /// Closes only the still-current, unblocked owner of the observed branch.
    pub(crate) fn complete_already_landed(
        &self,
        ctx: &Ctx<'_, S>,
        candidate: &VerificationCandidate,
        evidence: &AlreadyLanded,
    ) -> Result<GenerationWrite<()>, AppError> {
        evidence.validate()?;
        if ctx.project() != candidate.project {
            return Err(AppError::Validation(
                "landed context belongs to another project".into(),
            ));
        }
        let repository = super::super::github_repository::repository(ctx)?;
        if !repository
            .qualified()
            .eq_ignore_ascii_case(&evidence.repository)
        {
            return Err(AppError::Validation(
                "landed evidence belongs to another repository".into(),
            ));
        }
        if let Some(url) = &evidence.merged_pr
            && !candidate
                .pull_request
                .as_ref()
                .is_ok_and(|link| &link.url == url)
        {
            return Err(AppError::Validation(
                "landed evidence names an unlinked pull request".into(),
            ));
        }
        Ok(ctx.write_stories(|tx| {
            let prefix = project_prefix(tx, candidate.project)?;
            let (story, row) = resolve_story(tx, candidate.project, &prefix, &candidate.story_id)?;
            // Re-derive all queue holds and resource identities in the writing transaction.
            let current = ordered_candidates_for(tx, candidate.project)?.into_iter()
                .find(|c| c.story_id == candidate.story_id);
            if current.as_ref().is_none_or(|current|
                current.verifying_generation != candidate.verifying_generation
                || current.cleanup_lease != candidate.cleanup_lease
                || current.checkout != candidate.checkout
                || current.project_slug != candidate.project_slug
                || current.pull_request != candidate.pull_request
                || current.human_only_revision != candidate.human_only_revision
                || current.blocking_revision != candidate.blocking_revision
                || !current.blocked_by.is_empty()
                || current.landing_pending)
                || !candidate.blocked_by.is_empty()
                || candidate.landing_pending || !candidate_is_current(tx, &row, candidate)?
            {
                return Ok(GenerationWrite::Superseded);
            }
            let tree_prefix = format!("{VERIFICATION_GREEN_PREFIX} merge tree `{}` passed `", evidence.base_tree);
            let certified_by = tx.stories(candidate.project, &StoryQuery::all())?.into_iter()
                .find(|other| other.snapshot.comments.iter().any(|comment| comment.text.starts_with(&tree_prefix)))
                .map(|other| other.story_no.to_id(&prefix));
            let certification = match certified_by {
                Some(id) => format!("A retained central GREEN on {id} certified this exact tree."),
                None => "No central GREEN was found for this exact tree. Completion is based on commit ancestry.".into(),
            };
            let text = format!(
                "{VERIFICATION_ALREADY_LANDED_PREFIX} head `{}` is contained by origin/{} at base commit `{}` (tree `{}`). {certification}",
                evidence.head_oid, evidence.base, evidence.base_oid, evidence.base_tree,
            );
            let mut events = vec![StoryEvent::StoryCommentAdded { at: ctx.now(), text }];
            if let Some(url) = &evidence.merged_pr
                && tx.open_pr_links_for_story(candidate.project, story)?.iter().any(|link| &link.url == url)
            {
                events.push(StoryEvent::StoryPrMerged { at: ctx.now(), url: url.clone() });
            }
            let done = completion_state_or_refuse(&tx.states(candidate.project)?)?;
            let states = tx.state_map(candidate.project)?;
            clear_candidate_incident(tx, candidate)?;
            append_state_transition(tx, candidate.project, story, &row, &prefix, &states,
                &done, &ctx.now(), events, ctx.provenance())?;
            Ok(GenerationWrite::Applied(()))
        })?)
    }
}

/// Open links take precedence. Only a lease-free generation can recover a merged
/// link, and a prior closed lifecycle cannot lend its PR to a reopened story.
pub(super) fn candidate_links(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
    leased: bool,
) -> Result<Vec<PrLink>, StoreError> {
    let open: Vec<_> = tx
        .open_pr_links_for_story(project, story)?
        .into_iter()
        .filter(|link| link.close_on_merge)
        .collect();
    if leased || !open.is_empty() {
        return Ok(open);
    }
    let events = tx.events_for(project, story)?;
    let states = tx.state_map(project)?;
    let start = events
        .iter()
        .rposition(|event| {
            matches!(event.known(),
        Some(StoryEvent::StoryStateChanged { state, .. } | StoryEvent::StoryClosedAndArchived { state, .. })
            if states.get(state).is_some_and(|s| s.super_state == SuperState::Closed))
        })
        .map_or(0, |index| index + 1);
    let linked: Vec<_> = events[start..]
        .iter()
        .filter_map(|event| match event.known() {
            Some(StoryEvent::StoryPrLinked { url, .. }) => Some(url.as_str()),
            _ => None,
        })
        .collect();
    Ok(tx
        .pr_links(project)?
        .into_iter()
        .filter_map(|(number, link)| {
            (number == story
                && link.close_on_merge
                && link.status == "merged"
                && linked.contains(&link.url.as_str()))
            .then_some(link)
        })
        .collect())
}
