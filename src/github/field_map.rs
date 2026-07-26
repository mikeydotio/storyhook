use std::collections::BTreeMap;

use crate::domain::{
    Member, Priority, StateDef, StoryComment, StoryRelation, StorySnapshot, SuperState,
};
use crate::github::body_block::{BlockRelationship, StoryhookBlock, extract_block, render_block};
use crate::github::diff::FieldUpdates;
use crate::github::types::{CreateIssueRequest, GithubComment, GithubIssue, UpdateIssueRequest};

/// Native relationship types that use GitHub's own APIs (sub-issues, task lists)
/// and should NOT be stored in the storyhook body block.
const NATIVE_RELATIONS: &[&str] = &["parent-of", "child-of", "blocks", "blocked-by"];

fn is_native_relation(relation: &str) -> bool {
    NATIVE_RELATIONS.contains(&relation)
}

/// A snapshot of story-relevant fields extracted from a GitHub Issue.
#[derive(Debug, Clone)]
pub struct RemoteSnapshot {
    pub title: String,
    pub state: String,
    pub superstate: SuperState,
    pub assignee: Option<String>,
    pub priority: Priority,
    pub awaiting: Option<String>,
    pub labels: Vec<String>,
    pub body_text: Option<String>,
    pub story_id: Option<String>,
    pub non_native_relationships: Vec<StoryRelation>,
}

// ---------------------------------------------------------------------------
// 1. Story -> CreateIssueRequest
// ---------------------------------------------------------------------------

/// Build a [`CreateIssueRequest`] from a [`StorySnapshot`].
///
/// The body includes the fenced storyhook code block for non-native fields.
pub fn story_to_create_request(
    story: &StorySnapshot,
    members: &[Member],
    description: Option<&str>,
) -> CreateIssueRequest {
    let block = story_to_block(story);
    let body_text = description.unwrap_or("");
    let body = render_block(body_text, &block);

    let assignees = story
        .assignee
        .as_ref()
        .and_then(|member_id| resolve_github_handle(member_id, members))
        .map(|handle| vec![handle]);

    let labels = if story.labels.is_empty() {
        None
    } else {
        Some(story.labels.clone())
    };

    CreateIssueRequest {
        title: story.title.clone(),
        body: Some(body),
        assignees,
        labels,
        milestone: None,
    }
}

// ---------------------------------------------------------------------------
// 2. GithubIssue -> RemoteSnapshot
// ---------------------------------------------------------------------------

/// Extract story-relevant fields from a [`GithubIssue`].
///
/// Returns a [`RemoteSnapshot`] that can be compared against local state.
/// The `state` slug is determined by mapping GitHub's open/closed to the
/// appropriate storyhook state via `states`. The `prefix` is currently
/// reserved for future use (e.g. filtering label namespaces).
pub fn issue_to_remote_snapshot(
    issue: &GithubIssue,
    states: &[StateDef],
    members: &[Member],
    _prefix: &str,
) -> RemoteSnapshot {
    // --- state mapping ---
    let (state_slug, superstate) = map_github_state(&issue.state, states);

    // --- assignee mapping ---
    let assignee = issue
        .assignees
        .first()
        .and_then(|gh_user| resolve_member_id(&gh_user.login, members));

    // --- labels ---
    let labels: Vec<String> = issue.labels.iter().map(|l| l.name.clone()).collect();

    // --- body block parsing ---
    let (body_text, story_id, priority, awaiting, non_native_relationships) =
        match issue.body.as_deref() {
            Some(body) => match extract_block(body) {
                Some((text, block)) => {
                    let priority = block
                        .priority
                        .as_deref()
                        .and_then(Priority::parse)
                        .unwrap_or(Priority::None);

                    let relationships: Vec<StoryRelation> = block
                        .relationships
                        .iter()
                        .flat_map(block_relationship_to_story_relations)
                        .collect();

                    let text_opt = if text.is_empty() { None } else { Some(text) };

                    (
                        text_opt,
                        Some(block.story_id),
                        priority,
                        block.awaiting,
                        relationships,
                    )
                }
                None => {
                    let text = if body.trim().is_empty() {
                        None
                    } else {
                        Some(body.to_string())
                    };
                    (text, None, Priority::None, None, Vec::new())
                }
            },
            None => (None, None, Priority::None, None, Vec::new()),
        };

    RemoteSnapshot {
        title: issue.title.clone(),
        state: state_slug,
        superstate,
        assignee,
        priority,
        awaiting,
        labels,
        body_text,
        story_id,
        non_native_relationships,
    }
}

// ---------------------------------------------------------------------------
// 3. FieldUpdates -> UpdateIssueRequest
// ---------------------------------------------------------------------------

/// Convert [`FieldUpdates`] (from the diff engine) into a GitHub
/// [`UpdateIssueRequest`].
pub fn updates_to_issue_request(
    updates: &FieldUpdates,
    story: &StorySnapshot,
    members: &[Member],
    states: &[StateDef],
) -> UpdateIssueRequest {
    let title = updates.title.clone();

    let state = updates.state.as_ref().map(|slug| {
        let sd = states.iter().find(|s| s.slug == *slug);
        match sd.map(|s| &s.super_state) {
            Some(SuperState::Open) => "open".to_string(),
            Some(SuperState::Closed) => "closed".to_string(),
            None => "open".to_string(),
        }
    });

    let state_reason = updates.state.as_ref().and_then(|slug| {
        let sd = states.iter().find(|s| s.slug == *slug);
        match sd.map(|s| &s.super_state) {
            Some(SuperState::Closed) => Some("completed".to_string()),
            _ => None,
        }
    });

    let assignees = updates.assignee.as_ref().map(|opt| match opt {
        Some(member_id) => resolve_github_handle(member_id, members)
            .map(|h| vec![h])
            .unwrap_or_default(),
        None => vec![],
    });

    let labels = updates.labels.clone();

    // If priority, awaiting, or description changed, we need to re-render the
    // body block.
    let body = if updates.priority.is_some()
        || updates.awaiting.is_some()
        || updates.description.is_some()
    {
        // Build the block from the current story state, then overlay the updates.
        let mut block = story_to_block(story);
        if let Some(ref prio) = updates.priority {
            block.priority = if *prio == Priority::None {
                None
            } else {
                Some(prio.as_str().to_string())
            };
        }
        if let Some(ref aw) = updates.awaiting {
            block.awaiting = aw.clone();
        }
        // The free-text body is the description when it changed, or the
        // story's current description otherwise — never blank it out just
        // because a different field (e.g. priority) triggered this rebuild.
        let body_text = match &updates.description {
            Some(new_description) => new_description.clone().unwrap_or_default(),
            None => story.description.clone().unwrap_or_default(),
        };
        Some(render_block(&body_text, &block))
    } else {
        None
    };

    UpdateIssueRequest {
        title,
        body,
        state,
        state_reason,
        assignees,
        labels,
        milestone: None,
    }
}

// ---------------------------------------------------------------------------
// 4. Story -> StoryhookBlock
// ---------------------------------------------------------------------------

/// Build a [`StoryhookBlock`] from a [`StorySnapshot`] for embedding in the
/// issue body. Only includes non-native relationships.
pub fn story_to_block(story: &StorySnapshot) -> StoryhookBlock {
    let priority = if story.priority == Priority::None {
        None
    } else {
        Some(story.priority.as_str().to_string())
    };

    let relationships: Vec<BlockRelationship> = story
        .relationships
        .iter()
        .filter(|r| !is_native_relation(&r.relation))
        .map(|r| BlockRelationship {
            entries: BTreeMap::from([(r.relation.clone(), r.other_id.clone())]),
        })
        .collect();

    StoryhookBlock {
        story_id: story.id.clone(),
        priority,
        awaiting: story.awaiting.clone(),
        relationships,
        sync_version: None,
    }
}

// ---------------------------------------------------------------------------
// 5. Comment mapping
// ---------------------------------------------------------------------------

/// Format a local story comment for posting to GitHub.
/// Prefixes with `[storyhook]` to identify sync-generated comments.
pub fn format_comment_for_github(comment: &StoryComment) -> String {
    format!("[storyhook] {}", comment.text)
}

/// Convert a GitHub comment to a [`StoryComment`].
/// Prefixes with `[github]` to identify sync-generated comments.
pub fn github_comment_to_story(comment: &GithubComment) -> StoryComment {
    StoryComment {
        at: comment.created_at.clone(),
        text: format!("[github] {}", comment.body),
    }
}

/// Check if a comment was generated by sync (has `[storyhook]` or `[github]` prefix).
pub fn is_sync_generated_comment(text: &str) -> bool {
    text.starts_with("[storyhook]") || text.starts_with("[github]")
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Map a GitHub state string ("open" / "closed") to the appropriate storyhook
/// state slug and [`SuperState`], using the first matching state definition.
fn map_github_state(github_state: &str, states: &[StateDef]) -> (String, SuperState) {
    let target_super = if github_state == "closed" {
        SuperState::Closed
    } else {
        SuperState::Open
    };

    states
        .iter()
        .find(|s| s.super_state == target_super)
        .map(|s| (s.slug.clone(), s.super_state.clone()))
        .unwrap_or_else(|| ("unknown".to_string(), SuperState::Open))
}

/// Find the GitHub handle for a storyhook member id.
fn resolve_github_handle(member_id: &str, members: &[Member]) -> Option<String> {
    members
        .iter()
        .find(|m| m.id == member_id)
        .and_then(|m| m.github.clone())
}

/// Find the storyhook member id for a GitHub login (case-insensitive).
fn resolve_member_id(github_login: &str, members: &[Member]) -> Option<String> {
    let login_lower = github_login.to_ascii_lowercase();
    members
        .iter()
        .find(|m| {
            m.github
                .as_deref()
                .map(|g| g.to_ascii_lowercase() == login_lower)
                .unwrap_or(false)
        })
        .map(|m| m.id.clone())
}

/// Convert a [`BlockRelationship`] to [`StoryRelation`]s (one per entry in
/// the flattened map, though in practice each block relationship has exactly
/// one entry).
fn block_relationship_to_story_relations(br: &BlockRelationship) -> Vec<StoryRelation> {
    br.entries
        .iter()
        .map(|(relation, other_id)| StoryRelation {
            relation: relation.clone(),
            other_id: other_id.clone(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::types::{GithubLabel, GithubUser};

    fn test_members() -> Vec<Member> {
        vec![
            Member {
                id: "alice".to_string(),
                display_name: "Alice Smith".to_string(),
                email: Some("alice@example.com".to_string()),
                github: Some("alicegithub".to_string()),
                created_at: "2026-01-01T00:00:00Z".to_string(),
            },
            Member {
                id: "bob".to_string(),
                display_name: "Bob Jones".to_string(),
                email: None,
                github: Some("BobGH".to_string()),
                created_at: "2026-01-01T00:00:00Z".to_string(),
            },
        ]
    }

    fn test_states() -> Vec<StateDef> {
        vec![
            StateDef {
                slug: "todo".to_string(),
                super_state: SuperState::Open,
                role: None,
                description: None,
            },
            StateDef {
                slug: "in-progress".to_string(),
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
    }

    fn test_story() -> StorySnapshot {
        StorySnapshot {
            id: "SH-42".to_string(),
            title: "Implement field mapping".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-02T00:00:00Z".to_string(),
            state: "todo".to_string(),
            superstate: SuperState::Open,
            assignee: Some("alice".to_string()),
            awaiting: Some("code review".to_string()),
            comments: vec![StoryComment {
                at: "2026-01-01T12:00:00Z".to_string(),
                text: "Started work".to_string(),
            }],
            relationships: vec![
                StoryRelation {
                    relation: "relates-to".to_string(),
                    other_id: "SH-10".to_string(),
                },
                StoryRelation {
                    relation: "obviates".to_string(),
                    other_id: "SH-15".to_string(),
                },
                StoryRelation {
                    relation: "parent-of".to_string(),
                    other_id: "SH-43".to_string(),
                },
                StoryRelation {
                    relation: "blocks".to_string(),
                    other_id: "SH-50".to_string(),
                },
            ],
            priority: Priority::High,
            labels: vec!["bug".to_string(), "frontend".to_string()],
            story_type: None,
            description: None,
            closed_at: None,
            deleted: false,
            deleted_reason: None,
        }
    }

    // -----------------------------------------------------------------------
    // story_to_create_request
    // -----------------------------------------------------------------------

    #[test]
    fn story_to_create_request_all_fields() {
        let story = test_story();
        let members = test_members();
        let req = story_to_create_request(&story, &members, Some("A detailed description."));

        assert_eq!(req.title, "Implement field mapping");

        // Assignee should be mapped to github handle
        assert_eq!(req.assignees, Some(vec!["alicegithub".to_string()]));

        // Labels
        assert_eq!(
            req.labels,
            Some(vec!["bug".to_string(), "frontend".to_string()])
        );

        // Body should contain the description and the storyhook block
        let body = req.body.unwrap();
        assert!(body.contains("A detailed description."));
        assert!(body.contains("```storyhook"));
        assert!(body.contains("story_id: SH-42"));
        assert!(body.contains("priority: high"));
        assert!(body.contains("awaiting: code review"));

        // Non-native relationships should be in the block
        assert!(body.contains("relates-to"));
        assert!(body.contains("SH-10"));
        assert!(body.contains("obviates"));
        assert!(body.contains("SH-15"));

        // Native relationships should NOT be in the block
        assert!(!body.contains("parent-of"));
        assert!(!body.contains("SH-43"));
        // "blocks" could appear as "blocks:" in YAML but parent-of/child-of should be absent
        // We check the block specifically
        let (_, block) = extract_block(&body).unwrap();
        let block_relations: Vec<String> = block
            .relationships
            .iter()
            .flat_map(|r| r.entries.keys().cloned())
            .collect();
        assert!(!block_relations.contains(&"parent-of".to_string()));
        assert!(!block_relations.contains(&"child-of".to_string()));
        assert!(!block_relations.contains(&"blocks".to_string()));
        assert!(!block_relations.contains(&"blocked-by".to_string()));
    }

    #[test]
    fn story_to_create_request_no_description() {
        let story = test_story();
        let members = test_members();
        let req = story_to_create_request(&story, &members, None);

        let body = req.body.unwrap();
        // Body should still have the storyhook block
        assert!(body.contains("```storyhook"));
    }

    #[test]
    fn story_to_create_request_no_assignee() {
        let mut story = test_story();
        story.assignee = None;
        let members = test_members();
        let req = story_to_create_request(&story, &members, None);

        assert!(req.assignees.is_none());
    }

    #[test]
    fn story_to_create_request_no_labels() {
        let mut story = test_story();
        story.labels.clear();
        let members = test_members();
        let req = story_to_create_request(&story, &members, None);

        assert!(req.labels.is_none());
    }

    // -----------------------------------------------------------------------
    // issue_to_remote_snapshot — with storyhook block
    // -----------------------------------------------------------------------

    #[test]
    fn issue_to_remote_snapshot_with_block() {
        let body = [
            "Human-written description.",
            "",
            "---",
            "",
            "```storyhook",
            "story_id: SH-42",
            "priority: high",
            "awaiting: code review",
            "relationships:",
            "  - relates-to: SH-10",
            "  - obviates: SH-15",
            "```",
            "",
        ]
        .join("\n");

        let issue = GithubIssue {
            number: 7,
            title: "Test issue".to_string(),
            body: Some(body),
            state: "open".to_string(),
            state_reason: None,
            labels: vec![GithubLabel {
                name: "bug".to_string(),
                color: None,
            }],
            assignees: vec![GithubUser {
                login: "alicegithub".to_string(),
            }],
            milestone: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-02T00:00:00Z".to_string(),
            closed_at: None,
            pull_request: None,
            comments: 0,
        };

        let snap = issue_to_remote_snapshot(&issue, &test_states(), &test_members(), "SH");

        assert_eq!(snap.title, "Test issue");
        assert_eq!(snap.state, "todo");
        assert_eq!(snap.superstate, SuperState::Open);
        assert_eq!(snap.assignee, Some("alice".to_string()));
        assert_eq!(snap.priority, Priority::High);
        assert_eq!(snap.awaiting.as_deref(), Some("code review"));
        assert_eq!(snap.labels, vec!["bug".to_string()]);
        assert_eq!(
            snap.body_text.as_deref(),
            Some("Human-written description.")
        );
        assert_eq!(snap.story_id, Some("SH-42".to_string()));
        assert_eq!(snap.non_native_relationships.len(), 2);
        assert!(snap.non_native_relationships.contains(&StoryRelation {
            relation: "relates-to".to_string(),
            other_id: "SH-10".to_string(),
        }));
        assert!(snap.non_native_relationships.contains(&StoryRelation {
            relation: "obviates".to_string(),
            other_id: "SH-15".to_string(),
        }));
    }

    // -----------------------------------------------------------------------
    // issue_to_remote_snapshot — without storyhook block
    // -----------------------------------------------------------------------

    #[test]
    fn issue_to_remote_snapshot_without_block() {
        let issue = GithubIssue {
            number: 8,
            title: "Plain issue".to_string(),
            body: Some("Just a normal issue body.".to_string()),
            state: "closed".to_string(),
            state_reason: Some("completed".to_string()),
            labels: vec![],
            assignees: vec![],
            milestone: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-02T00:00:00Z".to_string(),
            closed_at: Some("2026-01-02T00:00:00Z".to_string()),
            pull_request: None,
            comments: 0,
        };

        let snap = issue_to_remote_snapshot(&issue, &test_states(), &test_members(), "SH");

        assert_eq!(snap.title, "Plain issue");
        assert_eq!(snap.state, "done");
        assert_eq!(snap.superstate, SuperState::Closed);
        assert!(snap.assignee.is_none());
        assert_eq!(snap.priority, Priority::None);
        assert!(snap.awaiting.is_none());
        assert!(snap.labels.is_empty());
        assert_eq!(snap.body_text.as_deref(), Some("Just a normal issue body."));
        assert!(snap.story_id.is_none());
        assert!(snap.non_native_relationships.is_empty());
    }

    #[test]
    fn issue_to_remote_snapshot_no_body() {
        let issue = GithubIssue {
            number: 9,
            title: "No body".to_string(),
            body: None,
            state: "open".to_string(),
            state_reason: None,
            labels: vec![],
            assignees: vec![],
            milestone: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            closed_at: None,
            pull_request: None,
            comments: 0,
        };

        let snap = issue_to_remote_snapshot(&issue, &test_states(), &test_members(), "SH");

        assert!(snap.body_text.is_none());
        assert!(snap.story_id.is_none());
    }

    // -----------------------------------------------------------------------
    // State mapping
    // -----------------------------------------------------------------------

    #[test]
    fn state_mapping_open() {
        let (slug, ss) = map_github_state("open", &test_states());
        assert_eq!(slug, "todo"); // first OPEN state
        assert_eq!(ss, SuperState::Open);
    }

    #[test]
    fn state_mapping_closed() {
        let (slug, ss) = map_github_state("closed", &test_states());
        assert_eq!(slug, "done"); // first CLOSED state
        assert_eq!(ss, SuperState::Closed);
    }

    // -----------------------------------------------------------------------
    // Assignee mapping
    // -----------------------------------------------------------------------

    #[test]
    fn assignee_resolve_case_insensitive() {
        let members = test_members();
        // BobGH stored in member, try lowercase
        let id = resolve_member_id("bobgh", &members);
        assert_eq!(id, Some("bob".to_string()));
    }

    #[test]
    fn assignee_resolve_github_handle() {
        let members = test_members();
        let handle = resolve_github_handle("alice", &members);
        assert_eq!(handle, Some("alicegithub".to_string()));
    }

    #[test]
    fn assignee_resolve_unknown() {
        let members = test_members();
        let id = resolve_member_id("unknownuser", &members);
        assert!(id.is_none());
    }

    // -----------------------------------------------------------------------
    // story_to_block — non-native only
    // -----------------------------------------------------------------------

    #[test]
    fn story_to_block_non_native_only() {
        let story = test_story();
        let block = story_to_block(&story);

        assert_eq!(block.story_id, "SH-42");
        assert_eq!(block.priority, Some("high".to_string()));
        assert_eq!(block.awaiting, Some("code review".to_string()));

        // Should have only non-native relations
        let relation_types: Vec<String> = block
            .relationships
            .iter()
            .flat_map(|r| r.entries.keys().cloned())
            .collect();
        assert!(relation_types.contains(&"relates-to".to_string()));
        assert!(relation_types.contains(&"obviates".to_string()));
        assert!(!relation_types.contains(&"parent-of".to_string()));
        assert!(!relation_types.contains(&"blocks".to_string()));
        assert_eq!(block.relationships.len(), 2);
    }

    #[test]
    fn story_to_block_no_priority() {
        let mut story = test_story();
        story.priority = Priority::None;
        let block = story_to_block(&story);
        assert!(block.priority.is_none());
    }

    // -----------------------------------------------------------------------
    // Comment formatting and detection
    // -----------------------------------------------------------------------

    #[test]
    fn format_comment_for_github_adds_prefix() {
        let comment = StoryComment {
            at: "2026-01-01T00:00:00Z".to_string(),
            text: "This is a local comment".to_string(),
        };
        let formatted = format_comment_for_github(&comment);
        assert_eq!(formatted, "[storyhook] This is a local comment");
    }

    #[test]
    fn github_comment_to_story_adds_prefix() {
        let gh_comment = GithubComment {
            id: 123,
            body: "This is a GitHub comment".to_string(),
            user: GithubUser {
                login: "octocat".to_string(),
            },
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let story_comment = github_comment_to_story(&gh_comment);
        assert_eq!(story_comment.text, "[github] This is a GitHub comment");
        assert_eq!(story_comment.at, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn is_sync_generated_storyhook() {
        assert!(is_sync_generated_comment("[storyhook] some text"));
    }

    #[test]
    fn is_sync_generated_github() {
        assert!(is_sync_generated_comment("[github] some text"));
    }

    #[test]
    fn is_sync_generated_normal() {
        assert!(!is_sync_generated_comment("Just a normal comment"));
    }

    #[test]
    fn is_sync_generated_partial() {
        assert!(!is_sync_generated_comment("storyhook] missing bracket"));
        assert!(!is_sync_generated_comment("github] missing bracket"));
    }

    // -----------------------------------------------------------------------
    // updates_to_issue_request
    // -----------------------------------------------------------------------

    #[test]
    fn updates_to_issue_request_title_and_state() {
        let updates = FieldUpdates {
            title: Some("New title".to_string()),
            state: Some("done".to_string()),
            ..Default::default()
        };

        let story = test_story();
        let req = updates_to_issue_request(&updates, &story, &test_members(), &test_states());

        assert_eq!(req.title, Some("New title".to_string()));
        assert_eq!(req.state, Some("closed".to_string()));
        assert_eq!(req.state_reason, Some("completed".to_string()));
    }

    #[test]
    fn updates_to_issue_request_assignee_set() {
        let updates = FieldUpdates {
            assignee: Some(Some("bob".to_string())),
            ..Default::default()
        };

        let story = test_story();
        let req = updates_to_issue_request(&updates, &story, &test_members(), &test_states());

        assert_eq!(req.assignees, Some(vec!["BobGH".to_string()]));
    }

    #[test]
    fn updates_to_issue_request_assignee_cleared() {
        let updates = FieldUpdates {
            assignee: Some(None),
            ..Default::default()
        };

        let story = test_story();
        let req = updates_to_issue_request(&updates, &story, &test_members(), &test_states());

        assert_eq!(req.assignees, Some(vec![]));
    }

    #[test]
    fn updates_to_issue_request_priority_triggers_body_update() {
        let updates = FieldUpdates {
            priority: Some(Priority::Critical),
            ..Default::default()
        };

        let story = test_story();
        let req = updates_to_issue_request(&updates, &story, &test_members(), &test_states());

        let body = req.body.unwrap();
        assert!(body.contains("priority: critical"));
    }

    #[test]
    fn updates_to_issue_request_awaiting_triggers_body_update() {
        let updates = FieldUpdates {
            awaiting: Some(Some("design review".to_string())),
            ..Default::default()
        };

        let story = test_story();
        let req = updates_to_issue_request(&updates, &story, &test_members(), &test_states());

        let body = req.body.unwrap();
        assert!(body.contains("awaiting: design review"));
    }

    #[test]
    fn updates_to_issue_request_description_triggers_body_update() {
        let updates = FieldUpdates {
            description: Some(Some("New free text".to_string())),
            ..Default::default()
        };

        let story = test_story();
        let req = updates_to_issue_request(&updates, &story, &test_members(), &test_states());

        let body = req.body.unwrap();
        assert!(body.starts_with("New free text"));
        assert!(body.contains("```storyhook"));
    }

    #[test]
    fn updates_to_issue_request_description_cleared_blanks_body_text() {
        let mut story = test_story();
        story.description = Some("Will be cleared".to_string());
        let updates = FieldUpdates {
            description: Some(None),
            ..Default::default()
        };

        let req = updates_to_issue_request(&updates, &story, &test_members(), &test_states());

        let body = req.body.unwrap();
        assert!(!body.contains("Will be cleared"));
    }

    #[test]
    fn updates_to_issue_request_priority_change_preserves_existing_description() {
        // Regression: rebuilding the body block for an unrelated field change
        // (priority here) must not blank out the story's existing description.
        let mut story = test_story();
        story.description = Some("Existing free text".to_string());
        let updates = FieldUpdates {
            priority: Some(Priority::Low),
            ..Default::default()
        };

        let req = updates_to_issue_request(&updates, &story, &test_members(), &test_states());

        let body = req.body.unwrap();
        assert!(body.contains("Existing free text"));
        assert!(body.contains("priority: low"));
    }

    #[test]
    fn updates_to_issue_request_empty_updates() {
        let updates = FieldUpdates::default();
        let story = test_story();
        let req = updates_to_issue_request(&updates, &story, &test_members(), &test_states());

        assert!(req.title.is_none());
        assert!(req.state.is_none());
        assert!(req.body.is_none());
        assert!(req.assignees.is_none());
        assert!(req.labels.is_none());
    }

    #[test]
    fn updates_to_issue_request_open_state() {
        let updates = FieldUpdates {
            state: Some("todo".to_string()),
            ..Default::default()
        };

        let story = test_story();
        let req = updates_to_issue_request(&updates, &story, &test_members(), &test_states());

        assert_eq!(req.state, Some("open".to_string()));
        assert!(req.state_reason.is_none());
    }
}
