//! Turning stored stories into the [`StoryView`]s a response carries.
//!
//! This is the read side, and it is here in its minimal form because every
//! write in [`crate::service`] answers with the story it changed. The query
//! service that will own filtering, sorting, and the summary surfaces is a
//! later wave's work; what lives here is only what a lifecycle response needs,
//! and it is expected to be absorbed rather than duplicated when that arrives.
//!
//! One legacy behaviour is deliberately absent. `app::build_story_views` flags
//! a story that exists in *both* the open directory and the archive — the
//! SH-20 split-brain shape. A story in the store is one row; the condition is
//! not representable, so there is nothing to flag.

use std::collections::BTreeMap;

use crate::domain::{
    StorySnapshot, compute_integrity_issues, compute_progress, derive_family_relationships,
};
use crate::error::AppError;
use crate::output::{Response, StoryView};
use crate::store::{ProjectId, ReadOps, StoryQuery};

/// Every story in the project, keyed by id.
pub(crate) fn story_map(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<BTreeMap<String, StorySnapshot>, AppError> {
    Ok(tx
        .stories(project, &StoryQuery::all())?
        .into_iter()
        .map(|row| (row.snapshot.id.clone(), row.snapshot))
        .collect())
}

/// Every story in the project as a view, with the cross-story facts —
/// integrity flags, progress rollups, and optionally the derived family
/// relationships — filled in.
pub fn story_views(
    tx: &impl ReadOps,
    project: ProjectId,
    include_derived: bool,
) -> Result<Vec<StoryView>, AppError> {
    let stories = story_map(tx, project)?;
    let mut issues = compute_integrity_issues(&stories);
    let derived_relationships = if include_derived {
        derive_family_relationships(&stories)
    } else {
        BTreeMap::new()
    };

    let progress_map: BTreeMap<String, _> = stories
        .values()
        .filter_map(|story| compute_progress(story, &stories).map(|p| (story.id.clone(), p)))
        .collect();

    let mut views = Vec::with_capacity(stories.len());
    for story in stories.into_values() {
        let story_id = story.id.clone();
        let mut flagged_reasons = issues.remove(&story_id).unwrap_or_default();
        if story
            .relationships
            .iter()
            .any(|relation| relation.relation == "obviated-by")
        {
            flagged_reasons.push("story is obviated by another story".to_string());
        }
        flagged_reasons.sort();
        flagged_reasons.dedup();

        views.push(StoryView {
            story,
            derived_relationships: derived_relationships
                .get(&story_id)
                .cloned()
                .unwrap_or_default(),
            warnings: Vec::new(),
            flagged_reasons,
            stale_info: None,
            progress: progress_map.get(&story_id).cloned(),
        });
    }

    Ok(views)
}

/// The response for one story, with its derived family relationships included.
pub fn story_view(tx: &impl ReadOps, project: ProjectId, id: &str) -> Result<Response, AppError> {
    let view = story_views(tx, project, true)?
        .into_iter()
        .find(|candidate| candidate.story.id == id)
        .ok_or_else(|| AppError::NotFound(format!("story `{id}` not found")))?;
    Ok(Response::Story(Box::new(view)))
}
