//! Store-backed contracts for the SH-521 centralized verification queue.

use storyhook::daemon::verification::{
    ShellVerificationActuator, TickResult, VerificationActuator, VerificationOutcome, tick_with,
};
use storyhook::domain::remote::RemoteUrl;
use storyhook::domain::{Priority, SuperState};
use storyhook::env::Environment;
use storyhook::error::AppError;
use storyhook::service::{
    ConfigService, NewStoryInput, PrLinkService, StoryService,
    VERIFICATION_CLEANUP_COMPLETE_PREFIX, VERIFICATION_GREEN_PREFIX, VerificationCandidate,
    VerificationProblem, VerificationQueue,
};
use storyhook::store::{PrLink, ReadOps, Store, StoryNo, WriteOps};
use storyhook_test_support::ServiceFixture;
use storyhook_test_support::scratch_dir;

use std::process::Command;
use std::sync::Mutex;

const PR_ONE: &str = "https://github.com/acme/widgets/pull/1";
const PR_TWO: &str = "https://github.com/acme/widgets/pull/2";

fn submitted(fixture: &ServiceFixture, title: &str, priority: Priority, url: &str) -> String {
    let ctx = fixture.ctx();
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: title.into(),
            priority: Some(priority.as_str().to_string()),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id;
    PrLinkService::new(&ctx).link(&id, url, true).unwrap();
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    id
}

#[test]
fn the_queue_selects_the_highest_priority_verifying_story() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let low = submitted(&fixture, "older low", Priority::Low, PR_ONE);
    let high = submitted(&fixture, "newer high", Priority::High, PR_TWO);

    let selected = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();

    assert_eq!(selected.story_id, high);
    assert_ne!(selected.story_id, low);
    assert_eq!(selected.pull_request.unwrap().url, PR_TWO);
}

#[test]
fn equal_priority_and_time_use_story_identity_as_a_stable_tie_break() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let first = submitted(&fixture, "first", Priority::Medium, PR_ONE);
    submitted(&fixture, "second", Priority::Medium, PR_TWO);

    let selected = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(selected.story_id, first);
}

#[test]
fn a_submission_without_one_close_on_merge_pr_is_returned_as_ambiguous() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "missing PR".into(),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id;
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();

    let selected = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(selected.story_id, id);
    assert_eq!(
        selected.pull_request,
        Err(VerificationProblem::MissingPullRequest)
    );
}

#[test]
fn a_project_without_a_checkout_remains_visible_as_configuration_work() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "missing checkout".into(),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id;
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), None))
        .unwrap();

    let selected = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(selected.story_id, id);
    assert_eq!(
        selected.pull_request,
        Err(VerificationProblem::MissingCheckout)
    );
}

#[test]
fn a_submission_with_two_open_close_on_merge_prs_names_both() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "two PRs", Priority::High, PR_ONE);
    PrLinkService::new(&fixture.ctx())
        .link(&id, PR_TWO, true)
        .unwrap();

    let selected = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(
        selected.pull_request,
        Err(VerificationProblem::MultiplePullRequests(vec![
            PR_ONE.to_string(),
            PR_TWO.to_string(),
        ]))
    );
}

#[test]
fn a_link_is_revalidated_after_the_registered_repository_changes() {
    let fixture = ServiceFixture::new();
    let original = "https://github.com/acme/widgets";
    fixture.link_origin(original);
    let id = submitted(&fixture, "stale remote", Priority::High, PR_ONE);
    let original = RemoteUrl::normalize(original).unwrap();
    fixture
        .store()
        .write(|tx| tx.unlink_remote(fixture.project(), &original).map(|_| ()))
        .unwrap();
    fixture.link_origin("https://github.com/acme/replacement");

    let selected = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(selected.story_id, id);
    assert_eq!(
        selected.pull_request,
        Err(VerificationProblem::UnregisteredPullRequest {
            url: PR_ONE.to_string(),
            registered: vec!["acme/replacement".to_string()],
        })
    );
}

#[test]
fn recording_the_verified_merge_closes_the_story_and_the_pr_projection() {
    let fixture = ServiceFixture::new();
    let config_ctx = fixture.ctx();
    let config = ConfigService::new(&config_ctx);
    config
        .add_state("abandoned", SuperState::Closed, None, None)
        .unwrap();
    config
        .reorder_states(
            &[
                "todo",
                "in-progress",
                "verifying",
                "blocked",
                "abandoned",
                "done",
                "closed",
            ]
            .map(str::to_string),
        )
        .unwrap();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "verified", Priority::High, PR_ONE);
    let ctx = fixture.ctx();

    VerificationQueue::new(fixture.store())
        .record_merged(&ctx, &id, PR_ONE)
        .unwrap();

    let story_no = StoryNo::parse_id("SH", &id).unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), story_no))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "done");
    assert!(row.archived);
    let links = fixture
        .store()
        .read(|tx| tx.pr_links(fixture.project()))
        .unwrap();
    assert_eq!(links[0].1.status, "merged");
}

struct FakeActuator {
    outcome: VerificationOutcome,
    notification_error: Option<String>,
    notified: Mutex<Vec<String>>,
    reaped: Mutex<Vec<String>>,
}

impl VerificationActuator for FakeActuator {
    fn verify(
        &self,
        _candidate: &VerificationCandidate,
        _pull_request: &storyhook::store::PrLink,
    ) -> VerificationOutcome {
        self.outcome.clone()
    }

    fn notify(&self, candidate: &VerificationCandidate, message: &str) -> Result<(), AppError> {
        if let Some(error) = &self.notification_error {
            return Err(AppError::Storage(error.clone()));
        }
        self.notified
            .lock()
            .unwrap()
            .push(format!("{}:{message}", candidate.story_id));
        Ok(())
    }

    fn reap(&self, candidate: &VerificationCandidate) -> Result<(), AppError> {
        self.reaped.lock().unwrap().push(candidate.story_id.clone());
        Ok(())
    }
}

#[test]
fn a_conflict_returns_the_story_to_its_associated_agent() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "conflicted", Priority::High, PR_ONE);
    let root = scratch_dir();
    let env = Environment::at(root.path());
    let actuator = FakeActuator {
        outcome: VerificationOutcome::Conflict {
            detail: "both modified src/lib.rs".into(),
        },
        notification_error: None,
        notified: Mutex::new(Vec::new()),
        reaped: Mutex::new(Vec::new()),
    };

    assert_eq!(
        tick_with(fixture.store(), &env, &actuator).unwrap(),
        TickResult::Returned
    );
    let story_no = StoryNo::parse_id("SH", &id).unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), story_no))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "in-progress");
    assert!(
        row.snapshot
            .comments
            .last()
            .unwrap()
            .text
            .contains("CONFLICT")
    );
    assert_eq!(actuator.notified.lock().unwrap().len(), 1);
    assert!(actuator.reaped.lock().unwrap().is_empty());
}

#[test]
fn an_origin_mismatch_returns_the_story_for_a_safe_resubmission() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "wrong checkout", Priority::High, PR_ONE);
    let root = scratch_dir();
    let actuator = FakeActuator {
        outcome: VerificationOutcome::InvalidSubmission {
            detail: "checkout origin is acme/replacement".into(),
        },
        notification_error: None,
        notified: Mutex::new(Vec::new()),
        reaped: Mutex::new(Vec::new()),
    };

    assert_eq!(
        tick_with(fixture.store(), &Environment::at(root.path()), &actuator).unwrap(),
        TickResult::Returned
    );
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::parse_id("SH", &id).unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "in-progress");
    assert!(
        row.snapshot
            .comments
            .last()
            .unwrap()
            .text
            .contains("INVALID SUBMISSION")
    );
    assert_eq!(actuator.notified.lock().unwrap().len(), 1);
}

#[test]
fn the_shell_actuator_refuses_a_different_checkout_origin_before_running_github() {
    let fixture = ServiceFixture::new();
    let checkout = scratch_dir();
    let init = Command::new("git")
        .args(["init", "-q"])
        .current_dir(checkout.path())
        .output()
        .unwrap();
    assert!(init.status.success());
    let origin = Command::new("git")
        .args([
            "config",
            "remote.origin.url",
            "https://github.com/acme/replacement.git",
        ])
        .current_dir(checkout.path())
        .output()
        .unwrap();
    assert!(origin.status.success());
    let candidate = VerificationCandidate {
        project: fixture.project(),
        project_slug: "fixture".into(),
        story_id: "SH-1".into(),
        title: "wrong repository".into(),
        priority: Priority::High,
        created_at: "2026-01-01T00:00:00Z".into(),
        checkout: checkout.path().to_path_buf(),
        pull_request: Err(VerificationProblem::MissingPullRequest),
    };
    let pull_request = PrLink {
        owner: "acme".into(),
        repo: "widgets".into(),
        number: 1,
        url: PR_ONE.into(),
        close_on_merge: true,
        status: "open".into(),
        linked_at: "2026-01-01T00:00:00Z".into(),
        last_checked_at: None,
    };
    let env_root = scratch_dir();
    let actuator = ShellVerificationActuator::new(Environment::at(env_root.path()));

    let outcome = actuator.verify(&candidate, &pull_request);
    assert!(matches!(
        outcome,
        VerificationOutcome::InvalidSubmission { ref detail }
            if detail.contains("acme/replacement") && detail.contains("acme/widgets")
    ));
}

#[test]
fn an_unreachable_agent_is_marked_awaiting_instead_of_silently_retried() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "no pane", Priority::High, PR_ONE);
    let root = scratch_dir();
    let actuator = FakeActuator {
        outcome: VerificationOutcome::TestsFailed {
            tree: "deadbeef".into(),
            log: "/tmp/red.log".into(),
            detail: "one regression".into(),
        },
        notification_error: Some("pane unavailable".into()),
        notified: Mutex::new(Vec::new()),
        reaped: Mutex::new(Vec::new()),
    };

    tick_with(fixture.store(), &Environment::at(root.path()), &actuator).unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::parse_id("SH", &id).unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "in-progress");
    assert!(row.awaiting.unwrap().contains("pane unavailable"));
}

#[test]
fn identical_infrastructure_failures_are_deduplicated_and_remain_verifying() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "temporary outage", Priority::High, PR_ONE);
    let root = scratch_dir();
    let actuator = FakeActuator {
        outcome: VerificationOutcome::InfrastructureFailure {
            detail: "GitHub unavailable".into(),
        },
        notification_error: None,
        notified: Mutex::new(Vec::new()),
        reaped: Mutex::new(Vec::new()),
    };
    let env = Environment::at(root.path());

    tick_with(fixture.store(), &env, &actuator).unwrap();
    tick_with(fixture.store(), &env, &actuator).unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::parse_id("SH", &id).unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "verifying");
    assert_eq!(row.snapshot.comments.len(), 1);
}

#[test]
fn a_green_attempt_closes_then_reaps_the_story() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "green", Priority::High, PR_ONE);
    let root = scratch_dir();
    let env = Environment::at(root.path());
    let actuator = FakeActuator {
        outcome: VerificationOutcome::Merged {
            tree: "abc123".into(),
            detail: "landed".into(),
        },
        notification_error: None,
        notified: Mutex::new(Vec::new()),
        reaped: Mutex::new(Vec::new()),
    };

    assert_eq!(
        tick_with(fixture.store(), &env, &actuator).unwrap(),
        TickResult::Completed
    );
    let story_no = StoryNo::parse_id("SH", &id).unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), story_no))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "done");
    assert_eq!(actuator.reaped.lock().unwrap().as_slice(), [id]);
    assert!(row.snapshot.comments.iter().any(|comment| {
        comment
            .text
            .starts_with(VERIFICATION_CLEANUP_COMPLETE_PREFIX)
    }));
}

#[test]
fn a_restart_reaps_a_landed_story_without_repeating_completed_cleanup() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "landed before crash", Priority::High, PR_ONE);
    let ctx = fixture.ctx();
    StoryService::new(&ctx)
        .comment(
            &id,
            &format!(
                "{VERIFICATION_GREEN_PREFIX} merge tree `abc123` passed `make test` and pull request {PR_ONE} landed."
            ),
        )
        .unwrap();
    VerificationQueue::new(fixture.store())
        .record_merged(&ctx, &id, PR_ONE)
        .unwrap();
    let root = scratch_dir();
    let env = Environment::at(root.path());
    let actuator = FakeActuator {
        outcome: VerificationOutcome::InfrastructureFailure {
            detail: "verification must not run for cleanup".into(),
        },
        notification_error: None,
        notified: Mutex::new(Vec::new()),
        reaped: Mutex::new(Vec::new()),
    };

    assert_eq!(
        tick_with(fixture.store(), &env, &actuator).unwrap(),
        TickResult::Completed
    );
    assert_eq!(
        actuator.reaped.lock().unwrap().as_slice(),
        std::slice::from_ref(&id)
    );
    assert_eq!(
        tick_with(fixture.store(), &env, &actuator).unwrap(),
        TickResult::Idle
    );
    assert_eq!(actuator.reaped.lock().unwrap().as_slice(), [id]);
}
