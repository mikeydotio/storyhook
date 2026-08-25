//! Undo, without rewriting the past.
//!
//! The TUI's undo used to work by *replacing* a story's event log with the copy
//! it took before the mutation. That is unavailable here and would be a bad
//! idea if it were available: an append-only log whose entries can be deleted
//! is not an audit trail, and every folded read model that was right about the
//! old history becomes right about a history that never happened.
//!
//! So undo is a **compensating write**. [`restore`] is handed the log a story
//! had before some change, folds it, folds what the story has now, and appends
//! the events that carry the second back to the first. Nothing is removed;
//! `story show` reports the state the user asked to get back to, and the
//! history says both what happened and that it was undone.
//!
//! # What this changes for a user
//!
//! Two things, and both are honest rather than incidental.
//!
//! * **Undoing a story's creation deletes it** rather than making it never have
//!   existed. The id stays spent, and `story show` still answers — with a
//!   deleted story. A tracker that could make an id vanish would be a tracker
//!   whose ids cannot be quoted anywhere.
//! * **Undoing is itself in the history.** A retracted comment leaves a
//!   [`StoryCommentRetracted`](crate::domain::StoryEvent::StoryCommentRetracted)
//!   beside it, naming what was withdrawn.
//!
//! # Scope
//!
//! Every field the TUI can change has an inverse here, because those are the
//! operations that are undoable. Two fields have no clearing event and no TUI
//! path that would need one — a story's **type**, and a **relation to a story
//! that no longer exists** — and `restore` refuses those loudly, naming the
//! field, rather than silently leaving the story where it was.

use crate::domain::{StoryEvent, StorySnapshot, fold_story, normalize_labels};
use crate::error::AppError;
use crate::store::{ExpectedSeq, ReadOps, Store, StoryNo, partition_known};

use super::{Ctx, append_and_fold, project_prefix, resolve_story};

/// Restores a story to the state a given event log folds to, by appending the
/// events that get it there.
///
/// `target` is the log the caller wants the story to *look like*, not a log to
/// install. An empty `target` means the story did not exist before the change
/// being undone, which is expressed as a deletion.
///
/// Returns the events it appended, so a caller can report what undoing cost.
pub fn restore<S: Store>(
    ctx: &Ctx<'_, S>,
    id: &str,
    target: &[StoryEvent],
) -> Result<Vec<StoryEvent>, AppError> {
    let now = ctx.now();
    let project = ctx.project();

    let (story_no, current, states, prefix) = ctx.store().read(|tx| {
        let prefix = project_prefix(tx, project)?;
        let (story_no, _row) = resolve_story(tx, project, &prefix, id)?;
        let stored = tx.events_for(project, story_no)?;
        let (known, _unknown) = partition_known(story_no, &stored);
        Ok((story_no, known, tx.state_map(project)?, prefix))
    })?;

    let public = story_no.to_id(&prefix);
    let before = fold_story(&public, &current, &states)?;
    let after = if target.is_empty() {
        None
    } else {
        Some(fold_story(&public, target, &states)?)
    };

    let compensation = match &after {
        None => vec![StoryEvent::StoryDeleted {
            at: now.clone(),
            reason: "undone".to_string(),
        }],
        Some(after) => compensate(&before, after, &now)?,
    };

    if compensation.is_empty() {
        return Ok(Vec::new());
    }

    // The relation half has to reach the *other* story's history too, or the
    // undo would leave exactly the one-sided edge the schema and the doctor
    // exist to prevent. `RelationService` owns that rule; this defers to it
    // rather than reimplementing it.
    let (story_events, relation_events): (Vec<_>, Vec<_>) = compensation
        .iter()
        .cloned()
        .partition(|event| !is_relation(event));

    ctx.store().write(|tx| {
        if !story_events.is_empty() {
            append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Any,
                &story_events,
                ctx.provenance(),
            )?;
        }
        for event in &relation_events {
            let (other, relation, add) = relation_parts(event);
            let other_no = StoryNo::parse_id(&prefix, &other)?;
            let inverse = crate::domain::inverse_relation(&relation).ok_or_else(|| {
                AppError::Validation(format!("`{relation}` has no inverse to undo"))
            })?;
            let mirrored = if add {
                StoryEvent::StoryRelationshipAdded {
                    at: now.clone(),
                    other_id: public.clone(),
                    relation: inverse.to_string(),
                }
            } else {
                StoryEvent::StoryRelationshipRemoved {
                    at: now.clone(),
                    other_id: public.clone(),
                    relation: inverse.to_string(),
                }
            };
            append_and_fold(
                tx,
                project,
                story_no,
                &prefix,
                &states,
                ExpectedSeq::Any,
                std::slice::from_ref(event),
                ctx.provenance(),
            )?;
            append_and_fold(
                tx,
                project,
                other_no,
                &prefix,
                &states,
                ExpectedSeq::Any,
                &[mirrored],
                ctx.provenance(),
            )?;
        }
        Ok(())
    })?;

    Ok(compensation)
}

/// Whether an event is one of the two relation events.
fn is_relation(event: &StoryEvent) -> bool {
    matches!(
        event,
        StoryEvent::StoryRelationshipAdded { .. } | StoryEvent::StoryRelationshipRemoved { .. }
    )
}

/// `(other_id, relation, is_an_addition)` for a relation event.
fn relation_parts(event: &StoryEvent) -> (String, String, bool) {
    match event {
        StoryEvent::StoryRelationshipAdded {
            other_id, relation, ..
        } => (other_id.clone(), relation.clone(), true),
        StoryEvent::StoryRelationshipRemoved {
            other_id, relation, ..
        } => (other_id.clone(), relation.clone(), false),
        _ => unreachable!("relation_parts is only called on relation events"),
    }
}

/// The events that carry `before` to `after`.
///
/// Ordered field by field rather than by any notion of what the user did:
/// the fold is a function of the whole log, so what matters is that the result
/// is right, not that the events mirror the originals one for one.
fn compensate(
    before: &StorySnapshot,
    after: &StorySnapshot,
    now: &str,
) -> Result<Vec<StoryEvent>, AppError> {
    let mut events = Vec::new();
    let at = now.to_string();

    if before.title != after.title {
        events.push(StoryEvent::StoryTitleSet {
            at: at.clone(),
            title: after.title.clone(),
        });
    }
    if before.description != after.description {
        // An absent description is expressed as an empty one: there is no
        // "description cleared" event, and an empty string renders the same
        // everywhere a description is shown.
        events.push(StoryEvent::StoryDescriptionSet {
            at: at.clone(),
            description: after.description.clone().unwrap_or_default(),
        });
    }
    // Gated on *both* halves, because they move independently (SH-359). The
    // legacy case that needs it is invisible to a priority-only gate: undoing
    // a pre-SH-449 story's first-ever `story prioritize` targets `priority: none,
    // assessed: false`, and the story currently reads `priority: none,
    // assessed: true` — the levels are equal, so a gate keyed on `priority`
    // alone never fires and the restored story keeps claiming somebody
    // assessed it.
    if before.priority != after.priority || before.priority_assessed != after.priority_assessed {
        // `!after.priority_assessed` implies `after.priority == None` — the
        // invariant `fold_story` asserts on the way out — so one event
        // restores both halves and no third case exists.
        events.push(if after.priority_assessed {
            StoryEvent::StoryPrioritySet {
                at: at.clone(),
                priority: after.priority.clone(),
            }
        } else {
            StoryEvent::StoryPriorityCleared { at: at.clone() }
        });
    }
    if before.labels != after.labels {
        // Normalized rather than restored verbatim: `after` can be a snapshot
        // folded from history that predates SH-164's guard, and undo
        // reproducing a comma-bearing label would recreate exactly the
        // unaddressable state the guard exists to refuse.
        events.push(StoryEvent::StoryLabelsSet {
            at: at.clone(),
            labels: normalize_labels(&after.labels),
        });
    }
    if before.assignee != after.assignee {
        events.push(match &after.assignee {
            Some(member) => StoryEvent::StoryAssigned {
                at: at.clone(),
                member_id: member.clone(),
            },
            None => StoryEvent::StoryAssigneeCleared { at: at.clone() },
        });
    }
    if before.awaiting != after.awaiting {
        events.push(match &after.awaiting {
            Some(reason) => StoryEvent::StoryAwaitingSet {
                at: at.clone(),
                awaiting: reason.clone(),
            },
            None => StoryEvent::StoryAwaitingCleared { at: at.clone() },
        });
    }
    if before.story_type != after.story_type {
        match &after.story_type {
            Some(story_type) => events.push(StoryEvent::StoryTypeSet {
                at: at.clone(),
                story_type: story_type.clone(),
            }),
            // No clearing event exists, and no TUI path sets a type, so this is
            // unreachable from the undo stack. It refuses rather than silently
            // leaving the type behind, because a partial undo that reports
            // success is worse than one that says what it could not do.
            None => {
                return Err(AppError::Validation(
                    "cannot undo a story's type back to unset: there is no event that clears it"
                        .to_string(),
                ));
            }
        }
    }
    // State last among the scalar fields: moving into an OPEN state is what
    // *reopening* is, and it retracts the closure markers — so it has to be the
    // event that lands, not one an earlier field edit precedes.
    if before.state != after.state {
        events.push(StoryEvent::StoryStateChanged {
            at: at.clone(),
            state: after.state.clone(),
        });
    }

    for comment in &before.comments {
        if !after.comments.contains(comment) {
            events.push(StoryEvent::StoryCommentRetracted {
                at: at.clone(),
                comment_at: comment.at.clone(),
                text: comment.text.clone(),
            });
        }
    }
    for comment in &after.comments {
        if !before.comments.contains(comment) {
            events.push(StoryEvent::StoryCommentAdded {
                at: at.clone(),
                text: comment.text.clone(),
            });
        }
    }

    for relation in &before.relationships {
        if !after.relationships.contains(relation) {
            events.push(StoryEvent::StoryRelationshipRemoved {
                at: at.clone(),
                other_id: relation.other_id.clone(),
                relation: relation.relation.clone(),
            });
        }
    }
    for relation in &after.relationships {
        if !before.relationships.contains(relation) {
            events.push(StoryEvent::StoryRelationshipAdded {
                at: at.clone(),
                other_id: relation.other_id.clone(),
                relation: relation.relation.clone(),
            });
        }
    }

    // A deletion that the target does not have is undone by the state change
    // above, because `fold_story` retracts the closure markers on a move into
    // an OPEN state. The reverse — the target *is* deleted and the story is not
    // — needs saying explicitly.
    if after.deleted && !before.deleted {
        events.push(StoryEvent::StoryDeleted {
            at: at.clone(),
            reason: after
                .deleted_reason
                .clone()
                .unwrap_or_else(|| "undone".to_string()),
        });
    }

    Ok(events)
}
