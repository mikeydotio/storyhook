use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct GithubIssue {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub state: String,
    pub state_reason: Option<String>,
    pub labels: Vec<GithubLabel>,
    #[serde(default)]
    pub assignees: Vec<GithubUser>,
    pub milestone: Option<GithubMilestone>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub pull_request: Option<GithubPullRef>,
    #[serde(default)]
    pub comments: u64,
}

impl GithubIssue {
    pub fn is_pull_request(&self) -> bool {
        self.pull_request.is_some()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubLabel {
    pub name: String,
    pub color: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubUser {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubMilestone {
    pub title: String,
    pub number: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubPullRef {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubComment {
    pub id: u64,
    pub body: String,
    pub user: GithubUser,
    pub created_at: String,
    pub updated_at: String,
}

// Timeline event types

#[derive(Debug, Clone, Deserialize)]
pub struct TimelineEvent {
    pub event: Option<String>,
    pub created_at: Option<String>,
    // labeled/unlabeled
    pub label: Option<GithubLabel>,
    // renamed
    pub rename: Option<RenameEvent>,
    // assigned/unassigned
    pub assignee: Option<GithubUser>,
    // milestoned/demilestoned
    pub milestone: Option<GithubMilestone>,
    // commented (inline comment data)
    pub body: Option<String>,
    pub user: Option<GithubUser>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RenameEvent {
    pub from: String,
    pub to: String,
}

// Request types

#[derive(Debug, Clone, Serialize, Default)]
pub struct CreateIssueRequest {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignees: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub milestone: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct UpdateIssueRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignees: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub milestone: Option<u64>,
}
