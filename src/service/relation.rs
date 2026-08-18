//! Relations between stories, written from both ends or from neither.
//!
//! A relation is one fact asserted twice: `SH-1 blocks SH-2` and `SH-2
//! blocked-by SH-1` are the same edge seen from either side, and each end
//! records it in its own event log. The legacy path appended to the first
//! story, then appended to the second — two writes, no transaction, no
//! recovery. Anything that went wrong in between left half an edge, and half
//! an edge survives indefinitely because nothing ever looks: that is SH-60,
//! fifteen live violations in this repository's own tracker.
//!
//! Here both ends' events, both folds, and both read-model rows land in one
//! transaction. The store keeps the relations *table* symmetric by
//! construction; what this module guarantees is the thing the store cannot,
//! which is that both *histories* agree.
//!
//! [`RelationService::relate`]'s `a` is the story the command is about, and
//! stays under [`super::resolve_open_story`]'s full closed-story guard, same
//! as every other write. `b` is a target, not a subject: a still-open story
//! is allowed to record a relationship onto a closed one (SH-207) — for every
//! kind except `parent-of`/`child-of`, where a closed epic gaining a new open
//! child would move that epic's own displayed progress after it already
//! closed, so `b` stays guarded in exactly the role that would cause that.
//! Removal never grows a closed story's scope, so it has no such exception:
//! `unrelate` relaxes `b` uniformly, for every kind and every role.

use crate::domain::{
    StoryEvent, StoryRelation, StorySnapshot, relation_edges, would_create_parent_cycle,
};
use crate::error::AppError;
use crate::event_hooks::HookEventType;
use crate::store::{ExpectedSeq, ReadOps, Store};

use super::{Ctx, append_and_fold, project_prefix, query, resolve_open_story, resolve_story};

/// What [`RelationService::relate`] did.
#[derive(Clone, Debug)]
pub enum RelationOutcome {
    /// The edge changed; carries the first story as it now reads.
    Changed(Box<StorySnapshot>),
    /// Nothing to do — the relation was already in the requested state.
    Unchanged {
        /// `true` when the caller asked for a removal.
        remove: bool,
    },
}

/// Adding and removing relations between two stories of one project.
pub struct RelationService<'ctx, S: Store> {
    ctx: &'ctx Ctx<'ctx, S>,
}

impl<'ctx, S: Store> RelationService<'ctx, S> {
    /// A relation service bound to `ctx`.
    pub fn new(ctx: &'ctx Ctx<'ctx, S>) -> Self {
        Self { ctx }
    }

    /// Marks `id` as blocked by every story in `blockers`, and/or by a prose
    /// `awaiting` reason — SH-398's origin fix, generalising [`Self::relate`]
    /// from one target to N so a blocker that is a story can be recorded as a
    /// `blocked-by` edge (cited, and self-clearing when the blocker closes)
    /// in the SAME transaction as the reason, rather than as prose alone that
    /// never clears itself.
    ///
    /// `awaiting`, when `Some`, commits in the subject's own append —
    /// alongside every new `blocked-by` edge, never as a second, non-atomic
    /// write. That is the reason this exists rather than a loop of
    /// [`Self::relate`] calls plus a separate `set_awaiting`: the half-edge
    /// hazard this module's own doc names is exactly "blocked by A and B"
    /// landing with only A actually recorded, or the edges landing without
    /// the reason that explained them.
    ///
    /// Duplicate ids in `blockers` are collapsed before anything is read, so
    /// `--on SH-B --on SH-B` cannot double either edge's event. A blocker
    /// already recorded from an earlier call is left alone — this call
    /// writes only what is missing, on both ends independently, which also
    /// self-heals a half-edge inherited from before this existed.
    ///
    /// # Errors
    ///
    /// [`AppError::NotFound`] if `id` or any blocker does not resolve;
    /// [`AppError::Validation`] if a blocker names `id` itself, or if
    /// `awaiting` is present but blank after trimming.
    pub fn block_on(
        &self,
        id: &str,
        blockers: &[String],
        awaiting: Option<&str>,
    ) -> Result<(), AppError> {
        self.edge_batch(id, blockers, awaiting, false)
    }

    /// Removes `id`'s `blocked-by` edge onto every story in `blockers`, in one
    /// transaction — the inverse of [`Self::block_on`]. Never touches
    /// `awaiting`; bare `story unblock <id>` (no blockers named) is
    /// [`crate::service::story::StoryService::clear_awaiting`] instead, which
    /// owns the prose reason.
    ///
    /// # Errors
    ///
    /// [`AppError::NotFound`] if `id` or any blocker does not resolve;
    /// [`AppError::Validation`] if a blocker names `id` itself.
    pub fn unblock_from(&self, id: &str, blockers: &[String]) -> Result<(), AppError> {
        self.edge_batch(id, blockers, None, true)
    }

    /// Shared plumbing for [`Self::block_on`]/[`Self::unblock_from`]: batches
    /// every named blocker's own inverse-edge event and the subject's own
    /// `blocked-by` events (plus, when adding, an optional `awaiting` event)
    /// into one transaction — each blocker takes exactly one append (its own
    /// edge), and the subject takes exactly one append carrying everything
    /// that changes about it, mirroring [`Self::relate`]'s own per-story
    /// batching generalised from one target to N.
    fn edge_batch(
        &self,
        id: &str,
        blockers: &[String],
        awaiting: Option<&str>,
        remove: bool,
    ) -> Result<(), AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();

        let mut seen = std::collections::BTreeSet::new();
        let blockers: Vec<&String> = blockers
            .iter()
            .filter(|b| seen.insert((*b).clone()))
            .collect();

        let mut touched: Vec<String> = Vec::new();

        self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            let (a_no, a_row) = resolve_open_story(&*tx, project, &prefix, id)?;

            let mut b_rows = Vec::with_capacity(blockers.len());
            for b in &blockers {
                if b.as_str() == id {
                    return Err(AppError::Validation(
                        "stories cannot relate to themselves".to_string(),
                    )
                    .into());
                }
                let (b_no, b_row) = resolve_story(&*tx, project, &prefix, b.as_str())?;
                b_rows.push((*b, b_no, b_row));
            }

            let states = tx.state_map(project)?;

            for (b, b_no, b_row) in &b_rows {
                let has = has_relation(&b_row.snapshot, "blocks", id);
                let event = if remove {
                    has.then(|| StoryEvent::StoryRelationshipRemoved {
                        at: now.clone(),
                        other_id: id.to_string(),
                        relation: "blocks".to_string(),
                    })
                } else {
                    (!has).then(|| StoryEvent::StoryRelationshipAdded {
                        at: now.clone(),
                        other_id: id.to_string(),
                        relation: "blocks".to_string(),
                    })
                };
                if let Some(event) = event {
                    append_and_fold(
                        tx,
                        project,
                        *b_no,
                        &prefix,
                        &states,
                        ExpectedSeq::Exact(b_row.head_seq),
                        &[event],
                        self.ctx.provenance(),
                    )?;
                    touched.push((*b).clone());
                }
            }

            let mut a_events = Vec::new();
            for (b, _, _) in &b_rows {
                let has = has_relation(&a_row.snapshot, "blocked-by", b.as_str());
                let event = if remove {
                    has.then(|| StoryEvent::StoryRelationshipRemoved {
                        at: now.clone(),
                        other_id: (*b).clone(),
                        relation: "blocked-by".to_string(),
                    })
                } else {
                    (!has).then(|| StoryEvent::StoryRelationshipAdded {
                        at: now.clone(),
                        other_id: (*b).clone(),
                        relation: "blocked-by".to_string(),
                    })
                };
                if let Some(event) = event {
                    a_events.push(event);
                }
            }
            if let Some(reason) = awaiting {
                let reason = reason.trim().to_string();
                if reason.is_empty() {
                    return Err(AppError::Validation(
                        "awaiting reason must not be empty".to_string(),
                    )
                    .into());
                }
                a_events.push(StoryEvent::StoryAwaitingSet {
                    at: now.clone(),
                    awaiting: reason,
                });
            }
            if !a_events.is_empty() {
                append_and_fold(
                    tx,
                    project,
                    a_no,
                    &prefix,
                    &states,
                    ExpectedSeq::Exact(a_row.head_seq),
                    &a_events,
                    self.ctx.provenance(),
                )?;
            }
            Ok(())
        })?;

        for other_id in &touched {
            self.ctx.fire_hook(
                HookEventType::RelationshipChange,
                &serde_json::json!({
                    "event_type": "relationship_change",
                    "story_id": id,
                    "timestamp": self.ctx.now(),
                    "action": if remove { "removed" } else { "added" },
                    "relation": "blocked-by",
                    "other_id": other_id,
                }),
            );
        }
        Ok(())
    }

    /// Adds or removes `relation` between `a` and `b`, updating both stories in
    /// one transaction.
    ///
    /// Both ends are appended to, folded, and written back before the
    /// transaction commits, so there is no interval in which one story claims
    /// an edge the other does not. A rejected edge — a second parent, a cycle —
    /// rolls both halves back together.
    pub fn relate(
        &self,
        a: &str,
        relation: &str,
        b: &str,
        remove: bool,
    ) -> Result<RelationOutcome, AppError> {
        let now = self.ctx.now();
        let project = self.ctx.project();

        let outcome = self.ctx.store().write(|tx| {
            let prefix = project_prefix(&*tx, project)?;
            // Existence is checked before the self-relation rule, in this
            // order, because that is the order the errors have always come in:
            // `story relate SH-9 relates-to SH-9` on a project with no SH-9
            // reports the missing story, not the loop.
            let (a_no, a_row) = resolve_open_story(&*tx, project, &prefix, a)?;
            // `b` is the target, not the story the command is about: guarding
            // it exactly like `a` blocked a living story from ever recording
            // a relationship to one that has closed (SH-207) — the
            // asymmetric-relation failure this module exists to prevent,
            // reached from the opposite direction. So `b`'s existence is
            // resolved unconditionally here; whether it must also be open
            // depends on the role it is about to take, decided below once
            // `edges` is known.
            let (b_no, b_row) = resolve_story(&*tx, project, &prefix, b)?;
            if a == b {
                return Err(AppError::Validation(
                    "stories cannot relate to themselves".to_string(),
                )
                .into());
            }

            let edges = relation_edges(relation).ok_or_else(|| {
                AppError::Validation(format!("unsupported relationship `{relation}`"))
            })?;

            // `parent-of`/`child-of` is the one pair where relaxing `b`
            // unconditionally is not safe: a closed epic's displayed progress
            // rollup (`compute_progress`) is recomputed from every
            // `parent-of` edge with no guard on the epic's own superstate, so
            // letting a closed epic gain a NEW open child would change that
            // epic's own numbers after it already closed. Attaching a closed
            // story as a child of a still-open epic carries no such risk.
            // What decides it is the role `b` is about to take, not which
            // literal the caller typed first — read off `edges` rather than
            // the input string, since `relation_edges` assigns "parent" by
            // verb, so both phrasings of "attach a new child to a closed
            // epic" hit this the same way. Removal never grows a closed
            // story's scope, so `remove` always skips this guard, mirroring
            // `resolve_open_story`'s exemption for every other relation kind.
            if !remove
                && b_row.archived
                && edges
                    .iter()
                    .any(|(_, b_relation)| *b_relation == "parent-of")
            {
                return Err(AppError::Validation(super::closed_story_refusal(b)).into());
            }

            if !remove {
                let stories = query::story_map(&*tx, project)?;
                validate_parent_constraints(
                    &stories,
                    a,
                    b,
                    relation,
                    &a_row.snapshot,
                    &b_row.snapshot,
                )?;
            }

            let mut a_events = Vec::new();
            let mut b_events = Vec::new();
            for (a_relation, b_relation) in edges {
                let a_has = has_relation(&a_row.snapshot, a_relation, b);
                let b_has = has_relation(&b_row.snapshot, b_relation, a);
                if remove {
                    if a_has {
                        a_events.push(StoryEvent::StoryRelationshipRemoved {
                            at: now.clone(),
                            other_id: b.to_string(),
                            relation: a_relation.to_string(),
                        });
                    }
                    if b_has {
                        b_events.push(StoryEvent::StoryRelationshipRemoved {
                            at: now.clone(),
                            other_id: a.to_string(),
                            relation: b_relation.to_string(),
                        });
                    }
                } else {
                    if !a_has {
                        a_events.push(StoryEvent::StoryRelationshipAdded {
                            at: now.clone(),
                            other_id: b.to_string(),
                            relation: a_relation.to_string(),
                        });
                    }
                    if !b_has {
                        b_events.push(StoryEvent::StoryRelationshipAdded {
                            at: now.clone(),
                            other_id: a.to_string(),
                            relation: b_relation.to_string(),
                        });
                    }
                }
            }

            if a_events.is_empty() && b_events.is_empty() {
                return Ok(RelationOutcome::Unchanged { remove });
            }

            let states = tx.state_map(project)?;
            // `b` first, so that the snapshot returned for `a` is folded after
            // both halves exist. Either order commits atomically; this one
            // means the answer the caller renders is the final state rather
            // than an intermediate one.
            for (story, events, head) in [
                (b_no, &b_events, b_row.head_seq),
                (a_no, &a_events, a_row.head_seq),
            ] {
                if events.is_empty() {
                    continue;
                }
                append_and_fold(
                    tx,
                    project,
                    story,
                    &prefix,
                    &states,
                    ExpectedSeq::Exact(head),
                    events,
                    self.ctx.provenance(),
                )?;
            }

            // Read `a` back rather than reusing the pre-write snapshot: when
            // only `b` needed an event, `a` already asserted its half and was
            // never appended to, so its row is the answer either way.
            let stored_a = tx
                .story(project, a_no)?
                .ok_or_else(|| AppError::NotFound(format!("story `{a}` not found")))?;
            Ok(RelationOutcome::Changed(Box::new(stored_a.snapshot)))
        })?;

        if let RelationOutcome::Changed(snapshot) = &outcome {
            self.ctx.fire_hook(
                HookEventType::RelationshipChange,
                &serde_json::json!({
                    "event_type": "relationship_change",
                    "story_id": a,
                    "timestamp": self.ctx.now(),
                    "story_title": &snapshot.title,
                    "action": if remove { "removed" } else { "added" },
                    "relation": relation,
                    "other_id": b,
                }),
            );
        }
        Ok(outcome)
    }
}

/// Whether a story's snapshot already asserts this edge.
fn has_relation(story: &StorySnapshot, relation: &str, other_id: &str) -> bool {
    story
        .relationships
        .iter()
        .any(|candidate| candidate.relation == relation && candidate.other_id == other_id)
}

/// The two rules that make the parent hierarchy a tree: at most one parent per
/// story, and no cycles.
fn validate_parent_constraints(
    stories: &std::collections::BTreeMap<String, StorySnapshot>,
    a: &str,
    b: &str,
    relation: &str,
    a_story: &StorySnapshot,
    b_story: &StorySnapshot,
) -> Result<(), AppError> {
    let check: Option<(&StorySnapshot, &str)> = match relation {
        "parent-of" => Some((b_story, a)),
        "child-of" => Some((a_story, b)),
        _ => None,
    };

    if let Some((story, expected_parent)) = check {
        let has_other_parent = story
            .relationships
            .iter()
            .filter(|candidate: &&StoryRelation| candidate.relation == "child-of")
            .any(|candidate| candidate.other_id != expected_parent);
        if has_other_parent {
            return Err(AppError::Validation(format!(
                "story `{}` already has a different parent",
                story.id
            )));
        }
    }

    match relation {
        "parent-of" if would_create_parent_cycle(stories, a, b) => Err(AppError::Validation(
            format!("adding `parent-of` from `{a}` to `{b}` would create a cycle"),
        )),
        "child-of" if would_create_parent_cycle(stories, b, a) => Err(AppError::Validation(
            format!("adding `child-of` from `{a}` to `{b}` would create a cycle"),
        )),
        _ => Ok(()),
    }
}
