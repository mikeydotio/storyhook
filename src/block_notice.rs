//! The nudge every `awaiting`-setting door raises when a prose reason names a
//! blocker nothing records as one (SH-398).
//!
//! `story block <id> "<reason>"` writes free text into `awaiting`; a
//! `blocked-by` edge is a fact the store can cite, clears itself the moment
//! the blocker closes, and shows up wherever a relationship does. Prose does
//! neither — SH-397 named as the reason SH-394 was blocked, and there was no
//! edge to clear when SH-397 closed, which is the defect this story reports.
//!
//! Not a refusal: `story block` runs non-interactively from agents and from
//! the daemon, where there is no terminal to ask at, so the door stays open
//! and the nudge rides on the response the same way
//! [`crate::priority_notice`]'s does.
//!
//! Split the same way [`crate::priority_notice`] is, for the same reason:
//! [`unlinked_mentions`] is the one place that reads the store (generic over
//! [`Store`], so every caller shares it rather than re-deriving the check),
//! and [`warning`] is pure text with no dependency on either — the split
//! `tests/unassessed_priority_paths.rs`-style path fence in
//! `tests/block_notice_paths.rs` relies on: it can call the text function
//! directly with a hand-built mention list, with no store to stand up.
//!
//! `pub(crate)`: nothing outside this crate has business with these
//! functions, and `tests/dead_public_surface.rs` only scans `pub ` items —
//! keeping this `pub(crate)` is what keeps that scan meaningful here.

use std::collections::BTreeSet;

use crate::domain::{SuperState, ids_in_line};
use crate::service::{Ctx, project_prefix, resolve_story};
use crate::store::{ProjectId, ReadOps, Store, StoreError};

/// The transaction-scoped core of [`unlinked_mentions`] — every story id
/// mentioned in `awaiting` that this project can resolve, is still `OPEN`, is
/// not `subject_id` itself, and has no `blocked-by` edge recorded from
/// `subject_id` onto it. Sorted and deduplicated.
///
/// Split out so [`crate::service::integrity`]'s `story doctor` sweep — which
/// already holds a read transaction over every story in the project — can
/// call this directly rather than opening a second, nested one per story;
/// [`unlinked_mentions`] is the single-call convenience wrapper everywhere
/// else uses.
pub(crate) fn unlinked_mentions_tx(
    tx: &impl ReadOps,
    project: ProjectId,
    subject_id: &str,
    awaiting: &str,
    relationships: &[crate::domain::StoryRelation],
) -> Result<Vec<String>, StoreError> {
    let prefix = project_prefix(tx, project)?;
    let mut found = BTreeSet::new();
    for (start, end) in ids_in_line(&prefix, awaiting) {
        let candidate = &awaiting[start..end];
        if candidate == subject_id {
            continue;
        }
        if relationships
            .iter()
            .any(|r| r.relation == "blocked-by" && r.other_id == candidate)
        {
            continue;
        }
        let open = resolve_story(tx, project, &prefix, candidate)
            .map(|(_, row)| row.snapshot.superstate == SuperState::Open)
            .unwrap_or(false);
        if open {
            found.insert(candidate.to_string());
        }
    }
    Ok(found.into_iter().collect())
}

/// Every story id mentioned in `awaiting` that this project can resolve, is
/// still `OPEN`, is not `subject_id` itself, and has no `blocked-by` edge
/// recorded from `subject_id` onto it — in other words, every mention that
/// reads as a blocker but is not recorded as one.
///
/// One read transaction regardless of how many ids `awaiting` mentions.
/// A store error here is swallowed rather than propagated: by the time this
/// runs, the caller's real write has already committed, and a failure to
/// compute an advisory nudge must never be reported as a failure of the
/// command that asked for it.
pub(crate) fn unlinked_mentions<S: Store>(
    ctx: &Ctx<'_, S>,
    subject_id: &str,
    awaiting: &str,
    relationships: &[crate::domain::StoryRelation],
) -> Vec<String> {
    ctx.store()
        .read(|tx| unlinked_mentions_tx(tx, ctx.project(), subject_id, awaiting, relationships))
        .unwrap_or_default()
}

/// The clause every text below ends with — one copy, the way
/// [`crate::priority_notice::GUIDANCE`] is.
const REMEDY: &str =
    "story relate <id> blocked-by <blocker>` (or `story block <id> --on <blocker> \"<reason>\"`)";

/// What fires when `mentioned` is non-empty: `subject_id`'s reason named a
/// real, open, unlinked story. `None` when there is nothing to say.
///
/// Deliberately does not claim which mention is *the* blocker — a reason can
/// mention a story for other reasons (a related story, prior context), and
/// this only ever knows that nothing records a `blocked-by` edge, not that
/// one belongs.
pub(crate) fn warning(subject_id: &str, mentioned: &[String]) -> Option<String> {
    if mentioned.is_empty() {
        return None;
    }
    let names = mentioned.join(", ");
    Some(format!(
        "{subject_id}'s reason names {names}, but nothing records {names} as its blocker — a \
         prose mention does not clear itself when a named story closes, unlike a `blocked-by` \
         edge. If one of them is the blocker: `{}.",
        REMEDY.replace("<id>", subject_id)
    ))
}

/// Convenience for a call site that has `awaiting` as an `Option` — `None`
/// (nothing was set) and an empty [`unlinked_mentions`] both mean nothing to
/// say.
pub(crate) fn warnings<S: Store>(
    ctx: &Ctx<'_, S>,
    subject_id: &str,
    awaiting: Option<&str>,
    relationships: &[crate::domain::StoryRelation],
) -> Vec<String> {
    let Some(awaiting) = awaiting else {
        return Vec::new();
    };
    let mentioned = unlinked_mentions(ctx, subject_id, awaiting, relationships);
    warning(subject_id, &mentioned).into_iter().collect()
}

/// What bare `story unblock <id>` says when clearing `awaiting` leaves the
/// story blocked anyway — open `blocked-by`/`obviated-by` edges do not clear
/// with the prose reason, so reporting plain success would be the SH-312
/// "comforting falsehood" shape: an unblock that leaves the story blocked,
/// described as though it worked.
pub(crate) fn still_blocked_warning(
    id: &str,
    relationships: &[crate::domain::StoryRelation],
) -> Option<String> {
    let mut causes: Vec<String> = relationships
        .iter()
        .filter(|r| r.relation == "blocked-by" || r.relation == "obviated-by")
        .map(|r| format!("{} {}", r.relation, r.other_id))
        .collect();
    if causes.is_empty() {
        return None;
    }
    causes.sort();
    Some(format!(
        "{id} is still blocked: {}. Clearing the reason did not clear these — remove them with \
         `story unblock {id} --on <blocker>` or `story unrelate {id} <relation> <other>`.",
        causes.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::StoryRelation;

    #[test]
    fn no_mentions_means_no_warning() {
        assert_eq!(warning("SH-1", &[]), None);
    }

    #[test]
    fn the_warning_names_the_subject_and_the_mention_and_the_remedy() {
        let text = warning("SH-394", &["SH-397".to_string()]).expect("a warning");
        assert!(text.contains("SH-394"));
        assert!(text.contains("SH-397"));
        assert!(text.contains("story relate SH-394 blocked-by <blocker>"));
        assert!(text.contains("story block SH-394 --on <blocker>"));
    }

    #[test]
    fn multiple_mentions_are_all_named() {
        let text = warning("SH-1", &["SH-2".to_string(), "SH-3".to_string()]).expect("a warning");
        assert!(text.contains("SH-2, SH-3"));
    }

    #[test]
    fn no_edges_means_no_still_blocked_warning() {
        assert_eq!(still_blocked_warning("SH-1", &[]), None);
    }

    #[test]
    fn an_open_blocker_still_warns_after_unblock() {
        let relationships = vec![StoryRelation {
            relation: "blocked-by".to_string(),
            other_id: "SH-2".to_string(),
        }];
        let text = still_blocked_warning("SH-1", &relationships).expect("a warning");
        assert!(text.contains("SH-1"));
        assert!(text.contains("blocked-by SH-2"));
    }

    #[test]
    fn an_obviator_also_warns() {
        let relationships = vec![StoryRelation {
            relation: "obviated-by".to_string(),
            other_id: "SH-2".to_string(),
        }];
        let text = still_blocked_warning("SH-1", &relationships).expect("a warning");
        assert!(text.contains("obviated-by SH-2"));
    }

    #[test]
    fn an_unrelated_relation_does_not_warn() {
        let relationships = vec![StoryRelation {
            relation: "relates-to".to_string(),
            other_id: "SH-2".to_string(),
        }];
        assert_eq!(still_blocked_warning("SH-1", &relationships), None);
    }
}
