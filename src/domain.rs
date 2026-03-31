use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::AppError;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportStory {
    pub title: String,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub relationships: Option<Vec<ImportRelationship>>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportRelationship {
    pub relation: String,
    #[serde(default)]
    pub ref_index: Option<usize>,
    #[serde(default)]
    pub other_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Critical,
    High,
    Medium,
    Low,
    #[default]
    None,
}

impl Priority {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "critical" => Some(Self::Critical),
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SuperState {
    Open,
    Closed,
}

impl SuperState {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.to_ascii_uppercase().as_str() {
            "OPEN" => Some(Self::Open),
            "CLOSED" => Some(Self::Closed),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "OPEN",
            Self::Closed => "CLOSED",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateDef {
    pub slug: String,
    #[serde(rename = "super")]
    pub super_state: SuperState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Member {
    pub id: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoryComment {
    pub at: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct StoryRelation {
    pub relation: String,
    pub other_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StorySnapshot {
    pub id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub state: String,
    pub superstate: SuperState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default)]
    pub awaiting: Option<String>,
    #[serde(default)]
    pub comments: Vec<StoryComment>,
    #[serde(default)]
    pub relationships: Vec<StoryRelation>,
    #[serde(default)]
    pub priority: Priority,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum StoryEvent {
    StoryCreated {
        at: String,
        title: String,
        state: String,
    },
    StoryCommentAdded {
        at: String,
        text: String,
    },
    StoryAssigned {
        at: String,
        member_id: String,
    },
    StoryAwaitingSet {
        at: String,
        awaiting: String,
    },
    StoryAwaitingCleared {
        at: String,
    },
    StoryStateChanged {
        at: String,
        state: String,
    },
    StoryRelationshipAdded {
        at: String,
        other_id: String,
        relation: String,
    },
    StoryRelationshipRemoved {
        at: String,
        other_id: String,
        relation: String,
    },
    StoryPrioritySet {
        at: String,
        priority: Priority,
    },
    StoryLabelsSet {
        at: String,
        labels: Vec<String>,
    },
    StoryTitleSet {
        at: String,
        title: String,
    },
    StoryClosedAndArchived {
        at: String,
        state: String,
    },
}

pub fn last_activity_type(events: &[StoryEvent]) -> &'static str {
    events
        .last()
        .map(|event| match event {
            StoryEvent::StoryCreated { .. } => "created",
            StoryEvent::StoryCommentAdded { .. } => "comment",
            StoryEvent::StoryAssigned { .. } => "assigned",
            StoryEvent::StoryAwaitingSet { .. } => "awaiting-set",
            StoryEvent::StoryAwaitingCleared { .. } => "awaiting-cleared",
            StoryEvent::StoryStateChanged { .. } => "state-change",
            StoryEvent::StoryRelationshipAdded { .. } => "relationship-added",
            StoryEvent::StoryRelationshipRemoved { .. } => "relationship-removed",
            StoryEvent::StoryPrioritySet { .. } => "priority-set",
            StoryEvent::StoryLabelsSet { .. } => "labels-set",
            StoryEvent::StoryTitleSet { .. } => "title-set",
            StoryEvent::StoryClosedAndArchived { .. } => "archived",
        })
        .unwrap_or("unknown")
}

pub fn validate_state_defs(states: &[StateDef]) -> Result<(), AppError> {
    let has_open = states
        .iter()
        .any(|state| state.super_state == SuperState::Open);
    let has_closed = states
        .iter()
        .any(|state| state.super_state == SuperState::Closed);

    if has_open && has_closed {
        Ok(())
    } else {
        Err(AppError::Validation(
            "state set must include at least one OPEN state and one CLOSED state".to_string(),
        ))
    }
}

pub fn fold_story(
    id: &str,
    events: &[StoryEvent],
    states: &BTreeMap<String, StateDef>,
) -> Result<StorySnapshot, AppError> {
    let mut title = None;
    let mut created_at = None;
    let mut updated_at = None;
    let mut state = None;
    let mut assignee = None;
    let mut awaiting = None;
    let mut priority = Priority::None;
    let mut labels = Vec::new();
    let mut comments = Vec::new();
    let mut relationships = BTreeSet::new();
    let mut closed_at = None;

    for event in events {
        match event {
            StoryEvent::StoryCreated {
                at,
                title: story_title,
                state: story_state,
            } => {
                title = Some(story_title.clone());
                created_at = Some(at.clone());
                updated_at = Some(at.clone());
                state = Some(story_state.clone());
            }
            StoryEvent::StoryCommentAdded { at, text } => {
                comments.push(StoryComment {
                    at: at.clone(),
                    text: text.clone(),
                });
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryAssigned { at, member_id } => {
                assignee = Some(member_id.clone());
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryAwaitingSet {
                at,
                awaiting: blocked_on,
            } => {
                awaiting = Some(blocked_on.clone());
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryAwaitingCleared { at } => {
                awaiting = None;
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryStateChanged {
                at,
                state: story_state,
            } => {
                state = Some(story_state.clone());
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryPrioritySet {
                at,
                priority: new_priority,
            } => {
                priority = new_priority.clone();
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryLabelsSet {
                at,
                labels: new_labels,
            } => {
                labels = new_labels.clone();
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryTitleSet {
                at,
                title: new_title,
            } => {
                title = Some(new_title.clone());
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryRelationshipAdded {
                at,
                other_id,
                relation,
            } => {
                relationships.insert(StoryRelation {
                    relation: relation.clone(),
                    other_id: other_id.clone(),
                });
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryRelationshipRemoved {
                at,
                other_id,
                relation,
            } => {
                relationships.remove(&StoryRelation {
                    relation: relation.clone(),
                    other_id: other_id.clone(),
                });
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryClosedAndArchived {
                at,
                state: story_state,
            } => {
                closed_at = Some(at.clone());
                state = Some(story_state.clone());
                updated_at = Some(at.clone());
            }
        }
    }

    let state = state.ok_or_else(|| AppError::Integrity(format!("story {id} is missing state")))?;
    let title = title.ok_or_else(|| AppError::Integrity(format!("story {id} is missing title")))?;
    let created_at = created_at
        .ok_or_else(|| AppError::Integrity(format!("story {id} is missing created_at")))?;
    let updated_at = updated_at.unwrap_or_else(|| created_at.clone());
    let superstate = states
        .get(&state)
        .ok_or_else(|| {
            AppError::Validation(format!("story {id} references undefined state `{state}`"))
        })?
        .super_state
        .clone();

    Ok(StorySnapshot {
        id: id.to_string(),
        title,
        created_at,
        updated_at,
        state,
        superstate,
        assignee,
        awaiting,
        priority,
        labels,
        comments,
        relationships: relationships.into_iter().collect(),
        closed_at,
    })
}

pub fn is_relation_input(raw: &str) -> bool {
    relation_edges(raw).is_some()
}

pub fn relation_edges(input: &str) -> Option<Vec<(&'static str, &'static str)>> {
    match input {
        "relates-to" | "related-to" => Some(vec![("relates-to", "relates-to")]),
        "blocks" => Some(vec![("blocks", "blocked-by")]),
        "blocked-by" => Some(vec![("blocked-by", "blocks")]),
        "parent-of" => Some(vec![("parent-of", "child-of")]),
        "child-of" => Some(vec![("child-of", "parent-of")]),
        "duplicate-of" => Some(vec![("duplicate-of", "duplicate-of")]),
        "obviates" => Some(vec![("obviates", "obviated-by")]),
        "obviated-by" => Some(vec![("obviated-by", "obviates")]),
        _ => None,
    }
}

pub fn inverse_relation(relation: &str) -> Option<&'static str> {
    match relation {
        "relates-to" => Some("relates-to"),
        "blocks" => Some("blocked-by"),
        "blocked-by" => Some("blocks"),
        "parent-of" => Some("child-of"),
        "child-of" => Some("parent-of"),
        "duplicate-of" => Some("duplicate-of"),
        "obviates" => Some("obviated-by"),
        "obviated-by" => Some("obviates"),
        _ => None,
    }
}

pub fn is_mutual_relation(relation: &str) -> bool {
    matches!(
        relation,
        "relates-to" | "duplicate-of"
    )
}

pub fn compute_integrity_issues(
    stories: &BTreeMap<String, StorySnapshot>,
) -> BTreeMap<String, Vec<String>> {
    let mut issues: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let graph = HierarchyGraph::from_stories(stories);

    for story in stories.values() {
        let parent_count = graph.parents_of(&story.id).len();

        if parent_count > 1 {
            issues
                .entry(story.id.clone())
                .or_default()
                .push("story has multiple parents".to_string());
        }

        for relation in &story.relationships {
            let Some(other_story) = stories.get(&relation.other_id) else {
                issues.entry(story.id.clone()).or_default().push(format!(
                    "dangling relation `{}` to missing story `{}`",
                    relation.relation, relation.other_id
                ));
                continue;
            };

            if let Some(expected_inverse) = inverse_relation(&relation.relation) {
                let has_inverse = other_story.relationships.iter().any(|candidate| {
                    candidate.other_id == story.id && candidate.relation == expected_inverse
                });

                if !has_inverse {
                    let issue = if is_mutual_relation(&relation.relation) {
                        format!(
                            "missing reciprocal relation `{}` on story `{}`",
                            relation.relation, relation.other_id
                        )
                    } else {
                        format!(
                            "missing inverse relation `{}` on story `{}`",
                            expected_inverse, relation.other_id
                        )
                    };
                    issues.entry(story.id.clone()).or_default().push(issue);
                }
            }
        }
    }

    for node in graph.cycle_nodes() {
        issues
            .entry(node)
            .or_default()
            .push("parent/child cycle detected".to_string());
    }

    issues
}

pub fn is_ready(story: &StorySnapshot, all_stories: &BTreeMap<String, StorySnapshot>) -> bool {
    if story.superstate != SuperState::Open {
        return false;
    }
    if story.awaiting.is_some() {
        return false;
    }
    if story
        .relationships
        .iter()
        .any(|r| r.relation == "obviated-by")
    {
        return false;
    }
    for relation in &story.relationships {
        if relation.relation == "blocked-by"
            && let Some(other) = all_stories.get(&relation.other_id)
            && other.superstate == SuperState::Open
        {
            return false;
        }
    }
    true
}

pub fn would_create_parent_cycle(
    stories: &BTreeMap<String, StorySnapshot>,
    parent_id: &str,
    child_id: &str,
) -> bool {
    let graph = HierarchyGraph::from_stories(stories);
    graph.would_create_cycle(parent_id, child_id)
}

pub fn derive_family_relationships(
    stories: &BTreeMap<String, StorySnapshot>,
) -> BTreeMap<String, Vec<StoryRelation>> {
    let graph = HierarchyGraph::from_stories(stories);
    let cycle_nodes = graph.cycle_nodes();
    let mut derived = BTreeMap::new();

    for story_id in stories.keys() {
        if cycle_nodes.contains(story_id) {
            derived.insert(story_id.clone(), Vec::new());
            continue;
        }

        let direct_children = graph.children_of(story_id);
        let direct_parents = graph.parents_of(story_id);
        let descendants = graph.transitive_descendants(story_id, &cycle_nodes);
        let ancestors = graph.transitive_ancestors(story_id, &cycle_nodes);
        let mut relationships = Vec::new();

        for descendant in descendants.difference(&direct_children) {
            relationships.push(StoryRelation {
                relation: "ancestor-of".to_string(),
                other_id: descendant.clone(),
            });
        }

        for ancestor in ancestors.difference(&direct_parents) {
            relationships.push(StoryRelation {
                relation: "descendent-of".to_string(),
                other_id: ancestor.clone(),
            });
        }

        derived.insert(story_id.clone(), relationships);
    }

    derived
}

#[derive(Clone, Debug)]
struct HierarchyGraph {
    children_by_parent: BTreeMap<String, BTreeSet<String>>,
    parents_by_child: BTreeMap<String, BTreeSet<String>>,
}

impl HierarchyGraph {
    fn from_stories(stories: &BTreeMap<String, StorySnapshot>) -> Self {
        let mut children_by_parent = stories
            .keys()
            .cloned()
            .map(|story_id| (story_id, BTreeSet::new()))
            .collect::<BTreeMap<_, _>>();
        let mut parents_by_child = stories
            .keys()
            .cloned()
            .map(|story_id| (story_id, BTreeSet::new()))
            .collect::<BTreeMap<_, _>>();

        for story in stories.values() {
            for relation in &story.relationships {
                match relation.relation.as_str() {
                    "parent-of" if stories.contains_key(&relation.other_id) => {
                        children_by_parent
                            .entry(story.id.clone())
                            .or_default()
                            .insert(relation.other_id.clone());
                        parents_by_child
                            .entry(relation.other_id.clone())
                            .or_default()
                            .insert(story.id.clone());
                    }
                    "child-of" if stories.contains_key(&relation.other_id) => {
                        children_by_parent
                            .entry(relation.other_id.clone())
                            .or_default()
                            .insert(story.id.clone());
                        parents_by_child
                            .entry(story.id.clone())
                            .or_default()
                            .insert(relation.other_id.clone());
                    }
                    _ => {}
                }
            }
        }

        Self {
            children_by_parent,
            parents_by_child,
        }
    }

    fn children_of(&self, story_id: &str) -> BTreeSet<String> {
        self.children_by_parent
            .get(story_id)
            .cloned()
            .unwrap_or_default()
    }

    fn parents_of(&self, story_id: &str) -> BTreeSet<String> {
        self.parents_by_child
            .get(story_id)
            .cloned()
            .unwrap_or_default()
    }

    fn would_create_cycle(&self, parent_id: &str, child_id: &str) -> bool {
        if parent_id == child_id {
            return true;
        }

        let mut stack = vec![child_id.to_string()];
        let mut visited = BTreeSet::new();

        while let Some(current) = stack.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if current == parent_id {
                return true;
            }
            if let Some(children) = self.children_by_parent.get(&current) {
                for child in children {
                    stack.push(child.clone());
                }
            }
        }

        false
    }

    fn cycle_nodes(&self) -> BTreeSet<String> {
        let mut visited = BTreeSet::new();
        let mut visiting = BTreeSet::new();
        let mut stack = Vec::new();
        let mut cycle_nodes = BTreeSet::new();

        for node in self.children_by_parent.keys() {
            self.visit_cycle_nodes(
                node,
                &mut visited,
                &mut visiting,
                &mut stack,
                &mut cycle_nodes,
            );
        }

        cycle_nodes
    }

    fn visit_cycle_nodes(
        &self,
        node: &str,
        visited: &mut BTreeSet<String>,
        visiting: &mut BTreeSet<String>,
        stack: &mut Vec<String>,
        cycle_nodes: &mut BTreeSet<String>,
    ) {
        if visited.contains(node) {
            return;
        }

        visited.insert(node.to_string());
        visiting.insert(node.to_string());
        stack.push(node.to_string());

        if let Some(children) = self.children_by_parent.get(node) {
            for child in children {
                if visiting.contains(child) {
                    for item in stack.iter() {
                        cycle_nodes.insert(item.clone());
                    }
                    cycle_nodes.insert(child.clone());
                    continue;
                }

                self.visit_cycle_nodes(child, visited, visiting, stack, cycle_nodes);
            }
        }

        visiting.remove(node);
        stack.pop();
    }

    fn transitive_descendants(
        &self,
        story_id: &str,
        cycle_nodes: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        self.walk_transitive(story_id, cycle_nodes, true)
    }

    fn transitive_ancestors(
        &self,
        story_id: &str,
        cycle_nodes: &BTreeSet<String>,
    ) -> BTreeSet<String> {
        self.walk_transitive(story_id, cycle_nodes, false)
    }

    fn walk_transitive(
        &self,
        story_id: &str,
        cycle_nodes: &BTreeSet<String>,
        forward: bool,
    ) -> BTreeSet<String> {
        let mut visited = BTreeSet::new();
        let mut stack = if forward {
            self.children_of(story_id).into_iter().collect::<Vec<_>>()
        } else {
            self.parents_of(story_id).into_iter().collect::<Vec<_>>()
        };

        while let Some(current) = stack.pop() {
            if cycle_nodes.contains(&current) || !visited.insert(current.clone()) {
                continue;
            }

            let next = if forward {
                self.children_of(&current)
            } else {
                self.parents_of(&current)
            };
            for item in next {
                stack.push(item);
            }
        }

        visited
    }
}

pub fn parse_duration(input: &str) -> Option<chrono::Duration> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let (num_str, unit) = if let Some(s) = input.strip_suffix('h') {
        (s, 'h')
    } else if let Some(s) = input.strip_suffix('d') {
        (s, 'd')
    } else if let Some(s) = input.strip_suffix('m') {
        (s, 'm')
    } else if let Some(s) = input.strip_suffix('w') {
        (s, 'w')
    } else {
        return None;
    };
    let num: i64 = num_str.parse().ok()?;
    match unit {
        'm' => chrono::Duration::try_minutes(num),
        'h' => chrono::Duration::try_hours(num),
        'd' => chrono::Duration::try_days(num),
        'w' => chrono::Duration::try_weeks(num),
        _ => None,
    }
}

/// Dependency graph for open stories using follows/starts-after/precedes relationships
#[derive(Clone, Debug)]
pub struct DependencyGraph {
    /// story_id -> set of story_ids that must complete before this one
    predecessors: BTreeMap<String, BTreeSet<String>>,
    /// story_id -> set of story_ids that depend on this one
    successors: BTreeMap<String, BTreeSet<String>>,
    /// All open story IDs in this graph
    nodes: BTreeSet<String>,
}

impl DependencyGraph {
    pub fn from_open_stories(stories: &BTreeMap<String, StorySnapshot>) -> Self {
        let open: BTreeMap<&String, &StorySnapshot> = stories
            .iter()
            .filter(|(_, s)| s.superstate == SuperState::Open)
            .collect();

        let mut predecessors: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut successors: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut nodes = BTreeSet::new();

        for id in open.keys() {
            nodes.insert((*id).clone());
            predecessors.entry((*id).clone()).or_default();
            successors.entry((*id).clone()).or_default();
        }

        for (id, story) in &open {
            for rel in &story.relationships {
                if !open.contains_key(&rel.other_id) {
                    continue;
                }
                match rel.relation.as_str() {
                    "blocked-by" => {
                        predecessors
                            .entry((*id).clone())
                            .or_default()
                            .insert(rel.other_id.clone());
                        successors
                            .entry(rel.other_id.clone())
                            .or_default()
                            .insert((*id).clone());
                    }
                    "blocks" => {
                        predecessors
                            .entry(rel.other_id.clone())
                            .or_default()
                            .insert((*id).clone());
                        successors
                            .entry((*id).clone())
                            .or_default()
                            .insert(rel.other_id.clone());
                    }
                    _ => {}
                }
            }
        }

        Self {
            predecessors,
            successors,
            nodes,
        }
    }

    /// Longest chain of dependent stories (by count)
    pub fn critical_path(&self) -> Vec<String> {
        let mut longest_from: BTreeMap<String, Vec<String>> = BTreeMap::new();

        // Topological approach: compute longest path from each node using memoization
        fn longest(
            node: &str,
            successors: &BTreeMap<String, BTreeSet<String>>,
            memo: &mut BTreeMap<String, Vec<String>>,
            visiting: &mut BTreeSet<String>,
        ) -> Vec<String> {
            if let Some(cached) = memo.get(node) {
                return cached.clone();
            }
            if !visiting.insert(node.to_string()) {
                // Cycle detected — break it
                return vec![node.to_string()];
            }
            let mut best = Vec::new();
            if let Some(succs) = successors.get(node) {
                for succ in succs {
                    let path = longest(succ, successors, memo, visiting);
                    if path.len() > best.len() {
                        best = path;
                    }
                }
            }
            visiting.remove(node);
            let mut result = vec![node.to_string()];
            result.extend(best);
            memo.insert(node.to_string(), result.clone());
            result
        }

        let mut visiting = BTreeSet::new();
        for node in &self.nodes {
            let path = longest(node, &self.successors, &mut longest_from, &mut visiting);
            longest_from.insert(node.clone(), path);
        }

        longest_from
            .into_values()
            .max_by_key(|p| p.len())
            .unwrap_or_default()
    }

    /// Transitive set of stories blocked (directly or indirectly) by the given story
    pub fn blocked_chain(&self, id: &str) -> BTreeSet<String> {
        let mut visited = BTreeSet::new();
        let mut stack = vec![id.to_string()];
        while let Some(current) = stack.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if let Some(succs) = self.successors.get(&current) {
                for succ in succs {
                    stack.push(succ.clone());
                }
            }
        }
        visited.remove(id);
        visited
    }

    /// Independent clusters of stories with no inter-dependencies
    pub fn parallel_groups(&self) -> Vec<BTreeSet<String>> {
        let mut visited = BTreeSet::new();
        let mut groups = Vec::new();

        for node in &self.nodes {
            if visited.contains(node) {
                continue;
            }
            let mut group = BTreeSet::new();
            let mut stack = vec![node.clone()];
            while let Some(current) = stack.pop() {
                if !group.insert(current.clone()) {
                    continue;
                }
                if let Some(preds) = self.predecessors.get(&current) {
                    for pred in preds {
                        stack.push(pred.clone());
                    }
                }
                if let Some(succs) = self.successors.get(&current) {
                    for succ in succs {
                        stack.push(succ.clone());
                    }
                }
            }
            visited.extend(group.iter().cloned());
            groups.push(group);
        }

        groups
    }
}

/// Extract story IDs matching `{PREFIX}-{DIGITS}` from text.
/// Returns unique matches respecting word boundaries:
/// - Prefix must be preceded by start-of-string or a non-alphanumeric character
/// - Digits must be followed by end-of-string or a non-digit character
pub fn extract_story_ids(prefix: &str, text: &str) -> Vec<String> {
    let mut results = Vec::new();
    let prefix_bytes = prefix.as_bytes();
    let text_bytes = text.as_bytes();
    let prefix_len = prefix_bytes.len();
    let text_len = text_bytes.len();

    let mut i = 0;
    while i + prefix_len < text_len {
        // Check word boundary: preceded by start-of-string or non-alphanumeric
        if i > 0 && text_bytes[i - 1].is_ascii_alphanumeric() {
            i += 1;
            continue;
        }

        // Check prefix match
        if &text_bytes[i..i + prefix_len] != prefix_bytes {
            i += 1;
            continue;
        }

        // Check dash after prefix
        let dash_pos = i + prefix_len;
        if dash_pos >= text_len || text_bytes[dash_pos] != b'-' {
            i += 1;
            continue;
        }

        // Read digits after dash
        let digits_start = dash_pos + 1;
        let mut digits_end = digits_start;
        while digits_end < text_len && text_bytes[digits_end].is_ascii_digit() {
            digits_end += 1;
        }

        if digits_end == digits_start {
            // No digits found
            i += 1;
            continue;
        }

        // Boundary check: next char must be end-of-string or non-digit
        // (already satisfied since we stopped reading at non-digit)

        let matched = &text[i..digits_end];
        if !results.contains(&matched.to_string()) {
            results.push(matched.to_string());
        }
        i = digits_end;
    }

    results
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        Priority, StateDef, StoryEvent, StoryRelation, StorySnapshot, SuperState,
        derive_family_relationships, fold_story, last_activity_type, validate_state_defs,
        would_create_parent_cycle,
    };

    #[test]
    fn requires_open_and_closed_states() {
        let states = vec![StateDef {
            slug: "todo".to_string(),
            super_state: SuperState::Open,
            role: None,
        }];
        let error = validate_state_defs(&states).unwrap_err();
        assert!(error.to_string().contains("OPEN"));
    }

    #[test]
    fn fold_story_tracks_awaiting_events() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Awaiting".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryAwaitingSet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    awaiting: "blocked on API".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.awaiting.as_deref(), Some("blocked on API"));
    }

    #[test]
    fn fold_story_clears_awaiting() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Awaiting".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryAwaitingSet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    awaiting: "blocked on API".to_string(),
                },
                StoryEvent::StoryAwaitingCleared {
                    at: "2026-03-13T00:02:00Z".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.awaiting, None);
    }

    #[test]
    fn fold_story_closed_snapshot_has_awaiting_cleared() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Awaiting".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryAwaitingSet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    awaiting: "blocked on API".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:02:00Z".to_string(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryAwaitingCleared {
                    at: "2026-03-13T00:02:01Z".to_string(),
                },
                StoryEvent::StoryClosedAndArchived {
                    at: "2026-03-13T00:02:02Z".to_string(),
                    state: "done".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.awaiting, None);
        assert_eq!(story.closed_at.as_deref(), Some("2026-03-13T00:02:02Z"));
    }

    #[test]
    fn fold_story_tracks_priority() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Priority".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryPrioritySet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    priority: Priority::High,
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.priority, Priority::High);
    }

    #[test]
    fn fold_story_priority_defaults_to_none() {
        let story = fold_story(
            "SH-1",
            &[StoryEvent::StoryCreated {
                at: "2026-03-13T00:00:00Z".to_string(),
                title: "No priority".to_string(),
                state: "todo".to_string(),
            }],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.priority, Priority::None);
    }

    #[test]
    fn priority_ord_ranks_critical_first() {
        assert!(Priority::Critical < Priority::High);
        assert!(Priority::High < Priority::Medium);
        assert!(Priority::Medium < Priority::Low);
        assert!(Priority::Low < Priority::None);
    }

    #[test]
    fn derive_family_relationships_omits_immediate_edges() {
        let stories = sample_story_map();
        let derived = derive_family_relationships(&stories);

        assert_eq!(
            derived.get("SH-1").unwrap(),
            &vec![StoryRelation {
                relation: "ancestor-of".to_string(),
                other_id: "SH-3".to_string(),
            }]
        );
        assert_eq!(
            derived.get("SH-3").unwrap(),
            &vec![StoryRelation {
                relation: "descendent-of".to_string(),
                other_id: "SH-1".to_string(),
            }]
        );
        assert!(derived.get("SH-2").unwrap().is_empty());
    }

    #[test]
    fn detects_prospective_parent_cycle() {
        let stories = sample_story_map();
        assert!(would_create_parent_cycle(&stories, "SH-3", "SH-1"));
        assert!(!would_create_parent_cycle(&stories, "SH-1", "SH-4"));
    }

    fn state_map() -> BTreeMap<String, StateDef> {
        vec![
            StateDef {
                slug: "todo".to_string(),
                super_state: SuperState::Open,
                role: None,
            },
            StateDef {
                slug: "done".to_string(),
                super_state: SuperState::Closed,
                role: None,
            },
        ]
        .into_iter()
        .map(|state| (state.slug.clone(), state))
        .collect()
    }

    fn sample_story_map() -> BTreeMap<String, StorySnapshot> {
        let stories = vec![
            StorySnapshot {
                id: "SH-1".to_string(),
                title: "A".to_string(),
                created_at: "2026-03-13T00:00:00Z".to_string(),
                updated_at: "2026-03-13T00:00:00Z".to_string(),
                state: "todo".to_string(),
                superstate: SuperState::Open,
                assignee: None,
                awaiting: None,
                priority: Priority::None,
                labels: Vec::new(),
                comments: Vec::new(),
                relationships: vec![StoryRelation {
                    relation: "parent-of".to_string(),
                    other_id: "SH-2".to_string(),
                }],
                closed_at: None,
            },
            StorySnapshot {
                id: "SH-2".to_string(),
                title: "B".to_string(),
                created_at: "2026-03-13T00:00:00Z".to_string(),
                updated_at: "2026-03-13T00:00:00Z".to_string(),
                state: "todo".to_string(),
                superstate: SuperState::Open,
                assignee: None,
                awaiting: None,
                priority: Priority::None,
                labels: Vec::new(),
                comments: Vec::new(),
                relationships: vec![
                    StoryRelation {
                        relation: "child-of".to_string(),
                        other_id: "SH-1".to_string(),
                    },
                    StoryRelation {
                        relation: "parent-of".to_string(),
                        other_id: "SH-3".to_string(),
                    },
                ],
                closed_at: None,
            },
            StorySnapshot {
                id: "SH-3".to_string(),
                title: "C".to_string(),
                created_at: "2026-03-13T00:00:00Z".to_string(),
                updated_at: "2026-03-13T00:00:00Z".to_string(),
                state: "todo".to_string(),
                superstate: SuperState::Open,
                assignee: None,
                awaiting: None,
                priority: Priority::None,
                labels: Vec::new(),
                comments: Vec::new(),
                relationships: vec![StoryRelation {
                    relation: "child-of".to_string(),
                    other_id: "SH-2".to_string(),
                }],
                closed_at: None,
            },
            StorySnapshot {
                id: "SH-4".to_string(),
                title: "D".to_string(),
                created_at: "2026-03-13T00:00:00Z".to_string(),
                updated_at: "2026-03-13T00:00:00Z".to_string(),
                state: "todo".to_string(),
                superstate: SuperState::Open,
                assignee: None,
                awaiting: None,
                priority: Priority::None,
                labels: Vec::new(),
                comments: Vec::new(),
                relationships: Vec::new(),
                closed_at: None,
            },
        ];

        stories
            .into_iter()
            .map(|story| (story.id.clone(), story))
            .collect()
    }

    #[test]
    fn extract_story_ids_single_match() {
        assert_eq!(super::extract_story_ids("SH", "Fix SH-1 bug"), vec!["SH-1"]);
    }

    #[test]
    fn extract_story_ids_multiple_matches() {
        assert_eq!(
            super::extract_story_ids("SH", "SH-1 and SH-2"),
            vec!["SH-1", "SH-2"]
        );
    }

    #[test]
    fn extract_story_ids_no_matches() {
        let result: Vec<String> = Vec::new();
        assert_eq!(super::extract_story_ids("SH", "no matches here"), result);
    }

    #[test]
    fn extract_story_ids_custom_prefix() {
        assert_eq!(
            super::extract_story_ids("API", "API-42 done"),
            vec!["API-42"]
        );
    }

    #[test]
    fn extract_story_ids_no_false_positive_inside_word() {
        let result: Vec<String> = Vec::new();
        assert_eq!(super::extract_story_ids("SH", "PUSH-123"), result);
    }

    #[test]
    fn extract_story_ids_no_boundary_between_ids() {
        assert_eq!(super::extract_story_ids("SH", "SH-1SH-2"), vec!["SH-1"]);
    }

    #[test]
    fn last_activity_type_returns_correct_types() {
        assert_eq!(last_activity_type(&[]), "unknown");
        assert_eq!(
            last_activity_type(&[StoryEvent::StoryCreated {
                at: "2026-03-13T00:00:00Z".to_string(),
                title: "Test".to_string(),
                state: "todo".to_string(),
            }]),
            "created"
        );
        assert_eq!(
            last_activity_type(&[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Test".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryCommentAdded {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    text: "a comment".to_string(),
                },
            ]),
            "comment"
        );
        assert_eq!(
            last_activity_type(&[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Test".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    state: "in-progress".to_string(),
                },
            ]),
            "state-change"
        );
        assert_eq!(
            last_activity_type(&[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Test".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryPrioritySet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    priority: Priority::High,
                },
            ]),
            "priority-set"
        );
    }
}
