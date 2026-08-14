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

/// A pull request's merge/close status, from `GET /repos/{owner}/{repo}/pulls/{number}`
/// (SH-49).
///
/// `merged` and `state` are independent fields on GitHub's own response: a
/// closed-without-merging PR reports `state: "closed", merged: false`, which
/// is exactly the tuple `PrLinkService::check` uses to distinguish
/// [`StoryPrClosed`](crate::domain::StoryEvent::StoryPrClosed) from
/// [`StoryPrMerged`](crate::domain::StoryEvent::StoryPrMerged).
#[derive(Debug, Clone, Deserialize)]
pub struct PullRequestStatus {
    /// `"open"` or `"closed"` — never `"merged"`; GitHub reports a merge
    /// through `merged` instead.
    pub state: String,
    /// Whether this pull request has been merged.
    pub merged: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubComment {
    pub id: u64,
    pub body: String,
    pub user: GithubUser,
    pub created_at: String,
    pub updated_at: String,
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
