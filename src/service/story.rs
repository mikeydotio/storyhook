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

use crate::cli::UnclaimComment;
use crate::domain::provenance::Provenance;
use crate::domain::{
    Member, Priority, StateDef, StoryEvent, StorySnapshot, SuperState, VERIFYING_STATE_SLUG,
    active_state, is_epic, normalize_labels, undefined_state_error,
};
use crate::error::AppError;
use crate::event_hooks::HookEventType;
use crate::output::{HideStatePlan, StoryDeletePlan, UnclaimFallback, UnclaimOutcome};
use crate::store::{
    EventSeq, ExpectedSeq, ProjectId, ReadOps, Store, StoreError, StoryNo, StoryQuery, StoryRow,
    WriteOps,
};

use super::{
    Ctx, Intent, ReadyQueueFilters, append_and_fold, project_prefix, resolve_open_story,
    resolve_story,
};

/// The pseudo-state a lost claim reports as its `expected` half.
///
/// Not a slug any project defines, and deliberately so: a claim's
/// precondition is *any* state other than the project's active one, and
/// naming a single slug there would be a lie a caller could act on.
/// [`AppError::StateConflict`]'s other half is likewise a user-facing state
/// description rather than an assertion that every value is a catalog slug.
pub const UNCLAIMED: &str = "unclaimed";

/// What both claim verbs say when the project has no state to claim into.
///
/// One constructor rather than a literal per site: three call sites now
/// (`claim <id>`, `claim --next`, and the two dry-run planners) and a
/// hand-copied sentence is the shape this project has already paid for
/// repeatedly.
fn no_active_state_error() -> AppError {
    AppError::Validation(
        "no state has role `active`, and this project does not have exactly two OPEN states \
         — a claim has no state to resolve into; set one with `story state set <slug> --role \
         active`"
            .to_string(),
    )
}

/// The claim's own comment, as the extra events
/// [`state_transition_events`] folds into the transition batch.
///
/// One `Vec` either way, so the comment travels in the *same*
/// `append_and_fold` call as the state change rather than a second write. A
/// claim that landed with a comment that did not would be a partial write of
/// one intended action.
fn claim_comment_events(comment: Option<&str>, now: &str) -> Vec<StoryEvent> {
    comment
        .map(|text| StoryEvent::StoryCommentAdded {
            at: now.to_string(),
            text: text.to_string(),
        })
        .into_iter()
        .collect()
}

/// Where `story unclaim` sends a story when the replay cannot answer
/// (SH-483).
///
/// `todo` by user determination, and specifically because SH-125 makes it a
/// [`REQUIRED_STATE`](crate::domain::REQUIRED_STATES) that is OPEN — so the
/// fallback can never itself fail to resolve on a conforming project, which
/// is the whole reason a fallback is permitted to exist at all.
/// `unclaim_fallback_state_is_a_required_open_state` is what keeps the two
/// facts from drifting apart.
pub const UNCLAIM_FALLBACK_STATE: &str = "todo";

/// Where a claimed story should be put back, and why it is not always where
/// it came from (SH-483).
///
/// Three refusals of the replayed origin, all real, and each classified
/// rather than collapsed into one "could not restore":
///
/// 1. no origin at all — the story was created directly in the active state;
/// 2. the origin is no longer a state this project defines;
/// 3. the origin is no longer OPEN, so restoring the story to it would
///    *close* the story rather than release it.
///
/// Pure, and separate from the two service methods for exactly that reason:
/// the real release and its dry run must not be able to disagree about where
/// the story is going.
fn resolve_unclaim_destination(
    id: &str,
    events: &[StoryEvent],
    active: &str,
    states: &BTreeMap<String, StateDef>,
) -> UnclaimOutcome {
    let fallback = |reason: UnclaimFallback| UnclaimOutcome {
        id: id.to_string(),
        from: active.to_string(),
        restored_to: UNCLAIM_FALLBACK_STATE.to_string(),
        fallback: Some(reason),
    };
    let Some(origin) = crate::domain::state_claimed_from(events, active) else {
        return fallback(UnclaimFallback::NoPriorState);
    };
    let Some(def) = states.get(&origin) else {
        return fallback(UnclaimFallback::PriorStateRemoved(origin));
    };
    if def.super_state != SuperState::Open {
        return fallback(UnclaimFallback::PriorStateClosed(origin));
    }
    UnclaimOutcome {
        id: id.to_string(),
        from: active.to_string(),
        restored_to: origin,
        fallback: None,
    }
}

/// The release's own comment, as the extra events
/// [`state_transition_events`] folds into the transition batch.
///
/// The sibling of [`claim_comment_events`], and it takes the whole
/// [`UnclaimComment`] rather than an `Option<&str>` because
/// [`UnclaimComment::Default`] is composed *here* — in the store, inside the
/// write transaction — where the destination and the fallback are known.
/// That is the deliberate opposite of a claim's default, which names the
/// caller's own host and tmux window and so can only be composed by the
/// client.
///
/// A [`Custom`](UnclaimComment::Custom) sentence is written verbatim, a
/// fallback notwithstanding: it is the caller's own text and splicing into it
/// would corrupt what they meant to say. The fallback is reported in
/// [`UnclaimOutcome::fallback`] on every path regardless, which is where a
/// caller reads it.
fn unclaim_comment_events(
    comment: &UnclaimComment,
    outcome: &UnclaimOutcome,
    now: &str,
) -> Vec<StoryEvent> {
    let text = match comment {
        UnclaimComment::Suppressed => return Vec::new(),
        UnclaimComment::Custom(text) => text.clone(),
        UnclaimComment::Default => default_unclaim_comment(outcome),
    };
    vec![StoryEvent::StoryCommentAdded {
        at: now.to_string(),
        text,
    }]
}

/// The sentence an unclaim posts when the caller named no text of their own.
///
/// It names no host and no tmux window, unlike a claim's, and that is not an
/// omission: "starting work *here*" is a locational fact and "I am done
/// holding this" is not, and who performed the write is already recorded
/// structurally on the event by [`Provenance`](crate::store::Provenance)
/// (SH-246). What it does name is the pair a reader cannot recover otherwise
/// — where the story went, and whether that was where it came from.
#[must_use]
pub fn default_unclaim_comment(outcome: &UnclaimOutcome) -> String {
    match &outcome.fallback {
        None => format!(
            "Unclaimed from {}; restored to {}, the state it was claimed from",
            outcome.from, outcome.restored_to
        ),
        Some(fallback) => format!(
            "Unclaimed from {}; restored to {} rather than the state it was claimed from, \
             because {}",
            outcome.from,
            outcome.restored_to,
            fallback.explain(&outcome.from)
        ),
    }
}

/// What a `--dry-run` claim would have written.
///
/// Only what the *write* would have been: every read the real claim performs
/// has already happened for real by the time one of these exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimPlan {
    /// The story that would be claimed, canonicalized.
    pub id: String,
    /// The state it would be claimed out of.
    pub from: String,
    /// The state it would be claimed into — the project's active-role one.
    pub to: String,
}

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
    /// The story type slug. Must be one the project defines; the project's
    /// first configured type is used when absent.
    pub story_type: Option<String>,
    /// A long-form description. Blank or whitespace-only is treated as absent.
    pub description: Option<String>,
    /// An assignable priority slug. Defaults to `low` when absent.
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
                self.ctx.provenance(),
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

    /// Adds a comment to a story, **including a closed one** (SH-261).
    ///
    /// The one [`Intent::Append`] call site in the codebase. A comment records
    /// an observation about a story without changing what the story is, so it
    /// outlives the story's closure the way the evidence it usually carries
    /// does: verification that arrives after a story closes belongs on the
    /// story it verifies, not on whichever open story happened to be nearby.
    ///
    pub fn comment(&self, id: &str, text: &str) -> Result<StorySnapshot, AppError> {
        let now = self.ctx.now();
        let snapshot = self.edit_story(id, Intent::Append, |_row, _states| {
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

    /// Rewrites this story's one comment whose text starts with `marker`, in
    /// place — a retract-and-add pair in the same write, or nothing at all
    /// when `body` is byte-identical to what is already there (SH-524).
    ///
    /// Comments are append-only [`StoryCommentAdded`](StoryEvent::StoryCommentAdded)
    /// with no id, no author, and no `updated_at` of their own; [`StoryCommentRetracted`
    /// ](StoryEvent::StoryCommentRetracted) is the only inverse, keyed by the exact
    /// `(comment_at, text)` pair. This is the codebase's second user of that event
    /// (`story undo` is the first) and its third [`Intent::Append`] grant —
    /// `tests/invoker_seam.rs` fences the set at exactly three, named.
    ///
    /// The bool in the returned pair is whether anything was actually
    /// written — the SH-524 progress publisher's own backoff needs to tell a
    /// real rewrite apart from a settled, unchanged republish.
    ///
    /// `marker` must be a prefix no ordinary user comment would ever start
    /// with, and the caller's own `body` must not itself begin with a
    /// *different* self-identifying prefix mistaken for `marker`'s.
    pub(crate) fn upsert_marked_comment(
        &self,
        id: &str,
        marker: &str,
        body: &str,
    ) -> Result<(StorySnapshot, bool), AppError> {
        let now = self.ctx.now();
        let wrote = std::cell::Cell::new(false);
        let snapshot = self.edit_story(id, Intent::Append, |row, _states| {
            let existing = row
                .snapshot
                .comments
                .iter()
                .rev()
                .find(|comment| comment.text.starts_with(marker));
            if existing.is_some_and(|comment| comment.text == body) {
                return Ok(Vec::new());
            }
            wrote.set(true);
            let mut events = Vec::new();
            if let Some(existing) = existing {
                events.push(StoryEvent::StoryCommentRetracted {
                    at: now.clone(),
                    comment_at: existing.at.clone(),
                    text: existing.text.clone(),
                });
            }
            events.push(StoryEvent::StoryCommentAdded {
                at: now.clone(),
                text: body.to_string(),
            });
            Ok(events)
        })?;
        if wrote.get() {
            self.ctx.fire_hook(
                HookEventType::Comment,
                &serde_json::json!({
                    "event_type": "comment",
                    "story_id": id,
                    "timestamp": self.ctx.now(),
                    "story_title": &snapshot.title,
                    "comment_text": body,
                }),
            );
        }
        Ok((snapshot, wrote.get()))
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
                self.ctx.provenance(),
            )?)
        })?)
    }

    /// Sets an open story's priority.
    pub fn set_priority(&self, id: &str, priority: &str) -> Result<StorySnapshot, AppError> {
        let level = assignable_priority(priority)?;
        let now = self.ctx.now();
        let snapshot = self.edit_story(id, Intent::Edit, |_row, _states| {
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
        let snapshot = self.edit_story(id, Intent::Edit, |row, _states| {
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
        self.edit_story(id, Intent::Edit, |_row, _states| {
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
        self.edit_story(id, Intent::Edit, |row, _states| {
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
        let cleanup_lease = if state == VERIFYING_STATE_SLUG {
            super::cleanup_lease::marker_at(self.ctx.cwd())?
        } else {
            None
        };
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
            let project_record = tx.project(project)?.ok_or_else(|| {
                StoreError::Corrupt(format!("project id {} disappeared", project.get()))
            })?;

            if let Some(expected) = if_state {
                let (_, current) = resolve_story(&*tx, project, &prefix, id)?;
                if current.state != expected {
                    return Err(AppError::StateConflict(
                        expected.to_string(),
                        current.state.clone(),
                    )
                    .into());
                }
            }

            let (story_no, row) = resolve_open_story(&*tx, project, &prefix, id)?;
            if let Some(lease) = &cleanup_lease {
                let canonical_id = story_no.to_id(&prefix);
                if lease.project_slug != project_record.slug {
                    return Err(StoreError::Validation(format!(
                        "cleanup lease project mismatch: marker says `{}`, selected project is `{}`",
                        lease.project_slug, project_record.slug
                    )));
                }
                if lease.story_id != canonical_id {
                    return Err(StoreError::Validation(format!(
                        "cleanup lease story mismatch: marker says `{}`, transition targets `{canonical_id}`",
                        lease.story_id
                    )));
                }
            }
            refuse_epic_state_change(&row.snapshot)?;
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
            let mut extra = Vec::new();
            if let Some(lease) = cleanup_lease.clone() {
                extra.push(StoryEvent::StoryCleanupLeaseRecorded {
                    at: now.clone(),
                    lease: Box::new(lease),
                });
            }
            extra.extend(comment.map(|text| StoryEvent::StoryCommentAdded {
                    at: now.clone(),
                    text: text.to_string(),
                })
                .into_iter()
                .chain(awaiting.clone().map(|reason| StoryEvent::StoryAwaitingSet {
                    at: now.clone(),
                    awaiting: reason,
                })));
            let snapshot = append_state_transition(
                tx,
                project,
                story_no,
                &row,
                &prefix,
                &states,
                &target,
                &now,
                extra,
                self.ctx.provenance(),
            )?;
            Ok((row.snapshot, snapshot))
        })?;

        self.fire_transition_hooks(id, &before.title, &before.state, state, &snapshot, &now);
        Ok(snapshot)
    }

    /// `story claim --next` (SH-476, the mechanism SH-344 introduced) — picks
    /// the same story [`QueryService::next`](super::QueryService::next) would,
    /// and moves it into the project's active state before returning it, both
    /// inside one write transaction.
    ///
    /// That single transaction is the whole correctness argument: a write
    /// transaction is `BEGIN IMMEDIATE`, exclusive among writers, so
    /// selection is *re-run* under the same lock the claim commits under
    /// rather than trusted from an earlier read — there is no window between
    /// "decide" and "take" for a second caller to land in. Contrast
    /// `set_state`'s `--if-state`, which closes the same window with a
    /// compare-and-swap over two round trips; a claim needs no CAS because it
    /// never had a second round trip to race across.
    ///
    /// Returns `Ok(None)` when nothing is ready — a legitimate outcome, not
    /// an error, mirroring [`QueryService::next`](super::QueryService::next)
    /// returning an empty list. The first element of the pair is the
    /// snapshot as it stood *before* the claim, so a caller that has to undo
    /// its own claim (the plugin's dispatch rollback) knows what to move
    /// back to.
    ///
    /// # Errors
    ///
    /// [`AppError::Validation`] when the project has no state a claim can
    /// resolve to — [`domain::active_state`] answers `None` for a project
    /// with neither an explicit `active` role nor exactly two OPEN states.
    /// Checked before selection runs, so this never depends on whether a
    /// story happens to be ready.
    /// Applies the CLI's ready-queue filters before claiming.
    pub fn claim_next_filtered(
        &self,
        filters: ReadyQueueFilters<'_>,
        comment: Option<&str>,
    ) -> Result<Option<(StorySnapshot, StorySnapshot)>, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        let claimed = self.ctx.store().write(|tx| {
            let active = active_state(&tx.states(project)?).ok_or_else(no_active_state_error)?;
            let states = tx.state_map(project)?;
            let query = super::QueryService::new(&*tx, project, &now);
            let Some(candidate) = query.next_filtered(1, filters)?.into_iter().next() else {
                return Ok(None);
            };
            let prefix = project_prefix(&*tx, project)?;
            let (story_no, row) = resolve_open_story(&*tx, project, &prefix, &candidate.story.id)?;
            let events = state_transition_events(
                &active,
                row.awaiting.is_some(),
                &now,
                claim_comment_events(comment, &now),
            );
            let snapshot = append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &events,
                self.ctx.provenance(),
            )?;
            Ok(Some((row.snapshot, snapshot)))
        })?;

        let Some((before, snapshot)) = claimed else {
            return Ok(None);
        };
        self.fire_transition_hooks(
            &before.id,
            &before.title,
            &before.state,
            &snapshot.state,
            &snapshot,
            &now,
        );
        Ok(Some((before, snapshot)))
    }

    /// `story claim <id>` (SH-476) — moves one *named* story into the
    /// project's active state, and returns the snapshot it came from beside
    /// the one it landed in.
    ///
    /// The sibling of [`Self::claim_next`], and atomic for the same reason:
    /// the state is read and the transition is appended inside one
    /// `BEGIN IMMEDIATE` write transaction, so the read-then-move window the
    /// hand-rolled `show` + `move --if-state` dance had is not merely
    /// narrowed, it does not exist. That is why this takes no `--if-state`
    /// witness from the caller: there is no second round trip for a witness
    /// to go stale across.
    ///
    /// # The one race that survives, and how it is reported
    ///
    /// Somebody else claiming the story first. That is answered with
    /// [`AppError::StateConflict`] — `result:"conflict"`, exit 9, `.actual`
    /// naming the state found — exactly as `story move --if-state` answers a
    /// lost compare-and-swap, so a caller reads both the same way.
    ///
    /// `expected` carries the pseudo-state [`UNCLAIMED`]. A claim's
    /// precondition is not one slug — it is *any* state other than the
    /// active one — so naming a single slug there would be a lie. The pair
    /// already carries user-facing state descriptions on the other side for
    /// the same reason.
    ///
    /// # What this deliberately does not check
    ///
    /// Readiness. A caller naming a specific id is making a specific request,
    /// and the ready-gate belongs to the dispatcher that decides whether work
    /// should start at all (`story.sh`'s `cmd_dispatch`, which keeps its
    /// own). [`Self::claim_next`] needs no such gate:
    /// [`QueryService::next`](super::QueryService::next) only ever answers
    /// with ready stories.
    ///
    /// # Errors
    ///
    /// [`AppError::Validation`] when the project has no state a claim can
    /// resolve to, or when the story is closed;
    /// [`AppError::NotFound`] when no such story exists;
    /// [`AppError::StateConflict`] when the story is already claimed.
    pub fn claim_story(
        &self,
        id: &str,
        comment: Option<&str>,
    ) -> Result<(StorySnapshot, StorySnapshot), AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        let (before, snapshot) = self.ctx.store().write(|tx| {
            let active = active_state(&tx.states(project)?).ok_or_else(no_active_state_error)?;
            let states = tx.state_map(project)?;
            let prefix = project_prefix(&*tx, project)?;
            let (story_no, row) = resolve_open_story(&*tx, project, &prefix, id)?;
            if row.state == active.slug {
                return Err(
                    AppError::StateConflict(UNCLAIMED.to_string(), row.state.clone()).into(),
                );
            }
            let events = state_transition_events(
                &active,
                row.awaiting.is_some(),
                &now,
                claim_comment_events(comment, &now),
            );
            let snapshot = append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &events,
                self.ctx.provenance(),
            )?;
            Ok((row.snapshot, snapshot))
        })?;

        self.fire_transition_hooks(
            &before.id,
            &before.title,
            &before.state,
            &snapshot.state,
            &snapshot,
            &now,
        );
        Ok((before, snapshot))
    }

    /// `story unclaim <id>` (SH-483) — moves one story out of the project's
    /// active state and back to the state it was claimed from, returning the
    /// snapshot it left, the one it landed in, and what became of it.
    ///
    /// The inverse of [`Self::claim_story`] and atomic for the same reason:
    /// the state is read, the origin is replayed out of the story's own event
    /// log, the comment is composed and the transition is appended inside one
    /// `BEGIN IMMEDIATE` write transaction. There is no second round trip for
    /// a caller's witness to go stale across, so this takes no `--if-state`.
    ///
    /// # The origin, and why nothing records it
    ///
    /// `StoryStateChanged` carries only the destination state, so where a
    /// story was claimed *from* is derived by
    /// [`domain::state_claimed_from`](crate::domain::state_claimed_from)
    /// rather than stored: no schema change, no column, no `--to` flag, and
    /// every caller — a human, MCP, the Full Auto engine — gets restoration
    /// for free because the store answers the question instead of the caller
    /// carrying the answer around.
    ///
    /// # The fallback, and why it is loud
    ///
    /// [`UNCLAIM_FALLBACK_STATE`] when the replay cannot answer, the origin
    /// is no longer a state this project defines, or it is no longer OPEN.
    /// Reported in [`UnclaimOutcome::fallback`] on every path and written
    /// into the default comment on the path that writes one: a silent
    /// substitution here is a wrong answer stored about where the work came
    /// from.
    ///
    /// # The one race that survives, and how it is reported
    ///
    /// Somebody else moving the story first. Answered with
    /// [`AppError::StateConflict`] — `result:"conflict"`, exit 9 — naming the
    /// *real* active slug as `expected`, not a pseudo-state. A claim's
    /// precondition is any state but the active one and so cannot be named by
    /// one slug ([`UNCLAIMED`]); an unclaim's precondition is exactly that
    /// slug, so naming it is the truth.
    ///
    /// # Errors
    ///
    /// [`AppError::Validation`] when the project has no active state to
    /// release from, or when the story is closed;
    /// [`AppError::NotFound`] when no such story exists;
    /// [`AppError::StateConflict`] when the story is not currently claimed.
    pub fn unclaim_story(
        &self,
        id: &str,
        comment: &UnclaimComment,
    ) -> Result<(StorySnapshot, StorySnapshot, UnclaimOutcome), AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        let (before, snapshot, outcome) = self.ctx.store().write(|tx| {
            let active = active_state(&tx.states(project)?).ok_or_else(no_active_state_error)?;
            let states = tx.state_map(project)?;
            let prefix = project_prefix(&*tx, project)?;
            let (story_no, row) = resolve_open_story(&*tx, project, &prefix, id)?;
            if row.state != active.slug {
                return Err(AppError::StateConflict(active.slug.clone(), row.state.clone()).into());
            }
            let stored = tx.events_for(project, story_no)?;
            let known: Vec<StoryEvent> = stored.iter().filter_map(|e| e.known().cloned()).collect();
            let outcome =
                resolve_unclaim_destination(&row.snapshot.id, &known, &active.slug, &states);
            let target = states
                .get(&outcome.restored_to)
                .cloned()
                .ok_or_else(|| undefined_state_error(&outcome.restored_to, &states))?;
            let events = state_transition_events(
                &target,
                row.awaiting.is_some(),
                &now,
                unclaim_comment_events(comment, &outcome, &now),
            );
            let snapshot = append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &events,
                self.ctx.provenance(),
            )?;
            Ok((row.snapshot, snapshot, outcome))
        })?;

        self.fire_transition_hooks(
            &before.id,
            &before.title,
            &before.state,
            &snapshot.state,
            &snapshot,
            &now,
        );
        Ok((before, snapshot, outcome))
    }

    /// What a `--dry-run` unclaim of `id` would do, without writing anything.
    ///
    /// Every refusal the real release makes is made here too — no active
    /// state, no such story, a closed story, one that is not claimed —
    /// because a dry run that reports a plan the real command would refuse is
    /// worse than no dry run at all.
    ///
    /// # Errors
    ///
    /// The same set [`Self::unclaim_story`] returns.
    pub fn plan_unclaim(&self, id: &str) -> Result<UnclaimOutcome, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| {
            let active = active_state(&tx.states(project)?).ok_or_else(no_active_state_error)?;
            let states = tx.state_map(project)?;
            let prefix = project_prefix(tx, project)?;
            let (story_no, row) = resolve_open_story(tx, project, &prefix, id)?;
            if row.state != active.slug {
                return Err(AppError::StateConflict(active.slug.clone(), row.state.clone()).into());
            }
            let stored = tx.events_for(project, story_no)?;
            let known: Vec<StoryEvent> = stored.iter().filter_map(|e| e.known().cloned()).collect();
            let outcome =
                resolve_unclaim_destination(&row.snapshot.id, &known, &active.slug, &states);
            // The same lookup the real path performs, so a project missing
            // `todo` outright is refused here too rather than promised a
            // destination that does not exist.
            if !states.contains_key(&outcome.restored_to) {
                return Err(undefined_state_error(&outcome.restored_to, &states).into());
            }
            Ok(outcome)
        })?)
    }

    /// What a `--dry-run` claim of `id` would do, without writing anything.
    ///
    /// Every refusal the real claim makes is made here too — no active state,
    /// no such story, a closed story, an already-claimed one — because a dry
    /// run that reports a plan the real command would refuse is worse than no
    /// dry run at all. Only the write is symbolic; the reads are real, which
    /// is the same asymmetry `story decompose --dry-run` and `story migrate
    /// --dry-run` already follow.
    ///
    /// Returns the story's id and the state it would be claimed out of and
    /// into.
    ///
    /// # Errors
    ///
    /// The same set [`Self::claim_story`] returns.
    pub fn plan_claim_story(&self, id: &str) -> Result<ClaimPlan, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| {
            let active = active_state(&tx.states(project)?).ok_or_else(no_active_state_error)?;
            let prefix = project_prefix(tx, project)?;
            let (_, row) = resolve_open_story(tx, project, &prefix, id)?;
            if row.state == active.slug {
                return Err(
                    AppError::StateConflict(UNCLAIMED.to_string(), row.state.clone()).into(),
                );
            }
            Ok(ClaimPlan {
                id: row.snapshot.id.clone(),
                from: row.state.clone(),
                to: active.slug.clone(),
            })
        })?)
    }

    /// What a `--dry-run` `story claim --next` would do, without writing.
    ///
    /// `Ok(None)` is "nothing is ready", the same real answer
    /// [`Self::claim_next_filtered`] gives.
    ///
    /// # Errors
    ///
    /// [`AppError::Validation`] when the project has no state a claim can
    /// resolve to.
    /// Applies the CLI's ready-queue filters without writing.
    pub fn plan_claim_next_filtered(
        &self,
        filters: ReadyQueueFilters<'_>,
    ) -> Result<Option<ClaimPlan>, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| {
            let active = active_state(&tx.states(project)?).ok_or_else(no_active_state_error)?;
            let query = super::QueryService::new(tx, project, &now);
            let Some(candidate) = query.next_filtered(1, filters)?.into_iter().next() else {
                return Ok(None);
            };
            Ok(Some(ClaimPlan {
                id: candidate.story.id.clone(),
                from: candidate.story.state.clone(),
                to: active.slug.clone(),
            }))
        })?)
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
            if plan.moved_to.is_some() {
                refuse_epic_state_change(&row.snapshot)?;
            }
            let mut snapshot = append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &plan.events,
                self.ctx.provenance(),
            )?;
            let closes = plan
                .moved_to
                .as_ref()
                .and_then(|slug| states.get(slug))
                .is_some_and(|target| target.super_state == SuperState::Closed);
            if closes {
                super::relation::retract_closed_blocker_edges(
                    tx,
                    project,
                    story_no,
                    &prefix,
                    &states,
                    &now,
                    self.ctx.provenance(),
                )?;
                snapshot = tx
                    .story(project, story_no)?
                    .map(|row| row.snapshot)
                    .ok_or_else(|| AppError::NotFound(format!("story `{id}` not found")))?;
            }
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

    /// Everything a [`delete`](Self::delete) would destroy. Writes nothing.
    ///
    /// The first half of the two-step: this travels to whichever process has a
    /// terminal and becomes a prompt there, or — with `--json`, or no terminal
    /// — a refusal naming `--force`.
    pub fn delete_plan(&self, id: &str) -> Result<StoryDeletePlan, AppError> {
        let project = self.ctx.project();
        Ok(self.ctx.store().read(|tx| {
            let prefix = project_prefix(tx, project)?;
            let (story_no, row) = resolve_story(tx, project, &prefix, id)?;
            let canonical = story_no.to_id(&prefix);
            Ok(StoryDeletePlan {
                title: row.snapshot.title.clone(),
                events: tx.events_for(project, story_no)?.len(),
                retracted: surviving_claims(tx, project, &prefix, story_no, &canonical)?,
                id: canonical,
            })
        })?)
    }

    /// Removes a story permanently: its events, its row, and every trace of it.
    ///
    /// The only irreversible thing that can be done to a single story. The
    /// caller must have passed through [`delete_plan`](Self::delete_plan), or
    /// asserted the equivalent `--force` contract.
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
    pub fn delete(&self, id: &str) -> Result<String, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();
        let (canonical, title, retracted, removed) = self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let (story_no, row) = resolve_story(&*tx, project, &prefix, id)?;
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
                    self.ctx.provenance(),
                )?;
            }

            let removed = tx.purge_story(project, story_no)?;
            Ok((canonical, row.snapshot.title.clone(), retracted, removed))
        })?;

        let mut message = format!(
            "deleted {canonical} — {title}\n{} event{} permanently deleted",
            removed.events,
            if removed.events == 1 { "" } else { "s" },
        );
        for (other_id, relation) in &retracted {
            message.push_str(&format!("\nretracted {other_id} {relation} {canonical}"));
        }
        message.push_str(&format!("\n{canonical} will never be reused as a story id"));
        Ok(message)
    }

    /// Reopens a closed story into the project's default open state.
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
            refuse_epic_state_change(&row.snapshot)?;
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
                self.ctx.provenance(),
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
                self.ctx.provenance(),
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
                self.ctx.provenance(),
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
                self.ctx.provenance(),
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
                    self.ctx.provenance(),
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

    /// The read-modify-write every single-event write to one story shares:
    /// resolve under `intent`, build the batch from the story as it reads
    /// *inside* the transaction, append, fold, and write the snapshot back.
    ///
    /// `intent` is the only thing that varies between callers, and it is
    /// required rather than defaulted: which stories a write may reach is a
    /// decision each caller makes deliberately, at a call site a reader can see
    /// it at. See [`Intent`].
    fn edit_story<F>(&self, id: &str, intent: Intent, build: F) -> Result<StorySnapshot, AppError>
    where
        F: FnOnce(&StoryRow, &BTreeMap<String, StateDef>) -> Result<Vec<StoryEvent>, AppError>,
    {
        let project = self.ctx.project();
        Ok(self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let states = tx.state_map(project)?;
            let (story_no, row) = intent.resolve(&*tx, project, &prefix, id)?;
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
                self.ctx.provenance(),
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

/// Refuses a direct state change on an epic, whose state is computed from its
/// children rather than set.
///
/// Gated on the TYPE (SH-499). It used to gate on `has_children`, and the
/// message used to teach that rule to everyone who hit it — so an ordinary
/// story that acquired one child became un-moveable and was told the reason was
/// its children. Not every story with children is a folder.
fn refuse_epic_state_change(story: &StorySnapshot) -> Result<(), AppError> {
    if is_epic(story) {
        return Err(AppError::Validation(format!(
            "story `{}` is an epic, so its state is computed from its children and cannot be moved directly; \
move a child instead, or change this story's type if it is not a folder",
            story.id
        )));
    }
    Ok(())
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

/// The project's default story type, with the creation-time refusal an empty
/// legacy catalog needs.
pub(super) fn default_story_type(
    tx: &impl ReadOps,
    project: ProjectId,
) -> Result<String, AppError> {
    crate::domain::default_type(&tx.types(project)?)
        .map(|story_type| story_type.slug)
        .ok_or_else(|| AppError::Validation("project has no configured story types".to_string()))
}

/// Parses a priority accepted by a current mutation path.
///
/// `Priority::None` remains decodable for old events, snapshots and exports,
/// but it is no longer a value a caller may assign.
pub(super) fn assignable_priority(raw: &str) -> Result<Priority, AppError> {
    match Priority::parse(raw) {
        Some(
            priority @ (Priority::Critical | Priority::High | Priority::Medium | Priority::Low),
        ) => Ok(priority),
        Some(Priority::None) | None => Err(AppError::Validation(
            "priority must be one of: critical, high, medium, low".to_string(),
        )),
    }
}

/// The events `story new` writes, with every field validated first.
fn creation_events(
    tx: &impl ReadOps,
    project: ProjectId,
    states: &[StateDef],
    input: &NewStoryInput,
    now: &str,
) -> Result<Vec<StoryEvent>, AppError> {
    let story_type = match &input.story_type {
        Some(slug) => {
            require_known_type(tx, project, slug)?;
            slug.clone()
        }
        None => default_story_type(tx, project)?,
    };
    let priority = input
        .priority
        .as_deref()
        .map(assignable_priority)
        .transpose()?
        .unwrap_or(Priority::Low);
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
    events.push(StoryEvent::StoryPrioritySet {
        at: now.to_string(),
        priority,
    });
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
    events.push(StoryEvent::StoryTypeSet {
        at: now.to_string(),
        story_type,
    });
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
        let priority = assignable_priority(raw)?;
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
                let priority = assignable_priority(raw)?;
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

/// Appends one state transition and, if its final destination is CLOSED,
/// retracts every task dependency this story used to impose before the write
/// transaction commits.
#[allow(clippy::too_many_arguments)]
pub(crate) fn append_state_transition(
    tx: &mut impl WriteOps,
    project: ProjectId,
    story_no: StoryNo,
    row: &StoryRow,
    prefix: &str,
    states: &BTreeMap<String, StateDef>,
    target: &StateDef,
    at: &str,
    extra: Vec<StoryEvent>,
    provenance: &Provenance,
) -> Result<StorySnapshot, AppError> {
    let events = state_transition_events(target, row.awaiting.is_some(), at, extra);
    let snapshot = append_and_fold(
        tx,
        project,
        story_no,
        prefix,
        states,
        ExpectedSeq::Exact(row.head_seq),
        &events,
        provenance,
    )?;
    if target.super_state != SuperState::Closed {
        return Ok(snapshot);
    }

    super::relation::retract_closed_blocker_edges(
        tx, project, story_no, prefix, states, at, provenance,
    )?;
    tx.story(project, story_no)?
        .map(|row| row.snapshot)
        .ok_or_else(|| AppError::NotFound(format!("story `{}` not found", story_no.to_id(prefix))))
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

/// The pure halves of `story unclaim` (SH-483): where a release goes, and the
/// sentence it posts. Both are decided without a store, which is what lets
/// them be proved without one.
#[cfg(test)]
mod unclaim_tests {
    use super::{
        StateDef, UNCLAIM_FALLBACK_STATE, UnclaimComment, UnclaimFallback, UnclaimOutcome,
        default_unclaim_comment, resolve_unclaim_destination, unclaim_comment_events,
    };
    use crate::domain::{REQUIRED_STATES, StoryEvent, SuperState};
    use std::collections::BTreeMap;

    /// The guarantee the fallback rests on. `todo` may be substituted for a
    /// state that cannot be restored only because SH-125 makes it a state
    /// every project has, in a superstate a story can actually sit open in —
    /// a fallback that could itself fail to resolve is not a fallback.
    #[test]
    fn unclaim_fallback_state_is_a_required_open_state() {
        let required = REQUIRED_STATES
            .iter()
            .find(|state| state.slug == UNCLAIM_FALLBACK_STATE)
            .expect("the unclaim fallback must be a state every project is required to have");
        assert_eq!(required.super_state, SuperState::Open);
    }

    fn states(defs: &[(&str, SuperState)]) -> BTreeMap<String, StateDef> {
        defs.iter()
            .map(|(slug, super_state)| {
                (
                    (*slug).to_string(),
                    StateDef {
                        slug: (*slug).to_string(),
                        super_state: super_state.clone(),
                        role: None,
                        description: None,
                    },
                )
            })
            .collect()
    }

    fn ordinary_states() -> BTreeMap<String, StateDef> {
        states(&[
            ("todo", SuperState::Open),
            ("triage", SuperState::Open),
            ("in-progress", SuperState::Open),
            ("done", SuperState::Closed),
        ])
    }

    fn created(state: &str) -> StoryEvent {
        StoryEvent::StoryCreated {
            at: "2026-08-26T00:00:00Z".to_string(),
            title: "A story".to_string(),
            state: state.to_string(),
        }
    }

    fn moved(state: &str) -> StoryEvent {
        StoryEvent::StoryStateChanged {
            at: "2026-08-26T00:00:01Z".to_string(),
            state: state.to_string(),
        }
    }

    #[test]
    fn an_ordinary_release_restores_the_state_it_was_claimed_from() {
        let outcome = resolve_unclaim_destination(
            "SH-1",
            &[created("todo"), moved("triage"), moved("in-progress")],
            "in-progress",
            &ordinary_states(),
        );
        assert_eq!(
            outcome,
            UnclaimOutcome {
                id: "SH-1".to_string(),
                from: "in-progress".to_string(),
                restored_to: "triage".to_string(),
                fallback: None,
            }
        );
    }

    /// Fallback 1: `story new --state in-progress`.
    #[test]
    fn a_story_created_in_the_active_state_falls_back_and_says_why() {
        let outcome = resolve_unclaim_destination(
            "SH-1",
            &[created("in-progress")],
            "in-progress",
            &ordinary_states(),
        );
        assert_eq!(outcome.restored_to, UNCLAIM_FALLBACK_STATE);
        assert_eq!(outcome.fallback, Some(UnclaimFallback::NoPriorState));
    }

    /// Fallback 2: `story state remove triage` between the claim and the
    /// release.
    #[test]
    fn an_origin_the_project_no_longer_defines_falls_back_naming_it() {
        let outcome = resolve_unclaim_destination(
            "SH-1",
            &[created("todo"), moved("triage"), moved("in-progress")],
            "in-progress",
            &states(&[
                ("todo", SuperState::Open),
                ("in-progress", SuperState::Open),
                ("done", SuperState::Closed),
            ]),
        );
        assert_eq!(outcome.restored_to, UNCLAIM_FALLBACK_STATE);
        assert_eq!(
            outcome.fallback,
            Some(UnclaimFallback::PriorStateRemoved("triage".to_string()))
        );
    }

    /// Fallback 3, and the one with teeth: restoring a story to a state that
    /// has since been reclassified CLOSED would *close* the story instead of
    /// releasing it.
    #[test]
    fn an_origin_that_is_no_longer_open_falls_back_rather_than_closing_the_story() {
        let outcome = resolve_unclaim_destination(
            "SH-1",
            &[created("todo"), moved("triage"), moved("in-progress")],
            "in-progress",
            &states(&[
                ("todo", SuperState::Open),
                ("triage", SuperState::Closed),
                ("in-progress", SuperState::Open),
                ("done", SuperState::Closed),
            ]),
        );
        assert_eq!(outcome.restored_to, UNCLAIM_FALLBACK_STATE);
        assert_eq!(
            outcome.fallback,
            Some(UnclaimFallback::PriorStateClosed("triage".to_string()))
        );
    }

    /// A story genuinely claimed out of `todo` lands in `todo` with **no**
    /// fallback recorded. The destination alone cannot distinguish a
    /// restoration from a substitution, which is exactly why the flag exists
    /// separately from it.
    #[test]
    fn landing_in_todo_on_purpose_is_not_a_fallback() {
        let outcome = resolve_unclaim_destination(
            "SH-1",
            &[created("todo"), moved("in-progress")],
            "in-progress",
            &ordinary_states(),
        );
        assert_eq!(outcome.restored_to, "todo");
        assert_eq!(outcome.fallback, None);
    }

    fn outcome(restored_to: &str, fallback: Option<UnclaimFallback>) -> UnclaimOutcome {
        UnclaimOutcome {
            id: "SH-1".to_string(),
            from: "in-progress".to_string(),
            restored_to: restored_to.to_string(),
            fallback,
        }
    }

    #[test]
    fn the_default_sentence_names_both_ends_of_the_move() {
        assert_eq!(
            default_unclaim_comment(&outcome("triage", None)),
            "Unclaimed from in-progress; restored to triage, the state it was claimed from"
        );
    }

    /// The determination's own requirement: a substituted destination is said
    /// out loud in the auto-comment, never left for a reader to infer from a
    /// state they were not expecting.
    #[test]
    fn the_default_sentence_says_when_the_destination_was_substituted() {
        for fallback in [
            UnclaimFallback::NoPriorState,
            UnclaimFallback::PriorStateRemoved("triage".to_string()),
            UnclaimFallback::PriorStateClosed("triage".to_string()),
        ] {
            let text = default_unclaim_comment(&outcome("todo", Some(fallback.clone())));
            assert!(
                text.contains("rather than the state it was claimed from"),
                "{}: {text}",
                fallback.code()
            );
            assert!(
                text.contains(&fallback.explain("in-progress")),
                "{}: {text}",
                fallback.code()
            );
        }
    }

    /// A caller's own sentence is written verbatim, fallback or not: splicing
    /// into text somebody else wrote would corrupt what they meant to say.
    /// The fallback still reaches them through `UnclaimOutcome::fallback`.
    #[test]
    fn a_custom_sentence_is_never_edited_by_the_fallback() {
        let events = unclaim_comment_events(
            &UnclaimComment::Custom("handing this back".to_string()),
            &outcome("todo", Some(UnclaimFallback::NoPriorState)),
            "2026-08-26T00:00:02Z",
        );
        assert_eq!(
            events,
            vec![StoryEvent::StoryCommentAdded {
                at: "2026-08-26T00:00:02Z".to_string(),
                text: "handing this back".to_string(),
            }]
        );
    }

    #[test]
    fn no_comment_writes_no_event_at_all() {
        assert!(
            unclaim_comment_events(
                &UnclaimComment::Suppressed,
                &outcome("todo", None),
                "2026-08-26T00:00:02Z",
            )
            .is_empty()
        );
    }

    /// The three fallback codes are distinct and stable, because a `--json`
    /// caller branches on them.
    #[test]
    fn every_fallback_code_is_distinct() {
        let codes = [
            UnclaimFallback::NoPriorState.code(),
            UnclaimFallback::PriorStateRemoved("x".to_string()).code(),
            UnclaimFallback::PriorStateClosed("x".to_string()).code(),
        ];
        let mut unique = codes.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            codes.len(),
            "codes must be distinct: {codes:?}"
        );
    }
}
