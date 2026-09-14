//! Story-relative evidence for an agent's obviation review.

use chrono::{DateTime, FixedOffset};
use serde::Serialize;

use super::{QueryService, project_prefix};
use crate::domain::StoryEvent;
use crate::error::AppError;
use crate::output::{Response, StoryView, render_response};
use crate::store::{ReadOps, StoryNo, partition_known};

#[derive(Serialize)]
struct ObviationReview {
    procedure: &'static str,
    target: StoryView,
    candidates: Vec<Candidate>,
}

#[derive(Serialize)]
struct Candidate {
    #[serde(flatten)]
    view: StoryView,
    reasons: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
}

impl<R: ReadOps> QueryService<'_, R> {
    /// Project context with all candidates for the named story's obviation review.
    ///
    /// Uses this query's existing read transaction for both snapshots and history.
    /// No similarity filter or preview limit may hide evidence from the agent.
    pub fn context_for_story(&self, id: &str, json: bool) -> Result<String, AppError> {
        let review = self.obviation_review(id)?;
        let mut context = self.context(json)?;
        let continuations: Vec<_> = self
            .tx
            .continuations(self.project)?
            .into_iter()
            .filter(|r| r.story_id == id)
            .collect();
        if json {
            let mut document: serde_json::Value = serde_json::from_str(&context)?;
            document["obviation_review"] = serde_json::to_value(review)?;
            document["continuations"] = serde_json::to_value(&continuations)?;
            return Ok(serde_json::to_string_pretty(&document)?);
        }
        if !continuations.is_empty() {
            context.push_str("\n## Continuation evidence\n\n");
            context.push_str(&serde_json::to_string_pretty(&continuations)?);
            context.push_str("\nRead current comments and relationships, inspect Git status/history/diff and tests, repeat obviation review, then acknowledge the exact current story sequence and HEAD. Existing approved scope and commits remain valid.\n");
        }
        context.push_str("\n## Obviation review\n\n");
        context.push_str(review.procedure);
        context.push_str("\n\n");
        context.push_str(
            "These are candidates to compare, not findings of obviation.\n\n### Target\n\n",
        );
        context.push_str(&render_view(review.target));
        if review.candidates.is_empty() {
            context.push_str("\nNo candidate stories in this review snapshot.\n");
        }
        for candidate in review.candidates {
            context.push_str(&format!(
                "\n### Candidate {}\n\nReasons: {}\n",
                candidate.view.story.id,
                candidate.reasons.join(", ")
            ));
            if let Some(at) = candidate.completed_at {
                context.push_str(&format!("Entered done: {at}\n"));
            }
            context.push('\n');
            context.push_str(&render_view(candidate.view));
        }
        Ok(context)
    }

    fn obviation_review(&self, id: &str) -> Result<ObviationReview, AppError> {
        let mut views = self.story_views(true)?;
        super::sort_story_views(&mut views);
        let target_index = views
            .iter()
            .position(|view| view.story.id == id)
            .ok_or_else(|| AppError::NotFound(format!("story `{id}` not found")))?;
        let target = views.remove(target_index);
        let created = instant(id, "created_at", &target.story.created_at)?;
        let prefix = project_prefix(self.tx, self.project)?;
        self.review_history(&prefix, id)?;
        let mut candidates = Vec::new();
        for view in views {
            let mut reasons = Vec::new();
            match view.story.state.as_str() {
                "in-progress" => reasons.push("in-progress"),
                "verifying" => reasons.push("verifying"),
                _ => {}
            }
            let events = self.review_history(&prefix, &view.story.id)?;
            let completed_at = completed_since(&view.story.id, &events, created)?;
            if completed_at.is_some() {
                reasons.push("completed-since-creation");
            }
            if !reasons.is_empty() {
                candidates.push(Candidate {
                    view,
                    reasons,
                    completed_at,
                });
            }
        }
        let procedure =
            crate::help_topics::get_help_topic("obviation-review").ok_or_else(|| {
                AppError::Storage("missing canonical obviation-review procedure".to_string())
            })?;
        Ok(ObviationReview {
            procedure,
            target,
            candidates,
        })
    }

    fn review_history(&self, prefix: &str, id: &str) -> Result<Vec<StoryEvent>, AppError> {
        let no = StoryNo::parse_id(prefix, id)
            .map_err(|error| AppError::Storage(format!("obviation review story {id}: {error}")))?;
        let stored = self
            .tx
            .events_for(self.project, no)
            .map_err(AppError::from)
            .map_err(|error| error.with_context(&format!("obviation review history for {id}")))?;
        let (events, _) = partition_known(no, &stored);
        // A surviving snapshot is not proof its completion history is available.
        if !events
            .iter()
            .any(|event| matches!(event, StoryEvent::StoryCreated { .. }))
        {
            return Err(AppError::Storage(format!(
                "obviation review: story `{id}` has no creation event in its history"
            )));
        }
        Ok(events)
    }
}

fn render_view(view: StoryView) -> String {
    render_response(&Response::Story(Box::new(view)), false, false)
}

fn instant(id: &str, field: &str, value: &str) -> Result<DateTime<FixedOffset>, AppError> {
    DateTime::parse_from_rfc3339(value).map_err(|error| {
        AppError::Storage(format!(
            "obviation review: story `{id}` has invalid {field} timestamp `{value}`: {error}"
        ))
    })
}

/// Track actual entries: a move followed by its archive event is one completion.
fn completed_since(
    id: &str,
    events: &[StoryEvent],
    created: DateTime<FixedOffset>,
) -> Result<Option<String>, AppError> {
    let mut state = None;
    let mut completed = None;
    for event in events {
        let (at, next) = match event {
            StoryEvent::StoryCreated { at, state, .. }
            | StoryEvent::StoryStateChanged { at, state }
            | StoryEvent::StoryClosedAndArchived { at, state } => (at, state.as_str()),
            // Legacy deletion abandons a story, even if it was done beforehand.
            StoryEvent::StoryDeleted { at, .. } => (at, crate::domain::DROPPED_STATE_SLUG),
            _ => continue,
        };
        if next == "done" && state != Some("done") && instant(id, "done-entry", at)? > created {
            completed = Some(at.clone());
        }
        state = Some(next);
    }
    Ok(completed)
}
