//! The text every creation path raises when a story lands unassessed (SH-358).
//!
//! `story new` has warned about this since SH-354, and its origin fix landed in
//! SH-359: [`crate::domain::StorySnapshot::priority_assessed`] is a fold-derived
//! fact, so any door that just created a story can ask the store whether anyone
//! assessed it rather than peeking at its own `Option<String>` before the move
//! into the service layer.
//!
//! This module exists because `story new` is no longer the only caller.
//! `src/github/mod.rs` needs the same text and cannot depend on `crate::invoke`
//! without a backwards edge (`invoke` already depends on the service layer
//! `github` builds on), and the batch paths (`import`, `decompose`) need a
//! second, aggregate shape the single-story warning never had. One module, one
//! copy of the guidance clause every text ends with — the alternative is what
//! SH-136 cost this project three times and SH-198 ten.
//!
//! `pub(crate)`: nothing outside this crate has business with these strings,
//! and `tests/dead_public_surface.rs` only scans `pub ` items for dead code —
//! keeping this `pub(crate)` is what keeps that scan meaningful here.

/// The clause every text below ends with: where the criteria are, and the
/// remedy for one story. Isolated so it exists in exactly one place — the
/// single copy `tests/priority_rubric.rs` and this module's own tests hold
/// each other to.
const GUIDANCE: &str = "run `story help priority-rubric` and then `story prioritize <id> <level>`";

/// What `story new` says when the story it just created carries nobody's
/// assessment (SH-354, text settled by SH-359).
///
/// Not a refusal and not a prompt. `story new` is the most-used command in the
/// tool, so refusing would break every scripted caller; and a prompt is useless
/// exactly where the defect happens, since `dispatch` runs inside the daemon
/// with no terminal to ask at and agents create stories non-interactively.
///
/// Careful to be *true*: `none` sorts last in `story next`, it is not excluded
/// from it, and this text does not claim the story was parked — only that
/// nobody said otherwise, which is what [`StorySnapshot::priority_assessed`]
/// answers.
///
/// [`StorySnapshot::priority_assessed`]: crate::domain::StorySnapshot::priority_assessed
pub(crate) fn unassessed_warning(id: &str) -> String {
    format!(
        "priority not set: nobody has assessed {id}, so it sorts last in \
         `story next` — alongside stories deliberately parked at `none`. If that \
         is not what you meant, {}.",
        GUIDANCE.replace("<id>", id)
    )
}

/// What a batch creator (`story import`, `story decompose`) says when some of
/// what it just filed landed unassessed.
///
/// One line regardless of batch size — the story's own constraint, so that a
/// spec of forty stories does not emit forty lines. Names up to ten ids and
/// then a count of the rest; exact attribution for a larger batch lives on
/// each affected [`StoryView`](crate::output::StoryView)'s own `warnings`,
/// which [`unassessed_warning`] populates per story.
///
/// The remedy this one names is `story list --unassessed` rather than a
/// `story prioritize` per id — SH-359 shipped that filter for exactly this
/// question, and naming forty commands would be worse than naming none.
pub(crate) fn unassessed_batch_warning(ids: &[String], total: usize) -> String {
    const NAMED: usize = 10;
    let mut named: String = ids
        .iter()
        .take(NAMED)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if ids.len() > NAMED {
        named.push_str(&format!(" (and {} more)", ids.len() - NAMED));
    }
    format!(
        "priority not set on {} of {total} {}: {named}. Each sorts last in \
         `story next` until it is. Run `story list --unassessed` to see them all, \
         then `story prioritize <id> <level>` — the criteria are at \
         `story help priority-rubric`.",
        ids.len(),
        if total == 1 { "story" } else { "stories" },
    )
}

/// What a batch importer says when an entry named a priority the domain
/// cannot parse, rather than silently dropping it.
///
/// The story is still imported — that leniency predates this warning
/// (`service::transfer::import_events`'s own doc names it) and this module
/// does not reverse it. What ends is the silence: before this, an unparseable
/// slug and an omitted one produced the identical, indistinguishable result.
pub(crate) fn unparseable_priority_warning(rejected: &[(String, String)]) -> String {
    let named = rejected
        .iter()
        .map(|(id, raw)| format!("{id} (`{raw}`)"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "priority not recognised and dropped, filed unassessed instead: {named}. Valid levels: \
         critical, high, medium, low, none — see `story help priority-rubric`."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_single_story_warning_names_the_story_and_the_remedy() {
        let text = unassessed_warning("SH-1");
        assert!(text.contains("SH-1"));
        assert!(text.contains("story next"));
        assert!(text.contains("story help priority-rubric"));
        assert!(text.contains("story prioritize SH-1 <level>"));
        // It must not claim a decision was made (SH-359's own correction).
        assert!(!text.contains("is filed at `none`, which means \"deliberately parked\""));
    }

    #[test]
    fn the_batch_warning_names_a_count_never_every_id_past_ten() {
        let ids: Vec<String> = (1..=12).map(|n| format!("SH-{n}")).collect();
        let text = unassessed_batch_warning(&ids, 20);
        assert!(text.contains("12 of 20"));
        assert!(text.contains("SH-1,"));
        assert!(text.contains("SH-10"));
        assert!(!text.contains("SH-11"));
        assert!(text.contains("and 2 more"));
        assert!(text.contains("story list --unassessed"));
    }

    #[test]
    fn a_small_batch_names_every_id_with_no_and_more_clause() {
        let ids = vec!["SH-1".to_string(), "SH-2".to_string()];
        let text = unassessed_batch_warning(&ids, 5);
        assert!(text.contains("SH-1, SH-2"));
        assert!(!text.contains("more"));
    }

    #[test]
    fn the_unparseable_warning_names_the_rejected_words() {
        let text = unparseable_priority_warning(&[("SH-2".to_string(), "urgent".to_string())]);
        assert!(text.contains("SH-2"));
        assert!(text.contains("`urgent`"));
        assert!(text.contains("filed unassessed"));
    }
}
