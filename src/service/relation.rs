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
                return Err(AppError::Validation(format!(
                    "story `{b}` is closed and cannot be modified"
                ))
                .into());
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
