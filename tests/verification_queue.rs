//! Store-backed contracts for the SH-521 centralized verification queue.

use storyhook::api::http::TrustedHosts;
use storyhook::api::rest;
use storyhook::daemon::http1::{Header, Method};
use storyhook::daemon::verification::{
    ShellVerificationActuator, TickResult, VerificationActivity, VerificationActuator,
    VerificationGuard, VerificationOutcome, journal_path, tick_with, tick_with_activity,
};
use storyhook::daemon::verification_progress::{VerificationStatus, publish_once, status_snapshot};
use storyhook::domain::provenance::Provenance;
use storyhook::domain::remote::RemoteUrl;
use storyhook::domain::{
    CLEANUP_LEASE_VERSION, Priority, StoryCleanupLease, StoryEvent, SuperState, TmuxCleanupTarget,
    fold_story,
};
use storyhook::env::Environment;
use storyhook::error::AppError;
use storyhook::service::gate_progress::GATE_PROGRESS_PREFIX;
use storyhook::service::{
    Clock, ConfigService, Ctx, NewStoryInput, PrLinkService, StoryService,
    VERIFICATION_CLEANUP_COMPLETE_PREFIX, VERIFICATION_GREEN_PREFIX, VerificationCandidate,
    VerificationProblem, VerificationQueue,
};
use storyhook::store::{
    ExpectedSeq, GlobalSeq, PrLink, ReadOps, Store, StoreError, StoryNo, WriteOps, partition_known,
};
use storyhook_test_support::ServiceFixture;
use storyhook_test_support::{FIXTURE_NOW, scratch_dir, story_binary};

use std::path::PathBuf;
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

/// `ordered()` (SH-524) is the whole queue `next()` itself drains from, in
/// the same order — a queued candidate's position and wait are computed from
/// this list, so it must actually agree with what `next()` selects first.
#[test]
fn ordered_lists_every_submitted_candidate_in_the_order_next_would_drain_them() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let low = submitted(&fixture, "older low", Priority::Low, PR_ONE);
    let high = submitted(&fixture, "newer high", Priority::High, PR_TWO);

    let queue = VerificationQueue::new(fixture.store());
    let ordered = queue.ordered().unwrap();
    let next = queue.next().unwrap().unwrap();

    assert_eq!(ordered.len(), 2);
    assert_eq!(ordered[0].story_id, high);
    assert_eq!(ordered[1].story_id, low);
    assert_eq!(next.story_id, ordered[0].story_id);
}

#[test]
fn a_higher_priority_arrival_does_not_steal_active_verification_ownership() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let low = submitted(&fixture, "already running", Priority::Low, PR_ONE);
    let low_candidate = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    let activity = VerificationActivity::new();
    let guard = activity.acquire(&low_candidate, FIXTURE_NOW.into());

    let high = submitted(&fixture, "arrived later", Priority::High, PR_TWO);
    let ordered = VerificationQueue::new(fixture.store()).ordered().unwrap();
    assert_eq!(
        ordered[0].story_id, high,
        "priority must still order waiting work"
    );
    assert_eq!(ordered[1].story_id, low);

    let statuses = status_snapshot(
        &ordered,
        activity.snapshot().as_ref(),
        fixture.env(),
        FIXTURE_NOW,
    );
    assert!(matches!(
        statuses
            .iter()
            .find(|(_, id, _)| id == &low)
            .map(|(_, _, status)| status),
        Some(VerificationStatus::Running { .. })
    ));
    assert!(matches!(
        statuses
            .iter()
            .find(|(_, id, _)| id == &high)
            .map(|(_, _, status)| status),
        Some(VerificationStatus::Queued { position: 1, .. })
    ));

    drop(guard);
    let statuses = status_snapshot(
        &ordered,
        activity.snapshot().as_ref(),
        fixture.env(),
        FIXTURE_NOW,
    );
    assert!(matches!(
        statuses
            .iter()
            .find(|(_, id, _)| id == &high)
            .map(|(_, _, status)| status),
        Some(VerificationStatus::Queued { position: 1, .. })
    ));
    assert!(matches!(
        statuses
            .iter()
            .find(|(_, id, _)| id == &low)
            .map(|(_, _, status)| status),
        Some(VerificationStatus::Queued { position: 2, .. })
    ));
}

#[test]
fn dashboard_data_exposes_running_and_queued_status_and_omits_other_states() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let running_id = submitted(&fixture, "active low", Priority::Low, PR_ONE);
    let running = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    let activity = VerificationActivity::new();
    let acquired_at = fixture.env().now();
    let _guard = activity.acquire(&running, acquired_at.clone());
    let journal = journal_path(fixture.env(), &running);
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(
        journal,
        attempt_journal(
            &running,
            &format!(
                "{{\"kind\":\"item\",\"path\":\"release gate/rust-suite\",\"status\":\"running\",\"at\":{at},\"total\":4}}\n\
                 {{\"kind\":\"case\",\"path\":\"release gate/rust-suite\",\"outcome\":\"pass\"}}\n",
                at = serde_json::to_string(&acquired_at).unwrap()
            ),
        ),
    )
    .unwrap();
    let queued_id = submitted(&fixture, "queued high", Priority::High, PR_TWO);
    let idle_id = StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "ordinary story".into(),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id;
    let path = format!("/api/repos/{}/data", running.project_slug);

    let routed = rest::route_with_activity(
        fixture.store(),
        fixture.env(),
        &activity,
        rest::RouteRequest::new(
            &Method::Get,
            &path,
            &[Header::from_bytes("Host", "127.0.0.1:3456").unwrap()],
            "",
        ),
        &TrustedHosts::default(),
    );
    assert_eq!(routed.reply.status, 200);
    let json: serde_json::Value = serde_json::from_str(routed.reply.body()).unwrap();
    let story = |id: &str| {
        json["stories"]
            .as_array()
            .unwrap()
            .iter()
            .find(|view| view["story"]["id"] == id)
            .unwrap()
    };

    let running = &story(&running_id)["verification"];
    assert_eq!(running["status"], "running");
    assert!(running["elapsed_seconds"].as_u64().unwrap() <= 1);
    assert_eq!(running["current_step"]["label"], "rust-suite");
    assert_eq!(running["tests"]["completed"], 1);
    assert_eq!(running["tests"]["total"], 4);
    assert_eq!(story(&queued_id)["verification"]["status"], "queued");
    assert_eq!(story(&queued_id)["verification"]["position"], 1);
    assert!(story(&idle_id).get("verification").is_none());
}

#[test]
fn a_different_verifying_generation_cannot_inherit_active_ownership() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    submitted(&fixture, "resubmitted", Priority::High, PR_ONE);
    let old_candidate = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    let (activity, _guard) = active_for(&old_candidate);
    let mut new_candidate = old_candidate.clone();
    new_candidate.verifying_generation = Some(GlobalSeq::new(
        old_candidate.verifying_generation.unwrap().get() + 1,
    ));

    let statuses = status_snapshot(
        &[new_candidate],
        activity.snapshot().as_ref(),
        fixture.env(),
        FIXTURE_NOW,
    );

    assert!(matches!(
        statuses[0].2,
        VerificationStatus::Queued { position: 1, .. }
    ));
}

#[test]
fn an_active_resubmission_does_not_reuse_an_older_journal_generation() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    submitted(&fixture, "fresh attempt", Priority::High, PR_ONE);
    let candidate = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    let journal = journal_path(fixture.env(), &candidate);
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(
        journal,
        format!(
            "{{\"kind\":\"run\",\"generation\":{},\"at\":{at}}}\n\
             {{\"kind\":\"item\",\"path\":\"release gate/old-suite\",\"status\":\"running\",\"at\":{at},\"total\":99}}\n",
            candidate.verifying_generation.unwrap().get() - 1,
            at = serde_json::to_string(FIXTURE_NOW).unwrap()
        ),
    )
    .unwrap();
    let (activity, _guard) = active_for(&candidate);

    let statuses = status_snapshot(
        &[candidate],
        activity.snapshot().as_ref(),
        fixture.env(),
        FIXTURE_NOW,
    );

    assert!(matches!(
        &statuses[0].2,
        VerificationStatus::Running {
            current_step: None,
            tests: None,
            ..
        }
    ));
}

struct ActivityObservingActuator {
    activity: VerificationActivity,
    observed_story: Mutex<Option<String>>,
    outcome: Option<VerificationOutcome>,
}

impl VerificationActuator for ActivityObservingActuator {
    fn verify(
        &self,
        candidate: &VerificationCandidate,
        _pull_request: &PrLink,
    ) -> VerificationOutcome {
        let active = self
            .activity
            .snapshot()
            .expect("ownership must be visible while the actuator runs");
        assert_eq!(active.project, candidate.project);
        assert_eq!(active.story_id, candidate.story_id);
        assert_eq!(active.generation, candidate.verifying_generation);
        *self.observed_story.lock().unwrap() = Some(candidate.story_id.clone());
        self.outcome
            .clone()
            .unwrap_or_else(|| panic!("simulated verifier panic"))
    }

    fn notify(&self, _candidate: &VerificationCandidate, _message: &str) -> Result<(), AppError> {
        Ok(())
    }

    fn reap(&self, _candidate: &VerificationCandidate) -> Result<(), AppError> {
        Ok(())
    }
}

#[test]
fn every_verification_outcome_releases_ownership_after_the_blocking_call() {
    let cases = [
        (
            VerificationOutcome::Merged {
                tree: "abc123".into(),
                detail: "landed".into(),
            },
            TickResult::Completed,
        ),
        (
            VerificationOutcome::Conflict {
                detail: "conflict".into(),
            },
            TickResult::Returned,
        ),
        (
            VerificationOutcome::InvalidSubmission {
                detail: "invalid".into(),
            },
            TickResult::Returned,
        ),
        (
            VerificationOutcome::TestsFailed {
                tree: "abc123".into(),
                log: "/tmp/red.log".into(),
                detail: "red".into(),
            },
            TickResult::Returned,
        ),
        (
            VerificationOutcome::InfrastructureFailure {
                detail: "retry later".into(),
            },
            TickResult::RetryLater,
        ),
    ];

    for (outcome, expected) in cases {
        let fixture = ServiceFixture::new();
        fixture.link_origin("https://github.com/acme/widgets");
        let id = submitted(&fixture, "owned while running", Priority::High, PR_ONE);
        let activity = VerificationActivity::new();
        let actuator = ActivityObservingActuator {
            activity: activity.clone(),
            observed_story: Mutex::new(None),
            outcome: Some(outcome),
        };

        assert_eq!(
            tick_with_activity(fixture.store(), fixture.env(), &actuator, &activity).unwrap(),
            expected
        );
        assert_eq!(
            actuator.observed_story.lock().unwrap().as_deref(),
            Some(id.as_str())
        );
        assert_eq!(activity.snapshot(), None);

        if expected == TickResult::RetryLater {
            let ordered = VerificationQueue::new(fixture.store()).ordered().unwrap();
            assert!(matches!(
                status_snapshot(
                    &ordered,
                    activity.snapshot().as_ref(),
                    fixture.env(),
                    FIXTURE_NOW
                )[0]
                .2,
                VerificationStatus::Queued { position: 1, .. }
            ));
        }
    }
}

#[test]
fn ownership_is_cleared_during_unwind() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    submitted(&fixture, "panicking attempt", Priority::High, PR_ONE);
    let activity = VerificationActivity::new();
    let actuator = ActivityObservingActuator {
        activity: activity.clone(),
        observed_story: Mutex::new(None),
        outcome: None,
    };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = tick_with_activity(fixture.store(), fixture.env(), &actuator, &activity);
    }));

    assert!(result.is_err());
    assert_eq!(
        actuator.observed_story.lock().unwrap().as_deref(),
        Some("SH-1")
    );
    assert_eq!(activity.snapshot(), None);
}

/// A story's own comment-driven `updated_at` moves on every publish of the
/// SH-524 progress checklist. `verifying_since` must not be fooled by that: it
/// answers "when did the state change", read from the story's
/// `StoryStateChanged` history, not "when was this row last written".
#[test]
fn verifying_since_reads_the_state_change_event_not_a_later_comments_updated_at() {
    let mut fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "queued", Priority::High, PR_ONE);
    fixture.set_clock(Clock::Fixed("2026-01-01T00:10:00Z".into()));
    StoryService::new(&fixture.ctx())
        .comment(&id, "an unrelated later comment")
        .unwrap();

    let selected = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();

    assert_eq!(selected.verifying_since.as_deref(), Some(FIXTURE_NOW));
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
        verifying_since: Some("2026-01-01T00:00:00Z".into()),
        verifying_generation: None,
        checkout: checkout.path().to_path_buf(),
        cleanup_lease: None,
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

fn cleanup_candidate(
    fixture: &ServiceFixture,
    repository: &std::path::Path,
) -> VerificationCandidate {
    VerificationCandidate {
        project: fixture.project(),
        project_slug: "fixture".into(),
        story_id: "SH-1".into(),
        title: "cleanup".into(),
        priority: Priority::High,
        created_at: FIXTURE_NOW.into(),
        verifying_since: Some(FIXTURE_NOW.into()),
        verifying_generation: None,
        checkout: repository.to_path_buf(),
        cleanup_lease: Some(StoryCleanupLease {
            version: CLEANUP_LEASE_VERSION,
            project_slug: "fixture".into(),
            story_id: "SH-1".into(),
            repository_path: repository.to_path_buf(),
            worktree_path: repository.join(".codex/worktrees/SH-1"),
            branch: "worktree-SH-1".into(),
            tmux: TmuxCleanupTarget {
                socket_path: repository.join("tmux.sock"),
            },
        }),
        pull_request: Err(VerificationProblem::MissingPullRequest),
    }
}

fn append_cleanup_lease(fixture: &ServiceFixture, story_id: &str, lease: StoryCleanupLease) {
    let story = StoryNo::parse_id("SH", story_id).unwrap();
    fixture
        .store()
        .write(|tx| {
            let head = tx.append_events(
                fixture.project(),
                story,
                ExpectedSeq::Any,
                &[StoryEvent::StoryCleanupLeaseRecorded {
                    at: FIXTURE_NOW.into(),
                    lease: Box::new(lease),
                }],
                &Provenance::unrecorded(),
            )?;
            let stored = tx.events_for(fixture.project(), story)?;
            let (known, _) = partition_known(story, &stored);
            let states = tx.state_map(fixture.project())?;
            let snapshot = fold_story(story_id, &known, &states).map_err(StoreError::from)?;
            tx.put_story(fixture.project(), &snapshot, head)
        })
        .unwrap();
}

#[test]
fn latest_generation_shadows_old_leases_and_restart_cleanup_survives_checkout_change() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "generations", Priority::High, PR_ONE);
    let first_root = scratch_dir();
    let first = cleanup_candidate(&fixture, first_root.path())
        .cleanup_lease
        .unwrap();
    append_cleanup_lease(&fixture, &id, first.clone());
    assert_eq!(
        VerificationQueue::new(fixture.store())
            .next()
            .unwrap()
            .unwrap()
            .cleanup_lease,
        Some(first)
    );

    StoryService::new(&fixture.ctx())
        .set_state(&id, "in-progress", None, None, None)
        .unwrap();
    StoryService::new(&fixture.ctx())
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    assert_eq!(
        VerificationQueue::new(fixture.store())
            .next()
            .unwrap()
            .unwrap()
            .cleanup_lease,
        None,
        "a later unleased verification must not reuse an older generation"
    );

    let second_root = scratch_dir();
    let second = cleanup_candidate(&fixture, second_root.path())
        .cleanup_lease
        .unwrap();
    append_cleanup_lease(&fixture, &id, second.clone());
    let replacement = scratch_dir();
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), Some(replacement.path())))
        .unwrap();
    let selected = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(selected.checkout, replacement.path());
    assert_eq!(selected.cleanup_lease, Some(second.clone()));

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

    let recovered = VerificationQueue::new(fixture.store())
        .next_cleanup()
        .unwrap()
        .unwrap();
    assert_eq!(recovered.checkout, replacement.path());
    assert_eq!(recovered.cleanup_lease, Some(second));
}

fn git_ok(dir: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

#[test]
fn verifying_transition_validates_and_atomically_records_a_private_git_marker() {
    let fixture = ServiceFixture::new();
    let id = StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "marker capture".into(),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id;
    let repository = scratch_dir();
    git_ok(repository.path(), &["init", "-q", "-b", "main"]);
    git_ok(repository.path(), &["config", "user.name", "Test"]);
    git_ok(
        repository.path(),
        &["config", "user.email", "test@example.test"],
    );
    git_ok(
        repository.path(),
        &["commit", "--allow-empty", "-qm", "base"],
    );
    let worktree = repository.path().join(".codex/worktrees").join(&id);
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git_ok(
        repository.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("worktree-{id}"),
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let repository_path = repository.path().canonicalize().unwrap();
    let worktree_path = worktree.canonicalize().unwrap();
    let private_git = PathBuf::from(git_ok(&worktree, &["rev-parse", "--absolute-git-dir"]));
    let marker = private_git.join("storyhook-cleanup-lease-v1.json");
    let mut lease = StoryCleanupLease {
        version: CLEANUP_LEASE_VERSION,
        project_slug: "fixture".into(),
        story_id: id.clone(),
        repository_path,
        worktree_path,
        branch: format!("worktree-{id}"),
        tmux: TmuxCleanupTarget {
            socket_path: repository.path().join("tmux.sock"),
        },
    };
    std::fs::write(&marker, b"not json").unwrap();
    let ctx = Ctx::new(
        fixture.store(),
        fixture.project(),
        &worktree,
        fixture.env().clone(),
    )
    .clock(Clock::Fixed(FIXTURE_NOW.into()));
    let malformed = StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap_err()
        .to_string();
    assert!(malformed.contains("malformed"), "{malformed}");

    lease.story_id = "SH-999".into();
    std::fs::write(&marker, serde_json::to_vec(&lease).unwrap()).unwrap();
    let mismatched = StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap_err()
        .to_string();
    assert!(mismatched.contains("story mismatch"), "{mismatched}");

    lease.story_id = id.clone();
    std::fs::write(&marker, serde_json::to_vec(&lease).unwrap()).unwrap();
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    let events = fixture
        .store()
        .read(|tx| tx.events_for(fixture.project(), StoryNo::parse_id("SH", &id).unwrap()))
        .unwrap();
    let verifying = events
        .iter()
        .rposition(|event| {
            matches!(
                event.known(),
                Some(StoryEvent::StoryStateChanged { state, .. }) if state == "verifying"
            )
        })
        .unwrap();
    assert!(matches!(
        events.get(verifying + 1).and_then(|event| event.known()),
        Some(StoryEvent::StoryCleanupLeaseRecorded { lease: recorded, .. }) if recorded.as_ref() == &lease
    ));
}

#[test]
fn verifying_without_a_private_git_marker_remains_an_unleased_legacy_submission() {
    let fixture = ServiceFixture::new();
    let id = submitted(&fixture, "legacy submission", Priority::High, PR_ONE);

    assert_eq!(
        VerificationQueue::new(fixture.store())
            .next()
            .unwrap()
            .unwrap()
            .cleanup_lease,
        None
    );
    assert_eq!(id, "SH-1");
}

fn write_receipt_helper(root: &std::path::Path, mutation: &str, exit_status: i32) -> PathBuf {
    let helper = root.join("receipt-helper.sh");
    std::fs::write(
        &helper,
        format!(
            r#"#!/bin/bash
lease="$STORYHOOK_REAP_LEASE_V1"
story=$(printf '%s' "$lease" | jq -r .story_id)
jq -n --argjson lease "$lease" --arg story "$story" \
  '{{ok:true,receipt_version:1,story_id:$story,lease:$lease,
     removed:{{worktree:false,branch:false,tmux:false}},
     postconditions:{{worktree_registration_absent:true,
                      worktree_path_absent:true,
                      branch_absent:true,
                      tmux_story_windows_absent:true}},
     display:"fixture receipt"}} | {mutation}'
exit {exit_status}
"#
        ),
    )
    .unwrap();
    helper
}

#[test]
fn shell_cleanup_requires_a_latest_generation_lease_before_spawning() {
    let fixture = ServiceFixture::new();
    let root = scratch_dir();
    let mut candidate = cleanup_candidate(&fixture, root.path());
    candidate.cleanup_lease = None;
    let actuator = ShellVerificationActuator::with_paths(
        Environment::at(root.path()),
        root.path().join("must-not-run"),
        PathBuf::from("/usr/bin/true"),
    );

    let error = actuator.reap(&candidate).unwrap_err().to_string();
    assert!(error.contains("no cleanup lease"), "{error}");
}

#[test]
fn shell_notification_rejects_success_json_from_a_failed_process() {
    let fixture = ServiceFixture::new();
    let root = scratch_dir();
    let candidate = cleanup_candidate(&fixture, root.path());
    let helper = root.path().join("notify-helper.sh");
    std::fs::write(
        &helper,
        "#!/bin/bash\nprintf '%s\\n' '{\"ok\":true,\"display\":\"not actually notified\"}'\nexit 31\n",
    )
    .unwrap();
    let actuator = ShellVerificationActuator::with_paths(
        Environment::at(root.path()),
        helper,
        PathBuf::from("/usr/bin/true"),
    );

    let error = actuator
        .notify(&candidate, "retry the failed gate")
        .unwrap_err()
        .to_string();

    assert!(error.contains("reported success"), "{error}");
    assert!(error.contains("31"), "{error}");
}

#[test]
fn shell_cleanup_accepts_only_an_exact_complete_typed_receipt() {
    let fixture = ServiceFixture::new();
    let root = scratch_dir();
    let candidate = cleanup_candidate(&fixture, root.path());
    let helper = write_receipt_helper(root.path(), ".", 0);
    let actuator = ShellVerificationActuator::with_paths(
        Environment::at(root.path()),
        helper,
        PathBuf::from("/usr/bin/true"),
    );

    actuator.reap(&candidate).unwrap();
}

#[test]
fn shell_cleanup_rejects_nonzero_identity_version_and_postcondition_receipts() {
    let fixture = ServiceFixture::new();
    for (mutation, status, expected) in [
        (".", 7, "exited"),
        (".story_id = \"SH-999\"", 0, "does not echo"),
        (".receipt_version = 2", 0, "unsupported version"),
        (
            ".postconditions.branch_absent = false",
            0,
            "without every exact postcondition",
        ),
    ] {
        let root = scratch_dir();
        let candidate = cleanup_candidate(&fixture, root.path());
        let helper = write_receipt_helper(root.path(), mutation, status);
        let actuator = ShellVerificationActuator::with_paths(
            Environment::at(root.path()),
            helper,
            PathBuf::from("/usr/bin/true"),
        );

        let error = actuator.reap(&candidate).unwrap_err().to_string();
        assert!(error.contains(expected), "{mutation}: {error}");
    }
}

#[test]
fn real_shell_actuator_reaps_the_leased_original_from_a_clean_replacement_checkout() {
    let fixture = ServiceFixture::new();
    let id = StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "real leased reap".into(),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id;
    StoryService::new(&fixture.ctx())
        .set_state(&id, "done", None, None, None)
        .unwrap();

    let repository = scratch_dir();
    git_ok(repository.path(), &["init", "-q", "-b", "main"]);
    git_ok(repository.path(), &["config", "user.name", "Test"]);
    git_ok(
        repository.path(),
        &["config", "user.email", "test@example.test"],
    );
    git_ok(
        repository.path(),
        &["commit", "--allow-empty", "-qm", "base"],
    );
    let worktree = repository.path().join(".codex/worktrees").join(&id);
    std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
    git_ok(
        repository.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("worktree-{id}"),
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );

    let replacement = scratch_dir();
    git_ok(replacement.path(), &["init", "-q", "-b", "main"]);
    let mut candidate = cleanup_candidate(&fixture, repository.path());
    candidate.story_id = id.clone();
    candidate.checkout = replacement.path().to_path_buf();
    let lease = candidate.cleanup_lease.as_mut().unwrap();
    lease.story_id = id.clone();
    lease.repository_path = repository.path().canonicalize().unwrap();
    lease.worktree_path = worktree.canonicalize().unwrap();
    lease.branch = format!("worktree-{id}");
    lease.tmux.socket_path = repository.path().join("never-created-tmux.sock");

    let helper = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins/story/bin/story.sh");
    let actuator = ShellVerificationActuator::with_paths(
        fixture.env().clone(),
        helper,
        story_binary().to_path_buf(),
    );
    actuator.reap(&candidate).unwrap();

    assert!(!worktree.exists(), "the leased original worktree survived");
    let branch = Command::new("git")
        .args([
            "show-ref",
            "--verify",
            "--quiet",
            &format!("refs/heads/worktree-{id}"),
        ])
        .current_dir(repository.path())
        .status()
        .unwrap();
    assert!(!branch.success(), "the leased original branch survived");
    assert!(replacement.path().join(".git").exists());
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

// ---------------------------------------------------------------------------
// SH-524: the verification progress publisher
// ---------------------------------------------------------------------------

fn last_comment(fixture: &ServiceFixture, id: &str) -> String {
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::parse_id("SH", id).unwrap()))
        .unwrap()
        .unwrap();
    row.snapshot
        .comments
        .last()
        .unwrap_or_else(|| panic!("{id} has no comments"))
        .text
        .clone()
}

fn progress_comment_count(fixture: &ServiceFixture, id: &str) -> usize {
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::parse_id("SH", id).unwrap()))
        .unwrap()
        .unwrap();
    row.snapshot
        .comments
        .iter()
        .filter(|comment| comment.text.starts_with(GATE_PROGRESS_PREFIX))
        .count()
}

fn active_for(candidate: &VerificationCandidate) -> (VerificationActivity, VerificationGuard) {
    let activity = VerificationActivity::new();
    let guard = activity.acquire(candidate, "2026-01-01T00:00:00Z".into());
    (activity, guard)
}

fn attempt_journal(candidate: &VerificationCandidate, body: &str) -> String {
    format!(
        "{{\"kind\":\"run\",\"generation\":{},\"at\":\"2026-01-01T00:00:00Z\"}}\n{body}",
        candidate.verifying_generation.unwrap().get()
    )
}

#[test]
fn the_running_candidate_gets_a_live_checklist_from_its_own_journal() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "running", Priority::High, PR_ONE);

    let candidate = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    let journal = journal_path(fixture.env(), &candidate);
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(
        &journal,
        attempt_journal(&candidate, "{\"kind\":\"item\",\"path\":\"release gate/fmt\",\"status\":\"passed\",\"at\":\"t\",\"seconds\":2}\n\
         {\"kind\":\"item\",\"path\":\"release gate/rust-suite\",\"status\":\"running\",\"at\":\"t\",\"total\":4}\n\
         {\"kind\":\"case\",\"path\":\"release gate/rust-suite\",\"outcome\":\"pass\"}\n"),
    )
    .unwrap();

    let (activity, _guard) = active_for(&candidate);
    let moved = publish_once(
        fixture.store(),
        fixture.env(),
        "2026-01-01T00:02:00Z",
        &activity,
    )
    .unwrap();
    assert!(
        moved,
        "the first publish for a running candidate must write"
    );

    let comment = last_comment(&fixture, &id);
    assert!(comment.starts_with(GATE_PROGRESS_PREFIX), "{comment}");
    assert!(comment.contains("- [x] fmt (1/1, 2s)"), "{comment}");
    assert!(comment.contains("rust-suite (1/4, running)"), "{comment}");
    assert!(
        !comment.contains('~'),
        "a production progress comment must never imply its completed count is the total: {comment}"
    );
    assert_eq!(progress_comment_count(&fixture, &id), 1);
}

#[test]
fn a_queued_candidate_shows_its_position_and_what_is_ahead_of_it() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let high = submitted(&fixture, "running", Priority::High, PR_ONE);
    let low = submitted(&fixture, "queued", Priority::Low, PR_TWO);
    let candidate = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    let (activity, _guard) = active_for(&candidate);

    publish_once(
        fixture.store(),
        fixture.env(),
        "2026-01-01T00:02:00Z",
        &activity,
    )
    .unwrap();

    let running_comment = last_comment(&fixture, &high);
    assert!(
        running_comment.contains("Verification ("),
        "{running_comment}"
    );

    let queued_comment = last_comment(&fixture, &low);
    assert!(
        queued_comment.contains("QUEUED (position 1)"),
        "{queued_comment}"
    );
    assert!(
        queued_comment.contains("0 candidates of higher priority, 0 of equal priority and older"),
        "{queued_comment}"
    );
}

#[test]
fn republishing_rewrites_the_one_comment_rather_than_appending_a_new_one() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "running", Priority::High, PR_ONE);
    let candidate = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    let journal = journal_path(fixture.env(), &candidate);
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(
        &journal,
        attempt_journal(&candidate, "{\"kind\":\"item\",\"path\":\"release gate/fmt\",\"status\":\"running\",\"at\":\"t\"}\n"),
    )
    .unwrap();
    let (activity, _guard) = active_for(&candidate);

    let first = publish_once(
        fixture.store(),
        fixture.env(),
        "2026-01-01T00:01:00Z",
        &activity,
    )
    .unwrap();
    assert!(first);
    assert_eq!(progress_comment_count(&fixture, &id), 1);

    std::fs::write(
        &journal,
        attempt_journal(&candidate, "{\"kind\":\"item\",\"path\":\"release gate/fmt\",\"status\":\"passed\",\"at\":\"t\",\"seconds\":5}\n"),
    )
    .unwrap();
    let second = publish_once(
        fixture.store(),
        fixture.env(),
        "2026-01-01T00:02:00Z",
        &activity,
    )
    .unwrap();
    assert!(second, "a changed journal must publish again");
    assert_eq!(
        progress_comment_count(&fixture, &id),
        1,
        "the comment is rewritten in place, never appended a second time"
    );
    assert!(last_comment(&fixture, &id).contains("fmt (1/1, 5s)"));
}

#[test]
fn an_unchanged_journal_writes_nothing_on_the_next_publish() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "running", Priority::High, PR_ONE);
    let candidate = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    let journal = journal_path(fixture.env(), &candidate);
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(
        &journal,
        attempt_journal(&candidate, "{\"kind\":\"item\",\"path\":\"release gate/fmt\",\"status\":\"running\",\"at\":\"t\"}\n"),
    )
    .unwrap();
    let (activity, _guard) = active_for(&candidate);

    // Same `now` both times, so even the header's own timestamp is identical
    // and the rendered body is byte-for-byte the same on the second call.
    assert!(
        publish_once(
            fixture.store(),
            fixture.env(),
            "2026-01-01T00:01:00Z",
            &activity
        )
        .unwrap()
    );
    let moved_again = publish_once(
        fixture.store(),
        fixture.env(),
        "2026-01-01T00:01:00Z",
        &activity,
    )
    .unwrap();
    assert!(
        !moved_again,
        "an identical body must not be rewritten a second time"
    );
    assert_eq!(progress_comment_count(&fixture, &id), 1);
}

#[test]
fn a_story_that_leaves_verifying_stops_receiving_progress_updates() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "green", Priority::High, PR_ONE);
    let candidate = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    let journal = journal_path(fixture.env(), &candidate);
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(
        &journal,
        attempt_journal(&candidate, "{\"kind\":\"item\",\"path\":\"release gate/fmt\",\"status\":\"running\",\"at\":\"t\"}\n"),
    )
    .unwrap();
    let (activity, _guard) = active_for(&candidate);
    assert!(
        publish_once(
            fixture.store(),
            fixture.env(),
            "2026-01-01T00:01:00Z",
            &activity
        )
        .unwrap()
    );
    assert_eq!(progress_comment_count(&fixture, &id), 1);

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

    std::fs::write(
        &journal,
        attempt_journal(&candidate, "{\"kind\":\"item\",\"path\":\"release gate/fmt\",\"status\":\"passed\",\"at\":\"t\"}\n"),
    )
    .unwrap();
    let moved = publish_once(
        fixture.store(),
        fixture.env(),
        "2026-01-01T00:05:00Z",
        &activity,
    )
    .unwrap();
    assert!(
        !moved,
        "a story no longer in `verifying` must not be touched by the publisher"
    );
    assert_eq!(
        progress_comment_count(&fixture, &id),
        1,
        "the checklist stays frozen at its last state once the story leaves verifying"
    );
}
