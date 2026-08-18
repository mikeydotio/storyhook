use serde::Deserialize;

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
