//! `story doctor`: what is wrong with this project, and what can be fixed.
//!
//! # Doctor now asks two questions, not one
//!
//! The legacy doctor asked only about *stories*: dangling relations, missing
//! inverses, multiple parents, parent/child cycles, unknown types. Those are
//! all properties of the snapshots — and a snapshot is a **cache** of a fold of
//! a story's events. If the cache and the events disagree, every one of those
//! answers is computed from the wrong input, and the legacy doctor could not
//! tell: it had no independent copy of the truth to compare against.
//!
//! The store does. [`crate::store::diff_read_model`] re-folds every story from
//! its own history and compares the result with the persisted row, which is
//! exactly the oracle the read model lacked. So this service reports both
//! dimensions:
//!
//! * **story-level integrity** — the legacy checks, reproduced verbatim,
//!   including the two flag kinds `doctor` deliberately suppresses; and
//! * **read-model drift** — rows that disagree with their events, stories with
//!   events and no row (or a row and no events), histories that will not fold,
//!   and edges only one end's history claims.
//!
//! Only the first kind of drift is repairable. Re-folding fixes a stale row;
//! it cannot fix a missing *event*, which is what an asymmetric relation is.
//! `--fix` says so rather than claiming a repair it did not make.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::{StoryEvent, StorySnapshot, inverse_relation};
use crate::error::AppError;
use crate::store::{
    ExpectedSeq, ProjectId, ReadOps, Store, StoryNo, StoryQuery, diff_read_model, repair_read_model,
};

use super::{Ctx, append_and_fold, project_prefix, query::story_views};

/// Reports and repairs a project's integrity.
pub struct IntegrityService<'a, S: Store> {
    ctx: &'a Ctx<'a, S>,
}

impl<'a, S: Store> IntegrityService<'a, S> {
    /// An integrity service for `ctx`'s project.
    pub fn new(ctx: &'a Ctx<'a, S>) -> Self {
        Self { ctx }
    }

    /// Every problem `story doctor` reports, in the order it reports them:
    /// story-level findings first, then read-model drift.
    ///
    /// An empty vector means the project is healthy; the caller turns a
    /// non-empty one into [`AppError::Integrity`].
    pub fn report(&self) -> Result<Vec<String>, AppError> {
        let mut issues = self
            .ctx
            .store()
            .read(|tx| Ok(story_issues(tx, self.project())))??;

        // The oracle the legacy read model never had. On a healthy project it
        // contributes nothing, which is why adding it does not move a single
        // byte of `doctor`'s existing output.
        issues.extend(drift_issues(&diff_read_model(
            self.ctx.store(),
            self.project(),
        )?));
        Ok(issues)
    }

    /// Repairs what can be repaired, then reports what is left.
    ///
    /// Three things happen, in this order and for this reason:
    ///
    /// 1. **Missing inverse edges are written**, as events, on the story that
    ///    is missing them — the only layer that can fix an asymmetric history.
    /// 2. **Relations pointing at stories that do not exist are retracted**,
    ///    also as events.
    /// 3. **The read model is re-folded** from the resulting events, which
    ///    subsumes the legacy path's archived-snapshot repair and covers every
    ///    story rather than only the archived ones.
    ///
    /// Then [`report`](Self::report) runs again and its verdict is the
    /// command's: a repair that did not actually fix the project must not exit
    /// zero.
    ///
    /// # One deliberate divergence from the legacy repair
    ///
    /// "Does the other end exist?" is asked of **every** story, where the
    /// legacy path asked it of the open ones only. That difference is data
    /// loss: relate two stories, close or delete one of them, and legacy
    /// `doctor --fix` retracts the survivor's edges — because the archived
    /// counterpart is not in the open set — and then reports the asymmetry it
    /// just created as an integrity failure the command can never clear.
    /// Appends still go to open stories only, which is the part of that rule
    /// that was right.
    pub fn fix(&self) -> Result<String, AppError> {
        let now = self.ctx.now();
        let project = self.project();
        let touched = self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            let all = all_stories(&*tx, project)?;
            let open = open_stories(&*tx, project)?;
            let mut touched: BTreeSet<String> = BTreeSet::new();

            for (id, story) in &open {
                let mut own_events: Vec<StoryEvent> = Vec::new();
                for relation in &story.relationships {
                    let Some(other) = all.get(&relation.other_id) else {
                        own_events.push(StoryEvent::StoryRelationshipRemoved {
                            at: now.clone(),
                            other_id: relation.other_id.clone(),
                            relation: relation.relation.clone(),
                        });
                        continue;
                    };
                    let Some(expected) = inverse_relation(&relation.relation) else {
                        continue;
                    };
                    if has_relation(other, expected, id) {
                        continue;
                    }
                    // An archived story's history is closed; a missing inverse
                    // on one is a finding, not something to append to.
                    if !open.contains_key(&relation.other_id) {
                        continue;
                    }
                    let other_no = StoryNo::parse_id(&prefix, &relation.other_id)
                        .map_err(|error| AppError::Storage(format!("unparseable id: {error}")))?;
                    append_and_fold(
                        tx,
                        project,
                        other_no,
                        &prefix,
                        &states,
                        ExpectedSeq::Any,
                        &[StoryEvent::StoryRelationshipAdded {
                            at: now.clone(),
                            other_id: id.clone(),
                            relation: expected.to_string(),
                        }],
                    )?;
                    touched.insert(relation.other_id.clone());
                }

                if !own_events.is_empty() {
                    let story_no = StoryNo::parse_id(&prefix, id)
                        .map_err(|error| AppError::Storage(format!("unparseable id: {error}")))?;
                    append_and_fold(
                        tx,
                        project,
                        story_no,
                        &prefix,
                        &states,
                        ExpectedSeq::Any,
                        &own_events,
                    )?;
                    touched.insert(id.clone());
                }
            }
            Ok(touched)
        })?;

        let repair = repair_read_model(self.ctx.store(), project)?;
        let mut touched: BTreeSet<String> = touched;
        touched.extend(
            repair
                .repaired
                .divergences
                .iter()
                .map(|divergence| divergence.story_no.to_string()),
        );

        let remaining = self.report()?;
        if !remaining.is_empty() {
            return Err(AppError::Integrity(remaining.join("\n")));
        }

        let mut message = if touched.is_empty() {
            "doctor found nothing to fix".to_string()
        } else {
            "doctor repaired supported integrity issues".to_string()
        };
        if !repair.unrepairable.is_empty() {
            message.push_str(&format!(
                "\n{} stor{} could not be repaired:\n{}",
                repair.unrepairable.len(),
                if repair.unrepairable.len() == 1 {
                    "y"
                } else {
                    "ies"
                },
                repair
                    .unrepairable
                    .iter()
                    .map(|(story, reason)| format!("{story}: {reason}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        Ok(message)
    }

    fn project(&self) -> ProjectId {
        self.ctx.project()
    }
}

/// The legacy story-level checks, reproduced exactly.
///
/// Two flag kinds are deliberately suppressed, as they always have been:
/// `obviated` (a deliberate authoring decision, not damage) and `conflicts`
/// (advisory). The unknown-type check is separate because it is a property of
/// the story *and the catalog*, which the cross-story integrity pass does not
/// see.
fn story_issues(tx: &impl ReadOps, project: ProjectId) -> Result<Vec<String>, AppError> {
    let types: BTreeSet<String> = tx
        .types(project)?
        .into_iter()
        .map(|story_type| story_type.slug)
        .collect();

    let mut issues = Vec::new();
    for view in story_views(tx, project, false)? {
        for issue in view.flagged_reasons {
            if issue.contains("obviated") || issue.contains("conflicts") {
                continue;
            }
            issues.push(format!("{}: {}", view.story.id, issue));
        }
        if let Some(slug) = &view.story.story_type
            && !types.contains(slug)
        {
            issues.push(format!("{}: unknown type `{slug}`", view.story.id));
        }
    }
    Ok(issues)
}

/// The read-model drift a doctor report adds to the story-level findings.
///
/// Deliberately **not** [`ReadModelDiff::describe`]: that also lists
/// asymmetric relations, which [`story_issues`] already reports from the
/// snapshots in the legacy wording (`missing inverse relation ...`). Printing
/// both would say the same thing twice, in two vocabularies, and would move
/// `doctor`'s bytes for a project the legacy path already diagnosed. What is
/// left is exactly the question the legacy doctor could not ask: does the read
/// model still equal a fold of its own events?
fn drift_issues(drift: &crate::store::ReadModelDiff) -> Vec<String> {
    let mut lines = Vec::new();
    for story in &drift.missing_rows {
        lines.push(format!("story {story}: has events but no read-model row"));
    }
    for story in &drift.extra_rows {
        lines.push(format!("story {story}: read-model row with no events"));
    }
    for (story, reason) in &drift.fold_failures {
        lines.push(format!("story {story}: cannot be folded: {reason}"));
    }
    for divergence in &drift.divergences {
        lines.push(format!(
            "story {}: {} is `{}` but the events say `{}`",
            divergence.story_no, divergence.field, divergence.persisted, divergence.rebuilt
        ));
    }
    lines
}

/// Every story in the project, keyed by id — archived and deleted included.
fn all_stories(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<BTreeMap<String, StorySnapshot>, AppError> {
    Ok(tx
        .stories(project, &StoryQuery::all())?
        .into_iter()
        .map(|row| (row.snapshot.id.clone(), row.snapshot))
        .collect())
}

/// The project's unarchived stories, keyed by id.
///
/// `--fix` has always worked on open stories only: an archived story's history
/// is closed, and appending a repair event to it would reopen a question the
/// project already settled.
fn open_stories(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<BTreeMap<String, StorySnapshot>, AppError> {
    Ok(tx
        .stories(project, &StoryQuery::all().archived(false))?
        .into_iter()
        .map(|row| (row.snapshot.id.clone(), row.snapshot))
        .collect())
}

/// Whether `story` already asserts `relation` to `other_id`.
fn has_relation(story: &StorySnapshot, relation: &str, other_id: &str) -> bool {
    story
        .relationships
        .iter()
        .any(|candidate| candidate.relation == relation && candidate.other_id == other_id)
}
