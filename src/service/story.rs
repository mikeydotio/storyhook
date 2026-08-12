//! The story lifecycle: creating a story, changing it, closing it, reopening
//! it.
//!
//! # The one state-transition batch
//!
//! Every path that moves a story between states goes through
//! [`state_transition_events`], and it is private to this module. That is the
//! most load-bearing fact here. The legacy code had four callers assembling the
//! batch by hand and a comment warning that they must agree — "forgetting the
//! awaiting clear archives a story that still reads as blocked, and forgetting
//! the close marker leaves a CLOSED story that `archive_story` will happily
//! archive but that reopens inconsistently". A comment cannot enforce that. One
//! private function that is the only way to build the batch can.

use std::collections::{BTreeMap, BTreeSet};

use crate::domain::{
    Member, Priority, StateDef, StoryEvent, StorySnapshot, SuperState, normalize_labels,
    undefined_state_error,
};
use crate::error::AppError;
use crate::event_hooks::HookEventType;
use crate::output::{HideStatePlan, PurgePlan, UndeletePlan};
use crate::store::{
    EventSeq, ExpectedSeq, ProjectId, ReadOps, Store, StoryNo, StoryQuery, StoryRow, WriteOps,
};

use super::{Ctx, append_and_fold, project_prefix, resolve_open_story, resolve_story};

/// Everything `story new` can set at creation time.
///
/// A struct rather than seven positional arguments because almost every field
/// is an `Option<String>`, which is exactly the shape an argument list gets
/// wrong silently.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewStoryInput {
    /// The story's title.
    pub title: String,
    /// The state to open in; the project's default open state when absent.
    pub state: Option<String>,
    /// The story type slug. Must be one the project defines.
    pub story_type: Option<String>,
    /// A long-form description. Blank or whitespace-only is treated as absent.
    pub description: Option<String>,
    /// A priority slug.
    pub priority: Option<String>,
    /// Labels, deduplicated and sorted before they are written.
    pub labels: Option<Vec<String>>,
    /// A member id or GitHub handle to assign to.
    pub assignee: Option<String>,
    /// Creates the story as a draft (SH-175) — `story new --draft`. Claims a
    /// story id like any other creation; `StoryService::publish` is the only
    /// way out, and it is one-way.
    pub draft: bool,
}

/// The edits `story set` can apply in one call.
///
/// Mirrors `Invocation::SetFields` field for field, so the dispatch arm is a
/// move rather than a translation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldEdits {
    /// A new title.
    pub title: Option<String>,
    /// A new state, which may close the story.
    pub state: Option<String>,
    /// A new priority slug.
    pub priority: Option<String>,
    /// A member id or GitHub handle.
    pub assignee: Option<String>,
    /// Comma-separated labels to *add* to the story's current set.
    pub labels: Option<String>,
    /// What the story is now awaiting.
    pub blocked: Option<String>,
    /// Clears whatever the story was awaiting.
    pub unblocked: bool,
    /// A JSON object of field edits, applied after the flags.
    pub json: Option<String>,
    /// A new story type slug.
    pub story_type: Option<String>,
    /// A new description.
    pub description: Option<String>,
}

/// The story lifecycle, over one project in one store.
pub struct StoryService<'ctx, S: Store> {
    ctx: &'ctx Ctx<'ctx, S>,
}

impl<'ctx, S: Store> StoryService<'ctx, S> {
    /// A lifecycle service bound to `ctx`.
    pub fn new(ctx: &'ctx Ctx<'ctx, S>) -> Self {
        Self { ctx }
    }

    /// Creates a story, writing its creation event and every enrichment event
    /// as one batch.
    ///
    /// The story number is allocated inside the transaction that uses it, so
    /// two writers cannot receive the same one — the collision that twice
    /// corrupted this repository's own tracker. Every enrichment field is
    /// validated before anything is written, so invalid input leaves neither a
    /// half-created story nor a burnt story number.
    pub fn create(&self, input: &NewStoryInput) -> Result<StorySnapshot, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        let snapshot = self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let ordered = tx.states(project)?;
            let states = state_map(&ordered);
            let events = creation_events(&*tx, project, &ordered, input, &now)?;
            let story = tx.allocate_story_no(project)?;
            Ok(append_and_fold(
                tx,
                project,
                story,
                &prefix,
                &states,
                ExpectedSeq::Exact(EventSeq::ZERO),
                &events,
            )?)
        })?;

        self.ctx.fire_hook(
            HookEventType::Create,
            &serde_json::json!({
                "event_type": "create",
                "story_id": &snapshot.id,
                "timestamp": &snapshot.created_at,
                "story_title": &snapshot.title,
                "initial_state": &snapshot.state,
            }),
        );
        Ok(snapshot)
    }

    /// Adds a comment to an open story.
    pub fn comment(&self, id: &str, text: &str) -> Result<StorySnapshot, AppError> {
        let now = self.ctx.now();
        let snapshot = self.edit_open(id, |_row, _states| {
            Ok(vec![StoryEvent::StoryCommentAdded {
                at: now.clone(),
                text: text.to_string(),
            }])
        })?;

        self.ctx.fire_hook(
            HookEventType::Comment,
            &serde_json::json!({
                "event_type": "comment",
                "story_id": id,
                "timestamp": self.ctx.now(),
                "story_title": &snapshot.title,
                "comment_text": text,
            }),
        );
        Ok(snapshot)
    }

    /// Assigns an open story to a project member, looked up by member id or by
    /// GitHub handle.
    ///
    /// The lookup happens inside the write transaction, so a member cannot be
    /// removed between being found and being recorded.
    pub fn assign(&self, id: &str, member: &str) -> Result<StorySnapshot, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        Ok(self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            let (story_no, row) = resolve_open_story(&*tx, project, &prefix, id)?;
            let member = find_member(&*tx, project, member)?;
            Ok(append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &[StoryEvent::StoryAssigned {
                    at: now.clone(),
                    member_id: member.id,
                }],
            )?)
        })?)
    }

    /// Sets an open story's priority.
    pub fn set_priority(&self, id: &str, priority: &str) -> Result<StorySnapshot, AppError> {
        let level = Priority::parse(priority).ok_or_else(|| {
            AppError::Validation(
                "priority must be one of: critical, high, medium, low, none".to_string(),
            )
        })?;
        let now = self.ctx.now();
        let snapshot = self.edit_open(id, |_row, _states| {
            Ok(vec![StoryEvent::StoryPrioritySet {
                at: now.clone(),
                priority: level.clone(),
            }])
        })?;

        self.ctx.fire_hook(
            HookEventType::PriorityChange,
            &serde_json::json!({
                "event_type": "priority_change",
                "story_id": id,
                "timestamp": self.ctx.now(),
                "story_title": &snapshot.title,
                "priority": level.as_str(),
            }),
        );
        Ok(snapshot)
    }

    /// Adds and removes labels on an open story.
    ///
    /// `StoryLabelsSet` assigns the whole set, so the new set is computed from
    /// the story's labels *as read inside the transaction*. That is what stops
    /// two concurrent label edits from each silently discarding the other's.
    pub fn set_labels(
        &self,
        id: &str,
        add: &[String],
        remove: &[String],
    ) -> Result<StorySnapshot, AppError> {
        let now = self.ctx.now();
        // `add`/`remove` are normalized here rather than trusted from the
        // caller: the CLI's `story label`/`unlabel` already split on comma,
        // but the REST `/labels` route (SH-164) hands this a raw JSON array
        // that may not have. Removing a normalized `remove` value against the
        // story's already-normalized stored labels is also what makes
        // `story unlabel <id> "web,sse"` finally able to name the label
        // SH-145 could never be unlabeled with.
        let add = normalize_labels(add);
        let remove = normalize_labels(remove);
        let snapshot = self.edit_open(id, |row, _states| {
            let mut labels: BTreeSet<String> = row.snapshot.labels.iter().cloned().collect();
            for label in &add {
                labels.insert(label.clone());
            }
            for label in &remove {
                labels.remove(label);
            }
            Ok(vec![StoryEvent::StoryLabelsSet {
                at: now.clone(),
                labels: normalize_labels(labels),
            }])
        })?;

        self.ctx.fire_hook(
            HookEventType::LabelChange,
            &serde_json::json!({
                "event_type": "label_change",
                "story_id": id,
                "timestamp": self.ctx.now(),
                "story_title": &snapshot.title,
                "labels": &snapshot.labels,
            }),
        );
        Ok(snapshot)
    }

    /// Records what an open story is waiting on.
    pub fn set_awaiting(&self, id: &str, awaiting: &str) -> Result<StorySnapshot, AppError> {
        let awaiting = awaiting.trim().to_string();
        if awaiting.is_empty() {
            return Err(AppError::Validation(
                "awaiting reason must not be empty".to_string(),
            ));
        }
        let now = self.ctx.now();
        self.edit_open(id, |_row, _states| {
            Ok(vec![StoryEvent::StoryAwaitingSet {
                at: now.clone(),
                awaiting: awaiting.clone(),
            }])
        })
    }

    /// Clears what an open story was waiting on, writing nothing when it was
    /// waiting on nothing.
    pub fn clear_awaiting(&self, id: &str) -> Result<StorySnapshot, AppError> {
        let now = self.ctx.now();
        self.edit_open(id, |row, _states| {
            if row.awaiting.is_none() {
                return Ok(Vec::new());
            }
            Ok(vec![StoryEvent::StoryAwaitingCleared { at: now.clone() }])
        })
    }

    /// Moves a story into `state`, optionally guarded by a compare-and-swap on
    /// the state it is in now.
    ///
    /// `if_state` is the claim `story move --if-state` makes and the plugin's
    /// work-claiming protocol relies on. It is checked against ground truth
    /// *inside the write transaction*: under the old design the check read one
    /// file and the write touched another, with a per-directory lock in between
    /// that a second checkout of the same repository did not even share.
    ///
    /// Deletion is part of that ground truth rather than something inferred
    /// from the slug. `story delete` leaves the state slug alone and only
    /// forces the story closed, so a stale `--if-state` naming the
    /// pre-deletion slug would otherwise sail through.
    /// `awaiting`, when given, is set atomically with the state change
    /// (SH-205) — the CLI's `story move <id> blocked --reason "<text>"` and
    /// the dashboard's Blocked-column drop prompt both thread a reason
    /// through here rather than issuing a second, non-atomic `set_awaiting`
    /// call. Refused for a move into a CLOSED state, which already clears
    /// `awaiting` unconditionally (`state_transition_events`) — setting one
    /// in the same breath it gets cleared has no coherent meaning. To set a
    /// reason without moving state, use [`Self::set_awaiting`] (`story
    /// block`) instead.
    pub fn set_state(
        &self,
        id: &str,
        state: &str,
        comment: Option<&str>,
        if_state: Option<&str>,
        awaiting: Option<&str>,
    ) -> Result<StorySnapshot, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        let awaiting = awaiting
            .map(str::trim)
            .map(|reason| {
                if reason.is_empty() {
                    Err(AppError::Validation(
                        "awaiting reason must not be empty".to_string(),
                    ))
                } else {
                    Ok(reason.to_string())
                }
            })
            .transpose()?;
        let (before, snapshot) = self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;

            if let Some(expected) = if_state {
                let (_, current) = resolve_story(&*tx, project, &prefix, id)?;
                if current.deleted || current.state != expected {
                    let actual = if current.deleted {
                        "deleted".to_string()
                    } else {
                        current.state.clone()
                    };
                    return Err(AppError::StateConflict(expected.to_string(), actual).into());
                }
            }

            let (story_no, row) = resolve_open_story(&*tx, project, &prefix, id)?;
            let target = states
                .get(state)
                .cloned()
                .ok_or_else(|| undefined_state_error(state, &states))?;
            if awaiting.is_some() && target.super_state == SuperState::Closed {
                return Err(AppError::Validation(
                    "--reason cannot be combined with a move to a closed state; \
                     awaiting is cleared on close"
                        .to_string(),
                )
                .into());
            }
            let extra = comment
                .map(|text| StoryEvent::StoryCommentAdded {
                    at: now.clone(),
                    text: text.to_string(),
                })
                .into_iter()
                .chain(awaiting.clone().map(|reason| StoryEvent::StoryAwaitingSet {
                    at: now.clone(),
                    awaiting: reason,
                }))
                .collect();
            let events = state_transition_events(&target, row.awaiting.is_some(), &now, extra);
            let snapshot = append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &events,
            )?;
            Ok((row.snapshot, snapshot))
        })?;

        self.fire_transition_hooks(id, &before.title, &before.state, state, &snapshot, &now);
        Ok(snapshot)
    }

    /// Applies a batch of field edits to an open story, returning the
    /// human-readable summary of what changed.
    ///
    /// Edits apply in a fixed order — the flags in declaration order, then the
    /// JSON patch's keys — and a call that produces no events at all is a usage
    /// error rather than a silent no-op.
    pub fn set_fields(&self, id: &str, edits: &FieldEdits) -> Result<String, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        let (plan, before, snapshot) = self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            let (story_no, row) = resolve_open_story(&*tx, project, &prefix, id)?;
            let plan = plan_field_edits(&*tx, project, &states, &row.snapshot, edits, &now)?;
            if plan.events.is_empty() {
                return Err(AppError::Usage("no fields to update".to_string()).into());
            }
            let snapshot = append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &plan.events,
            )?;
            Ok((plan, row.snapshot, snapshot))
        })?;

        if let Some(to_state) = &plan.moved_to {
            self.fire_transition_hooks(id, &before.title, &before.state, to_state, &snapshot, &now);
        }
        Ok(format!("updated {id}: {}", plan.changes.join(", ")))
    }

    /// Moves a list of stories into named states, one independent transaction
    /// each, and reports the outcome of every one.
    ///
    /// Best-effort per item is the legacy contract and is preserved
    /// deliberately: the output format carries a per-item `error —` line, so a
    /// caller can already see which items failed, and making the batch atomic
    /// would silently discard work a script has been told landed. What *is* new
    /// is that each item is atomic on its own — under the old design an item's
    /// event append, archive insert, and open-file removal were three separate
    /// filesystem operations, and a failure between them left exactly the
    /// split-brain the store makes unrepresentable.
    pub fn bulk_update(&self, updates: &[(String, String)]) -> Result<String, AppError> {
        let states = self
            .ctx
            .store()
            .read(|tx| tx.state_map(self.ctx.project()))?;
        let mut results = Vec::with_capacity(updates.len());

        for (id, state_slug) in updates {
            if !states.contains_key(state_slug) {
                results.push(format!(
                    "{id}: error — {}",
                    undefined_state_error(state_slug, &states)
                ));
                continue;
            }
            match self.set_state(id, state_slug, None, None, None) {
                Ok(snapshot) if snapshot.superstate == SuperState::Closed => {
                    results.push(format!("{id}: {state_slug} (archived)"));
                }
                Ok(_) => results.push(format!("{id}: {state_slug}")),
                Err(AppError::NotFound(_) | AppError::Validation(_)) => {
                    results.push(format!("{id}: error — story not found or not open"));
                }
                Err(error) => results.push(format!("{id}: error — {error}")),
            }
        }

        Ok(results.join("\n"))
    }

    /// Soft-deletes a story: records the reason as a comment and as a
    /// `StoryDeleted` event, which closes and archives it.
    pub fn delete(&self, id: &str, reason: &str) -> Result<String, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            // An already-archived story is *not found* rather than *closed*:
            // the legacy path looked for its open event log and said so.
            let (story_no, row) = resolve_story(&*tx, project, &prefix, id)?;
            if row.archived {
                return Err(AppError::NotFound(format!("story `{id}` not found")).into());
            }
            append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &[
                    StoryEvent::StoryCommentAdded {
                        at: now.clone(),
                        text: format!("[deleted] {reason}"),
                    },
                    StoryEvent::StoryDeleted {
                        at: now.clone(),
                        reason: reason.to_string(),
                    },
                ],
            )?;
            Ok(())
        })?;
        Ok(format!("deleted {id}: {reason}"))
    }

    /// Everything a [`purge`](Self::purge) would destroy. Writes nothing.
    ///
    /// The first half of the two-step: this travels to whichever process has a
    /// terminal and becomes a prompt there, or — with `--json`, or no terminal
    /// — a refusal naming `--force`.
    pub fn purge_plan(&self, id: &str) -> Result<PurgePlan, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| {
            let prefix = project_prefix(tx, project)?;
            let (story_no, row) = resolve_purgeable_story(tx, project, &prefix, id)?;
            let canonical = story_no.to_id(&prefix);
            Ok(PurgePlan {
                title: row.snapshot.title.clone(),
                deleted_reason: row.snapshot.deleted_reason.clone(),
                events: tx.events_for(project, story_no)?.len(),
                retracted: surviving_claims(tx, project, &prefix, story_no, &canonical)?,
                id: canonical,
            })
        })?)
    }

    /// Removes a story permanently: its events, its row, and every trace of it.
    ///
    /// **The only irreversible thing that can be done to a single story**, and
    /// deliberately a verb of its own rather than a flag on `delete`. A flag
    /// that turns a reversible act into an irreversible one is the wrong shape
    /// — `story delete --hard` is one keystroke away from `story delete`, and
    /// the two do incomparable things. `story project delete` sets the
    /// precedent.
    ///
    /// It **refuses a story that has not been soft-deleted**, which is also the
    /// answer to what soft delete is for now: the reversible tombstone, and the
    /// required antechamber to the irreversible act. Everything a purge
    /// destroys was already marked as unwanted, by someone, with a reason on
    /// the record.
    ///
    /// Two steps, in this order, and the order is the whole of the correctness:
    ///
    /// 1. Every surviving story that still claims an edge into this one has
    ///    that claim retracted with a real `StoryRelationshipRemoved` event.
    ///    Without it the claimant's own history keeps asserting an edge whose
    ///    far end no longer exists, `rebuild.rs` reports a `relations`
    ///    divergence, and `doctor --fix` cannot repair it — re-folding the
    ///    claimant re-derives the same dead edge, and the foreign key refuses
    ///    to write it.
    /// 2. The story itself goes, through [`WriteOps::purge_story`].
    ///
    /// The retractions are real events on real stories rather than a silent
    /// table edit, because that is what makes the claimant's history true: the
    /// edge genuinely was removed, at this moment, by this act.
    pub fn purge(&self, id: &str) -> Result<String, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        let (canonical, title, retracted, removed) = self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let (story_no, row) = resolve_purgeable_story(&*tx, project, &prefix, id)?;
            let canonical = story_no.to_id(&prefix);
            let retracted = surviving_claims(&*tx, project, &prefix, story_no, &canonical)?;
            let states = tx.state_map(project)?;

            for (other_id, relation) in &retracted {
                let (other_no, other_row) = resolve_story(&*tx, project, &prefix, other_id)?;
                append_and_fold(
                    tx,
                    project,
                    other_no,
                    &prefix,
                    &states,
                    ExpectedSeq::Exact(other_row.head_seq),
                    &[StoryEvent::StoryRelationshipRemoved {
                        at: now.clone(),
                        other_id: canonical.clone(),
                        relation: relation.clone(),
                    }],
                )?;
            }

            let removed = tx.purge_story(project, story_no)?;
            Ok((canonical, row.snapshot.title.clone(), retracted, removed))
        })?;

        let mut message = format!(
            "purged {canonical} — {title}\n{} event{} permanently deleted",
            removed.events,
            if removed.events == 1 { "" } else { "s" },
        );
        for (other_id, relation) in &retracted {
            message.push_str(&format!("\nretracted {other_id} {relation} {canonical}"));
        }
        message.push_str(&format!("\n{canonical} will never be reused as a story id"));
        Ok(message)
    }

    /// Everything an undelete of `id` would need to confirm, or `None` if
    /// `id` is closed but was never soft-deleted — an ordinary reopen needs no
    /// confirmation at all. Writes nothing.
    ///
    /// The first half of the two-step [`purge_plan`](Self::purge_plan)
    /// already established: read before anything is asked, so the question
    /// can travel to whichever process has a terminal. A service running
    /// inside the daemon has none — [`reopen`](Self::reopen) prompting from
    /// here directly, as it once did, meant `story reopen` could never
    /// actually ask and always refused (SH-154).
    pub fn reopen_plan(&self, id: &str) -> Result<Option<UndeletePlan>, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| {
            let prefix = project_prefix(tx, project)?;
            let (story_no, row) = resolve_story(tx, project, &prefix, id)?;
            if !row.archived {
                return Err(AppError::Validation(format!("story `{id}` is already open")).into());
            }
            if !row.deleted {
                return Ok(None);
            }
            Ok(Some(UndeletePlan {
                id: story_no.to_id(&prefix),
                title: row.snapshot.title.clone(),
                deleted_reason: row.snapshot.deleted_reason.clone(),
            }))
        })?)
    }

    /// Reopens a closed story into the project's default open state.
    ///
    /// Unconditional — it does not ask whether a soft-deleted story should be
    /// restored. By the time this runs that question is already settled:
    /// either [`reopen_plan`](Self::reopen_plan) found nothing to ask, or the
    /// caller already holds a `Yes` to the plan it returned. Mirrors
    /// [`purge`](Self::purge), the same split for the same reason.
    ///
    /// This is a pure append. The legacy path rewrote the story's event log to
    /// strip its closure markers — history a store whose events are append-only
    /// cannot delete, and history that should not be deleted anyway.
    /// [`crate::domain::fold_story`] now retracts those markers when a story
    /// moves back into an open state, so the same observable result comes from
    /// adding one event instead of removing three.
    pub fn reopen(&self, id: &str) -> Result<StorySnapshot, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();

        let (before, snapshot) = self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let ordered = tx.states(project)?;
            let states = state_map(&ordered);
            let (story_no, row) = resolve_story(&*tx, project, &prefix, id)?;
            if !row.archived {
                return Err(AppError::Validation(format!("story `{id}` is already open")).into());
            }
            let target = default_open_state(&ordered)?;
            let snapshot = append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &[StoryEvent::StoryStateChanged {
                    at: now.clone(),
                    state: target.slug.clone(),
                }],
            )?;
            Ok((row.snapshot, snapshot))
        })?;

        self.ctx.fire_hook(
            HookEventType::StateChange,
            &serde_json::json!({
                "event_type": "state_change",
                "story_id": id,
                "timestamp": self.ctx.now(),
                "story_title": &before.title,
                "from_state": "closed",
                "to_state": &snapshot.state,
            }),
        );
        Ok(snapshot)
    }

    /// Hides a closed story from the primary UI — the "Archive" action
    /// (SH-43). Reversible: [`unhide`](Self::unhide) undoes it.
    ///
    /// Refuses an open story rather than silently no-op-ing: hiding is only
    /// ever a display fact layered on top of a story that is already closed,
    /// and a caller asking to archive an open one has almost certainly named
    /// the wrong story. Idempotent on an already-hidden one, matching
    /// [`reopen`](Self::reopen)'s "already open" shape in reverse — no event
    /// is appended for a fact already true.
    pub fn hide(&self, id: &str) -> Result<StorySnapshot, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        Ok(self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            let (story_no, row) = resolve_story(&*tx, project, &prefix, id)?;
            if row.superstate != SuperState::Closed {
                return Err(AppError::Validation(format!(
                    "story `{id}` is open; only a closed story can be archived"
                ))
                .into());
            }
            if row.snapshot.hidden_at.is_some() {
                return Ok(row.snapshot);
            }
            Ok(append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &[StoryEvent::StoryHidden { at: now.clone() }],
            )?)
        })?)
    }

    /// The inverse of [`hide`](Self::hide) — the "Unarchive" action.
    /// Idempotent on a story that is not hidden.
    pub fn unhide(&self, id: &str) -> Result<StorySnapshot, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        Ok(self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            let (story_no, row) = resolve_story(&*tx, project, &prefix, id)?;
            if row.snapshot.hidden_at.is_none() {
                return Ok(row.snapshot);
            }
            Ok(append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &[StoryEvent::StoryUnhidden { at: now.clone() }],
            )?)
        })?)
    }

    /// Makes a draft story live — `story publish <id>` (SH-175). One-way:
    /// there is no code path back to `draft: true` after this runs, which is
    /// what makes publishing irreversible rather than merely defaulted.
    /// Idempotent on a story that is already live — publishing again has
    /// nothing left to do, so it is not an error.
    pub fn publish(&self, id: &str) -> Result<StorySnapshot, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        Ok(self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            let (story_no, row) = resolve_story(&*tx, project, &prefix, id)?;
            if !row.snapshot.draft {
                return Ok(row.snapshot);
            }
            Ok(append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &[StoryEvent::StoryPublished { at: now.clone() }],
            )?)
        })?)
    }

    /// Everything a bulk "Archive" of `state_slug`'s column would hide, read
    /// before anything is (SH-43). Writes nothing.
    ///
    /// The dry-run half of the two-phase preview/commit contract the SH-43
    /// council mandated: [`hide_state`](Self::hide_state) must be called back
    /// with exactly the `ids` this returns, so every surface confirms off the
    /// same data rather than each recomputing "what's in this column"
    /// independently between the two calls.
    pub fn hide_state_plan(&self, state_slug: &str) -> Result<HideStatePlan, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| {
            let prefix = project_prefix(tx, project)?;
            let rows = archivable_occupants(tx, project, state_slug)?;
            let ids = rows.iter().map(|row| row.story_no.to_id(&prefix)).collect();
            Ok(HideStatePlan {
                state: state_slug.to_string(),
                ids,
            })
        })?)
    }

    /// Archives every story currently in `state_slug` — the CLOSED-superstate
    /// column's bulk "Archive" button, committing what
    /// [`hide_state_plan`](Self::hide_state_plan) previewed.
    ///
    /// Re-reads the column's current occupants inside the write transaction
    /// rather than being handed the previewed id list, mirroring
    /// [`purge`](Self::purge)/[`reopen`](Self::reopen)'s `force` half: a
    /// caller that skipped confirmation gets acted on against state as it is
    /// *now*, in one atomic transaction, rather than against a snapshot that
    /// could have gone stale between two calls.
    pub fn hide_state(&self, state_slug: &str) -> Result<String, AppError> {
        let project = self.ctx.project();
        let now = self.ctx.now();
        let archived = self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            let rows = archivable_occupants(&*tx, project, state_slug)?;
            let mut archived = Vec::with_capacity(rows.len());
            for row in rows {
                append_and_fold(
                    tx,
                    project,
                    row.story_no,
                    &prefix,
                    &states,
                    ExpectedSeq::Exact(row.head_seq),
                    &[StoryEvent::StoryHidden { at: now.clone() }],
                )?;
                archived.push(row.story_no);
            }
            Ok(archived)
        })?;
        Ok(format!(
            "archived {} stor{} from `{state_slug}`",
            archived.len(),
            if archived.len() == 1 { "y" } else { "ies" }
        ))
    }

    /// The read-modify-write every single-event edit of an open story shares:
    /// resolve, refuse if closed, build the batch from the story as it reads
    /// *inside* the transaction, append, fold, and write the snapshot back.
    fn edit_open<F>(&self, id: &str, build: F) -> Result<StorySnapshot, AppError>
    where
        F: FnOnce(&StoryRow, &BTreeMap<String, StateDef>) -> Result<Vec<StoryEvent>, AppError>,
    {
        let project = self.ctx.project();
        Ok(self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            let (story_no, row) = resolve_open_story(&*tx, project, &prefix, id)?;
            let events = build(&row, &states)?;
            if events.is_empty() {
                return Ok(row.snapshot);
            }
            Ok(append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &events,
            )?)
        })?)
    }

    /// Fires the hooks a state change owes: always `state_change`, plus
    /// `close` when the story ended up closed.
    fn fire_transition_hooks(
        &self,
        id: &str,
        title: &str,
        from_state: &str,
        to_state: &str,
        snapshot: &StorySnapshot,
        now: &str,
    ) {
        self.ctx.fire_hook(
            HookEventType::StateChange,
            &serde_json::json!({
                "event_type": "state_change",
                "story_id": id,
                "timestamp": now,
                "story_title": title,
                "from_state": from_state,
                "to_state": to_state,
            }),
        );
        if snapshot.superstate == SuperState::Closed {
            self.ctx.fire_hook(
                HookEventType::Close,
                &serde_json::json!({
                    "event_type": "close",
                    "story_id": id,
                    "timestamp": now,
                    "story_title": title,
                    "final_state": to_state,
                }),
            );
        }
    }
}

/// A slug-keyed view of an ordered state list.
///
/// [`crate::domain::fold_story`] wants the map; anything that has to answer
/// "which state comes first" wants the list. Deriving one from the other keeps
/// a caller from reading the catalog twice and getting two answers.
fn state_map(states: &[StateDef]) -> BTreeMap<String, StateDef> {
    states
        .iter()
        .map(|state| (state.slug.clone(), state.clone()))
        .collect()
}

/// Every not-yet-hidden story in `state_slug`, after confirming `state_slug`
/// is defined and its superstate is CLOSED.
///
/// Shared by [`StoryService::hide_state_plan`] and [`StoryService::hide_state`]
/// so the rule for which column can be archived and which of its occupants
/// qualify is stated once — the same reason [`state_transition_events`] is
/// the sole place a state-transition batch is built.
fn archivable_occupants(
    tx: &impl ReadOps,
    project: ProjectId,
    state_slug: &str,
) -> Result<Vec<StoryRow>, AppError> {
    let states = tx.state_map(project)?;
    let def = states
        .get(state_slug)
        .ok_or_else(|| undefined_state_error(state_slug, &states))?;
    if def.super_state != SuperState::Closed {
        return Err(AppError::Validation(format!(
            "state `{state_slug}` is open; only a closed-superstate column can be archived"
        )));
    }
    Ok(tx.stories(project, &StoryQuery::all().state(state_slug).hidden(false))?)
}

/// The event batch that moves a story into `target`.
///
/// The **only** place this batch is built, and crate-private so it stays that
/// way — the configuration service migrates occupants out of a state it is
/// removing and has to produce the identical batch. Order is
/// `StoryStateChanged`, then any caller-supplied extras, then — when the
/// target closes the story — the close markers: `StoryAwaitingCleared` if the
/// story was awaiting something, followed by `StoryClosedAndArchived`.
pub(crate) fn state_transition_events(
    target: &StateDef,
    awaiting: bool,
    at: &str,
    extra: Vec<StoryEvent>,
) -> Vec<StoryEvent> {
    let mut events = vec![StoryEvent::StoryStateChanged {
        at: at.to_string(),
        state: target.slug.clone(),
    }];
    events.extend(extra);
    if target.super_state == SuperState::Closed {
        if awaiting {
            events.push(StoryEvent::StoryAwaitingCleared { at: at.to_string() });
        }
        events.push(StoryEvent::StoryClosedAndArchived {
            at: at.to_string(),
            state: target.slug.clone(),
        });
    }
    events
}

/// The project's default open state: the first *configured* one that is OPEN.
///
/// Delegates to [`crate::domain::default_open_state`] — the same pure
/// selection the dashboard's `meta.defaults.state` (`src/api/rest.rs`) reads
/// off the identical ordered `Vec<StateDef>`, so a new story's actual default
/// and the web form's preselected one cannot drift apart (SH-44). This
/// wrapper only adds the validation error a creation path needs when a
/// project somehow has no OPEN state at all — the dashboard, which is
/// display-only, is content with `None`.
fn default_open_state(states: &[StateDef]) -> Result<StateDef, AppError> {
    crate::domain::default_open_state(states)
        .ok_or_else(|| AppError::Validation("project has no OPEN-mapped default state".to_string()))
}

/// The events `story new` writes, with every field validated first.
fn creation_events(
    tx: &impl ReadOps,
    project: ProjectId,
    states: &[StateDef],
    input: &NewStoryInput,
    now: &str,
) -> Result<Vec<StoryEvent>, AppError> {
    if let Some(slug) = &input.story_type {
        require_known_type(tx, project, slug)?;
    }
    let priority = input
        .priority
        .as_deref()
        .map(|p| {
            Priority::parse(p)
                .ok_or_else(|| AppError::Validation(format!("invalid priority `{p}`")))
        })
        .transpose()?;
    let assignee = input
        .assignee
        .as_deref()
        .map(|lookup| find_member(tx, project, lookup))
        .transpose()?;

    let state_slug = match &input.state {
        Some(slug) => {
            let open = states
                .iter()
                .any(|state| &state.slug == slug && state.super_state == SuperState::Open);
            if !open {
                return Err(AppError::Validation(format!(
                    "'{slug}' is not a valid OPEN state. Available OPEN states: {}",
                    open_state_slugs(states).join(", ")
                )));
            }
            slug.clone()
        }
        None => default_open_state(states)?.slug,
    };

    let mut events = vec![StoryEvent::StoryCreated {
        at: now.to_string(),
        title: input.title.clone(),
        state: state_slug,
    }];
    if input.draft {
        events.push(StoryEvent::StoryCreatedAsDraft {
            at: now.to_string(),
        });
    }
    if let Some(priority) = priority {
        events.push(StoryEvent::StoryPrioritySet {
            at: now.to_string(),
            priority,
        });
    }
    if let Some(labels) = &input.labels {
        let normalized = normalize_labels(labels);
        if !normalized.is_empty() {
            events.push(StoryEvent::StoryLabelsSet {
                at: now.to_string(),
                labels: normalized,
            });
        }
    }
    if let Some(member) = assignee {
        events.push(StoryEvent::StoryAssigned {
            at: now.to_string(),
            member_id: member.id,
        });
    }
    if let Some(description) = &input.description
        && !description.trim().is_empty()
    {
        events.push(StoryEvent::StoryDescriptionSet {
            at: now.to_string(),
            description: description.clone(),
        });
    }
    if let Some(story_type) = &input.story_type {
        events.push(StoryEvent::StoryTypeSet {
            at: now.to_string(),
            story_type: story_type.clone(),
        });
    }
    Ok(events)
}

/// The events and the change summary one `story set` call produces.
struct EditPlan {
    events: Vec<StoryEvent>,
    changes: Vec<String>,
    /// The state the story was moved into, when the edits moved it.
    moved_to: Option<String>,
}

/// Turns a [`FieldEdits`] into events, validating as it goes.
///
/// Every rejection here aborts the whole call: the plan is built before
/// anything is appended, so a batch containing one invalid field writes
/// nothing at all.
fn plan_field_edits(
    tx: &impl ReadOps,
    project: ProjectId,
    states: &BTreeMap<String, StateDef>,
    story: &StorySnapshot,
    edits: &FieldEdits,
    now: &str,
) -> Result<EditPlan, AppError> {
    let mut plan = EditPlan {
        events: Vec::new(),
        changes: Vec::new(),
        moved_to: None,
    };

    if let Some(title) = &edits.title {
        plan.events.push(StoryEvent::StoryTitleSet {
            at: now.to_string(),
            title: title.clone(),
        });
        plan.changes.push(format!("title -> {title}"));
    }
    if let Some(slug) = &edits.state {
        push_state_change(&mut plan, states, story, slug, now)?;
    }
    if let Some(raw) = &edits.priority {
        let priority = Priority::parse(raw)
            .ok_or_else(|| AppError::Validation(format!("invalid priority `{raw}`")))?;
        plan.events.push(StoryEvent::StoryPrioritySet {
            at: now.to_string(),
            priority,
        });
        plan.changes.push(format!("priority -> {raw}"));
    }
    if let Some(lookup) = &edits.assignee {
        plan.events.push(StoryEvent::StoryAssigned {
            at: now.to_string(),
            member_id: resolve_assignee(tx, project, lookup)?,
        });
        plan.changes.push(format!("assignee -> {lookup}"));
    }
    if let Some(csv) = &edits.labels {
        let labels = normalize_labels(
            story
                .labels
                .iter()
                .cloned()
                .chain(std::iter::once(csv.clone())),
        );
        plan.events.push(StoryEvent::StoryLabelsSet {
            at: now.to_string(),
            labels,
        });
        plan.changes.push(format!("labels += {csv}"));
    }
    if let Some(reason) = &edits.blocked {
        plan.events.push(StoryEvent::StoryAwaitingSet {
            at: now.to_string(),
            awaiting: reason.clone(),
        });
        plan.changes.push(format!("blocked: {reason}"));
    }
    if edits.unblocked {
        plan.events.push(StoryEvent::StoryAwaitingCleared {
            at: now.to_string(),
        });
        plan.changes.push("unblocked".to_string());
    }
    if let Some(slug) = &edits.story_type {
        require_known_type(tx, project, slug)?;
        plan.events.push(StoryEvent::StoryTypeSet {
            at: now.to_string(),
            story_type: slug.clone(),
        });
        plan.changes.push(format!("type -> {slug}"));
    }
    if let Some(description) = &edits.description {
        plan.events.push(StoryEvent::StoryDescriptionSet {
            at: now.to_string(),
            description: description.clone(),
        });
        plan.changes.push("description updated".to_string());
    }
    if let Some(raw) = &edits.json {
        apply_json_patch(tx, project, states, story, raw, now, &mut plan)?;
    }

    Ok(plan)
}

/// Applies the `--json` patch object's keys, in the object's own order.
fn apply_json_patch(
    tx: &impl ReadOps,
    project: ProjectId,
    states: &BTreeMap<String, StateDef>,
    story: &StorySnapshot,
    raw: &str,
    now: &str,
    plan: &mut EditPlan,
) -> Result<(), AppError> {
    let patch: serde_json::Value = serde_json::from_str(raw)
        .map_err(|e| AppError::Validation(format!("invalid JSON: {e}")))?;
    let object = patch
        .as_object()
        .ok_or_else(|| AppError::Validation("JSON must be an object".to_string()))?;

    for (key, value) in object {
        match key.as_str() {
            "title" => {
                let title = json_str(value, "title")?;
                if !title.is_empty() {
                    plan.events.push(StoryEvent::StoryTitleSet {
                        at: now.to_string(),
                        title: title.to_string(),
                    });
                    plan.changes.push(format!("title -> {title}"));
                }
            }
            "state" => push_state_change(plan, states, story, json_str(value, "state")?, now)?,
            "priority" => {
                let raw = json_str(value, "priority")?;
                let priority = Priority::parse(raw)
                    .ok_or_else(|| AppError::Validation(format!("invalid priority `{raw}`")))?;
                plan.events.push(StoryEvent::StoryPrioritySet {
                    at: now.to_string(),
                    priority,
                });
                plan.changes.push(format!("priority -> {raw}"));
            }
            "assignee" => match value {
                serde_json::Value::Null => plan.changes.push("assignee cleared".to_string()),
                serde_json::Value::String(lookup) if lookup.is_empty() => {
                    plan.changes.push("assignee cleared".to_string());
                }
                serde_json::Value::String(lookup) => {
                    plan.events.push(StoryEvent::StoryAssigned {
                        at: now.to_string(),
                        member_id: resolve_assignee(tx, project, lookup)?,
                    });
                    plan.changes.push(format!("assignee -> {lookup}"));
                }
                _ => {
                    return Err(AppError::Validation(
                        "assignee must be a string or null".to_string(),
                    ));
                }
            },
            "labels" => {
                let items = value.as_array().ok_or_else(|| {
                    AppError::Validation("labels must be an array of strings".to_string())
                })?;
                // A JSON array is exactly the shape a REST caller or a
                // decompose-generated batch hands in; neither is guaranteed
                // to have split a comma-bearing value already (SH-164).
                let labels = normalize_labels(items.iter().filter_map(|v| v.as_str()));
                plan.changes
                    .push(format!("labels -> [{}]", labels.join(", ")));
                plan.events.push(StoryEvent::StoryLabelsSet {
                    at: now.to_string(),
                    labels,
                });
            }
            "blocked" => match value {
                serde_json::Value::Null => {
                    plan.events.push(StoryEvent::StoryAwaitingCleared {
                        at: now.to_string(),
                    });
                    plan.changes.push("unblocked".to_string());
                }
                serde_json::Value::String(reason) if reason.is_empty() => {
                    plan.events.push(StoryEvent::StoryAwaitingCleared {
                        at: now.to_string(),
                    });
                    plan.changes.push("unblocked".to_string());
                }
                serde_json::Value::String(reason) => {
                    plan.events.push(StoryEvent::StoryAwaitingSet {
                        at: now.to_string(),
                        awaiting: reason.clone(),
                    });
                    plan.changes.push(format!("blocked: {reason}"));
                }
                _ => {
                    return Err(AppError::Validation(
                        "blocked must be a string or null".to_string(),
                    ));
                }
            },
            "story_type" => {
                let slug = json_str(value, "story_type")?;
                require_known_type(tx, project, slug)?;
                plan.events.push(StoryEvent::StoryTypeSet {
                    at: now.to_string(),
                    story_type: slug.to_string(),
                });
                plan.changes.push(format!("type -> {slug}"));
            }
            "description" => {
                let description = json_str(value, "description")?;
                plan.events.push(StoryEvent::StoryDescriptionSet {
                    at: now.to_string(),
                    description: description.to_string(),
                });
                plan.changes.push("description updated".to_string());
            }
            other => {
                return Err(AppError::Validation(format!(
                    "unknown field `{other}` in JSON. Valid fields: title, state, priority, \
                     assignee, labels, blocked, story_type, description"
                )));
            }
        }
    }
    Ok(())
}

/// Adds a state move to the plan, through the one transition batch.
fn push_state_change(
    plan: &mut EditPlan,
    states: &BTreeMap<String, StateDef>,
    story: &StorySnapshot,
    slug: &str,
    now: &str,
) -> Result<(), AppError> {
    let target = states
        .get(slug)
        .ok_or_else(|| undefined_state_error(slug, states))?;
    plan.events.extend(state_transition_events(
        target,
        story.awaiting.is_some(),
        now,
        Vec::new(),
    ));
    plan.changes.push(format!("state -> {slug}"));
    plan.moved_to = Some(slug.to_string());
    Ok(())
}

/// A JSON patch value that has to be a string.
fn json_str<'v>(value: &'v serde_json::Value, field: &str) -> Result<&'v str, AppError> {
    value
        .as_str()
        .ok_or_else(|| AppError::Validation(format!("{field} must be a string")))
}

/// Rejects a story type the project does not define, naming the ones it does.
fn require_known_type(tx: &impl ReadOps, project: ProjectId, slug: &str) -> Result<(), AppError> {
    let types = tx.types(project)?;
    if types.iter().any(|t| t.slug == slug) {
        return Ok(());
    }
    let mut known: Vec<&str> = types.iter().map(|t| t.slug.as_str()).collect();
    known.sort_unstable();
    Err(AppError::Validation(format!(
        "unknown type `{slug}`. Available types: {}",
        known.join(", ")
    )))
}

/// A member by id or GitHub handle. Absent is *not found*.
fn find_member(tx: &impl ReadOps, project: ProjectId, lookup: &str) -> Result<Member, AppError> {
    lookup_member(tx, project, lookup)?
        .ok_or_else(|| AppError::NotFound(format!("member `{lookup}` not found")))
}

/// A member id for `story set --assignee`. Absent is *invalid input*.
///
/// The two spellings are not an oversight: `story assign` reports a missing
/// member as not-found (exit 3) and `story set --assignee` as a validation
/// error (exit 2), and both are pinned by the error contract.
fn resolve_assignee(
    tx: &impl ReadOps,
    project: ProjectId,
    lookup: &str,
) -> Result<String, AppError> {
    lookup_member(tx, project, lookup)?
        .map(|member| member.id)
        .ok_or_else(|| AppError::Validation(format!("member `{lookup}` not found")))
}

fn lookup_member(
    tx: &impl ReadOps,
    project: ProjectId,
    lookup: &str,
) -> Result<Option<Member>, AppError> {
    Ok(tx
        .members(project)?
        .into_iter()
        .find(|member| member.id == lookup || member.github.as_deref() == Some(lookup)))
}

/// The project's OPEN state slugs, in configured order.
fn open_state_slugs(states: &[StateDef]) -> Vec<&str> {
    states
        .iter()
        .filter(|state| state.super_state == SuperState::Open)
        .map(|state| state.slug.as_str())
        .collect()
}

/// [`resolve_story`], rejecting a story that has not been soft-deleted.
///
/// The purge's precondition, in one place, so the plan and the act cannot
/// disagree about what is purgeable — a plan that described a story the act
/// then refused would be a prompt for something that could never happen.
///
/// The refusal names `story delete` rather than merely saying no. Someone
/// typing `story purge` has already decided the story should go; what they need
/// is the step they skipped, not a lecture.
fn resolve_purgeable_story(
    tx: &impl ReadOps,
    project: ProjectId,
    prefix: &str,
    id: &str,
) -> Result<(StoryNo, StoryRow), AppError> {
    let (story_no, row) = resolve_story(tx, project, prefix, id)?;
    if !row.deleted {
        return Err(AppError::Validation(format!(
            "story `{id}` has not been deleted, so there is nothing to purge. Run \
             `story delete {id} \"<reason>\"` first — soft delete is reversible, and \
             purging is not."
        )));
    }
    Ok((story_no, row))
}

/// Every edge a *surviving* story still claims into `story`, as `(id,
/// relation)` pairs naming the claimant and the relation as that claimant
/// spells it.
///
/// Read from the claimant's **snapshot** rather than from the relation table
/// alone, and the difference matters. The table materializes the mirror of
/// every edge, so a story that never asserted anything still has a row when its
/// neighbour asserted one; retracting from it would append an event annulling a
/// claim it never made. The pairs returned here are exactly the claims that
/// would otherwise outlive the story they name — which is what the rebuild
/// oracle compares against, since it derives its expected edges from folded
/// snapshots.
///
/// Sorted, because it is rendered to a user and appended as events, and neither
/// should depend on the order rows came back in.
fn surviving_claims(
    tx: &impl ReadOps,
    project: ProjectId,
    prefix: &str,
    story: StoryNo,
    canonical: &str,
) -> Result<Vec<(String, String)>, AppError> {
    let mut claims = BTreeSet::new();
    for edge in tx.relations_to(project, story)? {
        let Some(row) = tx.story(project, edge.story_no)? else {
            continue;
        };
        let claimant = edge.story_no.to_id(prefix);
        for relation in &row.snapshot.relationships {
            if relation.other_id == canonical {
                claims.insert((claimant.clone(), relation.relation.clone()));
            }
        }
    }
    Ok(claims.into_iter().collect())
}
