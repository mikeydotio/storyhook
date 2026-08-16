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
}
