//! Everything [`super::ready_order`] reads, resolved once over one story set.

use super::{BlockerFloors, Priority, StoryIndex, StorySnapshot, is_epic};

/// A story index paired with the [`BlockerFloors`] derived from that same
/// index.
///
/// One constructor builds both, so no caller can rank stories with lookups
/// from one story set and floors from another — the same invalid pairing the
/// TUI's `Readiness` exists to rule out for readiness.
#[derive(Clone, Debug)]
pub struct ReadyRanking<'a, I: StoryIndex> {
    index: &'a I,
    floors: BlockerFloors,
}

impl<'a, I: StoryIndex> ReadyRanking<'a, I> {
    /// Derives the floors of every story in `index`.
    #[must_use]
    pub fn new(index: &'a I) -> Self {
        Self {
            floors: BlockerFloors::compute(index),
            index,
        }
    }

    /// The index this ranking reads.
    #[must_use]
    pub fn index(&self) -> &'a I {
        self.index
    }

    /// The floors derived from [`Self::index`].
    #[must_use]
    pub fn floors(&self) -> &BlockerFloors {
        &self.floors
    }

    /// The level `story` sorts at: its blocker floor where one raises it,
    /// otherwise its own level.
    #[must_use]
    pub fn effective(&self, story: &StorySnapshot) -> Priority {
        self.floors.effective(story)
    }

    /// The effective level of the nearest parent epic, for ready-order ties.
    ///
    /// Only a `child-of` target that is actually an EPIC confers its level
    /// (SH-499): urgency is inherited from the initiative a story belongs to,
    /// and a normal story that happens to have children is not one. Multiple
    /// parents are legal; the most urgent of those equally-near parents wins,
    /// so membership in a critical epic cannot be masked by simultaneous
    /// membership in a less urgent one.
    ///
    /// A parentless story uses its own effective level, which neither lifts
    /// nor demotes independent work. Its *stored* level would demote a
    /// floored story: a low story sorting at critical would lose every
    /// critical tie on the second key (SH-788).
    #[must_use]
    pub fn parent_epic_priority(&self, story: &StorySnapshot) -> Priority {
        story
            .relationships
            .iter()
            .filter(|relation| relation.relation == "child-of")
            .filter_map(|relation| self.index.story(&relation.other_id))
            .filter(|parent| is_epic(parent))
            .map(|parent| self.effective(parent))
            .min()
            .unwrap_or_else(|| self.effective(story))
    }
}
