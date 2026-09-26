//! The blocker floor (SH-788): the more urgent level a story sorts at while it
//! blocks more urgent open work.
//!
//! A dependency is a scheduling fact, not a severity claim, so no relationship
//! ever changes the level a story *stores*. But a low story that blocks a
//! critical one must not wait behind unrelated medium work: the critical story
//! cannot start until it is done. The floor is that scheduling fact, derived
//! from the live graph every time it is read — no event, no store column, no
//! snapshot field — so it ends by itself the moment the blockage does.
//!
//! The rule, in full:
//!
//! - Only OPEN stories take part. A closed story neither lends nor receives.
//! - A level travels along the `blocked-by` side of an edge — the side
//!   [`super::is_blocked`] reads — from the waiting story to its blocker. No
//!   other relation carries one.
//! - A **source** lends its own level: open, published and not flagged by an
//!   `obviated-by` edge. A draft or a flagged story still blocks work, so it
//!   relays a level it receives, but its own level is not yet a claim on the
//!   queue.
//! - An epic closes only when its children close, so a *blocking* epic hands
//!   every level it receives to each open child, at every depth. An epic's
//!   own level never reaches its children: `parent-of` carries nothing.
//! - A floor is kept only where it is strictly more urgent than the story's
//!   own level. An equal level raises nothing and nothing is ever lowered.

use std::collections::{BTreeMap, VecDeque};

use super::{Priority, StoryIndex, StorySnapshot, SuperState, is_epic};

/// Every open story's blocker floor within one story set, where it raises the
/// story above its own level.
///
/// Computed once per read by [`Self::compute`]; cheap enough (linear in the
/// graph) that nothing caches it across writes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockerFloors {
    floors: BTreeMap<String, Priority>,
}

impl BlockerFloors {
    /// Derives every floor in `index`.
    ///
    /// A worklist fixed point: each source is visited once for its own level,
    /// and a story is revisited only when the level it has received becomes
    /// more urgent. There are five levels, so no story improves more than four
    /// times, the cost stays linear in stories plus edges, and a cycle ends.
    #[must_use]
    pub fn compute(index: &impl StoryIndex) -> Self {
        let mut received: BTreeMap<&str, Priority> = BTreeMap::new();
        let mut pending: VecDeque<&StorySnapshot> =
            index.stories().filter(|s| is_source(s)).collect();

        while let Some(story) = pending.pop_front() {
            let relayed = received.get(story.id.as_str()).cloned();
            let own = is_source(story).then(|| story.priority.clone());
            // `None` claims no urgency, so passing it on could raise nothing.
            let Some(level) = own
                .into_iter()
                .chain(relayed.clone())
                .min()
                .filter(|level| *level != Priority::None)
            else {
                continue;
            };
            for blocker in open_related(story, "blocked-by", index) {
                receive(&mut received, &mut pending, blocker, &level);
            }
            if let Some(relayed) = relayed.filter(|_| is_epic(story)) {
                for child in open_related(story, "parent-of", index) {
                    receive(&mut received, &mut pending, child, &relayed);
                }
            }
        }

        let floors = received
            .into_iter()
            .filter_map(|(id, floor)| {
                let story = index.story(id)?;
                (floor < story.priority).then(|| (id.to_string(), floor))
            })
            .collect();
        Self { floors }
    }

    /// The floor raising `story`, if one does.
    ///
    /// `None` both when nothing more urgent waits on the story and when the
    /// most urgent level that waits is no more urgent than its own.
    #[must_use]
    pub fn floor(&self, story: &StorySnapshot) -> Option<&Priority> {
        self.floors
            .get(&story.id)
            .filter(|floor| **floor < story.priority)
    }

    /// The level `story` sorts at: its floor where one raises it, otherwise
    /// its own level.
    #[must_use]
    pub fn effective(&self, story: &StorySnapshot) -> Priority {
        self.floor(story)
            .cloned()
            .unwrap_or_else(|| story.priority.clone())
    }
}

/// Hands `level` to `story`, queueing it to pass the level on if that is more
/// urgent than anything it has received so far.
fn receive<'a>(
    received: &mut BTreeMap<&'a str, Priority>,
    pending: &mut VecDeque<&'a StorySnapshot>,
    story: &'a StorySnapshot,
    level: &Priority,
) {
    let current = received.get(story.id.as_str());
    if current.is_none_or(|current| level < current) {
        received.insert(story.id.as_str(), level.clone());
        pending.push_back(story);
    }
}

/// Whether `story` lends its own level to the stories it waits on.
fn is_source(story: &StorySnapshot) -> bool {
    is_open(story)
        && !story.draft
        && !story
            .relationships
            .iter()
            .any(|relation| relation.relation == "obviated-by")
}

fn is_open(story: &StorySnapshot) -> bool {
    story.superstate == SuperState::Open
}

/// The open stories `story` names through `relation`, skipping any id the
/// index does not carry.
fn open_related<'a>(
    story: &StorySnapshot,
    relation: &str,
    index: &'a impl StoryIndex,
) -> impl Iterator<Item = &'a StorySnapshot> {
    story
        .relationships
        .iter()
        .filter(move |edge| edge.relation == relation)
        .filter_map(|edge| index.story(&edge.other_id))
        .filter(|other| is_open(other))
}

#[cfg(test)]
mod tests_support {
    use crate::domain::{Priority, StorySnapshot, SuperState};

    /// An open, published, parentless story with no relationships.
    pub(super) fn story(id: &str, priority: Priority) -> StorySnapshot {
        StorySnapshot {
            id: id.to_string(),
            title: id.to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            state: "todo".to_string(),
            state_computed: false,
            superstate: SuperState::Open,
            awaiting: None,
            comments: Vec::new(),
            referenced_by_commits: Vec::new(),
            relationships: Vec::new(),
            priority_assessed: priority != Priority::None,
            complexity: Default::default(),
            complexity_assessed: false,
            priority,
            labels: Vec::new(),
            story_type: None,
            description: None,
            closed_at: None,
            hidden_at: None,
            draft: false,
            attachments: Vec::new(),
            next_attachment_id: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::BlockerFloors;
    use super::tests_support::story;
    use crate::domain::{Priority, StoryRelation, StorySnapshot, SuperState};

    type Stories = BTreeMap<String, StorySnapshot>;

    fn stories(entries: &[(&str, Priority)]) -> Stories {
        entries
            .iter()
            .map(|(id, priority)| (id.to_string(), story(id, priority.clone())))
            .collect()
    }

    fn edge(stories: &mut Stories, id: &str, relation: &str, other: &str) {
        stories
            .get_mut(id)
            .expect("a fixture story")
            .relationships
            .push(StoryRelation {
                relation: relation.to_string(),
                other_id: other.to_string(),
            });
    }

    /// Records `blocker` blocks `dependent` on both ends, as the store does.
    fn blocks(stories: &mut Stories, blocker: &str, dependent: &str) {
        edge(stories, blocker, "blocks", dependent);
        edge(stories, dependent, "blocked-by", blocker);
    }

    /// Records `parent` parent-of `child` on both ends, as the store does.
    fn parent_of(stories: &mut Stories, parent: &str, child: &str) {
        edge(stories, parent, "parent-of", child);
        edge(stories, child, "child-of", parent);
    }

    fn epic(stories: &mut Stories, id: &str) {
        stories.get_mut(id).expect("a fixture story").story_type = Some("epic".to_string());
    }

    fn close(stories: &mut Stories, id: &str) {
        let story = stories.get_mut(id).expect("a fixture story");
        story.state = "done".to_string();
        story.superstate = SuperState::Closed;
    }

    /// Every raised story and its floor, in id order.
    fn floors(stories: &Stories) -> Vec<(String, Priority)> {
        let floors = BlockerFloors::compute(stories);
        stories
            .values()
            .filter_map(|story| {
                floors
                    .floor(story)
                    .map(|floor| (story.id.clone(), floor.clone()))
            })
            .collect()
    }

    fn raised(entries: &[(&str, Priority)]) -> Vec<(String, Priority)> {
        entries
            .iter()
            .map(|(id, priority)| (id.to_string(), priority.clone()))
            .collect()
    }

    #[test]
    fn no_edges_raise_nothing() {
        let set = stories(&[("SH-1", Priority::Low), ("SH-2", Priority::Critical)]);
        assert_eq!(floors(&set), raised(&[]));
    }

    #[test]
    fn a_blocker_takes_the_level_of_the_more_urgent_story_it_blocks() {
        let mut set = stories(&[("SH-1", Priority::Low), ("SH-2", Priority::Critical)]);
        blocks(&mut set, "SH-1", "SH-2");
        assert_eq!(floors(&set), raised(&[("SH-1", Priority::Critical)]));
        let computed = BlockerFloors::compute(&set);
        assert_eq!(computed.effective(&set["SH-1"]), Priority::Critical);
        assert_eq!(computed.effective(&set["SH-2"]), Priority::Critical);
    }

    #[test]
    fn an_equal_level_raises_nothing() {
        let mut set = stories(&[("SH-1", Priority::High), ("SH-2", Priority::High)]);
        blocks(&mut set, "SH-1", "SH-2");
        assert_eq!(floors(&set), raised(&[]));
    }

    #[test]
    fn a_blocker_more_urgent_than_its_dependent_is_never_lowered() {
        let mut set = stories(&[("SH-1", Priority::Critical), ("SH-2", Priority::Low)]);
        blocks(&mut set, "SH-1", "SH-2");
        assert_eq!(floors(&set), raised(&[]));
        let computed = BlockerFloors::compute(&set);
        assert_eq!(computed.effective(&set["SH-1"]), Priority::Critical);
        assert_eq!(computed.effective(&set["SH-2"]), Priority::Low);
    }

    #[test]
    fn a_chain_carries_the_level_to_its_root() {
        let mut set = stories(&[
            ("SH-1", Priority::Low),
            ("SH-2", Priority::Medium),
            ("SH-3", Priority::Critical),
        ]);
        blocks(&mut set, "SH-1", "SH-2");
        blocks(&mut set, "SH-2", "SH-3");
        assert_eq!(
            floors(&set),
            raised(&[("SH-1", Priority::Critical), ("SH-2", Priority::Critical)])
        );
    }

    #[test]
    fn the_most_urgent_of_several_dependents_wins() {
        let mut set = stories(&[
            ("SH-1", Priority::Low),
            ("SH-2", Priority::Medium),
            ("SH-3", Priority::High),
            ("SH-4", Priority::Low),
        ]);
        for dependent in ["SH-2", "SH-3", "SH-4"] {
            blocks(&mut set, "SH-1", dependent);
        }
        assert_eq!(floors(&set), raised(&[("SH-1", Priority::High)]));
    }

    #[test]
    fn a_diamond_raises_every_path_once() {
        let mut set = stories(&[
            ("SH-1", Priority::Low),
            ("SH-2", Priority::High),
            ("SH-3", Priority::Medium),
            ("SH-4", Priority::Critical),
        ]);
        blocks(&mut set, "SH-1", "SH-2");
        blocks(&mut set, "SH-1", "SH-3");
        blocks(&mut set, "SH-2", "SH-4");
        blocks(&mut set, "SH-3", "SH-4");
        assert_eq!(
            floors(&set),
            raised(&[
                ("SH-1", Priority::Critical),
                ("SH-2", Priority::Critical),
                ("SH-3", Priority::Critical),
            ])
        );
    }

    #[test]
    fn a_cycle_settles_on_its_most_urgent_member() {
        let mut set = stories(&[("SH-1", Priority::Low), ("SH-2", Priority::High)]);
        blocks(&mut set, "SH-1", "SH-2");
        blocks(&mut set, "SH-2", "SH-1");
        assert_eq!(floors(&set), raised(&[("SH-1", Priority::High)]));
    }

    #[test]
    fn a_cycle_fed_from_outside_carries_the_outside_level_around() {
        let mut set = stories(&[
            ("SH-1", Priority::Low),
            ("SH-2", Priority::Medium),
            ("SH-3", Priority::Critical),
        ]);
        blocks(&mut set, "SH-1", "SH-2");
        blocks(&mut set, "SH-2", "SH-1");
        blocks(&mut set, "SH-2", "SH-3");
        assert_eq!(
            floors(&set),
            raised(&[("SH-1", Priority::Critical), ("SH-2", Priority::Critical)])
        );
    }

    #[test]
    fn a_closed_dependent_lends_nothing_even_with_a_lingering_edge() {
        let mut set = stories(&[("SH-1", Priority::Low), ("SH-2", Priority::Critical)]);
        blocks(&mut set, "SH-1", "SH-2");
        close(&mut set, "SH-2");
        assert_eq!(floors(&set), raised(&[]));
    }

    #[test]
    fn a_closed_blocker_is_never_raised() {
        // SH-207 lets an open story record `blocked-by` onto a story that is
        // already closed; readiness ignores that edge, and so does the floor.
        let mut set = stories(&[("SH-1", Priority::Low), ("SH-2", Priority::Critical)]);
        blocks(&mut set, "SH-1", "SH-2");
        close(&mut set, "SH-1");
        assert_eq!(floors(&set), raised(&[]));
    }

    #[test]
    fn a_closed_story_does_not_relay_a_chain() {
        let mut set = stories(&[
            ("SH-1", Priority::Low),
            ("SH-2", Priority::Low),
            ("SH-3", Priority::Critical),
        ]);
        blocks(&mut set, "SH-1", "SH-2");
        blocks(&mut set, "SH-2", "SH-3");
        close(&mut set, "SH-2");
        assert_eq!(floors(&set), raised(&[]));
    }

    #[test]
    fn a_draft_dependent_lends_nothing() {
        let mut set = stories(&[("SH-1", Priority::Low), ("SH-2", Priority::Critical)]);
        blocks(&mut set, "SH-1", "SH-2");
        set.get_mut("SH-2").unwrap().draft = true;
        assert_eq!(floors(&set), raised(&[]));
    }

    #[test]
    fn a_draft_blocker_is_raised_and_relays_the_level() {
        let mut set = stories(&[
            ("SH-1", Priority::Low),
            ("SH-2", Priority::Low),
            ("SH-3", Priority::Critical),
        ]);
        blocks(&mut set, "SH-1", "SH-2");
        blocks(&mut set, "SH-2", "SH-3");
        set.get_mut("SH-2").unwrap().draft = true;
        assert_eq!(
            floors(&set),
            raised(&[("SH-1", Priority::Critical), ("SH-2", Priority::Critical)])
        );
    }

    #[test]
    fn an_obviation_flagged_story_relays_but_lends_nothing_of_its_own() {
        let mut set = stories(&[
            ("SH-1", Priority::Low),
            ("SH-2", Priority::High),
            ("SH-3", Priority::Critical),
            ("SH-4", Priority::Low),
            ("SH-5", Priority::Low),
        ]);
        // SH-2 is flagged: its own `high` reaches nobody.
        edge(&mut set, "SH-2", "obviated-by", "SH-5");
        blocks(&mut set, "SH-1", "SH-2");
        assert_eq!(floors(&set), raised(&[]));
        // A critical dependent's level still travels through it.
        blocks(&mut set, "SH-2", "SH-3");
        blocks(&mut set, "SH-4", "SH-2");
        assert_eq!(
            floors(&set),
            raised(&[
                ("SH-1", Priority::Critical),
                ("SH-2", Priority::Critical),
                ("SH-4", Priority::Critical),
            ])
        );
    }

    #[test]
    fn a_blocking_epic_hands_the_level_to_every_open_descendant() {
        let mut set = stories(&[
            ("SH-1", Priority::Low),
            ("SH-2", Priority::Low),
            ("SH-3", Priority::Low),
            ("SH-4", Priority::Low),
            ("SH-5", Priority::Low),
            ("SH-6", Priority::Low),
            ("SH-9", Priority::Critical),
        ]);
        epic(&mut set, "SH-1");
        epic(&mut set, "SH-3");
        parent_of(&mut set, "SH-1", "SH-2");
        parent_of(&mut set, "SH-1", "SH-3");
        parent_of(&mut set, "SH-3", "SH-4");
        parent_of(&mut set, "SH-1", "SH-5");
        close(&mut set, "SH-5");
        // A child's own blocker is on the same path.
        blocks(&mut set, "SH-6", "SH-4");
        blocks(&mut set, "SH-1", "SH-9");
        assert_eq!(
            floors(&set),
            raised(&[
                ("SH-1", Priority::Critical),
                ("SH-2", Priority::Critical),
                ("SH-3", Priority::Critical),
                ("SH-4", Priority::Critical),
                ("SH-6", Priority::Critical),
            ])
        );
    }

    #[test]
    fn an_epic_own_level_never_reaches_its_children() {
        let mut set = stories(&[
            ("SH-1", Priority::Critical),
            ("SH-2", Priority::Low),
            ("SH-3", Priority::Low),
        ]);
        epic(&mut set, "SH-1");
        parent_of(&mut set, "SH-1", "SH-2");
        // The epic waits on SH-3: SH-3 takes the epic's level, SH-2 does not.
        blocks(&mut set, "SH-3", "SH-1");
        assert_eq!(floors(&set), raised(&[("SH-3", Priority::Critical)]));
    }

    #[test]
    fn a_story_with_children_that_is_not_an_epic_does_not_relay_to_them() {
        let mut set = stories(&[
            ("SH-1", Priority::Low),
            ("SH-2", Priority::Low),
            ("SH-3", Priority::Critical),
        ]);
        parent_of(&mut set, "SH-1", "SH-2");
        blocks(&mut set, "SH-1", "SH-3");
        assert_eq!(floors(&set), raised(&[("SH-1", Priority::Critical)]));
    }

    #[test]
    fn a_damaged_hierarchy_cycle_under_a_blocking_epic_terminates() {
        let mut set = stories(&[
            ("SH-1", Priority::Low),
            ("SH-2", Priority::Low),
            ("SH-3", Priority::Critical),
        ]);
        epic(&mut set, "SH-1");
        epic(&mut set, "SH-2");
        parent_of(&mut set, "SH-1", "SH-2");
        parent_of(&mut set, "SH-2", "SH-1");
        blocks(&mut set, "SH-1", "SH-3");
        assert_eq!(
            floors(&set),
            raised(&[("SH-1", Priority::Critical), ("SH-2", Priority::Critical)])
        );
    }

    #[test]
    fn no_other_relation_carries_a_level() {
        for relation in [
            "relates-to",
            "parent-of",
            "child-of",
            "obviates",
            "duplicate-of",
            "blocks",
        ] {
            let mut set = stories(&[("SH-1", Priority::Low), ("SH-2", Priority::Critical)]);
            // Only the dependent's `blocked-by` entry carries a level, so a
            // lone `blocks` entry on the critical story is inert too.
            edge(&mut set, "SH-2", relation, "SH-1");
            assert_eq!(floors(&set), raised(&[]), "`{relation}` carried a level");
        }
    }

    #[test]
    fn an_edge_to_a_story_outside_the_index_is_ignored() {
        let mut set = stories(&[("SH-2", Priority::Critical)]);
        edge(&mut set, "SH-2", "blocked-by", "SH-404");
        assert_eq!(floors(&set), raised(&[]));
    }

    #[test]
    fn a_none_level_lends_nothing_and_a_none_blocker_is_raised() {
        let mut set = stories(&[
            ("SH-1", Priority::Low),
            ("SH-2", Priority::None),
            ("SH-3", Priority::None),
            ("SH-4", Priority::Low),
        ]);
        blocks(&mut set, "SH-1", "SH-2");
        blocks(&mut set, "SH-3", "SH-4");
        assert_eq!(floors(&set), raised(&[("SH-3", Priority::Low)]));
    }

    #[test]
    fn effective_never_reports_a_floor_below_a_changed_own_level() {
        let mut set = stories(&[("SH-1", Priority::Low), ("SH-2", Priority::High)]);
        blocks(&mut set, "SH-1", "SH-2");
        let computed = BlockerFloors::compute(&set);
        // The story's own level became critical after the floors were read.
        let mut changed = set["SH-1"].clone();
        changed.priority = Priority::Critical;
        assert_eq!(computed.floor(&changed), None);
        assert_eq!(computed.effective(&changed), Priority::Critical);
    }

    #[test]
    fn a_borrowed_index_gives_the_same_floors() {
        let mut set = stories(&[("SH-1", Priority::Low), ("SH-2", Priority::Critical)]);
        blocks(&mut set, "SH-1", "SH-2");
        let borrowed: BTreeMap<&str, &StorySnapshot> = set
            .values()
            .map(|story| (story.id.as_str(), story))
            .collect();
        assert_eq!(
            BlockerFloors::compute(&borrowed),
            BlockerFloors::compute(&set)
        );
    }
}

#[cfg(test)]
mod properties {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    use proptest::prelude::*;

    use super::BlockerFloors;
    use crate::domain::{Priority, StoryRelation, StorySnapshot, SuperState};

    const IDS: [&str; 6] = ["SH-1", "SH-2", "SH-3", "SH-4", "SH-5", "SH-6"];

    fn priority_at(index: u8) -> Priority {
        match index % 5 {
            0 => Priority::Critical,
            1 => Priority::High,
            2 => Priority::Medium,
            3 => Priority::Low,
            _ => Priority::None,
        }
    }

    /// One fixture story: `kind` 0 open, 1 closed, 2 draft, 3 obviation-flagged.
    fn node(id: &str, priority: Priority, kind: u8) -> StorySnapshot {
        let mut story = super::tests_support::story(id, priority);
        match kind % 4 {
            1 => {
                story.state = "done".to_string();
                story.superstate = SuperState::Closed;
            }
            2 => story.draft = true,
            3 => story.relationships.push(StoryRelation {
                relation: "obviated-by".to_string(),
                other_id: "SH-99".to_string(),
            }),
            _ => {}
        }
        story
    }

    /// The rule stated as reachability, independently of the worklist: a
    /// story's floor is the most urgent own level of any source it can be
    /// reached from along open `blocked-by` edges.
    fn reference(stories: &BTreeMap<String, StorySnapshot>) -> BTreeMap<String, Priority> {
        let open = |id: &str| {
            stories
                .get(id)
                .is_some_and(|story| story.superstate == SuperState::Open)
        };
        let mut floors = BTreeMap::new();
        for source in stories.values().filter(|story| super::is_source(story)) {
            let mut seen = BTreeSet::new();
            let mut queue = VecDeque::from([source.id.as_str()]);
            while let Some(id) = queue.pop_front() {
                for edge in &stories[id].relationships {
                    if edge.relation == "blocked-by"
                        && open(&edge.other_id)
                        && seen.insert(edge.other_id.as_str())
                    {
                        queue.push_back(edge.other_id.as_str());
                    }
                }
            }
            for reached in seen {
                let floor = floors.entry(reached.to_string()).or_insert(Priority::None);
                if source.priority < *floor {
                    *floor = source.priority.clone();
                }
            }
        }
        floors
            .into_iter()
            .filter(|(id, floor)| *floor < stories[id].priority)
            .collect()
    }

    proptest! {
        #[test]
        fn floors_match_reachability_and_never_invert_an_edge(
            priorities in prop::collection::vec(0u8..5, IDS.len()),
            kinds in prop::collection::vec(0u8..4, IDS.len()),
            edges in prop::collection::vec((0usize..6, 0usize..6), 0..12),
        ) {
            let mut stories: BTreeMap<String, StorySnapshot> = IDS
                .iter()
                .zip(priorities.iter().zip(kinds.iter()))
                .map(|(id, (&priority, &kind))| {
                    (id.to_string(), node(id, priority_at(priority), kind))
                })
                .collect();
            for (dependent, blocker) in edges {
                if dependent == blocker {
                    continue;
                }
                let dependent = stories.get_mut(IDS[dependent]).unwrap();
                dependent.relationships.push(StoryRelation {
                    relation: "blocked-by".to_string(),
                    other_id: IDS[blocker].to_string(),
                });
            }

            let computed = BlockerFloors::compute(&stories);
            let actual: BTreeMap<String, Priority> = stories
                .values()
                .filter_map(|story| {
                    computed.floor(story).map(|floor| (story.id.clone(), floor.clone()))
                })
                .collect();
            prop_assert_eq!(&actual, &reference(&stories));

            for story in stories.values() {
                prop_assert!(computed.effective(story) <= story.priority);
                if !super::is_source(story) {
                    continue;
                }
                for edge in story.relationships.iter().filter(|e| e.relation == "blocked-by") {
                    let blocker = &stories[&edge.other_id];
                    if blocker.superstate == SuperState::Open {
                        prop_assert!(
                            computed.effective(blocker) <= computed.effective(story),
                            "{} blocks {} yet sorts later", blocker.id, story.id
                        );
                    }
                }
            }
        }
    }
}
