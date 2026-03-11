use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::AppError;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub awaiting: Option<String>,
    #[serde(default)]
    pub comments: Vec<StoryComment>,
    #[serde(default)]
    pub relationships: Vec<StoryRelation>,
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
    StoryClosedAndArchived {
        at: String,
        state: String,
    },
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
            StoryEvent::StoryStateChanged {
                at,
                state: story_state,
            } => {
                state = Some(story_state.clone());
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
        awaiting: None,
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
        "starts-before" => Some(vec![("starts-before", "starts-after")]),
        "starts-after" => Some(vec![("starts-after", "starts-before")]),
        "starts-with" => Some(vec![("starts-with", "starts-with")]),
        "finishes-before" => Some(vec![("finishes-before", "finishes-after")]),
        "finishes-after" => Some(vec![("finishes-after", "finishes-before")]),
        "finishes-with" => Some(vec![("finishes-with", "finishes-with")]),
        "precedes" => Some(vec![("precedes", "follows")]),
        "follows" => Some(vec![("follows", "precedes")]),
        "relieves" => Some(vec![("relieves", "relieved-by")]),
        "relieved-by" => Some(vec![("relieved-by", "relieves")]),
        "conflicts-with" => Some(vec![("conflicts-with", "conflicts-with")]),
        "coincides-with" => Some(vec![
            ("starts-with", "starts-with"),
            ("finishes-with", "finishes-with"),
        ]),
        "parent-of" => Some(vec![
            ("parent-of", "child-of"),
            ("starts-before", "starts-after"),
            ("finishes-after", "finishes-before"),
        ]),
        "child-of" => Some(vec![
            ("child-of", "parent-of"),
            ("starts-after", "starts-before"),
            ("finishes-before", "finishes-after"),
        ]),
        "relates-to" => Some(vec![("relates-to", "relates-to")]),
        "obviates" => Some(vec![("obviates", "obviated-by")]),
        "obviated-by" => Some(vec![("obviated-by", "obviates")]),
        _ => None,
    }
}

pub fn inverse_relation(relation: &str) -> Option<&'static str> {
    match relation {
        "starts-before" => Some("starts-after"),
        "starts-after" => Some("starts-before"),
        "starts-with" => Some("starts-with"),
        "finishes-before" => Some("finishes-after"),
        "finishes-after" => Some("finishes-before"),
        "finishes-with" => Some("finishes-with"),
        "precedes" => Some("follows"),
        "follows" => Some("precedes"),
        "relieves" => Some("relieved-by"),
        "relieved-by" => Some("relieves"),
        "conflicts-with" => Some("conflicts-with"),
        "parent-of" => Some("child-of"),
        "child-of" => Some("parent-of"),
        "relates-to" => Some("relates-to"),
        "obviates" => Some("obviated-by"),
        "obviated-by" => Some("obviates"),
        _ => None,
    }
}

pub fn is_mutual_relation(relation: &str) -> bool {
    matches!(
        relation,
        "starts-with" | "finishes-with" | "conflicts-with" | "relates-to"
    )
}

pub fn compute_integrity_issues(
    stories: &BTreeMap<String, StorySnapshot>,
) -> BTreeMap<String, Vec<String>> {
    let mut issues: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for story in stories.values() {
        let parent_count = story
            .relationships
            .iter()
            .filter(|relation| relation.relation == "child-of")
            .count();

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

    for node in find_parent_cycle_nodes(stories) {
        issues
            .entry(node)
            .or_default()
            .push("parent/child cycle detected".to_string());
    }

    issues
}

fn find_parent_cycle_nodes(stories: &BTreeMap<String, StorySnapshot>) -> BTreeSet<String> {
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for story in stories.values() {
        let children = story
            .relationships
            .iter()
            .filter(|relation| relation.relation == "parent-of")
            .filter_map(|relation| {
                if stories.contains_key(&relation.other_id) {
                    Some(relation.other_id.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        graph.insert(story.id.clone(), children);
    }

    let mut visited = BTreeSet::new();
    let mut visiting = BTreeSet::new();
    let mut stack = Vec::new();
    let mut cycle_nodes = BTreeSet::new();

    for node in graph.keys() {
        visit_cycle_nodes(
            node,
            &graph,
            &mut visited,
            &mut visiting,
            &mut stack,
            &mut cycle_nodes,
        );
    }

    cycle_nodes
}

fn visit_cycle_nodes(
    node: &str,
    graph: &BTreeMap<String, Vec<String>>,
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

    if let Some(children) = graph.get(node) {
        for child in children {
            if visiting.contains(child) {
                for item in stack.iter() {
                    cycle_nodes.insert(item.clone());
                }
                cycle_nodes.insert(child.clone());
                continue;
            }

            visit_cycle_nodes(child, graph, visited, visiting, stack, cycle_nodes);
        }
    }

    visiting.remove(node);
    stack.pop();
}

#[cfg(test)]
mod tests {
    use super::{StateDef, SuperState, validate_state_defs};

    #[test]
    fn requires_open_and_closed_states() {
        let states = vec![StateDef {
            slug: "todo".to_string(),
            super_state: SuperState::Open,
        }];
        let error = validate_state_defs(&states).unwrap_err();
        assert!(error.to_string().contains("OPEN"));
    }
}
