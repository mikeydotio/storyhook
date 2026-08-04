//! Phases and epics — the two ways storyhook groups stories.
//!
//! Neither is a first-class thing in the data model, and deliberately so. A
//! phase is a `phase:<n>` label; an epic is a story whose type is `epic` and
//! whose members are its `parent-of` children. Both are therefore built out of
//! the primitives the lifecycle and relation services already own, and this
//! module's job is to be the one place that knows the conventions: that a
//! story belongs to at most one phase, that a phase's title comes from a story
//! called `Phase 2: …`, that adding a story to an epic is a `parent-of` edge
//! and not something new.
//!
//! # Hooks
//!
//! `story phase create`, `story epic create` and `story epic add` fire no
//! event hooks, where the commands that do the same underlying thing —
//! `story new`, `story relate` — do. That is inconsistent, and it is
//! replicated exactly: these commands run against a hook-suppressed context so
//! the services they delegate to stay the only implementation of the write
//! without also changing when a user's hook runs. `story phase add` *does*
//! fire the label-change hook, and does so with the story re-read after the
//! write, which is the legacy payload.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::{StoryEvent, StorySnapshot, SuperState, is_ready, normalize_labels};
use crate::error::AppError;
use crate::event_hooks::HookEventType;
use crate::output::{PhaseView, StoryView};
use crate::store::{ExpectedSeq, ReadOps, Store};

use super::query::{sort_story_views, story_views};
use super::{
    Clock, Ctx, NewStoryInput, RelationService, StoryService, append_and_fold, project_prefix,
    resolve_open_story,
};

/// The label prefix that marks a story's phase.
const PHASE_PREFIX: &str = "phase:";

/// What clearing a story's phase did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PhaseCleared {
    /// The assignment was removed; carries the story as it now reads.
    Removed(Box<StorySnapshot>),
    /// The story had no phase, so nothing was written.
    NoAssignment,
}

/// Phases and epics for one project.
pub struct GroupingService<'ctx, S: Store> {
    ctx: &'ctx Ctx<'ctx, S>,
}

impl<'ctx, S: Store> GroupingService<'ctx, S> {
    /// A grouping service bound to `ctx`.
    pub fn new(ctx: &'ctx Ctx<'ctx, S>) -> Self {
        Self { ctx }
    }

    // --- phases ------------------------------------------------------------

    /// Every phase the project's labels describe, with its progress rollup.
    ///
    /// Phases are ordered by their label text, which is what a `BTreeMap` over
    /// `phase:<n>` has always given: `10` before `2`. Left as it is because it
    /// is the ordering the dashboard and the CLI already render.
    pub fn phases(&self) -> Result<Vec<PhaseView>, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| {
            let views = story_views(tx, project, false)?;
            let stories: BTreeMap<String, StorySnapshot> = views
                .iter()
                .map(|view| (view.story.id.clone(), view.story.clone()))
                .collect();

            let mut grouped: BTreeMap<String, Vec<&StoryView>> = BTreeMap::new();
            for view in &views {
                for label in &view.story.labels {
                    if let Some(number) = label.strip_prefix(PHASE_PREFIX) {
                        grouped.entry(number.to_string()).or_default().push(view);
                    }
                }
            }
            if grouped.is_empty() {
                return Ok(Vec::new());
            }

            let default_open = tx
                .states(project)?
                .into_iter()
                .find(|state| state.super_state == SuperState::Open)
                .map_or_else(|| "todo".to_string(), |state| state.slug);

            Ok(grouped
                .iter()
                .map(|(number, members)| rollup(number, members, &stories, &default_open))
                .collect())
        })?)
    }

    /// Every story in one phase, in story-number order.
    pub fn phase_stories(&self, phase: &str) -> Result<Vec<StoryView>, AppError> {
        let project = self.ctx.project();
        let label = format!("{PHASE_PREFIX}{phase}");
        Ok(self.ctx.store().read(|tx| {
            let mut views = story_views(tx, project, false)?;
            views.retain(|view| view.story.labels.contains(&label));
            sort_story_views(&mut views);
            Ok(views)
        })?)
    }

    /// Assigns an open story to a phase, replacing any phase it already had.
    ///
    /// A story belongs to at most one phase, so the existing `phase:*` labels
    /// are removed in the same write rather than accumulated.
    pub fn assign_phase(&self, id: &str, phase: &str) -> Result<StorySnapshot, AppError> {
        let now = self.ctx.now();
        let snapshot = self.set_labels(id, |labels| {
            labels.retain(|label| !label.starts_with(PHASE_PREFIX));
            labels.insert(format!("{PHASE_PREFIX}{phase}"));
            true
        })?;
        let snapshot = snapshot.expect("the label set always changes");

        self.ctx.fire_hook(
            HookEventType::LabelChange,
            &serde_json::json!({
                "event_type": "label_change",
                "story_id": id,
                "timestamp": now,
                "story_title": &snapshot.title,
                "labels": &snapshot.labels,
            }),
        );
        Ok(snapshot)
    }

    /// Removes an open story's phase assignment, writing nothing when it had
    /// none.
    pub fn clear_phase(&self, id: &str) -> Result<PhaseCleared, AppError> {
        let cleared = self.set_labels(id, |labels| {
            let had = labels.iter().any(|label| label.starts_with(PHASE_PREFIX));
            labels.retain(|label| !label.starts_with(PHASE_PREFIX));
            had
        })?;
        Ok(match cleared {
            Some(snapshot) => PhaseCleared::Removed(Box::new(snapshot)),
            None => PhaseCleared::NoAssignment,
        })
    }

    /// Creates the story that names a phase, already carrying its label.
    pub fn create_phase(
        &self,
        phase: &str,
        title: Option<&str>,
    ) -> Result<StorySnapshot, AppError> {
        let title = match title {
            Some(title) => format!("Phase {phase}: {title}"),
            None => format!("Phase {phase}"),
        };
        let quiet = self.quiet();
        let story = StoryService::new(&quiet).create(&NewStoryInput {
            title,
            ..NewStoryInput::default()
        })?;
        self.set_labels(&story.id, |labels| {
            labels.insert(format!("{PHASE_PREFIX}{phase}"));
            true
        })?
        .ok_or_else(|| AppError::Storage(format!("story `{}` vanished mid-write", story.id)))
    }

    // --- epics -------------------------------------------------------------

    /// Every story typed `epic`, in story-number order.
    pub fn epics(&self) -> Result<Vec<StoryView>, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| {
            let mut views = story_views(tx, project, false)?;
            views.retain(|view| view.story.story_type.as_deref() == Some("epic"));
            sort_story_views(&mut views);
            Ok(views)
        })?)
    }

    /// Creates a story typed `epic`.
    ///
    /// The type is checked first, with its own message: `story epic create` in
    /// a project that has removed the `epic` type is a configuration problem,
    /// and telling the user which command adds it back is more use than
    /// listing every type that does exist.
    pub fn create_epic(&self, title: &str) -> Result<StorySnapshot, AppError> {
        let project = self.ctx.project();
        self.ctx.store().read(|tx| {
            if !tx.types(project)?.iter().any(|t| t.slug == "epic") {
                return Err(AppError::Validation(
                    "type `epic` is not defined. Add it with: story type add epic".to_string(),
                )
                .into());
            }
            Ok(())
        })?;

        let quiet = self.quiet();
        StoryService::new(&quiet).create(&NewStoryInput {
            title: title.to_string(),
            story_type: Some("epic".to_string()),
            ..NewStoryInput::default()
        })
    }

    /// Makes `story_id` a child of `epic_id`.
    ///
    /// A `parent-of` edge and nothing else, so the single-parent rule and the
    /// cycle check are the relation service's, written once.
    pub fn add_to_epic(&self, epic_id: &str, story_id: &str) -> Result<(), AppError> {
        let quiet = self.quiet();
        RelationService::new(&quiet).relate(epic_id, "parent-of", story_id, false)?;
        Ok(())
    }

    // --- shared ------------------------------------------------------------

    /// A hook-suppressed context over the same store, project and directory.
    ///
    /// The grouping commands that create a story or add an edge fire no hooks
    /// in the legacy path. Suppressing them here is what lets this module
    /// delegate to the services that own those writes without also changing
    /// when a user's hook runs.
    fn quiet(&self) -> Ctx<'_, S> {
        Ctx::new(
            self.ctx.store(),
            self.ctx.project(),
            self.ctx.cwd(),
            self.ctx.env().clone(),
        )
        .no_hooks(true)
        .clock(Clock::Fixed(self.ctx.now()))
    }

    /// Rewrites one open story's label set, writing nothing when `edit`
    /// reports there was nothing to do.
    ///
    /// `edit` returns whether the write is worth making — not whether it
    /// changed the set. `story phase remove` reports "no phase assignment" for
    /// a story that had none, which is a different answer from "removed",
    /// and only the closure can tell them apart.
    fn set_labels(
        &self,
        id: &str,
        edit: impl FnOnce(&mut BTreeSet<String>) -> bool,
    ) -> Result<Option<StorySnapshot>, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        Ok(self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            let (story_no, row) = resolve_open_story(&*tx, project, &prefix, id)?;

            let mut labels: BTreeSet<String> = row.snapshot.labels.iter().cloned().collect();
            if !edit(&mut labels) {
                return Ok(None);
            }
            // Normalizing the whole resulting set, not just the phase label
            // `edit` touched, is what lets this write succeed on a story
            // whose stored labels predate SH-164's guard — it self-heals a
            // comma-bearing label on its way back out rather than tripping
            // the write-path guard on an edit that never asked to change it.
            let events = [StoryEvent::StoryLabelsSet {
                at: now.clone(),
                labels: normalize_labels(labels),
            }];
            let snapshot = append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &events,
            )?;
            Ok(Some(snapshot))
        })?)
    }
}

/// One phase's progress rollup.
///
/// The four buckets are mutually exclusive and tested in this order: closed
/// stories are *done*; an open story nothing has unblocked is *blocked*; an
/// open story that has left the default state is *in progress*; everything
/// else is *todo*.
fn rollup(
    number: &str,
    members: &[&StoryView],
    stories: &BTreeMap<String, StorySnapshot>,
    default_open: &str,
) -> PhaseView {
    let mut done = 0;
    let mut in_progress = 0;
    let mut todo = 0;
    let mut blocked = 0;
    let mut title = None;
    let mut story_ids = Vec::with_capacity(members.len());

    for view in members {
        let story = &view.story;
        story_ids.push(story.id.clone());
        if story.superstate == SuperState::Closed {
            done += 1;
        } else if !is_ready(story, stories) {
            blocked += 1;
        } else if story.state != default_open {
            in_progress += 1;
        } else {
            todo += 1;
        }

        // A story called `Phase 2: the migration` names the phase; a story
        // called `Phase 2` belongs to it without naming it.
        let prefix = format!("Phase {number}:");
        if let Some(rest) = story.title.strip_prefix(&prefix) {
            let rest = rest.trim();
            if !rest.is_empty() {
                title = Some(rest.to_string());
            }
        }
    }

    PhaseView {
        phase: number.to_string(),
        title,
        total: members.len(),
        done,
        in_progress,
        todo,
        blocked,
        story_ids,
    }
}
