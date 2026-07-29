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
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub story_type: Option<String>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDef {
    pub slug: String,
    #[serde(rename = "super")]
    pub super_state: SuperState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Free text explaining what the state means, surfaced in the UIs.
    ///
    /// Round-tripping this is not optional: `save_states` rewrites the whole
    /// file, so a field the struct doesn't know about is silently destroyed
    /// on the next `story state add` (SH-49).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The only [`StateDef::role`] the tool understands — see
/// [`crate::storage::find_active_state`], which uses it to pick the state a
/// story moves into when work starts.
pub const STATE_ROLE_ACTIVE: &str = "active";

/// A three-way edit of an optional field: leave it as it is, empty it, or
/// give it a value.
///
/// `Option<Option<T>>` says the same thing and is misread at every call site.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum FieldEdit<T> {
    #[default]
    Keep,
    Clear,
    Set(T),
}

impl<T> FieldEdit<T> {
    /// Resolves the edit against the field's current value.
    pub fn apply(self, current: Option<T>) -> Option<T> {
        match self {
            FieldEdit::Keep => current,
            FieldEdit::Clear => None,
            FieldEdit::Set(value) => Some(value),
        }
    }

    /// Whether this edit would leave the field untouched.
    pub fn is_keep(&self) -> bool {
        matches!(self, FieldEdit::Keep)
    }
}

/// The parts of a [`StateDef`] that can be edited in place.
///
/// `slug` is deliberately absent: a state's slug is recorded in every
/// `StoryStateChanged` event ever written, so renaming one would orphan
/// history rather than update it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StateChanges {
    pub super_state: Option<SuperState>,
    pub role: FieldEdit<String>,
    pub description: FieldEdit<String>,
}

impl StateChanges {
    /// Whether this would change nothing at all — callers reject a no-op edit
    /// rather than rewriting states.toml for it.
    pub fn is_empty(&self) -> bool {
        self.super_state.is_none() && self.role.is_keep() && self.description.is_keep()
    }
}

/// How many stories reference a state, split by where they live.
///
/// Open stories can be migrated elsewhere; archived ones cannot, which is
/// why the two are counted separately rather than summed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateUsage {
    pub open: usize,
    pub archived: usize,
}

impl StateUsage {
    /// Total stories in this state.
    pub fn total(&self) -> usize {
        self.open + self.archived
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeDef {
    pub slug: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProgressRollup {
    pub children_done: usize,
    pub children_total: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    pub id: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub github: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoryComment {
    pub at: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct StoryRelation {
    pub relation: String,
    pub other_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub story_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
    /// `true` when the story was removed via `story delete` rather than
    /// closed through the normal state machine. A deleted story is always
    /// folded to `superstate: CLOSED` regardless of its `state` slug.
    #[serde(default, skip_serializing_if = "is_false")]
    pub deleted: bool,
    /// The reason passed to `story delete`, when [`deleted`](Self::deleted)
    /// is `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_reason: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Retracts a comment that was added earlier — the inverse of
    /// [`StoryCommentAdded`](Self::StoryCommentAdded).
    ///
    /// Comments are the one part of a story that only ever accumulates, so
    /// they are the one part an append-only undo cannot express by setting a
    /// field back. The retraction names the comment it annuls by *both* its
    /// instant and its text: storyhook's timestamps have second precision, so
    /// two comments can share an `at`, and an audit log should say what was
    /// withdrawn rather than merely when.
    StoryCommentRetracted {
        at: String,
        comment_at: String,
        text: String,
    },
    StoryAssigned {
        at: String,
        member_id: String,
    },
    /// Clears a story's assignee — the inverse of
    /// [`StoryAssigned`](Self::StoryAssigned) when the story had nobody before.
    ///
    /// Sibling of [`StoryAwaitingCleared`](Self::StoryAwaitingCleared), and
    /// added for the same reason: a field with an event that sets it and none
    /// that clears it cannot be put back without rewriting history.
    StoryAssigneeCleared {
        at: String,
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
    StoryTypeSet {
        at: String,
        story_type: String,
    },
    StoryLabelsSet {
        at: String,
        labels: Vec<String>,
    },
    StoryTitleSet {
        at: String,
        title: String,
    },
    StoryDescriptionSet {
        at: String,
        description: String,
    },
    StoryClosedAndArchived {
        at: String,
        state: String,
    },
    StoryDeleted {
        at: String,
        reason: String,
    },
}

/// Every `kind` tag [`StoryEvent`] answers to.
///
/// The legacy importer is why this is a list rather than an implicit property
/// of the derive: reading a `.storyhook` event log has to tell *this kind is
/// from a newer storyhook and must be kept verbatim* apart from *this event is
/// corrupt*, and both look identical to `serde_json::from_str::<StoryEvent>` —
/// it returns `Err` either way. Retaining a corrupt `StoryCreated` as an
/// unknown payload would import a story with no title and never say so.
///
/// It cannot drift: `every_known_kind_is_a_variant_and_every_variant_is_known`
/// reads serde's own `unknown variant, expected one of …` list back out of the
/// derive and compares it to this array.
pub const EVENT_KINDS: [&str; 17] = [
    "StoryCreated",
    "StoryCommentAdded",
    "StoryCommentRetracted",
    "StoryAssigned",
    "StoryAssigneeCleared",
    "StoryAwaitingSet",
    "StoryAwaitingCleared",
    "StoryStateChanged",
    "StoryRelationshipAdded",
    "StoryRelationshipRemoved",
    "StoryPrioritySet",
    "StoryTypeSet",
    "StoryLabelsSet",
    "StoryTitleSet",
    "StoryDescriptionSet",
    "StoryClosedAndArchived",
    "StoryDeleted",
];

/// Whether `kind` is an event this binary can decode.
#[must_use]
pub fn is_known_event_kind(kind: &str) -> bool {
    EVENT_KINDS.contains(&kind)
}

/// The serde `kind` tag `event` serializes with.
///
/// An exhaustive match, so a new variant cannot reach the store's `kind` column
/// without a name being chosen for it here.
#[must_use]
pub fn event_kind(event: &StoryEvent) -> &'static str {
    match event {
        StoryEvent::StoryCreated { .. } => "StoryCreated",
        StoryEvent::StoryCommentAdded { .. } => "StoryCommentAdded",
        StoryEvent::StoryCommentRetracted { .. } => "StoryCommentRetracted",
        StoryEvent::StoryAssigned { .. } => "StoryAssigned",
        StoryEvent::StoryAssigneeCleared { .. } => "StoryAssigneeCleared",
        StoryEvent::StoryAwaitingSet { .. } => "StoryAwaitingSet",
        StoryEvent::StoryAwaitingCleared { .. } => "StoryAwaitingCleared",
        StoryEvent::StoryStateChanged { .. } => "StoryStateChanged",
        StoryEvent::StoryRelationshipAdded { .. } => "StoryRelationshipAdded",
        StoryEvent::StoryRelationshipRemoved { .. } => "StoryRelationshipRemoved",
        StoryEvent::StoryPrioritySet { .. } => "StoryPrioritySet",
        StoryEvent::StoryTypeSet { .. } => "StoryTypeSet",
        StoryEvent::StoryLabelsSet { .. } => "StoryLabelsSet",
        StoryEvent::StoryTitleSet { .. } => "StoryTitleSet",
        StoryEvent::StoryDescriptionSet { .. } => "StoryDescriptionSet",
        StoryEvent::StoryClosedAndArchived { .. } => "StoryClosedAndArchived",
        StoryEvent::StoryDeleted { .. } => "StoryDeleted",
    }
}

pub fn last_activity_type(events: &[StoryEvent]) -> &'static str {
    events
        .last()
        .map(|event| match event {
            StoryEvent::StoryCreated { .. } => "created",
            StoryEvent::StoryCommentAdded { .. } => "comment",
            StoryEvent::StoryCommentRetracted { .. } => "comment-retracted",
            StoryEvent::StoryAssigned { .. } => "assigned",
            StoryEvent::StoryAssigneeCleared { .. } => "assignee-cleared",
            StoryEvent::StoryAwaitingSet { .. } => "awaiting-set",
            StoryEvent::StoryAwaitingCleared { .. } => "awaiting-cleared",
            StoryEvent::StoryStateChanged { .. } => "state-change",
            StoryEvent::StoryRelationshipAdded { .. } => "relationship-added",
            StoryEvent::StoryRelationshipRemoved { .. } => "relationship-removed",
            StoryEvent::StoryPrioritySet { .. } => "priority-set",
            StoryEvent::StoryTypeSet { .. } => "type-set",
            StoryEvent::StoryLabelsSet { .. } => "labels-set",
            StoryEvent::StoryTitleSet { .. } => "title-set",
            StoryEvent::StoryDescriptionSet { .. } => "description-set",
            StoryEvent::StoryClosedAndArchived { .. } => "archived",
            StoryEvent::StoryDeleted { .. } => "deleted",
        })
        .unwrap_or("unknown")
}

/// Read-path validation, applied by `storage::load_states` on **every** read.
///
/// Deliberately minimal: a rule added here can make an existing project
/// unreadable rather than merely uneditable. Rules that only a new state set
/// must satisfy belong in [`validate_state_defs_for_write`].
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

/// A state slug must be lowercase alphanumerics in dash-separated words.
///
/// Slugs are addresses, not labels: they are typed as CLI arguments
/// (`story move SH-1 <slug>`) and interpolated into URL path segments
/// (`DELETE /api/repos/<id>/states/<slug>`, split on `/` by the router). A
/// slug containing a space, slash, or capital cannot be addressed by either.
pub fn validate_state_slug(slug: &str) -> Result<(), AppError> {
    let invalid = |reason: &str| {
        Err(AppError::Validation(format!(
            "invalid state slug `{slug}`: {reason} (use lowercase letters, digits, and single dashes, e.g. `in-review`)"
        )))
    };

    if slug.is_empty() {
        return invalid("it is empty");
    }
    if let Some(bad) = slug
        .chars()
        .find(|ch| !ch.is_ascii_lowercase() && !ch.is_ascii_digit() && *ch != '-')
    {
        return invalid(&format!("`{bad}` is not allowed"));
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return invalid("it starts or ends with a dash");
    }
    if slug.contains("--") {
        return invalid("it contains a double dash");
    }
    Ok(())
}

/// Write-path validation: everything [`validate_state_defs`] requires, plus
/// the rules a state set must satisfy to be *written*.
///
/// Kept separate from the read path so a project carrying a legacy slug
/// still loads — it just reports the offending slug the first time someone
/// edits its states.
pub fn validate_state_defs_for_write(states: &[StateDef]) -> Result<(), AppError> {
    validate_state_defs(states)?;

    let mut seen = BTreeSet::new();
    for state in states {
        validate_state_slug(&state.slug)?;
        if !seen.insert(state.slug.as_str()) {
            return Err(AppError::Validation(format!(
                "state `{}` is defined more than once",
                state.slug
            )));
        }
        if let Some(role) = state.role.as_deref()
            && role != STATE_ROLE_ACTIVE
        {
            return Err(AppError::Validation(format!(
                "state `{}` has unknown role `{role}` (the only role is `{STATE_ROLE_ACTIVE}`)",
                state.slug
            )));
        }
    }

    let active: Vec<&str> = states
        .iter()
        .filter(|state| state.role.as_deref() == Some(STATE_ROLE_ACTIVE))
        .map(|state| state.slug.as_str())
        .collect();
    if active.len() > 1 {
        return Err(AppError::Validation(format!(
            "only one state may have role `{STATE_ROLE_ACTIVE}`, but {} do: {}",
            active.len(),
            active.join(", ")
        )));
    }

    Ok(())
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
    let mut story_type = None;
    let mut description = None;
    let mut labels = Vec::new();
    let mut comments = Vec::new();
    let mut relationships = BTreeSet::new();
    let mut closed_at = None;
    let mut deleted = false;
    let mut deleted_reason = None;

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
            StoryEvent::StoryCommentRetracted {
                at,
                comment_at,
                text,
            } => {
                // The most recent match, and a miss is a no-op rather than an
                // error: an event log is replayed, and a replay that could fail
                // on its own history would make a story unreadable rather than
                // merely wrong.
                if let Some(index) = comments
                    .iter()
                    .rposition(|c| &c.at == comment_at && &c.text == text)
                {
                    comments.remove(index);
                }
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryAssigned { at, member_id } => {
                assignee = Some(member_id.clone());
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryAssigneeCleared { at } => {
                assignee = None;
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
                // Moving into an OPEN state is what *reopening* is, so it
                // retracts the closure markers. Without this the fold is not
                // total: `closed_at` and `deleted` have events that set them
                // and none that clear them, so the only way to reopen a story
                // is to delete history — which is precisely what
                // `unarchive_story` does, and precisely what an append-only
                // event store cannot do.
                //
                // Scoped to states the project actually defines and actually
                // calls open: an unrecognised slug leaves the flags alone, so
                // a story that folds today because `deleted` forces its
                // superstate cannot be made unfoldable by this rule.
                if states
                    .get(story_state)
                    .is_some_and(|def| def.super_state == SuperState::Open)
                {
                    closed_at = None;
                    deleted = false;
                    deleted_reason = None;
                }
            }
            StoryEvent::StoryPrioritySet {
                at,
                priority: new_priority,
            } => {
                priority = new_priority.clone();
                updated_at = Some(at.clone());
            }
            StoryEvent::StoryTypeSet {
                at,
                story_type: new_type,
            } => {
                story_type = Some(new_type.clone());
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
            StoryEvent::StoryDescriptionSet {
                at,
                description: new_description,
            } => {
                description = Some(new_description.clone());
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
            StoryEvent::StoryDeleted { at, reason } => {
                deleted = true;
                deleted_reason = Some(reason.clone());
                updated_at = Some(at.clone());
                if closed_at.is_none() {
                    closed_at = Some(at.clone());
                }
            }
        }
    }

    let state = state.ok_or_else(|| AppError::Integrity(format!("story {id} is missing state")))?;
    let title = title.ok_or_else(|| AppError::Integrity(format!("story {id} is missing title")))?;
    let created_at = created_at
        .ok_or_else(|| AppError::Integrity(format!("story {id} is missing created_at")))?;
    let updated_at = updated_at.unwrap_or_else(|| created_at.clone());
    // A deleted story is always CLOSED regardless of its last `state` slug —
    // deletion is a terminal act independent of the normal state machine, and
    // forcing this here (rather than trusting the state map) also keeps a
    // deleted story's superstate correct even if its state slug is later
    // removed from the project's configured state set.
    let superstate = if deleted {
        SuperState::Closed
    } else {
        states
            .get(&state)
            .ok_or_else(|| {
                AppError::Validation(format!("story {id} references undefined state `{state}`"))
            })?
            .super_state
            .clone()
    };

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
        story_type,
        description,
        comments,
        relationships: relationships.into_iter().collect(),
        closed_at,
        deleted,
        deleted_reason,
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
    matches!(relation, "relates-to" | "duplicate-of")
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

pub fn has_children(story: &StorySnapshot) -> bool {
    story
        .relationships
        .iter()
        .any(|r| r.relation == "parent-of")
}

pub fn compute_progress(
    story: &StorySnapshot,
    all_stories: &BTreeMap<String, StorySnapshot>,
) -> Option<ProgressRollup> {
    let children: Vec<&str> = story
        .relationships
        .iter()
        .filter(|r| r.relation == "parent-of")
        .map(|r| r.other_id.as_str())
        .collect();

    if children.is_empty() {
        return None;
    }

    let children_total = children.len();
    let children_done = children
        .iter()
        .filter(|child_id| {
            all_stories
                .get(**child_id)
                .is_some_and(|s| s.superstate == SuperState::Closed)
        })
        .count();

    Some(ProgressRollup {
        children_done,
        children_total,
    })
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
    } else {
        let s = input.strip_suffix('w')?;
        (s, 'w')
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
        FieldEdit, Priority, StateChanges, StateDef, StateUsage, StoryEvent, StoryRelation,
        StorySnapshot, SuperState, compute_progress, derive_family_relationships, fold_story,
        has_children, is_ready, last_activity_type, validate_state_defs,
        validate_state_defs_for_write, validate_state_slug, would_create_parent_cycle,
    };

    #[test]
    fn requires_open_and_closed_states() {
        let states = vec![StateDef {
            slug: "todo".to_string(),
            super_state: SuperState::Open,
            role: None,
            description: None,
        }];
        let error = validate_state_defs(&states).unwrap_err();
        assert!(error.to_string().contains("OPEN"));
    }

    // --- state slug / write-path validation ---

    fn state(slug: &str, super_state: SuperState, role: Option<&str>) -> StateDef {
        StateDef {
            slug: slug.to_string(),
            super_state,
            role: role.map(str::to_string),
            description: None,
        }
    }

    #[test]
    fn validate_state_slug_accepts_dash_separated_lowercase() {
        for good in ["todo", "in-progress", "wave-2", "x"] {
            validate_state_slug(good).unwrap_or_else(|e| panic!("`{good}` rejected: {e}"));
        }
    }

    #[test]
    fn validate_state_slug_rejects_unaddressable_slugs() {
        // Each of these breaks either the CLI (`story move SH-1 <slug>`) or
        // the web router, which splits paths on `/`.
        for bad in [
            "",
            "In Review",
            "in review",
            "in/review",
            "in_review",
            "-todo",
            "todo-",
            "in--review",
            "tödo",
        ] {
            let error = validate_state_slug(bad).unwrap_err();
            assert!(
                error.to_string().contains("invalid state slug"),
                "`{bad}` should have been rejected"
            );
        }
    }

    #[test]
    fn write_validation_rejects_duplicate_slugs() {
        let states = vec![
            state("todo", SuperState::Open, None),
            state("todo", SuperState::Open, None),
            state("done", SuperState::Closed, None),
        ];
        let error = validate_state_defs_for_write(&states).unwrap_err();
        assert!(error.to_string().contains("defined more than once"));
    }

    #[test]
    fn write_validation_rejects_more_than_one_active_role() {
        let states = vec![
            state("todo", SuperState::Open, Some("active")),
            state("doing", SuperState::Open, Some("active")),
            state("done", SuperState::Closed, None),
        ];
        let error = validate_state_defs_for_write(&states).unwrap_err();
        assert!(error.to_string().contains("only one state may have role"));
    }

    #[test]
    fn write_validation_rejects_unknown_roles() {
        let states = vec![
            state("todo", SuperState::Open, Some("triage")),
            state("done", SuperState::Closed, None),
        ];
        let error = validate_state_defs_for_write(&states).unwrap_err();
        assert!(error.to_string().contains("unknown role `triage`"));
    }

    /// The read path stays permissive on purpose: tightening it would make a
    /// project carrying a legacy slug unloadable rather than uneditable.
    #[test]
    fn read_validation_tolerates_what_the_write_path_rejects() {
        let states = vec![
            state("In Review", SuperState::Open, Some("triage")),
            state("done", SuperState::Closed, None),
        ];
        validate_state_defs(&states).unwrap();
        assert!(validate_state_defs_for_write(&states).is_err());
    }

    // --- FieldEdit ---

    #[test]
    fn field_edit_keep_clear_and_set() {
        let current = || Some("before".to_string());
        assert_eq!(FieldEdit::Keep.apply(current()), current());
        assert_eq!(FieldEdit::Clear.apply(current()), None);
        assert_eq!(
            FieldEdit::Set("after".to_string()).apply(current()),
            Some("after".to_string())
        );
        assert_eq!(
            FieldEdit::Set("after".to_string()).apply(None),
            Some("after".to_string())
        );
    }

    #[test]
    fn state_changes_default_is_a_no_op() {
        assert!(StateChanges::default().is_empty());
        assert!(
            !StateChanges {
                description: FieldEdit::Clear,
                ..StateChanges::default()
            }
            .is_empty(),
            "clearing a field is a change, not a no-op"
        );
    }

    #[test]
    fn state_usage_totals_both_stores() {
        assert_eq!(
            StateUsage {
                open: 2,
                archived: 3
            }
            .total(),
            5
        );
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
    fn fold_story_deleted_forces_closed_superstate() {
        // Regression test for #18: deleting a story left `state`/`superstate`
        // unchanged, so a story deleted while `todo` (OPEN) stayed OPEN.
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Deleted while open".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryCommentAdded {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    text: "[deleted] created in error".to_string(),
                },
                StoryEvent::StoryDeleted {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    reason: "created in error".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.superstate, SuperState::Closed);
        assert!(story.deleted);
        assert_eq!(story.deleted_reason.as_deref(), Some("created in error"));
        // The state slug itself is preserved as a truthful record of what the
        // story was when it was deleted — only superstate is forced CLOSED.
        assert_eq!(story.state, "todo");
        assert_eq!(story.closed_at.as_deref(), Some("2026-03-13T00:01:00Z"));
    }

    #[test]
    fn fold_story_deleted_while_closed_keeps_original_closed_at() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Deleted after closing".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryClosedAndArchived {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryDeleted {
                    at: "2026-03-13T00:02:00Z".to_string(),
                    reason: "cleaning up archive".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.superstate, SuperState::Closed);
        assert!(story.deleted);
        // `closed_at` reflects the original close, not the later deletion —
        // `fold_story` only backfills `closed_at` when it was never set.
        assert_eq!(story.closed_at.as_deref(), Some("2026-03-13T00:01:00Z"));
    }

    #[test]
    fn fold_story_reopens_a_closed_story_by_appending_a_move_to_an_open_state() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Closed then reopened".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryClosedAndArchived {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:02:00Z".to_string(),
                    state: "todo".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.superstate, SuperState::Open);
        assert_eq!(story.state, "todo");
        assert_eq!(story.closed_at, None);
    }

    #[test]
    fn fold_story_undeletes_when_moved_back_into_an_open_state() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Deleted then undeleted".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryCommentAdded {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    text: "[deleted] created in error".to_string(),
                },
                StoryEvent::StoryDeleted {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    reason: "created in error".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:02:00Z".to_string(),
                    state: "todo".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert!(!story.deleted);
        assert_eq!(story.deleted_reason, None);
        assert_eq!(story.closed_at, None);
        assert_eq!(story.superstate, SuperState::Open);
        // The audit trail survives the undelete, exactly as it does when
        // `unarchive_story` rewrites the log: the marker events go, the
        // `[deleted]` comment stays.
        assert_eq!(story.comments.len(), 1);
    }

    #[test]
    fn fold_story_move_into_a_closed_state_does_not_reopen() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Closed twice".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryClosedAndArchived {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    state: "done".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:02:00Z".to_string(),
                    state: "done".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.superstate, SuperState::Closed);
        assert_eq!(story.closed_at.as_deref(), Some("2026-03-13T00:01:00Z"));
    }

    #[test]
    fn fold_story_move_into_an_undefined_state_leaves_deletion_alone() {
        // A deleted story folds even when its state slug is no longer
        // configured, because `deleted` forces the superstate. Retracting the
        // deletion on an unrecognised slug would turn that into a hard fold
        // failure, so the rule requires a state the project defines *and*
        // calls open.
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Deleted, then moved somewhere unknown".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryDeleted {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    reason: "created in error".to_string(),
                },
                StoryEvent::StoryStateChanged {
                    at: "2026-03-13T00:02:00Z".to_string(),
                    state: "retired".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert!(story.deleted);
        assert_eq!(story.superstate, SuperState::Closed);
    }

    #[test]
    fn is_ready_returns_false_for_deleted_story() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Deleted".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryDeleted {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    reason: "created in error".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert!(!is_ready(&story, &BTreeMap::new()));
    }

    #[test]
    fn deleted_blocker_no_longer_blocks_dependent() {
        // Regression test for #18: before the fix, a deleted `blocked-by`
        // blocker still had `superstate: OPEN`, so `is_ready` kept treating
        // its dependent as blocked forever.
        let blocker = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Blocker".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryDeleted {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    reason: "no longer needed".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        let dependent = fold_story(
            "SH-2",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Dependent".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryRelationshipAdded {
                    at: "2026-03-13T00:00:01Z".to_string(),
                    other_id: "SH-1".to_string(),
                    relation: "blocked-by".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        let mut all_stories = BTreeMap::new();
        all_stories.insert(blocker.id.clone(), blocker);
        all_stories.insert(dependent.id.clone(), dependent.clone());

        assert!(is_ready(&dependent, &all_stories));
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
                description: None,
            },
            StateDef {
                slug: "done".to_string(),
                super_state: SuperState::Closed,
                role: None,
                description: None,
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
                story_type: None,
                description: None,
                comments: Vec::new(),
                relationships: vec![StoryRelation {
                    relation: "parent-of".to_string(),
                    other_id: "SH-2".to_string(),
                }],
                closed_at: None,
                deleted: false,
                deleted_reason: None,
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
                story_type: None,
                description: None,
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
                deleted: false,
                deleted_reason: None,
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
                story_type: None,
                description: None,
                comments: Vec::new(),
                relationships: vec![StoryRelation {
                    relation: "child-of".to_string(),
                    other_id: "SH-2".to_string(),
                }],
                closed_at: None,
                deleted: false,
                deleted_reason: None,
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
                story_type: None,
                description: None,
                comments: Vec::new(),
                relationships: Vec::new(),
                closed_at: None,
                deleted: false,
                deleted_reason: None,
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
        assert_eq!(
            last_activity_type(&[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Test".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryTypeSet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    story_type: "epic".to_string(),
                },
            ]),
            "type-set"
        );
        assert_eq!(
            last_activity_type(&[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Test".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryDescriptionSet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    description: "Some description".to_string(),
                },
            ]),
            "description-set"
        );
    }

    #[test]
    fn fold_story_tracks_story_type() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Typed story".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryTypeSet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    story_type: "epic".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.story_type.as_deref(), Some("epic"));
        assert_eq!(story.updated_at, "2026-03-13T00:01:00Z");
    }

    #[test]
    fn fold_story_story_type_defaults_to_none() {
        let story = fold_story(
            "SH-1",
            &[StoryEvent::StoryCreated {
                at: "2026-03-13T00:00:00Z".to_string(),
                title: "No type".to_string(),
                state: "todo".to_string(),
            }],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.story_type, None);
    }

    #[test]
    fn fold_story_story_type_can_be_changed() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Changing type".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryTypeSet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    story_type: "epic".to_string(),
                },
                StoryEvent::StoryTypeSet {
                    at: "2026-03-13T00:02:00Z".to_string(),
                    story_type: "bug".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.story_type.as_deref(), Some("bug"));
        assert_eq!(story.updated_at, "2026-03-13T00:02:00Z");
    }

    #[test]
    fn fold_story_tracks_description() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Described story".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryDescriptionSet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    description: "What this story is about".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(
            story.description.as_deref(),
            Some("What this story is about")
        );
        assert_eq!(story.updated_at, "2026-03-13T00:01:00Z");
    }

    #[test]
    fn fold_story_description_defaults_to_none() {
        let story = fold_story(
            "SH-1",
            &[StoryEvent::StoryCreated {
                at: "2026-03-13T00:00:00Z".to_string(),
                title: "No description".to_string(),
                state: "todo".to_string(),
            }],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.description, None);
    }

    #[test]
    fn fold_story_description_last_write_wins() {
        let story = fold_story(
            "SH-1",
            &[
                StoryEvent::StoryCreated {
                    at: "2026-03-13T00:00:00Z".to_string(),
                    title: "Changing description".to_string(),
                    state: "todo".to_string(),
                },
                StoryEvent::StoryDescriptionSet {
                    at: "2026-03-13T00:01:00Z".to_string(),
                    description: "First draft".to_string(),
                },
                StoryEvent::StoryDescriptionSet {
                    at: "2026-03-13T00:02:00Z".to_string(),
                    description: "Final draft".to_string(),
                },
            ],
            &state_map(),
        )
        .unwrap();

        assert_eq!(story.description.as_deref(), Some("Final draft"));
        assert_eq!(story.updated_at, "2026-03-13T00:02:00Z");
    }

    #[test]
    fn snapshot_without_description_deserializes() {
        let json = r#"{
            "id": "SH-1",
            "title": "Legacy snapshot",
            "created_at": "2026-03-13T00:00:00Z",
            "updated_at": "2026-03-13T00:00:00Z",
            "state": "todo",
            "superstate": "OPEN"
        }"#;

        let story: StorySnapshot = serde_json::from_str(json).unwrap();

        assert_eq!(story.description, None);
    }

    #[test]
    fn has_children_true_for_parent_of() {
        let stories = sample_story_map();
        assert!(has_children(stories.get("SH-1").unwrap()));
    }

    #[test]
    fn has_children_false_for_leaf() {
        let stories = sample_story_map();
        assert!(!has_children(stories.get("SH-3").unwrap()));
        assert!(!has_children(stories.get("SH-4").unwrap()));
    }

    #[test]
    fn compute_progress_returns_rollup_for_parent() {
        let stories = sample_story_map();
        let progress = compute_progress(stories.get("SH-1").unwrap(), &stories);
        assert!(progress.is_some());
        let p = progress.unwrap();
        assert_eq!(p.children_total, 1);
        assert_eq!(p.children_done, 0);
    }

    #[test]
    fn compute_progress_counts_closed_children() {
        let mut stories = sample_story_map();
        // Close child SH-2
        stories.get_mut("SH-2").unwrap().superstate = SuperState::Closed;
        stories.get_mut("SH-2").unwrap().state = "done".to_string();

        let progress = compute_progress(stories.get("SH-1").unwrap(), &stories);
        let p = progress.unwrap();
        assert_eq!(p.children_total, 1);
        assert_eq!(p.children_done, 1);
    }

    #[test]
    fn compute_progress_returns_none_for_leaf() {
        let stories = sample_story_map();
        let progress = compute_progress(stories.get("SH-4").unwrap(), &stories);
        assert!(progress.is_none());
    }

    #[test]
    fn compute_progress_only_counts_direct_children() {
        let stories = sample_story_map();
        // SH-1 is parent-of SH-2, SH-2 is parent-of SH-3
        // SH-1 should only count SH-2 as direct child, not SH-3
        let progress = compute_progress(stories.get("SH-1").unwrap(), &stories);
        let p = progress.unwrap();
        assert_eq!(p.children_total, 1);
    }
}

#[cfg(test)]
mod event_kind_tests {
    use super::*;

    /// Reads the variant list back out of serde's own derive and compares it to
    /// [`EVENT_KINDS`], so adding a `StoryEvent` variant without listing its tag
    /// here is a red test rather than a legacy log the importer quietly
    /// misfiles as corrupt.
    #[test]
    fn every_known_kind_is_a_variant_and_every_variant_is_known() {
        let error = serde_json::from_str::<StoryEvent>(r#"{"kind":"NoSuchEventKind"}"#)
            .expect_err("a made-up kind must not deserialize");
        let message = error.to_string();
        let (_, listed) = message
            .split_once("expected one of ")
            .unwrap_or_else(|| panic!("serde no longer lists the variants it expected: {message}"));
        // serde appends ` at line 1 column N` after the list; the last variant
        // is the text between the final pair of backticks.
        let last_backtick = listed
            .rfind('`')
            .unwrap_or_else(|| panic!("serde's variant list is no longer backticked: {message}"));
        let from_serde: Vec<String> = listed[..=last_backtick]
            .split(',')
            .map(|name| name.trim().trim_matches('`').to_string())
            .filter(|name| !name.is_empty())
            .collect();

        assert_eq!(
            from_serde,
            EVENT_KINDS.to_vec(),
            "EVENT_KINDS must list exactly the variants `StoryEvent` defines, in order"
        );
    }

    /// `event_kind` must agree with the tag serde actually writes, or the
    /// store's `kind` column describes a payload it does not match.
    #[test]
    fn every_variants_tag_is_the_one_serde_writes() {
        for event in [
            StoryEvent::StoryCreated {
                at: "t".into(),
                title: "t".into(),
                state: "todo".into(),
            },
            StoryEvent::StoryRelationshipRemoved {
                at: "t".into(),
                other_id: "SH-2".into(),
                relation: "child-of".into(),
            },
            StoryEvent::StoryDeleted {
                at: "t".into(),
                reason: "r".into(),
            },
        ] {
            let encoded: serde_json::Value = serde_json::to_value(&event).unwrap();
            assert_eq!(
                encoded["kind"].as_str().unwrap(),
                event_kind(&event),
                "event_kind disagrees with serde for {event:?}"
            );
            assert!(is_known_event_kind(event_kind(&event)));
        }
    }

    #[test]
    fn a_kind_from_a_newer_storyhook_is_not_known() {
        assert!(is_known_event_kind("StoryCreated"));
        assert!(!is_known_event_kind("StoryPinned"));
        assert!(!is_known_event_kind("storycreated"));
    }
}
