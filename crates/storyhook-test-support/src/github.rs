//! An in-memory [`GithubApi`] for exercising `story pr-check` without a
//! network (SH-49).
//!
//! Until SH-408 this fake also drove the story↔GitHub-Issues sync engine's
//! own orchestration tests (issues, comments, labels) — see the council
//! verdict recorded on SH-158 (`story show SH-158`) for that design. That
//! engine is retired; what remains is the one call `pr_check::run_check`
//! makes, seeded reads and recorded calls only, no pagination or
//! rate-limit simulation.
//!
//! [`FakeGithubApiFactory::build`] always hands back a handle into the same
//! shared state (`Rc<RefCell<...>>`), regardless of the token/owner/repo
//! arguments passed in, so a test asserting on state after checking several
//! repositories' links sees every client's calls in one place.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use storyhook::error::AppError;
use storyhook::github::api::{GithubApi, GithubApiFactory};
use storyhook::github::types::PullRequestStatus;

/// One call the engine made against the fake, in the order it made them.
#[derive(Debug, Clone)]
pub enum RecordedCall {
    GetPullRequest(u64),
}

struct FakeGithubApiState {
    recorded: Vec<RecordedCall>,
    /// Pull requests seeded via `seed_pull_request`, keyed by number.
    pull_requests: BTreeMap<u64, PullRequestStatus>,
}

impl FakeGithubApiState {
    fn new() -> Self {
        Self {
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

    /// Every call the engine made, across every client this factory built, in
    /// order.
    #[must_use]
    pub fn recorded_calls(&self) -> Vec<RecordedCall> {
        self.state.borrow().recorded.clone()
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
