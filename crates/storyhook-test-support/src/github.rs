//! An in-memory [`GithubApi`] for exercising the sync engine's orchestration
//! without a network — SH-158.
//!
//! See the council verdict this implements, recorded on SH-158
//! (`story show SH-158`).
//! In short: seeded reads and recorded writes only, no pagination, no
//! rate-limit or timeline simulation, and no per-call error-injection hook —
//! `SyncReport.errors` accumulation is reachable by simply not seeding an
//! issue a mapping points at, which needs no injection mechanism at all.
//!
//! [`FakeGithubApiFactory::build`] always hands back a handle into the same
//! shared state (`Rc<RefCell<...>>`), regardless of the token/owner/repo
//! arguments passed in — this is load-bearing: `run_sync_with` calls
//! `run_initial_setup` internally on an unconfigured project, and each
//! constructs its own client, so a test asserting on state after one
//! `run_sync_with` call needs both constructions to land in the same place.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use storyhook::error::AppError;
use storyhook::github::api::{GithubApi, GithubApiFactory};
use storyhook::github::types::{
    CreateIssueRequest, GithubComment, GithubIssue, GithubLabel, GithubUser, PullRequestStatus,
    UpdateIssueRequest,
};

/// One call the engine made against the fake, in the order it made them.
#[derive(Debug, Clone)]
pub enum RecordedCall {
    ValidateToken,
    ListIssues {
        since: Option<String>,
        state: String,
    },
    GetIssue(u64),
    CreateIssue(CreateIssueRequest),
    UpdateIssue(u64, UpdateIssueRequest),
    ListComments {
        issue_number: u64,
        since: Option<String>,
    },
    CreateComment {
        issue_number: u64,
        body: String,
    },
    GetPullRequest(u64),
}

struct FakeGithubApiState {
    next_issue_number: u64,
    next_comment_id: u64,
    issues: Vec<GithubIssue>,
    comments: BTreeMap<u64, Vec<GithubComment>>,
    recorded: Vec<RecordedCall>,
    /// Pull requests seeded via `seed_pull_request`, keyed by number.
    pull_requests: BTreeMap<u64, PullRequestStatus>,
}

impl FakeGithubApiState {
    fn new() -> Self {
        Self {
            next_issue_number: 1,
            next_comment_id: 1,
            issues: Vec::new(),
            comments: BTreeMap::new(),
            recorded: Vec::new(),
            pull_requests: BTreeMap::new(),
        }
    }
}

/// A seedable, inspectable [`GithubApiFactory`] backed by in-memory state
/// shared across every client it builds.
#[derive(Clone)]
pub struct FakeGithubApiFactory {
    state: Rc<RefCell<FakeGithubApiState>>,
}

impl Default for FakeGithubApiFactory {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeGithubApiFactory {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Rc::new(RefCell::new(FakeGithubApiState::new())),
        }
    }

    /// Seeds an open issue with the next auto-assigned number and no body,
    /// returning the issue as seeded.
    pub fn seed_issue(&self, title: &str) -> GithubIssue {
        self.seed_issue_with_body(title, None)
    }

    /// Seeds an open issue with an explicit body — for tests that need a
    /// storyhook block already present, or that simulate a remote edit
    /// against a base.
    pub fn seed_issue_with_body(&self, title: &str, body: Option<&str>) -> GithubIssue {
        let mut state = self.state.borrow_mut();
        let number = state.next_issue_number;
        state.next_issue_number += 1;
        let issue = GithubIssue {
            number,
            title: title.to_string(),
            body: body.map(str::to_string),
            state: "open".to_string(),
            state_reason: None,
            labels: Vec::new(),
            assignees: Vec::new(),
            milestone: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            closed_at: None,
            pull_request: None,
            comments: 0,
        };
        state.issues.push(issue.clone());
        issue
    }

    /// Overwrites a seeded issue's title in place — for tests simulating a
    /// remote edit made since a base snapshot was taken.
    pub fn set_issue_title(&self, issue_number: u64, title: &str) {
        let mut state = self.state.borrow_mut();
        if let Some(issue) = state.issues.iter_mut().find(|i| i.number == issue_number) {
            issue.title = title.to_string();
        }
    }

    /// Overwrites a seeded issue's labels in place — for tests that need a
    /// remote label already present (e.g. a comma-bearing one, SH-179)
    /// before the sync engine's first fetch.
    pub fn set_issue_labels(&self, issue_number: u64, labels: &[&str]) {
        let mut state = self.state.borrow_mut();
        if let Some(issue) = state.issues.iter_mut().find(|i| i.number == issue_number) {
            issue.labels = labels
                .iter()
                .map(|name| GithubLabel {
                    name: (*name).to_string(),
                    color: None,
                })
                .collect();
        }
    }

    /// Every call the engine made, across every client this factory built, in
    /// order.
    #[must_use]
    pub fn recorded_calls(&self) -> Vec<RecordedCall> {
        self.state.borrow().recorded.clone()
    }

    /// Issues created via `create_issue`, in the order they were created.
    #[must_use]
    pub fn created_issues(&self) -> Vec<CreateIssueRequest> {
        self.recorded_calls()
            .into_iter()
            .filter_map(|call| match call {
                RecordedCall::CreateIssue(req) => Some(req),
                _ => None,
            })
            .collect()
    }

    /// The issue at `number`, as the fake currently has it — after whatever
    /// `create_issue`/`update_issue` calls have landed.
    #[must_use]
    pub fn issue(&self, number: u64) -> Option<GithubIssue> {
        self.state
            .borrow()
            .issues
            .iter()
            .find(|i| i.number == number)
            .cloned()
    }

    /// Seeds a pull request's merge/close status (SH-49), for
    /// `get_pull_request` to answer with.
    pub fn seed_pull_request(&self, number: u64, state: &str, merged: bool) {
        self.state.borrow_mut().pull_requests.insert(
            number,
            PullRequestStatus {
                state: state.to_string(),
                merged,
            },
        );
    }
}

impl GithubApiFactory for FakeGithubApiFactory {
    fn build(&self, _token: String, _owner: String, _repo: String) -> Box<dyn GithubApi> {
        Box::new(FakeGithubApi {
            state: Rc::clone(&self.state),
        })
    }
}

struct FakeGithubApi {
    state: Rc<RefCell<FakeGithubApiState>>,
}

impl GithubApi for FakeGithubApi {
    fn validate_token(&self) -> Result<(), AppError> {
        self.state
            .borrow_mut()
            .recorded
            .push(RecordedCall::ValidateToken);
        Ok(())
    }

    fn list_issues(&self, since: Option<&str>, state: &str) -> Result<Vec<GithubIssue>, AppError> {
        let mut s = self.state.borrow_mut();
        s.recorded.push(RecordedCall::ListIssues {
            since: since.map(str::to_string),
            state: state.to_string(),
        });
        Ok(s.issues.clone())
    }

    fn get_issue(&self, number: u64) -> Result<GithubIssue, AppError> {
        let mut s = self.state.borrow_mut();
        s.recorded.push(RecordedCall::GetIssue(number));
        s.issues
            .iter()
            .find(|i| i.number == number)
            .cloned()
            .ok_or_else(|| {
                AppError::NotFound(format!(
                    "GitHub resource not found (HTTP 404): issue #{number}"
                ))
            })
    }

    fn create_issue(&self, req: &CreateIssueRequest) -> Result<GithubIssue, AppError> {
        let mut s = self.state.borrow_mut();
        s.recorded.push(RecordedCall::CreateIssue(req.clone()));
        let number = s.next_issue_number;
        s.next_issue_number += 1;
        let issue = GithubIssue {
            number,
            title: req.title.clone(),
            body: req.body.clone(),
            state: "open".to_string(),
            state_reason: None,
            labels: Vec::new(),
            assignees: Vec::new(),
            milestone: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            closed_at: None,
            pull_request: None,
            comments: 0,
        };
        s.issues.push(issue.clone());
        Ok(issue)
    }

    fn update_issue(&self, number: u64, req: &UpdateIssueRequest) -> Result<GithubIssue, AppError> {
        let mut s = self.state.borrow_mut();
        s.recorded
            .push(RecordedCall::UpdateIssue(number, req.clone()));
        let issue = s
            .issues
            .iter_mut()
            .find(|i| i.number == number)
            .ok_or_else(|| {
                AppError::NotFound(format!(
                    "GitHub resource not found (HTTP 404): issue #{number}"
                ))
            })?;
        if let Some(title) = &req.title {
            issue.title = title.clone();
        }
        if let Some(body) = &req.body {
            issue.body = Some(body.clone());
        }
        if let Some(state) = &req.state {
            issue.state = state.clone();
            if state == "closed" {
                issue.closed_at = Some("2026-01-01T00:00:00Z".to_string());
            }
        }
        Ok(issue.clone())
    }

    fn list_comments(
        &self,
        issue_number: u64,
        since: Option<&str>,
    ) -> Result<Vec<GithubComment>, AppError> {
        let mut s = self.state.borrow_mut();
        s.recorded.push(RecordedCall::ListComments {
            issue_number,
            since: since.map(str::to_string),
        });
        Ok(s.comments.get(&issue_number).cloned().unwrap_or_default())
    }

    fn create_comment(&self, issue_number: u64, body: &str) -> Result<GithubComment, AppError> {
        let mut s = self.state.borrow_mut();
        s.recorded.push(RecordedCall::CreateComment {
            issue_number,
            body: body.to_string(),
        });
        let id = s.next_comment_id;
        s.next_comment_id += 1;
        let comment = GithubComment {
            id,
            body: body.to_string(),
            user: GithubUser {
                login: "fake-bot".to_string(),
            },
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
        };
        s.comments
            .entry(issue_number)
            .or_default()
            .push(comment.clone());
        Ok(comment)
    }

    fn get_pull_request(&self, number: u64) -> Result<PullRequestStatus, AppError> {
        let mut s = self.state.borrow_mut();
        s.recorded.push(RecordedCall::GetPullRequest(number));
        s.pull_requests.get(&number).cloned().ok_or_else(|| {
            AppError::NotFound(format!(
                "GitHub resource not found (HTTP 404): pull request #{number}"
            ))
        })
    }
}
