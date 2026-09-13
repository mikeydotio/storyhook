//! Store-backed contracts for the SH-521 centralized verification queue.

use storyhook::api::http::TrustedHosts;
use storyhook::api::rest;
use storyhook::daemon::http1::{Header, Method};
use storyhook::daemon::lifecycle::{self, InFlight};
use storyhook::daemon::verification::{
    NotifyDelivery, ResumePlan, ShellVerificationActuator, SubmissionFailure, TickResult,
    VerificationActivity, VerificationActuator, VerificationGuard, VerificationOutcome,
    journal_path, resume_plan, tick_with, tick_with_activity, tick_with_reconciliation,
};
use storyhook::daemon::verification_progress::{VerificationStatus, publish_once, status_snapshot};
use storyhook::domain::provenance::Provenance;
use storyhook::domain::remote::RemoteUrl;
use storyhook::domain::{
    CLEANUP_LEASE_VERSION, COMPLETION_STATE_SLUG, Priority, StoryCleanupLease, StoryEvent,
    SubmittedPullRequest, SuperState, TmuxCleanupTarget, fold_story,
};
use storyhook::env::Environment;
use storyhook::error::AppError;
use storyhook::service::gate_command::GateCommand;
use storyhook::service::gate_progress::GATE_PROGRESS_PREFIX;
use storyhook::service::verification_control::VerificationAction;
use storyhook::service::{
    Clock, ConfigService, Ctx, NewStoryInput, PrLinkService, StoryService,
    VERIFICATION_CLEANUP_COMPLETE_PREFIX, VERIFICATION_GREEN_PREFIX, VERIFICATION_SUBMITTED_PREFIX,
    VERIFICATION_WITHDRAWN_PREFIX, VerificationCandidate, VerificationProblem, VerificationQueue,
    acknowledge_verification_incident,
};
use storyhook::store::{
    ExpectedSeq, GlobalSeq, PrLink, ReadOps, SqliteStore, Store, StoreError, StoryNo,
    VerificationFailureDisposition, VerificationIncident, WriteOps, partition_known,
};
use storyhook_test_support::ServiceFixture;
use storyhook_test_support::{FIXTURE_NOW, scratch_dir, story_binary};

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

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
        activity.active_for(fixture.project()).as_ref(),
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
        activity.active_for(fixture.project()).as_ref(),
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
fn dashboard_data_exposes_running_queued_and_superseding_statuses_and_omits_other_states() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let running_id = submitted(&fixture, "active low", Priority::Low, PR_ONE);
    let running_candidate = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    let activity = VerificationActivity::new();
    let acquired_at = fixture.env().now();
    let _guard = activity.acquire(&running_candidate, acquired_at.clone());
    let journal = journal_path(fixture.env(), &running_candidate);
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(
        &journal,
        attempt_journal(
            &running_candidate,
            &format!(
                "{{\"kind\":\"item\",\"path\":\"release gate/rust-suite\",\"status\":\"passed\",\"at\":{at},\"total\":4}}\n\
                 {{\"kind\":\"case\",\"path\":\"release gate/rust-suite\",\"outcome\":\"pass\"}}\n\
                 {{\"kind\":\"item\",\"path\":\"release gate/rust-contracts\",\"status\":\"running\",\"at\":{at}}}\n\
                 {{\"kind\":\"activity\",\"path\":\"release gate/rust-contracts\",\"label\":\"waiting for gate lock\",\"status\":\"running\",\"at\":{at}}}\n",
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
    let path = format!("/api/repos/{}/data", running_candidate.project_slug);

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
    let json: serde_json::Value =
        serde_json::from_str(routed.reply.text_body().expect("UTF-8 text response")).unwrap();
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
    assert_eq!(running["current_step"]["label"], "waiting for gate lock");
    assert!(
        running.get("tests").is_none(),
        "a non-test activity must not inherit the completed rust-suite count: {running}"
    );

    std::fs::write(
        &journal,
        attempt_journal(
            &running_candidate,
            &format!(
                "{{\"kind\":\"item\",\"path\":\"release gate/rust-suite\",\"status\":\"passed\",\"at\":{at},\"total\":4}}\n\
                 {{\"kind\":\"case\",\"path\":\"release gate/rust-suite\",\"outcome\":\"pass\"}}\n\
                 {{\"kind\":\"item\",\"path\":\"release gate/rust-contracts\",\"status\":\"running\",\"at\":{at},\"total\":3}}\n\
                 {{\"kind\":\"case\",\"path\":\"release gate/rust-contracts\",\"outcome\":\"pass\"}}\n\
                 {{\"kind\":\"activity\",\"path\":\"release gate/rust-contracts\",\"label\":\"waiting for gate lock\",\"status\":\"running\",\"at\":{at}}}\n\
                 {{\"kind\":\"activity\",\"path\":\"release gate/rust-contracts\",\"label\":\"waiting for gate lock\",\"status\":\"passed\",\"at\":{at}}}\n",
                at = serde_json::to_string(&acquired_at).unwrap()
            ),
        ),
    )
    .unwrap();
    let resumed = rest::route_with_activity(
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
    let resumed_json: serde_json::Value =
        serde_json::from_str(resumed.reply.text_body().expect("UTF-8 text response")).unwrap();
    let resumed_running = resumed_json["stories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|view| view["story"]["id"] == running_id)
        .and_then(|view| view.get("verification"))
        .unwrap();
    assert_eq!(resumed_running["current_step"]["label"], "rust-contracts");
    assert_eq!(resumed_running["tests"]["completed"], 1);
    assert_eq!(resumed_running["tests"]["total"], 3);
    assert_eq!(story(&queued_id)["verification"]["status"], "queued");
    assert_eq!(story(&queued_id)["verification"]["position"], 1);
    assert!(story(&idle_id).get("verification").is_none());

    StoryService::new(&fixture.ctx())
        .set_state(&running_id, "verifying", None, Some("verifying"), None)
        .unwrap();
    let superseding = rest::route_with_activity(
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
    let superseding_json: serde_json::Value =
        serde_json::from_str(superseding.reply.text_body().expect("UTF-8 text response")).unwrap();
    let superseding_status = superseding_json["stories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|view| view["story"]["id"] == running_id)
        .and_then(|view| view.get("verification"))
        .unwrap();
    assert_eq!(superseding_status["status"], "superseding");
    assert_eq!(
        superseding_status["superseded_generation"],
        running_candidate.verifying_generation.unwrap().get()
    );
    assert!(superseding_status["generation"].as_u64().is_some());
    assert!(superseding_status["wait_seconds"].as_u64().is_some());
    assert!(
        superseding_status["active_elapsed_seconds"]
            .as_u64()
            .is_some()
    );
}

#[test]
fn a_resubmitted_generation_reports_the_superseded_attempt_that_still_owns_the_worker() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "resubmitted", Priority::High, PR_ONE);
    let old_candidate = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    let (activity, _guard) = active_for(&old_candidate);
    StoryService::new(&fixture.ctx())
        .set_state(&id, "verifying", None, Some("verifying"), None)
        .unwrap();
    let new_candidate = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    let old_generation = old_candidate.verifying_generation.unwrap();
    let new_generation = new_candidate.verifying_generation.unwrap();

    let statuses = status_snapshot(
        &[new_candidate],
        activity.active_for(fixture.project()).as_ref(),
        fixture.env(),
        FIXTURE_NOW,
    );

    assert!(matches!(
        statuses[0].2,
        VerificationStatus::Superseding {
            generation,
            superseded_generation,
            wait_seconds: Some(0),
            active_elapsed_seconds: 0,
        } if generation == new_generation && superseded_generation == old_generation
    ));
    assert!(publish_once(fixture.store(), fixture.env(), FIXTURE_NOW, &activity).unwrap());
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::parse_id("SH", &id).unwrap()))
        .unwrap()
        .unwrap();
    let progress = row
        .snapshot
        .comments
        .iter()
        .find(|comment| comment.text.starts_with(GATE_PROGRESS_PREFIX))
        .unwrap();
    assert!(progress.text.contains("Verification — RESUBMITTED"));
    assert!(
        progress
            .text
            .contains(&format!("Generation {}", new_generation.get()))
    );
    assert!(
        progress
            .text
            .contains(&format!("generation {}", old_generation.get()))
    );

    let after_restart = status_snapshot(
        &[VerificationQueue::new(fixture.store())
            .next()
            .unwrap()
            .unwrap()],
        VerificationActivity::new()
            .active_for(fixture.project())
            .as_ref(),
        fixture.env(),
        FIXTURE_NOW,
    );
    assert!(matches!(
        after_restart[0].2,
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
        activity.active_for(fixture.project()).as_ref(),
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

/// What a fake answers when asked to submit a candidate it has no scripted
/// answer for: adopt the pull request already linked, which is the steady
/// state of every resubmission (SH-647). A candidate with nothing linked and
/// nothing scripted is a fixture that did not expect to be submitted at all,
/// and says so loudly rather than inventing a pull request.
fn adopt_linked(
    candidate: &VerificationCandidate,
) -> Result<SubmittedPullRequest, SubmissionFailure> {
    match &candidate.pull_request {
        Ok(link) => Ok(SubmittedPullRequest {
            url: link.url.clone(),
            number: link.number,
            base: "dev".into(),
            head_oid: "fixture-head".into(),
            adopted: true,
        }),
        Err(problem) => panic!(
            "fixture asked to submit {} with no scripted answer and no linked pull request: {problem:?}",
            candidate.story_id
        ),
    }
}

struct ActivityObservingActuator {
    activity: VerificationActivity,
    env: Environment,
    observed_story: Mutex<Option<String>>,
    outcome: Option<VerificationOutcome>,
}

impl ActivityObservingActuator {
    fn assert_owned(&self, candidate: &VerificationCandidate) {
        let active = self
            .activity
            .active_for(candidate.project)
            .expect("ownership must be visible while verification is active");
        assert_eq!(active.project, candidate.project);
        assert_eq!(active.story_id, candidate.story_id);
        assert_eq!(active.generation, candidate.verifying_generation);
        let lifecycle_entries = lifecycle::read_inflight(&self.env);
        assert_eq!(lifecycle_entries.len(), 1);
        assert_eq!(lifecycle_entries[0].command, "verify");
        assert!(
            lifecycle_entries[0]
                .request_id
                .contains(&candidate.story_id)
        );
    }
}

impl VerificationActuator for ActivityObservingActuator {
    fn submit(
        &self,
        candidate: &VerificationCandidate,
    ) -> Result<SubmittedPullRequest, SubmissionFailure> {
        adopt_linked(candidate)
    }

    fn verify(
        &self,
        candidate: &VerificationCandidate,
        _pull_request: &PrLink,
    ) -> VerificationOutcome {
        self.assert_owned(candidate);
        *self.observed_story.lock().unwrap() = Some(candidate.story_id.clone());
        self.outcome
            .clone()
            .unwrap_or_else(|| panic!("simulated verifier panic"))
    }

    fn notify(
        &self,
        candidate: &VerificationCandidate,
        _message: &str,
    ) -> Result<NotifyDelivery, AppError> {
        self.assert_owned(candidate);
        Ok(NotifyDelivery::Delivered)
    }

    fn redispatch(
        &self,
        _candidate: &VerificationCandidate,
        _plan: &ResumePlan,
    ) -> Result<(), AppError> {
        panic!("a delivered notification never re-dispatches")
    }

    fn reap(&self, candidate: &VerificationCandidate) -> Result<(), AppError> {
        assert_eq!(
            self.activity
                .active_for(candidate.project)
                .unwrap()
                .story_id,
            candidate.story_id,
            "manual cancellation must retain ownership through post-merge cleanup"
        );
        assert!(
            lifecycle::read_inflight(&self.env).is_empty(),
            "shutdown ownership must end before post-merge cleanup"
        );
        Ok(())
    }
}

#[test]
fn every_single_attempt_outcome_releases_ownership_after_the_blocking_call() {
    let cases = [
        (
            VerificationOutcome::Merged {
                tree: "abc123".into(),
                detail: "landed".into(),
                gate: GateCommand::DEFAULT.into(),
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
                gate: GateCommand::DEFAULT.into(),
            },
            TickResult::Returned,
        ),
        (
            VerificationOutcome::InfrastructureFailure {
                detail: "retry later".into(),
                disposition: storyhook::store::VerificationFailureDisposition::Retryable,
            },
            TickResult::RetryLater,
        ),
    ];

    for (outcome, expected) in cases {
        let fixture = ServiceFixture::new();
        fixture.link_origin("https://github.com/acme/widgets");
        let id = submitted(&fixture, "owned while running", Priority::High, PR_ONE);
        let activity = VerificationActivity::new();
        std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
        let inflight = InFlight::new(fixture.env().clone());
        let actuator = ActivityObservingActuator {
            activity: activity.clone(),
            env: fixture.env().clone(),
            observed_story: Mutex::new(None),
            outcome: Some(outcome),
        };

        assert_eq!(
            tick_with_activity(
                fixture.store(),
                fixture.env(),
                &actuator,
                &activity,
                &inflight,
                fixture.project(),
            )
            .unwrap(),
            expected
        );
        assert_eq!(
            actuator.observed_story.lock().unwrap().as_deref(),
            Some(id.as_str())
        );
        assert_eq!(activity.active_for(fixture.project()), None);
        assert!(lifecycle::read_inflight(fixture.env()).is_empty());

        if expected == TickResult::RetryLater {
            let ordered = VerificationQueue::new(fixture.store()).ordered().unwrap();
            assert!(matches!(
                status_snapshot(
                    &ordered,
                    activity.active_for(fixture.project()).as_ref(),
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
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(fixture.env().clone());
    let actuator = ActivityObservingActuator {
        activity: activity.clone(),
        env: fixture.env().clone(),
        observed_story: Mutex::new(None),
        outcome: None,
    };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = tick_with_activity(
            fixture.store(),
            fixture.env(),
            &actuator,
            &activity,
            &inflight,
            fixture.project(),
        );
    }));

    assert!(result.is_err());
    assert_eq!(
        actuator.observed_story.lock().unwrap().as_deref(),
        Some("SH-1")
    );
    assert_eq!(activity.active_for(fixture.project()), None);
    assert!(lifecycle::read_inflight(fixture.env()).is_empty());
}

#[cfg(feature = "fault-injection")]
#[test]
fn ownership_is_cleared_when_outcome_recording_returns_an_error() {
    use storyhook::store::fault::{FaultAction, FaultPoint, arm};

    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    submitted(&fixture, "failing outcome write", Priority::High, PR_ONE);
    let activity = VerificationActivity::new();
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(fixture.env().clone());
    let actuator = ActivityObservingActuator {
        activity: activity.clone(),
        env: fixture.env().clone(),
        observed_story: Mutex::new(None),
        outcome: Some(VerificationOutcome::InfrastructureFailure {
            detail: "retry later".into(),
            disposition: storyhook::store::VerificationFailureDisposition::Retryable,
        }),
    };
    let _fault = arm(
        FaultPoint::BeforeCommit,
        FaultAction::Fail("outcome recording interrupted".into()),
    );

    let error = tick_with_activity(
        fixture.store(),
        fixture.env(),
        &actuator,
        &activity,
        &inflight,
        fixture.project(),
    )
    .expect_err("the injected outcome write must fail");

    assert!(error.to_string().contains("outcome recording interrupted"));
    assert_eq!(activity.active_for(fixture.project()), None);
    assert!(lifecycle::read_inflight(fixture.env()).is_empty());
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

    let actuator = FakeActuator::new(VerificationOutcome::Merged {
        tree: "must-not-run".into(),
        detail: "must-not-run".into(),
        gate: GateCommand::DEFAULT.into(),
    });
    assert_eq!(
        tick_with(fixture.store(), fixture.env(), &actuator, fixture.project()).unwrap(),
        TickResult::Returned
    );
    let returned = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::parse_id("SH", &id).unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(returned.state, "in-progress");
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
            registered: vec!["github.com/acme/replacement".to_string()],
        })
    );
}

#[test]
fn a_link_is_revalidated_after_the_registered_host_changes() {
    let fixture = ServiceFixture::new();
    let original = "https://github.com/acme/widgets";
    fixture.link_origin(original);
    let id = submitted(&fixture, "stale host", Priority::High, PR_ONE);
    let original = RemoteUrl::normalize(original).unwrap();
    fixture
        .store()
        .write(|tx| tx.unlink_remote(fixture.project(), &original).map(|_| ()))
        .unwrap();
    fixture.link_origin("https://github.example.com/acme/widgets");

    let selected = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(selected.story_id, id);
    assert_eq!(
        selected.pull_request,
        Err(VerificationProblem::UnregisteredPullRequest {
            url: PR_ONE.to_string(),
            registered: vec!["github.example.com/acme/widgets".to_string()],
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
                "dropped",
            ]
            .map(str::to_string),
        )
        .unwrap();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "verified", Priority::High, PR_ONE);
    let ctx = fixture.ctx();
    // The verdict precedes the close, as the verifier's own transaction
    // writes it: a `verifying` story completes only certified (SH-692).
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

/// One scripted answer to `notify`, consumed in order; an exhausted script
/// delivers.
enum NotifyScript {
    Absent(&'static str),
    Fail(&'static str),
}

struct FakeActuator {
    outcome: VerificationOutcome,
    /// Answers for successive `notify` calls; empty means every call delivers.
    notify_script: Mutex<VecDeque<NotifyScript>>,
    /// `Some(refusal)` makes `redispatch` refuse with that text.
    redispatch_refusal: Option<String>,
    notified: Mutex<Vec<String>>,
    redispatched: Mutex<Vec<(String, ResumePlan)>>,
    reaped: Mutex<Vec<String>>,
    /// Scripted submission answer; `None` adopts the linked pull request.
    submission: Option<Result<SubmittedPullRequest, SubmissionFailure>>,
    /// Every story this fake was asked to submit, in order.
    submitted: Mutex<Vec<String>>,
}

impl FakeActuator {
    fn new(outcome: VerificationOutcome) -> Self {
        Self {
            outcome,
            notify_script: Mutex::new(VecDeque::new()),
            redispatch_refusal: None,
            notified: Mutex::new(Vec::new()),
            redispatched: Mutex::new(Vec::new()),
            reaped: Mutex::new(Vec::new()),
            submission: None,
            submitted: Mutex::new(Vec::new()),
        }
    }

    /// Scripts the answer this fake gives `submit` (SH-647); `None` (the
    /// default) adopts whatever pull request the candidate already links.
    fn with_submission(
        mut self,
        submission: Result<SubmittedPullRequest, SubmissionFailure>,
    ) -> Self {
        self.submission = Some(submission);
        self
    }

    fn with_notify_script(self, script: impl IntoIterator<Item = NotifyScript>) -> Self {
        *self.notify_script.lock().unwrap() = script.into_iter().collect();
        self
    }

    fn refusing_redispatch(mut self, refusal: &str) -> Self {
        self.redispatch_refusal = Some(refusal.to_string());
        self
    }
}

impl VerificationActuator for FakeActuator {
    fn submit(
        &self,
        candidate: &VerificationCandidate,
    ) -> Result<SubmittedPullRequest, SubmissionFailure> {
        self.submitted
            .lock()
            .unwrap()
            .push(candidate.story_id.clone());
        match &self.submission {
            Some(scripted) => scripted.clone(),
            None => adopt_linked(candidate),
        }
    }

    fn verify(
        &self,
        _candidate: &VerificationCandidate,
        _pull_request: &storyhook::store::PrLink,
    ) -> VerificationOutcome {
        self.outcome.clone()
    }

    fn notify(
        &self,
        candidate: &VerificationCandidate,
        message: &str,
    ) -> Result<NotifyDelivery, AppError> {
        match self.notify_script.lock().unwrap().pop_front() {
            Some(NotifyScript::Fail(error)) => return Err(AppError::Storage(error.to_string())),
            Some(NotifyScript::Absent(reason)) => {
                return Ok(NotifyDelivery::AgentAbsent {
                    reason: reason.to_string(),
                    detail: format!("no live agent ({reason})"),
                });
            }
            None => {}
        }
        self.notified
            .lock()
            .unwrap()
            .push(format!("{}:{message}", candidate.story_id));
        Ok(NotifyDelivery::Delivered)
    }

    fn redispatch(
        &self,
        candidate: &VerificationCandidate,
        plan: &ResumePlan,
    ) -> Result<(), AppError> {
        self.redispatched
            .lock()
            .unwrap()
            .push((candidate.story_id.clone(), plan.clone()));
        match &self.redispatch_refusal {
            Some(refusal) => Err(AppError::Storage(refusal.clone())),
            None => Ok(()),
        }
    }

    fn reap(&self, candidate: &VerificationCandidate) -> Result<(), AppError> {
        self.reaped.lock().unwrap().push(candidate.story_id.clone());
        Ok(())
    }
}

#[test]
fn a_generationless_legacy_submission_remains_actionable_until_a_new_transition_exists() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let ctx = fixture.ctx();
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "legacy verification".into(),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id;
    PrLinkService::new(&ctx).link(&id, PR_ONE, true).unwrap();
    let story_no = StoryNo::parse_id("SH", &id).unwrap();
    fixture
        .store()
        .write(|tx| {
            let row = tx
                .story(fixture.project(), story_no)?
                .expect("the fixture story must exist");
            let mut snapshot = row.snapshot;
            snapshot.state = "verifying".into();
            tx.put_story(fixture.project(), &snapshot, row.head_seq)
        })
        .unwrap();
    let candidate = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    assert_eq!(candidate.verifying_generation, None);
    let actuator = FakeActuator::new(VerificationOutcome::TestsFailed {
        tree: "legacy-tree".into(),
        log: "/tmp/legacy.log".into(),
        detail: "legacy generation failed".into(),
        gate: GateCommand::DEFAULT.into(),
    });

    assert_eq!(
        tick_with(fixture.store(), fixture.env(), &actuator, fixture.project()).unwrap(),
        TickResult::Returned
    );
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), story_no))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "in-progress");
    assert!(
        row.snapshot
            .comments
            .iter()
            .any(|comment| comment.text.contains("legacy generation failed"))
    );
}

/// SH-692: the attempt PR #791's second `story move … verifying` cancelled
/// left nothing on the story — a resubmission withdraws the old generation's
/// authority and the old attempt is discarded, which is right, but the story
/// must say so. The superseded generation's PROGRESS comment ("running") is
/// retracted and a WITHDRAWN record names the replacement generation.
#[test]
fn a_superseded_attempt_records_its_withdrawal_naming_the_replacement_generation() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(
        &fixture,
        "resubmitted while running",
        Priority::High,
        PR_ONE,
    );
    let stale_progress = format!(
        "{GATE_PROGRESS_PREFIX} updated {FIXTURE_NOW}\n\nVerification (5/6, 3m 8s, running)\n"
    );
    StoryService::new(&fixture.ctx())
        .comment(&id, &stale_progress)
        .unwrap();
    let activity = VerificationActivity::new();
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(fixture.env().clone());
    let actuator = ResubmittingActuator {
        fixture: &fixture,
        outcomes: Mutex::new(VecDeque::from([
            VerificationOutcome::TestsFailed {
                tree: "stale-tree".into(),
                log: "/tmp/stale.log".into(),
                detail: "stale-tests-failed".into(),
                gate: GateCommand::DEFAULT.into(),
            },
            VerificationOutcome::TestsFailed {
                tree: "current-tree".into(),
                log: "/tmp/current.log".into(),
                detail: "current-generation-failed".into(),
                gate: GateCommand::DEFAULT.into(),
            },
        ])),
        verified_generations: Mutex::new(Vec::new()),
        notified: Mutex::new(Vec::new()),
        reaped: Mutex::new(Vec::new()),
    };

    assert_eq!(
        tick_with_reconciliation(
            fixture.store(),
            fixture.env(),
            &actuator,
            &activity,
            &inflight,
            fixture.project(),
            |_| Ok(None),
        )
        .unwrap(),
        TickResult::Returned
    );

    let generations = actuator.verified_generations.lock().unwrap();
    assert_eq!(generations.len(), 2);
    let row = story_row(&fixture, &id);
    let withdrawn: Vec<&str> = row
        .snapshot
        .comments
        .iter()
        .filter(|comment| comment.text.starts_with(VERIFICATION_WITHDRAWN_PREFIX))
        .map(|comment| comment.text.as_str())
        .collect();
    assert_eq!(withdrawn.len(), 1, "{:?}", row.snapshot.comments);
    assert!(
        withdrawn[0].contains(&format!(
            "resubmitted as generation {}",
            generations[1].get()
        )),
        "{}",
        withdrawn[0]
    );
    assert!(
        withdrawn[0].contains(&format!("(generation {})", generations[0].get())),
        "the record names the generation that was cancelled: {}",
        withdrawn[0]
    );
    assert!(withdrawn[0].contains(PR_ONE), "{}", withdrawn[0]);
    assert!(withdrawn[0].contains("judged nothing"), "{}", withdrawn[0]);
    assert!(
        !row.snapshot
            .comments
            .iter()
            .any(|comment| comment.text == stale_progress),
        "the superseded generation's 'running' PROGRESS comment is retracted: {:?}",
        row.snapshot.comments
    );
    assert!(
        row.snapshot
            .comments
            .iter()
            .any(|comment| comment.text.contains("current-generation-failed")),
        "the current generation's own verdict still lands: {:?}",
        row.snapshot.comments
    );
}

/// SH-692, the shape of the incident: a story is moved out of `verifying`
/// by hand while its gate runs. The attempt is withdrawn, its outcome is
/// discarded (never posted as a verdict about a story that has left the
/// queue), and the story records the withdrawal naming its new state.
#[test]
fn a_story_that_leaves_verifying_mid_attempt_records_its_withdrawal() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(
        &fixture,
        "moved out from under the gate",
        Priority::High,
        PR_ONE,
    );
    let activity = VerificationActivity::new();
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(fixture.env().clone());
    let actuator = DepartingActuator {
        fixture: &fixture,
        destination: "in-progress",
        notified: Mutex::new(Vec::new()),
    };

    assert_eq!(
        tick_with_activity(
            fixture.store(),
            fixture.env(),
            &actuator,
            &activity,
            &inflight,
            fixture.project(),
        )
        .unwrap(),
        TickResult::Returned
    );

    let row = story_row(&fixture, &id);
    assert_eq!(
        row.state, "in-progress",
        "the operator's state is preserved"
    );
    let withdrawn: Vec<&str> = row
        .snapshot
        .comments
        .iter()
        .filter(|comment| comment.text.starts_with(VERIFICATION_WITHDRAWN_PREFIX))
        .map(|comment| comment.text.as_str())
        .collect();
    assert_eq!(withdrawn.len(), 1, "{:?}", row.snapshot.comments);
    assert!(
        withdrawn[0].contains("left `verifying` (now `in-progress`)"),
        "{}",
        withdrawn[0]
    );
    assert!(
        !row.snapshot
            .comments
            .iter()
            .any(|comment| comment.text.contains("CENTRAL VERIFICATION RED")),
        "a discarded outcome is never posted: {:?}",
        row.snapshot.comments
    );
    assert!(
        actuator.notified.lock().unwrap().is_empty(),
        "nothing is delivered for a withdrawn attempt"
    );
    assert!(activity.active_for(fixture.project()).is_none());
}

/// SH-692: an operator stop (or daemon shutdown) during an attempt used to
/// leave the story's PROGRESS comment reading "running" indefinitely. The
/// stop rewrites it as INTERRUPTED; the story stays `verifying` and current,
/// so the next verifier start re-runs it from the beginning.
#[test]
fn a_manual_stop_during_an_attempt_rewrites_progress_as_interrupted() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "stopped mid-attempt", Priority::High, PR_ONE);
    let activity = VerificationActivity::new();
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(fixture.env().clone());
    let actuator = StoppingActuator {
        fixture: &fixture,
        activity: &activity,
    };

    assert_eq!(
        tick_with_activity(
            fixture.store(),
            fixture.env(),
            &actuator,
            &activity,
            &inflight,
            fixture.project(),
        )
        .unwrap(),
        TickResult::Stopped
    );

    let row = story_row(&fixture, &id);
    assert_eq!(row.state, "verifying", "a stop preserves the submission");
    let progress: Vec<&str> = row
        .snapshot
        .comments
        .iter()
        .filter(|comment| comment.text.starts_with(GATE_PROGRESS_PREFIX))
        .map(|comment| comment.text.as_str())
        .collect();
    assert_eq!(progress.len(), 1, "{:?}", row.snapshot.comments);
    assert!(progress[0].contains("INTERRUPTED"), "{}", progress[0]);
    assert!(progress[0].contains("judged nothing"), "{}", progress[0]);
    assert!(
        !row.snapshot
            .comments
            .iter()
            .any(|comment| comment.text.starts_with(VERIFICATION_WITHDRAWN_PREFIX)),
        "a stop is an interruption of a still-current generation, not a withdrawal: {:?}",
        row.snapshot.comments
    );
}

/// Moves the story out of `verifying` while its attempt runs, then answers
/// red — the outcome the verifier must discard (SH-692).
struct DepartingActuator<'a> {
    fixture: &'a ServiceFixture,
    destination: &'static str,
    notified: Mutex<Vec<String>>,
}

impl VerificationActuator for DepartingActuator<'_> {
    fn submit(
        &self,
        candidate: &VerificationCandidate,
    ) -> Result<SubmittedPullRequest, SubmissionFailure> {
        adopt_linked(candidate)
    }

    fn verify(
        &self,
        candidate: &VerificationCandidate,
        _pull_request: &PrLink,
    ) -> VerificationOutcome {
        StoryService::new(&self.fixture.ctx())
            .set_state(
                &candidate.story_id,
                self.destination,
                None,
                Some("verifying"),
                None,
            )
            .expect("the operator moves the story while the attempt runs");
        VerificationOutcome::TestsFailed {
            tree: "departed-tree".into(),
            log: "/tmp/departed.log".into(),
            detail: "a verdict about a story that left".into(),
            gate: GateCommand::DEFAULT.into(),
        }
    }

    fn notify(
        &self,
        candidate: &VerificationCandidate,
        message: &str,
    ) -> Result<NotifyDelivery, AppError> {
        self.notified
            .lock()
            .unwrap()
            .push(format!("{}: {message}", candidate.story_id));
        Ok(NotifyDelivery::Delivered)
    }

    fn redispatch(
        &self,
        _candidate: &VerificationCandidate,
        _plan: &ResumePlan,
    ) -> Result<(), AppError> {
        panic!("a withdrawn attempt never re-dispatches")
    }

    fn reap(&self, _candidate: &VerificationCandidate) -> Result<(), AppError> {
        panic!("a withdrawn attempt never reaps")
    }
}

/// Latches the operator's stop on the owned attempt from inside it, the way
/// a dashboard stop lands while a gate runs, and answers as a cancelled
/// subprocess would (SH-692).
struct StoppingActuator<'a> {
    fixture: &'a ServiceFixture,
    activity: &'a VerificationActivity,
}

impl VerificationActuator for StoppingActuator<'_> {
    fn submit(
        &self,
        candidate: &VerificationCandidate,
    ) -> Result<SubmittedPullRequest, SubmissionFailure> {
        adopt_linked(candidate)
    }

    fn verify(
        &self,
        candidate: &VerificationCandidate,
        _pull_request: &PrLink,
    ) -> VerificationOutcome {
        self.activity
            .control(
                self.fixture.store(),
                candidate.project,
                VerificationAction::Stop,
            )
            .expect("the operator stops the verifier while the attempt runs");
        VerificationOutcome::Cancelled
    }

    fn notify(
        &self,
        _candidate: &VerificationCandidate,
        _message: &str,
    ) -> Result<NotifyDelivery, AppError> {
        panic!("a stopped attempt never notifies")
    }

    fn redispatch(
        &self,
        _candidate: &VerificationCandidate,
        _plan: &ResumePlan,
    ) -> Result<(), AppError> {
        panic!("a stopped attempt never re-dispatches")
    }

    fn reap(&self, _candidate: &VerificationCandidate) -> Result<(), AppError> {
        panic!("a stopped attempt never reaps")
    }
}

struct ResubmittingActuator<'a> {
    fixture: &'a ServiceFixture,
    outcomes: Mutex<VecDeque<VerificationOutcome>>,
    verified_generations: Mutex<Vec<GlobalSeq>>,
    notified: Mutex<Vec<GlobalSeq>>,
    reaped: Mutex<Vec<GlobalSeq>>,
}

impl VerificationActuator for ResubmittingActuator<'_> {
    fn submit(
        &self,
        candidate: &VerificationCandidate,
    ) -> Result<SubmittedPullRequest, SubmissionFailure> {
        adopt_linked(candidate)
    }

    fn verify(
        &self,
        candidate: &VerificationCandidate,
        _pull_request: &PrLink,
    ) -> VerificationOutcome {
        let generation = candidate
            .verifying_generation
            .expect("every submitted fixture has a generation");
        let first = {
            let mut verified = self.verified_generations.lock().unwrap();
            verified.push(generation);
            verified.len() == 1
        };
        if first {
            StoryService::new(&self.fixture.ctx())
                .set_state(
                    &candidate.story_id,
                    "verifying",
                    None,
                    Some("verifying"),
                    None,
                )
                .expect("same-state resubmission during the blocked actuator");
        }
        self.outcomes
            .lock()
            .unwrap()
            .pop_front()
            .expect("every attempted generation has a fixture outcome")
    }

    fn notify(
        &self,
        candidate: &VerificationCandidate,
        _message: &str,
    ) -> Result<NotifyDelivery, AppError> {
        self.notified
            .lock()
            .unwrap()
            .push(candidate.verifying_generation.unwrap());
        Ok(NotifyDelivery::Delivered)
    }

    fn redispatch(
        &self,
        _candidate: &VerificationCandidate,
        _plan: &ResumePlan,
    ) -> Result<(), AppError> {
        panic!("a delivered notification never re-dispatches")
    }

    fn reap(&self, candidate: &VerificationCandidate) -> Result<(), AppError> {
        self.reaped
            .lock()
            .unwrap()
            .push(candidate.verifying_generation.unwrap());
        Ok(())
    }
}

#[test]
fn every_superseded_outcome_is_discarded_before_the_latest_generation_runs() {
    let stale_outcomes = [
        VerificationOutcome::Merged {
            tree: "stale-tree".into(),
            detail: "stale-merged".into(),
            gate: GateCommand::DEFAULT.into(),
        },
        VerificationOutcome::Conflict {
            detail: "stale-conflict".into(),
        },
        VerificationOutcome::InvalidSubmission {
            detail: "stale-invalid".into(),
        },
        VerificationOutcome::TestsFailed {
            tree: "stale-tree".into(),
            log: "/tmp/stale.log".into(),
            detail: "stale-tests-failed".into(),
            gate: GateCommand::DEFAULT.into(),
        },
        VerificationOutcome::InfrastructureFailure {
            detail: "stale-infrastructure".into(),
            disposition: VerificationFailureDisposition::Permanent,
        },
    ];

    for stale in stale_outcomes {
        let fixture = ServiceFixture::new();
        fixture.link_origin("https://github.com/acme/widgets");
        let id = submitted(
            &fixture,
            "resubmitted while running",
            Priority::High,
            PR_ONE,
        );
        let activity = VerificationActivity::new();
        std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
        let inflight = InFlight::new(fixture.env().clone());
        let actuator = ResubmittingActuator {
            fixture: &fixture,
            outcomes: Mutex::new(VecDeque::from([
                stale,
                VerificationOutcome::TestsFailed {
                    tree: "current-tree".into(),
                    log: "/tmp/current.log".into(),
                    detail: "current-generation-failed".into(),
                    gate: GateCommand::DEFAULT.into(),
                },
            ])),
            verified_generations: Mutex::new(Vec::new()),
            notified: Mutex::new(Vec::new()),
            reaped: Mutex::new(Vec::new()),
        };

        assert_eq!(
            tick_with_reconciliation(
                fixture.store(),
                fixture.env(),
                &actuator,
                &activity,
                &inflight,
                fixture.project(),
                |_| Ok(None),
            )
            .unwrap(),
            TickResult::Returned
        );

        let generations = actuator.verified_generations.lock().unwrap();
        assert_eq!(generations.len(), 2, "the latest generation must run next");
        assert_ne!(generations[0], generations[1]);
        assert_eq!(
            actuator.notified.lock().unwrap().as_slice(),
            &[generations[1]],
            "only the current red outcome may notify"
        );
        assert!(
            actuator.reaped.lock().unwrap().is_empty(),
            "a stale green outcome must not reap"
        );
        drop(generations);

        let row = fixture
            .store()
            .read(|tx| tx.story(fixture.project(), StoryNo::parse_id("SH", &id).unwrap()))
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "in-progress");
        assert!(row.snapshot.comments.iter().any(|comment| {
            comment.text.contains("CENTRAL VERIFICATION RED")
                && comment.text.contains("current-generation-failed")
                && comment.text.contains("current-tree")
        }));
        assert!(
            row.snapshot
                .comments
                .iter()
                .all(|comment| !comment.text.contains("stale-")),
            "a superseded outcome must leave no durable evidence"
        );
        assert!(
            fixture
                .store()
                .read(|tx| tx.verification_incident(fixture.project()))
                .unwrap()
                .is_none()
        );
    }
}

struct WebMutationActuator<'a> {
    fixture: &'a ServiceFixture,
    activity: VerificationActivity,
    reopen: bool,
    outcome: VerificationOutcome,
    reaped: Mutex<Vec<String>>,
}

impl WebMutationActuator<'_> {
    fn post(&self, path: &str, body: &str) {
        let headers = [
            Header::from_bytes("Host", "127.0.0.1:3456").unwrap(),
            Header::from_bytes("X-Storyhook", "1").unwrap(),
            Header::from_bytes("Content-Type", "application/json").unwrap(),
        ];
        let routed = rest::route_with_activity(
            self.fixture.store(),
            self.fixture.env(),
            &self.activity,
            rest::RouteRequest::new(&Method::Post, path, &headers, body),
            &TrustedHosts::default(),
        );
        assert_eq!(routed.reply.status, 200, "web mutation failed: {path}");
    }
}

impl VerificationActuator for WebMutationActuator<'_> {
    fn submit(
        &self,
        candidate: &VerificationCandidate,
    ) -> Result<SubmittedPullRequest, SubmissionFailure> {
        adopt_linked(candidate)
    }

    fn verify(
        &self,
        candidate: &VerificationCandidate,
        _pull_request: &PrLink,
    ) -> VerificationOutcome {
        let base = format!(
            "/api/repos/{}/story/{}",
            candidate.project_slug, candidate.story_id
        );
        // A UI completion of a `verifying` story is an override and carries
        // its reason (SH-692); the bare move is refused.
        self.post(
            &format!("{base}/move"),
            r#"{"state":"done","comment":"completed from the dashboard while the attempt ran"}"#,
        );
        if self.reopen {
            self.post(&format!("{base}/reopen"), "{}");
        }
        self.outcome.clone()
    }

    fn notify(
        &self,
        _candidate: &VerificationCandidate,
        _message: &str,
    ) -> Result<NotifyDelivery, AppError> {
        panic!("a stale UI-raced outcome must not notify")
    }

    fn redispatch(
        &self,
        _candidate: &VerificationCandidate,
        _plan: &ResumePlan,
    ) -> Result<(), AppError> {
        panic!("a stale UI-raced outcome must not re-dispatch")
    }

    fn reap(&self, candidate: &VerificationCandidate) -> Result<(), AppError> {
        self.reaped.lock().unwrap().push(candidate.story_id.clone());
        Ok(())
    }
}

#[test]
fn ui_done_and_reopen_make_every_delayed_outcome_authorityless() {
    let cases = [
        (
            false,
            VerificationOutcome::InfrastructureFailure {
                detail: "stale-after-ui-done".into(),
                disposition: VerificationFailureDisposition::Permanent,
            },
            "done",
        ),
        (
            false,
            VerificationOutcome::Merged {
                tree: "stale-tree".into(),
                detail: "stale-after-ui-done".into(),
                gate: GateCommand::DEFAULT.into(),
            },
            "done",
        ),
        (
            true,
            VerificationOutcome::TestsFailed {
                tree: "stale-tree".into(),
                log: "/tmp/stale.log".into(),
                detail: "stale-after-ui-reopen".into(),
                gate: GateCommand::DEFAULT.into(),
            },
            "todo",
        ),
    ];

    for (reopen, outcome, expected_state) in cases {
        let fixture = ServiceFixture::new();
        fixture.link_origin("https://github.com/acme/widgets");
        let id = submitted(&fixture, "UI raced verifier", Priority::High, PR_ONE);
        let activity = VerificationActivity::new();
        std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
        let inflight = InFlight::new(fixture.env().clone());
        let actuator = WebMutationActuator {
            fixture: &fixture,
            activity: activity.clone(),
            reopen,
            outcome,
            reaped: Mutex::new(Vec::new()),
        };

        assert_eq!(
            tick_with_activity(
                fixture.store(),
                fixture.env(),
                &actuator,
                &activity,
                &inflight,
                fixture.project(),
            )
            .unwrap(),
            TickResult::Returned
        );

        let row = fixture
            .store()
            .read(|tx| tx.story(fixture.project(), StoryNo::parse_id("SH", &id).unwrap()))
            .unwrap()
            .unwrap();
        assert_eq!(row.state, expected_state);
        assert!(
            row.snapshot
                .comments
                .iter()
                .all(|comment| !comment.text.contains("stale-after-ui"))
        );
        assert!(actuator.reaped.lock().unwrap().is_empty());
        assert!(
            fixture
                .store()
                .read(|tx| tx.verification_incident(fixture.project()))
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn a_conflict_without_a_resubmission_waiter_returns_the_story_to_its_agent() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "conflicted", Priority::High, PR_ONE);
    let root = scratch_dir();
    let env = Environment::at(root.path());
    let actuator = FakeActuator::new(VerificationOutcome::Conflict {
        detail: "both modified src/lib.rs".into(),
    });

    assert_eq!(
        tick_with(fixture.store(), &env, &actuator, fixture.project()).unwrap(),
        TickResult::Returned
    );
    let story_no = StoryNo::parse_id("SH", &id).unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), story_no))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "in-progress");
    let conflict = &row.snapshot.comments.last().unwrap().text;
    assert!(conflict.contains("CONFLICT"));
    assert!(conflict.contains("its current base branch"));
    assert!(!conflict.contains("origin/main"));
    assert_eq!(actuator.notified.lock().unwrap().len(), 1);
    assert!(actuator.reaped.lock().unwrap().is_empty());
}

struct SequencedActuator {
    outcomes: Mutex<VecDeque<VerificationOutcome>>,
    verified: Mutex<Vec<String>>,
    notified: Mutex<Vec<String>>,
    reaped: Mutex<Vec<String>>,
}

impl VerificationActuator for SequencedActuator {
    fn submit(
        &self,
        candidate: &VerificationCandidate,
    ) -> Result<SubmittedPullRequest, SubmissionFailure> {
        adopt_linked(candidate)
    }

    fn verify(
        &self,
        candidate: &VerificationCandidate,
        _pull_request: &PrLink,
    ) -> VerificationOutcome {
        self.verified
            .lock()
            .unwrap()
            .push(candidate.story_id.clone());
        self.outcomes
            .lock()
            .unwrap()
            .pop_front()
            .expect("every verification attempt must have a fixture outcome")
    }

    fn notify(
        &self,
        candidate: &VerificationCandidate,
        _message: &str,
    ) -> Result<NotifyDelivery, AppError> {
        self.notified
            .lock()
            .unwrap()
            .push(candidate.story_id.clone());
        Ok(NotifyDelivery::Delivered)
    }

    fn redispatch(
        &self,
        _candidate: &VerificationCandidate,
        _plan: &ResumePlan,
    ) -> Result<(), AppError> {
        panic!("a delivered notification never re-dispatches")
    }

    fn reap(&self, candidate: &VerificationCandidate) -> Result<(), AppError> {
        self.reaped.lock().unwrap().push(candidate.story_id.clone());
        Ok(())
    }
}

#[test]
fn reconciliation_keeps_the_verifier_until_the_same_story_is_reverified() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let held = submitted(&fixture, "already under test", Priority::Low, PR_ONE);
    let first_generation = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap()
        .verifying_generation;
    let activity = VerificationActivity::new();
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(fixture.env().clone());
    let actuator = SequencedActuator {
        outcomes: Mutex::new(VecDeque::from([
            VerificationOutcome::Conflict {
                detail: "both modified src/lib.rs".into(),
            },
            VerificationOutcome::Conflict {
                detail: "main advanced again".into(),
            },
            VerificationOutcome::Merged {
                tree: "abc123".into(),
                detail: "landed after reconciliation".into(),
                gate: GateCommand::DEFAULT.into(),
            },
        ])),
        verified: Mutex::new(Vec::new()),
        notified: Mutex::new(Vec::new()),
        reaped: Mutex::new(Vec::new()),
    };
    let waiting = Mutex::new(None);
    let reserved_generations = Mutex::new(Vec::new());

    let result = tick_with_reconciliation(
        fixture.store(),
        fixture.env(),
        &actuator,
        &activity,
        &inflight,
        fixture.project(),
        |reserved| {
            assert_eq!(reserved.story_id, held);
            reserved_generations
                .lock()
                .unwrap()
                .push(reserved.verifying_generation);
            assert_eq!(
                activity
                    .active_for(fixture.project())
                    .as_ref()
                    .map(|active| &active.story_id),
                Some(&held),
                "process-local ownership must span reconciliation"
            );
            assert_eq!(
                lifecycle::read_inflight(fixture.env()).len(),
                1,
                "shutdown ownership must span reconciliation"
            );

            let mut waiting = waiting.lock().unwrap();
            let waiting = waiting.get_or_insert_with(|| {
                submitted(
                    &fixture,
                    "higher priority arrival",
                    Priority::Critical,
                    PR_TWO,
                )
            });
            StoryService::new(&fixture.ctx())
                .set_state(&held, "verifying", None, Some("in-progress"), None)
                .unwrap();
            let ordered = VerificationQueue::new(fixture.store()).ordered().unwrap();
            assert_eq!(ordered[0].story_id, *waiting);
            Ok(ordered
                .into_iter()
                .find(|candidate| candidate.story_id == held))
        },
    )
    .unwrap();

    assert_eq!(result, TickResult::Completed);
    assert_eq!(
        actuator.verified.lock().unwrap().as_slice(),
        [held.as_str(), held.as_str(), held.as_str()],
        "queue priority must not preempt a reconciliation reservation"
    );
    assert_eq!(
        actuator.notified.lock().unwrap().as_slice(),
        [held.as_str(), held.as_str()]
    );
    let generations = reserved_generations.lock().unwrap();
    assert_eq!(generations.len(), 2);
    assert_eq!(generations[0], first_generation);
    assert_ne!(generations[0], generations[1]);
    assert_eq!(actuator.reaped.lock().unwrap().as_slice(), [held.as_str()]);
    assert_eq!(activity.active_for(fixture.project()), None);
    assert!(lifecycle::read_inflight(fixture.env()).is_empty());
}

fn story_row(fixture: &ServiceFixture, id: &str) -> storyhook::store::StoryRow {
    fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::parse_id("SH", id).unwrap()))
        .unwrap()
        .unwrap()
}

/// SH-650 (D-E): the conflict hold survives a dead pane. An absent agent is
/// re-dispatched into its own window with the resume clause, the diagnosis is
/// pasted afterwards, and the verifier keeps its reservation for the
/// resubmission exactly as it does when the first paste lands.
#[test]
fn a_conflict_returned_to_a_dead_pane_is_redispatched_and_still_holds_the_queue() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "dead pane reconciliation", Priority::High, PR_ONE);
    let activity = VerificationActivity::new();
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(fixture.env().clone());
    let actuator = FakeActuator::new(VerificationOutcome::Conflict {
        detail: "both modified src/lib.rs".into(),
    })
    .with_notify_script([NotifyScript::Absent("pane-dead")]);
    let waited = Mutex::new(Vec::new());

    assert_eq!(
        tick_with_reconciliation(
            fixture.store(),
            fixture.env(),
            &actuator,
            &activity,
            &inflight,
            fixture.project(),
            |reserved| {
                assert_eq!(reserved.story_id, id);
                assert_eq!(
                    activity
                        .active_for(fixture.project())
                        .map(|active| active.story_id),
                    Some(id.clone()),
                    "the reservation must survive the re-dispatch"
                );
                let row = story_row(&fixture, &id);
                assert_eq!(row.state, "in-progress");
                assert_eq!(row.awaiting, None, "a re-dispatched story is never parked");
                waited.lock().unwrap().push(reserved.story_id.clone());
                Ok(None)
            },
        )
        .unwrap(),
        TickResult::Returned
    );
    assert_eq!(waited.lock().unwrap().as_slice(), std::slice::from_ref(&id));
    let redispatched = actuator.redispatched.lock().unwrap();
    assert_eq!(redispatched.len(), 1, "exactly one resume re-dispatch");
    assert_eq!(redispatched[0].0, id);
    assert_eq!(
        redispatched[0].1,
        ResumePlan::default(),
        "no engine lane holds the story, so the helper reads the provider itself"
    );
    let notified = actuator.notified.lock().unwrap();
    assert_eq!(
        notified.len(),
        1,
        "the diagnosis is pasted once the agent is back"
    );
    assert!(notified[0].contains("CENTRAL VERIFICATION CONFLICT"));
    let comments: Vec<String> = story_row(&fixture, &id)
        .snapshot
        .comments
        .iter()
        .map(|comment| comment.text.clone())
        .collect();
    assert!(
        comments
            .iter()
            .any(|text| text.starts_with("CENTRAL VERIFICATION RESUME —")
                && text.contains("pane-dead")
                && text.contains("resume clause")),
        "the re-dispatch leaves a trail naming why: {comments:?}"
    );
    let resume_index = comments
        .iter()
        .position(|text| text.starts_with("CENTRAL VERIFICATION RESUME —"))
        .unwrap();
    let diagnosis_index = comments
        .iter()
        .position(|text| text.starts_with("CENTRAL VERIFICATION CONFLICT"))
        .unwrap();
    assert!(
        diagnosis_index < resume_index,
        "the diagnosis is on the story before the agent is re-dispatched to read it"
    );
}

/// SH-650: a refused re-dispatch is the ONE case that still parks the story,
/// naming the refusal, and the conflict reservation is released because no
/// resubmission will come.
#[test]
fn a_refused_resume_redispatch_parks_the_story_and_releases_the_reservation() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(
        &fixture,
        "unreachable reconciliation",
        Priority::High,
        PR_ONE,
    );
    let activity = VerificationActivity::new();
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(fixture.env().clone());
    let actuator = FakeActuator::new(VerificationOutcome::Conflict {
        detail: "both modified src/lib.rs".into(),
    })
    .with_notify_script([NotifyScript::Absent("pane-unavailable")])
    .refusing_redispatch("resume-unsafe: the worktree is registered on another branch");

    assert_eq!(
        tick_with_reconciliation(
            fixture.store(),
            fixture.env(),
            &actuator,
            &activity,
            &inflight,
            fixture.project(),
            |_| panic!("a refused re-dispatch must not enter the reservation wait"),
        )
        .unwrap(),
        TickResult::Returned
    );
    let row = story_row(&fixture, &id);
    assert_eq!(row.state, "in-progress");
    let awaiting = row.awaiting.unwrap();
    assert!(
        awaiting.contains("could not re-dispatch its agent"),
        "{awaiting}"
    );
    assert!(awaiting.contains("resume-unsafe"), "{awaiting}");
    assert!(awaiting.contains("pane-unavailable"), "{awaiting}");
    assert_eq!(actuator.redispatched.lock().unwrap().len(), 1);
    assert!(actuator.notified.lock().unwrap().is_empty());
    assert_eq!(activity.active_for(fixture.project()), None);
    assert!(lifecycle::read_inflight(fixture.env()).is_empty());
}

/// SH-650: a refusal that is not evidence of absence — tmux could not be
/// asked, or a live pane refused the paste — never triggers a respawn, which
/// would kill a live agent. The story is parked exactly as before.
#[test]
fn a_notify_failure_that_is_not_absence_parks_without_redispatching() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "paste refused", Priority::High, PR_ONE);
    let activity = VerificationActivity::new();
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(fixture.env().clone());
    let actuator = FakeActuator::new(VerificationOutcome::Conflict {
        detail: "both modified src/lib.rs".into(),
    })
    .with_notify_script([NotifyScript::Fail(
        "could not paste the verification remediation into pane `%3`",
    )]);

    assert_eq!(
        tick_with_reconciliation(
            fixture.store(),
            fixture.env(),
            &actuator,
            &activity,
            &inflight,
            fixture.project(),
            |_| panic!("a failed notification must not enter the reservation wait"),
        )
        .unwrap(),
        TickResult::Returned
    );
    let row = story_row(&fixture, &id);
    assert_eq!(row.state, "in-progress");
    assert!(
        row.awaiting
            .unwrap()
            .contains("could not paste the verification remediation")
    );
    assert!(
        actuator.redispatched.lock().unwrap().is_empty(),
        "a live pane is never respawned over"
    );
    assert_eq!(activity.active_for(fixture.project()), None);
    assert!(lifecycle::read_inflight(fixture.env()).is_empty());
}

/// SH-650 (D-E): once the re-dispatch succeeded the agent is live under the
/// resume charter, which reads the story's comments first — so a paste that
/// fails afterwards is recorded on the story and remediation still counts as
/// started; `awaiting` is set only when the re-dispatch itself is refused.
#[test]
fn a_paste_that_fails_after_a_successful_redispatch_is_recorded_not_parked() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "late paste", Priority::High, PR_ONE);
    let activity = VerificationActivity::new();
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(fixture.env().clone());
    let actuator = FakeActuator::new(VerificationOutcome::Conflict {
        detail: "both modified src/lib.rs".into(),
    })
    .with_notify_script([
        NotifyScript::Absent("pane-changed"),
        NotifyScript::Fail("tmux refused the submit key"),
    ]);
    let entered = Mutex::new(false);

    assert_eq!(
        tick_with_reconciliation(
            fixture.store(),
            fixture.env(),
            &actuator,
            &activity,
            &inflight,
            fixture.project(),
            |_| {
                *entered.lock().unwrap() = true;
                Ok(None)
            },
        )
        .unwrap(),
        TickResult::Returned
    );
    assert!(*entered.lock().unwrap(), "remediation counts as started");
    let row = story_row(&fixture, &id);
    assert_eq!(row.awaiting, None);
    let comments: Vec<String> = row
        .snapshot
        .comments
        .iter()
        .map(|comment| comment.text.clone())
        .collect();
    assert!(
        comments
            .iter()
            .any(|text| text.contains("could not be pasted afterwards")
                && text.contains("tmux refused the submit key")),
        "{comments:?}"
    );
    assert_eq!(actuator.redispatched.lock().unwrap().len(), 1);
}

#[test]
fn an_origin_mismatch_returns_the_story_for_a_safe_resubmission() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "wrong checkout", Priority::High, PR_ONE);
    let root = scratch_dir();
    let actuator = FakeActuator::new(VerificationOutcome::InvalidSubmission {
        detail: "checkout origin is acme/replacement".into(),
    });

    assert_eq!(
        tick_with(
            fixture.store(),
            &Environment::at(root.path()),
            &actuator,
            fixture.project(),
        )
        .unwrap(),
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

    for (origin_url, expected_origin) in [
        (
            "https://github.com/acme/replacement.git",
            "github.com/acme/replacement",
        ),
        (
            "https://github.example.com/acme/widgets.git",
            "github.example.com/acme/widgets",
        ),
    ] {
        let checkout = scratch_dir();
        let init = Command::new("git")
            .args(["init", "-q"])
            .current_dir(checkout.path())
            .output()
            .unwrap();
        assert!(init.status.success());
        let origin = Command::new("git")
            .args(["config", "remote.origin.url", origin_url])
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
            blocking_revision: None,
            checkout: checkout.path().to_path_buf(),
            cleanup_lease: None,
            pull_request: Err(VerificationProblem::MissingPullRequest),
        };

        let outcome = actuator.verify(&candidate, &pull_request);
        assert!(matches!(
            outcome,
            VerificationOutcome::InvalidSubmission { ref detail }
                if detail.contains(expected_origin)
                    && detail.contains("github.com/acme/widgets")
        ));
    }
}

#[test]
fn the_shell_actuator_times_out_the_whole_group_after_allowing_cleanup() {
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
            "https://github.com/acme/widgets.git",
        ])
        .current_dir(checkout.path())
        .output()
        .unwrap();
    assert!(origin.status.success());
    // The hanging verifier stands in for the bundle, from its own directory:
    // the checkout holds no scripts at all (SH-654). Its cwd is still the
    // checkout, which is where the cleanup witnesses land.
    let tools = scratch_dir();
    std::fs::write(
        tools.path().join("verify-pr.sh"),
        r#"#!/bin/bash
trap 'printf cleanup > cleanup-started; wait; exit 143' TERM
sh -c 'trap "" TERM; printf "%s" "$$" > stubborn-child-pid; while :; do sleep 30; done' &
wait
"#,
    )
    .unwrap();

    let candidate = VerificationCandidate {
        project: fixture.project(),
        project_slug: "fixture".into(),
        story_id: "SH-1".into(),
        title: "bounded verification".into(),
        priority: Priority::High,
        created_at: FIXTURE_NOW.into(),
        verifying_since: Some(FIXTURE_NOW.into()),
        verifying_generation: None,
        blocking_revision: None,
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
        linked_at: FIXTURE_NOW.into(),
        last_checked_at: None,
    };
    let env_root = scratch_dir();
    let daemon_env = Environment::at(env_root.path());
    let actuator = ShellVerificationActuator::with_paths_and_timing(
        daemon_env.clone(),
        checkout.path().join("unused-helper"),
        PathBuf::from("/usr/bin/true"),
        Duration::from_millis(500),
        Duration::from_secs(1),
        Duration::from_millis(100),
    )
    .with_verifier_script(tools.path().join("verify-pr.sh"));

    let outcome = thread::scope(|scope| {
        let running = scope.spawn(|| actuator.verify(&candidate, &pull_request));
        let ready_by = Instant::now() + Duration::from_millis(250);
        let active = loop {
            let active = lifecycle::read_owned_processes(&daemon_env);
            if !active.is_empty() || Instant::now() >= ready_by {
                break active;
            }
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(active.len(), 1, "the verifier group was never registered");
        assert_eq!(active[0].role, "verifier");
        assert_eq!(
            active[0].request_id.as_deref(),
            Some("verify:fixture:SH-1:legacy")
        );
        running.join().expect("the verifier thread must not panic")
    });
    assert!(
        lifecycle::read_owned_processes(&daemon_env).is_empty(),
        "the reaped verifier must retract its process-group registration"
    );

    let detail = match outcome {
        VerificationOutcome::InfrastructureFailure { detail, .. } => detail,
        other => panic!("a timed-out verifier is infrastructure failure, got {other:?}"),
    };
    assert!(detail.contains("500ms"), "{detail}");
    assert!(detail.contains("SIGTERM"), "{detail}");
    assert!(detail.contains("SIGKILL"), "{detail}");
    assert!(checkout.path().join("cleanup-started").is_file());
    let pid: i32 = std::fs::read_to_string(checkout.path().join("stubborn-child-pid"))
        .unwrap()
        .parse()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert_ne!(
        unsafe { libc::kill(pid, 0) },
        0,
        "stubborn verifier descendant {pid} survived the timeout"
    );
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
        blocking_revision: None,
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

/// SH-650: the shell actuator classifies the helper's refusal SLUG, never its
/// prose. Every slug `cmd_notify` can emit is in `NOTIFY_REFUSALS`
/// (`tests/notify_reasons.rs` derives that); here the wire shape is driven
/// through a stub helper so the classification is proven at the boundary.
#[test]
fn shell_notification_classifies_absence_by_the_helpers_reason_slug() {
    use storyhook::daemon::verification::{AgentPresence, NOTIFY_REFUSALS, agent_presence};

    let fixture = ServiceFixture::new();
    let root = scratch_dir();
    let candidate = cleanup_candidate(&fixture, root.path());
    let helper = root.path().join("notify-helper.sh");
    let actuator = ShellVerificationActuator::with_paths(
        Environment::at(root.path()),
        helper.clone(),
        PathBuf::from("/usr/bin/true"),
    );
    let refuse = |reason: &str| {
        std::fs::write(
            &helper,
            format!(
                "#!/bin/bash\nprintf '%s\\n' '{{\"ok\":false,\"reason\":\"{reason}\",\"display\":\"the helper said no ({reason})\"}}'\nexit 1\n"
            ),
        )
        .unwrap();
    };

    for (reason, presence) in NOTIFY_REFUSALS {
        refuse(reason);
        let answer = actuator.notify(&candidate, "diagnosis");
        match presence {
            AgentPresence::Absent => assert_eq!(
                answer.unwrap(),
                NotifyDelivery::AgentAbsent {
                    reason: reason.to_string(),
                    detail: format!("the helper said no ({reason})"),
                },
                "{reason} means the agent is absent"
            ),
            AgentPresence::NotAbsent => {
                let error = answer.unwrap_err().to_string();
                assert!(error.contains(reason), "{reason}: {error}");
            }
        }
    }

    // A slug the table does not know, and a refusal with no slug at all, are
    // never a licence to respawn (fail closed).
    refuse("pane-vaporised");
    assert!(actuator.notify(&candidate, "diagnosis").is_err());
    std::fs::write(
        &helper,
        "#!/bin/bash\nprintf '%s\\n' '{\"ok\":false,\"display\":\"no slug\"}'\nexit 1\n",
    )
    .unwrap();
    assert!(actuator.notify(&candidate, "diagnosis").is_err());
    assert_eq!(agent_presence(None), AgentPresence::NotAbsent);
    assert_eq!(
        agent_presence(Some("pane-vaporised")),
        AgentPresence::NotAbsent
    );

    std::fs::write(
        &helper,
        "#!/bin/bash\nprintf '%s\\n' '{\"ok\":true,\"display\":\"notified\"}'\n",
    )
    .unwrap();
    assert_eq!(
        actuator.notify(&candidate, "diagnosis").unwrap(),
        NotifyDelivery::Delivered
    );
}

/// SH-650: the re-dispatch is the helper's own `dispatch <id> --resume --auto`,
/// composed by the one argv composer the dashboard and the engine use, with
/// the target session a daemon must name — proven at the boundary by a stub
/// helper that records what it was asked.
#[test]
fn shell_redispatch_asks_the_helper_for_a_resume_of_the_same_story() {
    let fixture = ServiceFixture::new();
    let root = scratch_dir();
    let candidate = cleanup_candidate(&fixture, root.path());
    let helper = root.path().join("dispatch-helper.sh");
    let record = root.path().join("dispatch-record");
    std::fs::write(
        &helper,
        format!(
            "#!/bin/bash\nprintf '%s\\n' \"$*\" > {record}\nprintf 'STORY_TARGET_SESSION=%s STORY_CREATE_SESSION=%s\\n' \"${{STORY_TARGET_SESSION-unset}}\" \"${{STORY_CREATE_SESSION-unset}}\" >> {record}\nprintf '%s\\n' '{{\"ok\":true,\"display\":\"resumed\"}}'\n",
            record = record.display()
        ),
    )
    .unwrap();
    let actuator = ShellVerificationActuator::with_paths(
        Environment::at(root.path()),
        helper.clone(),
        PathBuf::from("/usr/bin/true"),
    );

    actuator
        .redispatch(&candidate, &ResumePlan::default())
        .unwrap();
    let recorded = std::fs::read_to_string(&record).unwrap();
    let argv = recorded.lines().next().unwrap();
    assert_eq!(
        argv,
        format!(
            "--project {} dispatch {} --resume --auto",
            candidate.project_slug, candidate.story_id
        ),
        "an attended story names no provider: the helper reads the dispatch's own record"
    );
    assert_eq!(
        recorded.lines().nth(1).unwrap(),
        format!(
            "STORY_TARGET_SESSION={} STORY_CREATE_SESSION=1",
            candidate.project_slug
        ),
        "a daemon has no $TMUX; the helper refuses without a target session"
    );

    actuator
        .redispatch(
            &candidate,
            &ResumePlan {
                agent: Some(storyhook::store::EngineAgent::Codex),
                model: Some("gpt-5-codex".into()),
                effort: Some("high".into()),
                fast: true,
                full_auto: true,
            },
        )
        .unwrap();
    let argv = std::fs::read_to_string(&record).unwrap();
    let argv = argv.lines().next().unwrap().to_string();
    assert_eq!(
        argv,
        format!(
            "--project {} dispatch {} --agent=codex --resume --auto --full-auto --model=gpt-5-codex --effort=high --speed=fast",
            candidate.project_slug, candidate.story_id
        ),
        "a Full Auto lane's story is re-dispatched as that lane, and never with --force beside --resume"
    );

    std::fs::write(
        &helper,
        "#!/bin/bash\nprintf '%s\\n' '{\"ok\":false,\"reason\":\"resume-unsafe\",\"display\":\"the worktree is registered on another branch\"}'\nexit 1\n",
    )
    .unwrap();
    let error = actuator
        .redispatch(&candidate, &ResumePlan::default())
        .unwrap_err()
        .to_string();
    assert!(error.contains("registered on another branch"), "{error}");
}

fn write_hanging_helper(root: &std::path::Path) -> PathBuf {
    let helper = root.join("hanging-helper.sh");
    std::fs::write(
        &helper,
        r#"#!/bin/bash
verb="$3"
sh -c 'trap "" TERM; printf "%s" "$$" > "$1"; while :; do sleep 30; done' helper-child "$PWD/$verb-child-pid" &
wait
"#,
    )
    .unwrap();
    helper
}

fn assert_recorded_process_stopped(pid_path: &std::path::Path) {
    let ready_by = Instant::now() + Duration::from_secs(2);
    while !pid_path.is_file() && Instant::now() < ready_by {
        thread::sleep(Duration::from_millis(10));
    }
    let pid: i32 = std::fs::read_to_string(pid_path)
        .unwrap_or_else(|error| panic!("helper did not record {}: {error}", pid_path.display()))
        .parse()
        .unwrap();
    let stopped_by = Instant::now() + Duration::from_secs(2);
    while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < stopped_by {
        thread::sleep(Duration::from_millis(10));
    }
    assert_ne!(
        unsafe { libc::kill(pid, 0) },
        0,
        "helper descendant {pid} survived the control timeout"
    );
}

#[test]
fn shell_notification_timeout_terminates_the_helper_process_group() {
    let fixture = ServiceFixture::new();
    let root = scratch_dir();
    let candidate = cleanup_candidate(&fixture, root.path());
    let actuator = ShellVerificationActuator::with_paths_and_timing(
        Environment::at(root.path()),
        write_hanging_helper(root.path()),
        PathBuf::from("/usr/bin/true"),
        Duration::from_secs(1),
        Duration::from_millis(100),
        Duration::from_millis(100),
    );

    let error = actuator
        .notify(&candidate, "timeout probe")
        .unwrap_err()
        .to_string();

    assert!(error.contains("`notify`"), "{error}");
    assert!(error.contains("100ms"), "{error}");
    assert_recorded_process_stopped(&root.path().join("notify-child-pid"));
}

const FIXTURE_SUBMIT_URL: &str = "https://github.com/acme/widgets/pull/7";

/// A `story.sh submit` stand-in that answers the receipt a real run would for
/// the lease it was handed, mutated by a jq expression, and exits as told.
fn write_submit_receipt_helper(
    root: &std::path::Path,
    mutation: &str,
    exit_status: i32,
) -> PathBuf {
    let helper = root.join("submit-receipt-helper.sh");
    std::fs::write(
        &helper,
        format!(
            r#"#!/bin/bash
[ "$3" = submit ] || {{ printf 'expected the submit verb, got %s\n' "$3" >&2; exit 64; }}
lease="$STORYHOOK_REAP_LEASE_V1"
story=$(printf '%s' "$lease" | jq -r .story_id)
jq -n --argjson lease "$lease" --arg story "$story" --arg url "{FIXTURE_SUBMIT_URL}" \
  '{{ok:true,receipt_version:1,story_id:$story,lease:$lease,pushed:true,
     pull_request:{{url:$url,number:7,base:"dev",head_oid:"0123abcd",adopted:false}},
     display:"fixture submission"}} | {mutation}'
exit {exit_status}
"#
        ),
    )
    .unwrap();
    helper
}

/// A `story.sh submit` stand-in that refuses with exactly `body`.
fn write_refusing_submit_helper(root: &std::path::Path, body: &str) -> PathBuf {
    let helper = root.join("submit-refusing-helper.sh");
    std::fs::write(
        &helper,
        format!("#!/bin/bash\nprintf '%s\\n' '{body}'\nexit 1\n"),
    )
    .unwrap();
    helper
}

fn submit_actuator(root: &std::path::Path, helper: PathBuf) -> ShellVerificationActuator {
    ShellVerificationActuator::with_paths(
        Environment::at(root),
        helper,
        PathBuf::from("/usr/bin/true"),
    )
}

#[test]
fn shell_submission_requires_a_latest_generation_lease_before_spawning() {
    let fixture = ServiceFixture::new();
    let root = scratch_dir();
    let mut candidate = cleanup_candidate(&fixture, root.path());
    candidate.cleanup_lease = None;
    let actuator = submit_actuator(root.path(), root.path().join("must-not-run"));

    let failure = actuator.submit(&candidate).unwrap_err();
    match failure {
        SubmissionFailure::Infrastructure { detail } => {
            assert!(detail.contains("no cleanup lease"), "{detail}");
        }
        other => panic!("a missing lease is the verifier's problem, not the agent's: {other:?}"),
    }
}

#[test]
fn shell_submission_accepts_an_exact_typed_receipt() {
    let fixture = ServiceFixture::new();
    let root = scratch_dir();
    let candidate = cleanup_candidate(&fixture, root.path());
    let actuator = submit_actuator(
        root.path(),
        write_submit_receipt_helper(root.path(), ".", 0),
    );

    let pull_request = actuator.submit(&candidate).unwrap();

    assert_eq!(
        pull_request,
        SubmittedPullRequest {
            url: FIXTURE_SUBMIT_URL.into(),
            number: 7,
            base: "dev".into(),
            head_oid: "0123abcd".into(),
            adopted: false,
        }
    );
}

/// The helper's `class` decides whose problem a refusal is; a refusal that
/// carries none is the verifier's, never the agent's — an agent told to fix
/// "usage: story.sh submit <story-id>" could do nothing with it.
#[test]
fn shell_submission_classifies_refusals_by_the_helpers_class() {
    let fixture = ServiceFixture::new();
    let repair = r#"{"ok":false,"reason":"dirty-worktree","class":"repair","display":"Dirty: a.rs, b.rs.","dirty_files":["a.rs","b.rs"]}"#;
    let infrastructure = r#"{"ok":false,"reason":"push-failed","class":"infrastructure","display":"origin unreachable"}"#;
    let bare = r#"{"ok":false,"display":"usage: story.sh submit <story-id>"}"#;
    for (body, expected) in [
        (
            repair,
            SubmissionFailure::Refused {
                reason: "dirty-worktree".into(),
                display: "Dirty: a.rs, b.rs.".into(),
            },
        ),
        (
            infrastructure,
            SubmissionFailure::Infrastructure {
                detail: "origin unreachable".into(),
            },
        ),
        (
            bare,
            SubmissionFailure::Infrastructure {
                detail: "usage: story.sh submit <story-id>".into(),
            },
        ),
    ] {
        let root = scratch_dir();
        let candidate = cleanup_candidate(&fixture, root.path());
        let actuator =
            submit_actuator(root.path(), write_refusing_submit_helper(root.path(), body));

        assert_eq!(actuator.submit(&candidate).unwrap_err(), expected, "{body}");
    }
}

/// A receipt the daemon cannot trust is infrastructure, never a refusal: none
/// of these is anything an agent could repair.
#[test]
fn shell_submission_rejects_untrustworthy_receipts_as_infrastructure() {
    let fixture = ServiceFixture::new();
    for (mutation, status, expected) in [
        (".", 7, "exited"),
        (".story_id = \"SH-999\"", 0, "does not echo"),
        (".lease.branch = \"worktree-elsewhere\"", 0, "does not echo"),
        (".receipt_version = 2", 0, "unsupported version"),
        ("del(.pull_request)", 0, "without a pull request"),
        (
            ".pull_request.url = \"not a pull request\"",
            0,
            "unusable URL",
        ),
        (".pull_request.number = 9", 0, "disagree"),
    ] {
        let root = scratch_dir();
        let candidate = cleanup_candidate(&fixture, root.path());
        let actuator = submit_actuator(
            root.path(),
            write_submit_receipt_helper(root.path(), mutation, status),
        );

        match actuator.submit(&candidate).unwrap_err() {
            SubmissionFailure::Infrastructure { detail } => {
                assert!(detail.contains(expected), "{mutation}: {detail}");
            }
            other => {
                panic!("{mutation}: an untrustworthy receipt was returned to the agent: {other:?}")
            }
        }
    }

    let root = scratch_dir();
    let candidate = cleanup_candidate(&fixture, root.path());
    let helper = root.path().join("garbage-helper.sh");
    std::fs::write(&helper, "#!/bin/bash\nprintf 'pushed!\\n'\nexit 0\n").unwrap();
    match submit_actuator(root.path(), helper)
        .submit(&candidate)
        .unwrap_err()
    {
        SubmissionFailure::Infrastructure { detail } => {
            assert!(detail.contains("invalid receipt"), "{detail}");
        }
        other => panic!("non-JSON output was returned to the agent: {other:?}"),
    }
}

#[test]
fn shell_submission_timeout_terminates_the_helper_process_group() {
    let fixture = ServiceFixture::new();
    let root = scratch_dir();
    let candidate = cleanup_candidate(&fixture, root.path());
    let actuator = ShellVerificationActuator::with_paths_and_timing(
        Environment::at(root.path()),
        write_hanging_helper(root.path()),
        PathBuf::from("/usr/bin/true"),
        Duration::from_secs(1),
        Duration::from_millis(100),
        Duration::from_millis(100),
    );

    let failure = actuator.submit(&candidate).unwrap_err();

    let SubmissionFailure::Infrastructure { detail } = failure else {
        panic!("a timeout is the verifier's problem: {failure:?}");
    };
    assert!(detail.contains("`submit`"), "{detail}");
    assert!(detail.contains("100ms"), "{detail}");
    assert_recorded_process_stopped(&root.path().join("submit-child-pid"));
}

#[test]
fn shell_cleanup_timeout_terminates_the_helper_process_group() {
    let fixture = ServiceFixture::new();
    let root = scratch_dir();
    let candidate = cleanup_candidate(&fixture, root.path());
    let actuator = ShellVerificationActuator::with_paths_and_timing(
        Environment::at(root.path()),
        write_hanging_helper(root.path()),
        PathBuf::from("/usr/bin/true"),
        Duration::from_secs(1),
        Duration::from_millis(100),
        Duration::from_millis(100),
    );

    let error = actuator.reap(&candidate).unwrap_err().to_string();

    assert!(error.contains("`reap`"), "{error}");
    assert!(error.contains("100ms"), "{error}");
    assert_recorded_process_stopped(&root.path().join("reap-child-pid"));
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

/// Stops the daemon a real helper started for a fixture store, on every exit
/// path — a daemon a test cannot get rid of is a leak (SH-306, SH-631).
struct StandDown(Environment);

impl Drop for StandDown {
    fn drop(&mut self) {
        let _ = lifecycle::stop(&self.0, lifecycle::StopMode::Force);
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
    // The real helper runs real `story` commands, which start a daemon for the
    // fixture store. Whatever else happens below, that daemon is stood down
    // where the fixture's own environment says it is (SH-631's rule) — which
    // only works because SH-633 made the child publish there.
    let _stand_down = StandDown(fixture.env().clone());
    actuator.reap(&candidate).unwrap();

    // SH-633: the child is told the state home its parent resolved, not only
    // the store. Before that fix the helper's `story` calls resolved the state
    // home from the developer's real $HOME, found no daemon there for this
    // store, and started a second one — leaving its pidfile, backups and
    // journal beside production's, once per run, for ever.
    assert!(
        fixture.env().daemon_file().exists(),
        "the daemon the helper started published no portfile under the fixture's own state \
         home ({}); it is serving the fixture store from some other state home",
        fixture.env().daemon_state_dir().display()
    );

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

/// SH-650: the resume plan for a story a live Full Auto lane holds carries the
/// run's own provider identity, so the lane is re-dispatched as the lane it
/// is; a story no live lane holds (an attended dispatch, or a lane the engine
/// has quarantined) is an ordinary autonomous resume whose provider the
/// helper reads from the dispatch's own record.
#[test]
fn the_resume_plan_carries_a_live_engine_lanes_identity_and_nothing_elses() {
    use storyhook::service::engine::{EngineService, StartRequest};
    use storyhook::store::{EngineLaneState, EngineScope, EngineSpeed};
    use storyhook_test_support::FakeDispatcher;

    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let held = submitted(&fixture, "held by a lane", Priority::High, PR_ONE);
    let attended = submitted(&fixture, "attended", Priority::Medium, PR_TWO);
    let ctx = fixture.ctx();
    let fake = FakeDispatcher::default();
    let run = EngineService::new(&ctx, &fake)
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 1,
            agent: storyhook::store::EngineAgent::Codex,
            model: Some("gpt-5-codex".into()),
            effort: Some("high".into()),
            speed: Some(EngineSpeed::Fast),
        })
        .unwrap()
        .id;
    let occupy = |state: EngineLaneState| {
        let mut lane = fixture
            .store()
            .read(|tx| tx.engine_lanes(&run))
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        lane.state = state;
        lane.story_id = Some(held.clone());
        lane.window_name = Some(held.clone());
        lane.dispatched_at = Some(FIXTURE_NOW.to_string());
        fixture
            .store()
            .write(|tx| tx.put_engine_lane(&lane))
            .unwrap();
    };
    let candidate = |id: &str| {
        VerificationQueue::new(fixture.store())
            .ordered()
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.story_id == id)
            .unwrap()
    };

    occupy(EngineLaneState::Working);
    assert_eq!(
        resume_plan(fixture.store(), &candidate(&held)).unwrap(),
        ResumePlan {
            agent: Some(storyhook::store::EngineAgent::Codex),
            model: Some("gpt-5-codex".into()),
            effort: Some("high".into()),
            fast: true,
            full_auto: true,
        },
        "a working lane's story is re-dispatched as that lane"
    );
    assert_eq!(
        resume_plan(fixture.store(), &candidate(&attended)).unwrap(),
        ResumePlan::default(),
        "a story no lane holds is an ordinary autonomous resume"
    );
    occupy(EngineLaneState::Dispatching);
    assert!(
        resume_plan(fixture.store(), &candidate(&held))
            .unwrap()
            .full_auto,
        "a lane still dispatching holds its story too"
    );
    occupy(EngineLaneState::Quarantined);
    assert_eq!(
        resume_plan(fixture.store(), &candidate(&held)).unwrap(),
        ResumePlan::default(),
        "a quarantined lane is one the engine has given up on; its identity would be a lie"
    );
}

/// SH-650 (D-E, step 4a): a RED story whose agent is gone is re-dispatched in
/// place and never parked; its resubmission then re-enters the queue and is
/// verified again like any other.
#[test]
fn a_red_story_returned_to_a_dead_pane_is_redispatched_and_reenters_the_queue() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "no pane", Priority::High, PR_ONE);
    let root = scratch_dir();
    let env = Environment::at(root.path());
    let actuator = FakeActuator::new(VerificationOutcome::TestsFailed {
        tree: "deadbeef".into(),
        log: "/tmp/red.log".into(),
        detail: "one regression".into(),
        gate: GateCommand::DEFAULT.into(),
    })
    .with_notify_script([NotifyScript::Absent("pane-dead")]);

    assert_eq!(
        tick_with(fixture.store(), &env, &actuator, fixture.project()).unwrap(),
        TickResult::Returned
    );
    let row = story_row(&fixture, &id);
    assert_eq!(row.state, "in-progress");
    assert_eq!(row.awaiting, None, "re-dispatched, not parked");
    assert_eq!(actuator.redispatched.lock().unwrap().len(), 1);
    let notified = actuator.notified.lock().unwrap();
    assert_eq!(notified.len(), 1);
    assert!(notified[0].contains("CENTRAL VERIFICATION RED"));
    drop(notified);
    assert_eq!(
        tick_with(fixture.store(), &env, &actuator, fixture.project()).unwrap(),
        TickResult::Idle,
        "a returned story is out of the queue until resubmitted"
    );

    // The (re-dispatched) agent resubmits; the story is verified again.
    let ctx = fixture.ctx();
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .expect("resubmitting after remediation");
    let actuator = FakeActuator::new(VerificationOutcome::Merged {
        tree: "cafef00d".into(),
        detail: "merged".into(),
        gate: GateCommand::DEFAULT.into(),
    });
    assert_eq!(
        tick_with(fixture.store(), &env, &actuator, fixture.project()).unwrap(),
        TickResult::Completed
    );
    assert_eq!(story_row(&fixture, &id).state, "done");
}

/// SH-650: a RED story whose notify failed for a reason that is not absence
/// is parked exactly as before, without a respawn.
#[test]
fn an_unreachable_agent_is_marked_awaiting_when_the_refusal_is_not_absence() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "no pane", Priority::High, PR_ONE);
    let root = scratch_dir();
    let actuator = FakeActuator::new(VerificationOutcome::TestsFailed {
        tree: "deadbeef".into(),
        log: "/tmp/red.log".into(),
        detail: "one regression".into(),
        gate: GateCommand::DEFAULT.into(),
    })
    .with_notify_script([NotifyScript::Fail("could not query tmux window")]);

    tick_with(
        fixture.store(),
        &Environment::at(root.path()),
        &actuator,
        fixture.project(),
    )
    .unwrap();
    let row = story_row(&fixture, &id);
    assert_eq!(row.state, "in-progress");
    assert!(
        row.awaiting
            .unwrap()
            .contains("could not query tmux window")
    );
    assert!(actuator.redispatched.lock().unwrap().is_empty());
}

#[test]
fn identical_retryable_failures_update_one_comment_and_halt_at_the_derived_ceiling() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "temporary outage", Priority::High, PR_ONE);
    let root = scratch_dir();
    let actuator = FakeActuator::new(VerificationOutcome::InfrastructureFailure {
        detail: "GitHub unavailable".into(),
        disposition: storyhook::store::VerificationFailureDisposition::Retryable,
    });
    let env = Environment::at(root.path());

    assert_eq!(
        tick_with(fixture.store(), &env, &actuator, fixture.project()).unwrap(),
        TickResult::RetryLater
    );
    assert_eq!(
        tick_with(fixture.store(), &env, &actuator, fixture.project()).unwrap(),
        TickResult::RetryLater
    );
    assert_eq!(
        tick_with(fixture.store(), &env, &actuator, fixture.project()).unwrap(),
        TickResult::Halted
    );
    assert_eq!(
        tick_with(fixture.store(), &env, &actuator, fixture.project()).unwrap(),
        TickResult::Halted
    );
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::parse_id("SH", &id).unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "verifying");
    assert_eq!(row.snapshot.comments.len(), 1);
    assert!(row.snapshot.comments[0].text.contains("Attempt 3 of 3"));
    assert!(row.snapshot.comments[0].text.contains("HALTED"));
    let incident = fixture
        .store()
        .read(|tx| tx.verification_incident(fixture.project()))
        .unwrap()
        .unwrap();
    assert!(incident.halted);
    assert_eq!(incident.attempts, 3);
}

#[test]
fn a_permanent_infrastructure_failure_halts_on_the_first_attempt() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "broken verifier", Priority::High, PR_ONE);
    let actuator = FakeActuator::new(VerificationOutcome::InfrastructureFailure {
        detail: "not inside a git worktree".into(),
        disposition: storyhook::store::VerificationFailureDisposition::Permanent,
    });

    assert_eq!(
        tick_with(fixture.store(), fixture.env(), &actuator, fixture.project()).unwrap(),
        TickResult::Halted
    );
    let incident = fixture
        .store()
        .read(|tx| tx.verification_incident(fixture.project()))
        .unwrap()
        .unwrap();
    assert_eq!(incident.attempts, 1);
    assert_eq!(incident.story.to_id("SH"), id);
    // SH-666: the halt says what it is (the verifier's), what it stops (the
    // whole queue), who is at fault (nobody), and how it is released.
    let halt = last_comment(&fixture, &id);
    assert!(
        halt.contains("Verification — HALTED") || halt.contains("INFRASTRUCTURE — HALTED"),
        "{halt}"
    );
    assert!(halt.contains("stops the verifier's whole queue"), "{halt}");
    assert!(halt.contains("No story is at fault"), "{halt}");
    assert!(
        halt.contains(&format!("story verifier ack {}", incident.incident_id)),
        "{halt}"
    );
    assert!(!halt.contains("blocked by"), "{halt}");
    let reopened = SqliteStore::open(fixture.store().path()).unwrap();
    assert_eq!(
        reopened
            .read(|tx| tx.verification_incident(fixture.project()))
            .unwrap()
            .as_ref(),
        Some(&incident),
        "a fresh verifier process must inherit the durable halt"
    );

    let path = "/api/repos/fixture/data";
    let data = rest::route(
        fixture.store(),
        fixture.env(),
        &Method::Get,
        path,
        &[Header::from_bytes("Host", "127.0.0.1:3456").unwrap()],
        "",
        &TrustedHosts::default(),
    );
    let json: serde_json::Value =
        serde_json::from_str(data.reply.text_body().expect("UTF-8 text response")).unwrap();
    assert_eq!(json["verification_incident"]["story_id"], id);
    assert_eq!(json["verification_incident"]["attempts"], 1);

    let ack_path = "/api/repos/fixture/verification/ack";
    let ack_headers = [
        Header::from_bytes("Host", "127.0.0.1:3456").unwrap(),
        Header::from_bytes("X-Storyhook", "1").unwrap(),
        Header::from_bytes("Content-Type", "application/json").unwrap(),
    ];
    let stale = rest::route(
        fixture.store(),
        fixture.env(),
        &Method::Post,
        ack_path,
        &ack_headers,
        r#"{"incident_id":"an-older-incident"}"#,
        &TrustedHosts::default(),
    );
    assert_eq!(
        stale.reply.status,
        422,
        "{}",
        stale.reply.text_body().expect("UTF-8 text response")
    );
    assert!(
        stale
            .reply
            .text_body()
            .expect("UTF-8 text response")
            .contains("is stale")
    );
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.verification_incident(fixture.project()))
            .unwrap()
            .as_ref()
            .map(|current| current.incident_id.as_str()),
        Some(incident.incident_id.as_str())
    );

    let body = serde_json::json!({"incident_id": incident.incident_id}).to_string();
    let ack = rest::route(
        fixture.store(),
        fixture.env(),
        &Method::Post,
        ack_path,
        &ack_headers,
        &body,
        &TrustedHosts::default(),
    );
    assert_eq!(
        ack.reply.status,
        200,
        "{}",
        ack.reply.text_body().expect("UTF-8 text response")
    );
    assert!(
        fixture
            .store()
            .read(|tx| tx.verification_incident(fixture.project()))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        tick_with(fixture.store(), fixture.env(), &actuator, fixture.project()).unwrap(),
        TickResult::Halted,
        "acknowledgement must make the still-current candidate eligible again"
    );
}

/// The CLI's `story verifier ack` and the dashboard's `POST .../verification/ack`
/// are one function (SH-666, SH-136): it refuses when nothing is halted, when
/// the incident is still retrying on its own, and when the id names an older
/// incident than the current one — and clears exactly the one it was given.
#[test]
fn acknowledging_an_incident_shares_one_exact_id_contract_across_both_doors() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let head = submitted(&fixture, "broken verifier", Priority::High, PR_ONE);
    let ctx = fixture.ctx();

    let none = acknowledge_verification_incident(&ctx, "2:1").unwrap_err();
    assert!(
        none.to_string()
            .contains("no verification incident is active"),
        "{none}"
    );

    let candidate = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    let incident_id = format!(
        "{}:{}",
        candidate.project.get(),
        candidate.verifying_generation.unwrap().get()
    );
    let mut incident = VerificationIncident {
        incident_id: incident_id.clone(),
        project: candidate.project,
        story: StoryNo::parse_id("SH", &head).unwrap(),
        generation: candidate.verifying_generation.unwrap(),
        disposition: VerificationFailureDisposition::Retryable,
        halted: false,
        attempts: 1,
        detail: "could not read submitted pull request".into(),
        first_failed_at: "2026-01-01T00:01:00Z".into(),
        last_failed_at: "2026-01-01T00:01:00Z".into(),
    };
    fixture
        .store()
        .write(|tx| tx.put_verification_incident(&incident))
        .unwrap();
    let retrying = acknowledge_verification_incident(&ctx, &incident_id).unwrap_err();
    assert!(
        retrying.to_string().contains("still retrying"),
        "{retrying}"
    );

    incident.halted = true;
    incident.disposition = VerificationFailureDisposition::Permanent;
    fixture
        .store()
        .write(|tx| tx.put_verification_incident(&incident))
        .unwrap();
    let stale = acknowledge_verification_incident(&ctx, "an-older-incident").unwrap_err();
    assert!(stale.to_string().contains("is stale"), "{stale}");
    assert!(stale.to_string().contains(&incident_id), "{stale}");
    assert!(
        fixture
            .store()
            .read(|tx| tx.verification_incident(fixture.project()))
            .unwrap()
            .is_some(),
        "a stale acknowledgement must clear nothing"
    );

    let cleared = acknowledge_verification_incident(&ctx, &incident_id).unwrap();
    assert_eq!(cleared.incident_id, incident_id);
    assert_eq!(cleared.story.to_id("SH"), head);
    assert!(
        fixture
            .store()
            .read(|tx| tx.verification_incident(fixture.project()))
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_halt_fires_one_post_commit_verification_hook() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), Some(fixture.cwd())))
        .unwrap();
    fixture.write_hooks_toml(
        "on_verification_halted = { command = \"cat >> hooks.log; echo >> hooks.log\" }\n",
    );
    submitted(&fixture, "broken verifier", Priority::High, PR_ONE);
    let actuator = FakeActuator::new(VerificationOutcome::InfrastructureFailure {
        detail: "jq is required".into(),
        disposition: VerificationFailureDisposition::Permanent,
    });

    assert_eq!(
        tick_with(fixture.store(), fixture.env(), &actuator, fixture.project()).unwrap(),
        TickResult::Halted
    );
    assert_eq!(
        tick_with(fixture.store(), fixture.env(), &actuator, fixture.project()).unwrap(),
        TickResult::Halted
    );
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(fixture.cwd().join("hooks.log"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["event_type"], "verification_halted");
    assert_eq!(lines[0]["attempts"], 1);
    assert_eq!(lines[0]["held_stories"].as_array().unwrap().len(), 1);
    assert!(
        lines[0]["remedy"]
            .as_str()
            .unwrap()
            .starts_with("story verifier ack ")
    );
    assert_eq!(
        lines[0]["diagnostics"],
        "story verifier status; story daemon logs"
    );
}

#[test]
fn a_recovered_attempt_clears_its_retrying_incident() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "recovered verifier", Priority::High, PR_ONE);
    let candidate = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    let generation = candidate.verifying_generation.unwrap();
    fixture
        .store()
        .write(|tx| {
            tx.put_verification_incident(&VerificationIncident {
                incident_id: format!("{}:{}", candidate.project.get(), generation.get()),
                project: candidate.project,
                story: StoryNo::parse_id("SH", &id).unwrap(),
                generation,
                disposition: VerificationFailureDisposition::Retryable,
                halted: false,
                attempts: 1,
                detail: "GitHub unavailable".into(),
                first_failed_at: FIXTURE_NOW.into(),
                last_failed_at: FIXTURE_NOW.into(),
            })
        })
        .unwrap();
    let actuator = FakeActuator::new(VerificationOutcome::Conflict {
        detail: "base changed after infrastructure recovered".into(),
    });

    assert_eq!(
        tick_with(fixture.store(), fixture.env(), &actuator, fixture.project()).unwrap(),
        TickResult::Returned
    );
    assert!(
        fixture
            .store()
            .read(|tx| tx.verification_incident(fixture.project()))
            .unwrap()
            .is_none()
    );
}

#[test]
fn a_stale_generation_incident_is_cleared_before_current_work_runs() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), Some(fixture.cwd())))
        .unwrap();
    fixture.write_hooks_toml(
        "on_verification_resumed = { command = \"cat >> resumed.log; echo >> resumed.log\" }\n",
    );
    let id = submitted(&fixture, "current generation", Priority::High, PR_ONE);
    let candidate = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    let stale_generation = GlobalSeq::new(candidate.verifying_generation.unwrap().get() + 1);
    fixture
        .store()
        .write(|tx| {
            tx.put_verification_incident(&VerificationIncident {
                incident_id: format!("{}:{}", candidate.project.get(), stale_generation.get()),
                project: candidate.project,
                story: StoryNo::parse_id("SH", &id).unwrap(),
                generation: stale_generation,
                disposition: VerificationFailureDisposition::Permanent,
                halted: true,
                attempts: 1,
                detail: "obsolete failure".into(),
                first_failed_at: FIXTURE_NOW.into(),
                last_failed_at: FIXTURE_NOW.into(),
            })
        })
        .unwrap();
    let actuator = FakeActuator::new(VerificationOutcome::Conflict {
        detail: "current generation reached the actuator".into(),
    });

    assert_eq!(
        tick_with(fixture.store(), fixture.env(), &actuator, fixture.project()).unwrap(),
        TickResult::Returned
    );
    assert!(
        fixture
            .store()
            .read(|tx| tx.verification_incident(fixture.project()))
            .unwrap()
            .is_none()
    );
    let events: Vec<serde_json::Value> = std::fs::read_to_string(fixture.cwd().join("resumed.log"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event_type"], "verification_resumed");
    assert_eq!(events[0]["reason"], "incident generation retired");
    assert_eq!(events[0]["enabled"], true);
    assert_eq!(events[0]["story_id"], id);
}

/// A fresh lease for `story_id`, rooted under `root`.
fn lease_for(root: &std::path::Path, story_id: &str) -> StoryCleanupLease {
    StoryCleanupLease {
        version: CLEANUP_LEASE_VERSION,
        project_slug: "fixture".into(),
        story_id: story_id.into(),
        repository_path: root.to_path_buf(),
        worktree_path: root.join(".claude/worktrees").join(story_id),
        branch: format!("worktree-{story_id}"),
        tmux: TmuxCleanupTarget {
            socket_path: root.join("tmux.sock"),
        },
    }
}

/// A story dispatched from a worktree and moved to `verifying` from inside
/// it: the lease follows the transition, exactly as `story move` records it
/// (SH-647). With `url`, the agent (or an earlier generation) linked a PR.
fn leased_submission(
    fixture: &ServiceFixture,
    root: &std::path::Path,
    title: &str,
    url: Option<&str>,
) -> (String, StoryCleanupLease) {
    let ctx = fixture.ctx();
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: title.into(),
            priority: Some(Priority::High.as_str().to_string()),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id;
    if let Some(url) = url {
        PrLinkService::new(&ctx).link(&id, url, true).unwrap();
    }
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    let lease = lease_for(root, &id);
    append_cleanup_lease(fixture, &id, lease.clone());
    (id, lease)
}

fn submitted_pr(url: &str, number: u64, adopted: bool) -> SubmittedPullRequest {
    SubmittedPullRequest {
        url: url.into(),
        number,
        base: "dev".into(),
        head_oid: "0123abcd".into(),
        adopted,
    }
}

fn submitting_actuator(
    outcome: VerificationOutcome,
    submission: Option<Result<SubmittedPullRequest, SubmissionFailure>>,
) -> FakeActuator {
    let actuator = FakeActuator::new(outcome);
    match submission {
        Some(scripted) => actuator.with_submission(scripted),
        None => actuator,
    }
}

/// The target: a leased story with no pull request is submitted first — the
/// link and a SUBMITTED comment land under the generation guard — and then
/// verified in the same tick against the pull request the helper opened.
#[test]
fn a_leased_story_without_a_pull_request_is_submitted_then_verified_in_one_tick() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let root = scratch_dir();
    let (id, lease) = leased_submission(&fixture, root.path(), "submit me", None);
    let actuator = submitting_actuator(
        VerificationOutcome::Merged {
            tree: "abc123".into(),
            detail: "landed".into(),
            gate: "make test".into(),
        },
        Some(Ok(submitted_pr(PR_ONE, 1, false))),
    );

    assert_eq!(
        tick_with(
            fixture.store(),
            &Environment::at(root.path()),
            &actuator,
            fixture.project(),
        )
        .unwrap(),
        TickResult::Completed
    );

    assert_eq!(
        actuator.submitted.lock().unwrap().as_slice(),
        std::slice::from_ref(&id)
    );
    let row = story_row(&fixture, &id);
    assert_eq!(
        row.state, "done",
        "verification ran on the submitted pull request"
    );
    let submitted_comment = row
        .snapshot
        .comments
        .iter()
        .find(|comment| comment.text.starts_with(VERIFICATION_SUBMITTED_PREFIX))
        .expect("the submission is recorded on the story");
    assert!(
        submitted_comment.text.contains(&lease.branch),
        "{}",
        submitted_comment.text
    );
    assert!(
        submitted_comment.text.contains(PR_ONE),
        "{}",
        submitted_comment.text
    );
    assert!(
        submitted_comment.text.contains("opened"),
        "{}",
        submitted_comment.text
    );
    let links = fixture
        .store()
        .read(|tx| tx.pr_links(fixture.project()))
        .unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].1.url, PR_ONE);
    assert!(links[0].1.close_on_merge);
    assert_eq!(links[0].1.status, "merged");
}

/// Every leased generation is submitted, linked pull request or not: after a
/// RED return the agent only commits, so the push is what carries the fix.
#[test]
fn a_leased_resubmission_with_a_linked_pull_request_is_pushed_again_before_verifying() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let root = scratch_dir();
    let (id, _) = leased_submission(&fixture, root.path(), "fixed", Some(PR_ONE));
    let actuator = submitting_actuator(
        VerificationOutcome::Merged {
            tree: "abc123".into(),
            detail: "landed".into(),
            gate: "make test".into(),
        },
        Some(Ok(submitted_pr(PR_ONE, 1, true))),
    );

    assert_eq!(
        tick_with(
            fixture.store(),
            &Environment::at(root.path()),
            &actuator,
            fixture.project(),
        )
        .unwrap(),
        TickResult::Completed
    );

    assert_eq!(
        actuator.submitted.lock().unwrap().as_slice(),
        std::slice::from_ref(&id),
        "submitted exactly once, then verified"
    );
    let row = story_row(&fixture, &id);
    assert_eq!(row.state, "done");
    let submitted_comments = row
        .snapshot
        .comments
        .iter()
        .filter(|comment| comment.text.starts_with(VERIFICATION_SUBMITTED_PREFIX))
        .count();
    assert_eq!(submitted_comments, 1);
    assert!(row.snapshot.comments.iter().any(|comment| {
        comment.text.starts_with(VERIFICATION_SUBMITTED_PREFIX) && comment.text.contains("adopted")
    }));
}

/// An unleased story is not the verifier's to push: it is returned naming the
/// cause — the story entered `verifying` from outside its worktree — and the
/// actuator is never asked to submit.
#[test]
fn an_unleased_story_without_a_pull_request_is_returned_without_a_submission_attempt() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let ctx = fixture.ctx();
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "moved from the main checkout".into(),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id;
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    let root = scratch_dir();
    let actuator = submitting_actuator(
        VerificationOutcome::Merged {
            tree: "must-not-run".into(),
            detail: "must-not-run".into(),
            gate: "make test".into(),
        },
        Some(Ok(submitted_pr(PR_ONE, 1, false))),
    );

    assert_eq!(
        tick_with(
            fixture.store(),
            &Environment::at(root.path()),
            &actuator,
            fixture.project(),
        )
        .unwrap(),
        TickResult::Returned
    );

    assert!(actuator.submitted.lock().unwrap().is_empty());
    let row = story_row(&fixture, &id);
    assert_eq!(row.state, "in-progress");
    let notified = actuator.notified.lock().unwrap();
    assert_eq!(notified.len(), 1);
    assert!(notified[0].contains("no cleanup lease"), "{}", notified[0]);
    assert!(
        notified[0].contains("story move <id> verifying"),
        "{}",
        notified[0]
    );
}

/// A refusal the helper classes as the agent's returns the story with the
/// helper's own words — the dirty files — and never reaches verification.
#[test]
fn a_refused_submission_returns_the_story_with_the_helpers_diagnosis() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let root = scratch_dir();
    let (id, _) = leased_submission(&fixture, root.path(), "dirty", None);
    let actuator = submitting_actuator(
        VerificationOutcome::Merged {
            tree: "must-not-run".into(),
            detail: "must-not-run".into(),
            gate: "make test".into(),
        },
        Some(Err(SubmissionFailure::Refused {
            reason: "dirty-worktree".into(),
            display: "story.sh submit: SH-1's worktree has uncommitted changes. Dirty: src/lib.rs."
                .into(),
        })),
    );

    assert_eq!(
        tick_with(
            fixture.store(),
            &Environment::at(root.path()),
            &actuator,
            fixture.project(),
        )
        .unwrap(),
        TickResult::Returned
    );

    let row = story_row(&fixture, &id);
    assert_eq!(row.state, "in-progress");
    assert!(
        row.snapshot
            .comments
            .iter()
            .any(|comment| comment.text.contains("Dirty: src/lib.rs")),
        "the diagnosis is durable on the story"
    );
    let notified = actuator.notified.lock().unwrap();
    assert_eq!(notified.len(), 1);
    assert!(notified[0].contains("Dirty: src/lib.rs"), "{}", notified[0]);
    let links = fixture
        .store()
        .read(|tx| tx.pr_links(fixture.project()))
        .unwrap();
    assert!(links.is_empty(), "a refused submission links nothing");
}

/// Infrastructure is the verifier's: a retryable incident is recorded, the
/// story stays in `verifying`, and the next tick re-runs the same idempotent
/// steps — adopting whatever the failed attempt left on GitHub.
#[test]
fn an_infrastructure_failure_during_submission_keeps_the_story_queued_and_retries() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let root = scratch_dir();
    let env = Environment::at(root.path());
    let (id, _) = leased_submission(&fixture, root.path(), "flaky github", None);
    let failing = submitting_actuator(
        VerificationOutcome::Merged {
            tree: "must-not-run".into(),
            detail: "must-not-run".into(),
            gate: "make test".into(),
        },
        Some(Err(SubmissionFailure::Infrastructure {
            detail: "gh could not list pull requests: error connecting to api.github.com".into(),
        })),
    );

    assert_eq!(
        tick_with(fixture.store(), &env, &failing, fixture.project()).unwrap(),
        TickResult::RetryLater
    );
    let row = story_row(&fixture, &id);
    assert_eq!(row.state, "verifying", "the story is still the verifier's");
    assert!(row.awaiting.is_none());
    assert!(
        failing.notified.lock().unwrap().is_empty(),
        "the agent is not bothered"
    );
    let incident = fixture
        .store()
        .read(|tx| tx.verification_incident(fixture.project()))
        .unwrap()
        .expect("an incident is recorded");
    assert!(
        incident.detail.contains("api.github.com"),
        "{}",
        incident.detail
    );
    assert!(!incident.halted);

    // GitHub came back: the crashed attempt's pull request is adopted and the
    // story proceeds to verification.
    let recovered = submitting_actuator(
        VerificationOutcome::Merged {
            tree: "abc123".into(),
            detail: "landed".into(),
            gate: "make test".into(),
        },
        Some(Ok(submitted_pr(PR_ONE, 1, true))),
    );
    assert_eq!(
        tick_with(fixture.store(), &env, &recovered, fixture.project()).unwrap(),
        TickResult::Completed
    );
    assert_eq!(story_row(&fixture, &id).state, "done");
    assert!(
        fixture
            .store()
            .read(|tx| tx.verification_incident(fixture.project()))
            .unwrap()
            .is_none(),
        "the incident clears with the generation"
    );
}

/// The helper's answer and the story's own link must name one pull request.
#[test]
fn an_adopted_pull_request_that_is_not_the_linked_one_returns_the_story() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let root = scratch_dir();
    let (id, _) = leased_submission(&fixture, root.path(), "two PRs", Some(PR_ONE));
    let actuator = submitting_actuator(
        VerificationOutcome::Merged {
            tree: "must-not-run".into(),
            detail: "must-not-run".into(),
            gate: "make test".into(),
        },
        Some(Ok(submitted_pr(PR_TWO, 2, true))),
    );

    assert_eq!(
        tick_with(
            fixture.store(),
            &Environment::at(root.path()),
            &actuator,
            fixture.project(),
        )
        .unwrap(),
        TickResult::Returned
    );

    assert_eq!(story_row(&fixture, &id).state, "in-progress");
    let notified = actuator.notified.lock().unwrap();
    assert!(
        notified[0].contains(PR_ONE) && notified[0].contains(PR_TWO),
        "{}",
        notified[0]
    );
}

/// A submission that lands on a repository the project has not registered is
/// a configuration fault between the worktree's origin and the project's:
/// never linked, and the queue halts loudly rather than re-pushing forever.
#[test]
fn a_submission_on_an_unregistered_repository_halts_instead_of_linking() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let root = scratch_dir();
    let (id, _) = leased_submission(&fixture, root.path(), "elsewhere", None);
    let actuator = submitting_actuator(
        VerificationOutcome::Merged {
            tree: "must-not-run".into(),
            detail: "must-not-run".into(),
            gate: "make test".into(),
        },
        Some(Ok(submitted_pr(
            "https://github.com/other/repo/pull/9",
            9,
            false,
        ))),
    );

    assert_eq!(
        tick_with(
            fixture.store(),
            &Environment::at(root.path()),
            &actuator,
            fixture.project(),
        )
        .unwrap(),
        TickResult::Halted
    );

    assert_eq!(story_row(&fixture, &id).state, "verifying");
    let incident = fixture
        .store()
        .read(|tx| tx.verification_incident(fixture.project()))
        .unwrap()
        .expect("a halting incident is recorded");
    assert!(incident.halted);
    assert!(
        incident.detail.contains("not registered"),
        "{}",
        incident.detail
    );
    assert!(
        fixture
            .store()
            .read(|tx| tx.pr_links(fixture.project()))
            .unwrap()
            .is_empty()
    );
}

/// A fake whose submission moves the story out of `verifying` underneath the
/// verifier — the agent reclaiming its story mid-submit.
struct MovingActuator<'a> {
    fixture: &'a ServiceFixture,
    story_id: String,
}

impl VerificationActuator for MovingActuator<'_> {
    fn submit(
        &self,
        _candidate: &VerificationCandidate,
    ) -> Result<SubmittedPullRequest, SubmissionFailure> {
        StoryService::new(&self.fixture.ctx())
            .set_state(&self.story_id, "in-progress", None, None, None)
            .unwrap();
        Ok(submitted_pr(PR_ONE, 1, false))
    }

    fn verify(
        &self,
        _candidate: &VerificationCandidate,
        _pull_request: &PrLink,
    ) -> VerificationOutcome {
        panic!("a superseded generation must never be verified")
    }

    fn notify(
        &self,
        _candidate: &VerificationCandidate,
        _message: &str,
    ) -> Result<NotifyDelivery, AppError> {
        panic!("nothing is returned to an agent that already took its story back")
    }

    fn redispatch(
        &self,
        _candidate: &VerificationCandidate,
        _plan: &ResumePlan,
    ) -> Result<(), AppError> {
        panic!("a superseded generation is never re-dispatched")
    }

    fn reap(&self, _candidate: &VerificationCandidate) -> Result<(), AppError> {
        panic!("nothing landed, nothing to reap")
    }
}

/// A recorded submission belongs to its generation: a story taken back while
/// the helper ran is left exactly as the agent left it — no link, no comment,
/// no verification.
#[test]
fn a_submission_recorded_after_the_generation_moved_is_superseded() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let root = scratch_dir();
    let (id, _) = leased_submission(&fixture, root.path(), "taken back", None);
    let actuator = MovingActuator {
        fixture: &fixture,
        story_id: id.clone(),
    };

    assert_eq!(
        tick_with(
            fixture.store(),
            &Environment::at(root.path()),
            &actuator,
            fixture.project(),
        )
        .unwrap(),
        TickResult::Returned
    );

    let row = story_row(&fixture, &id);
    assert_eq!(row.state, "in-progress");
    assert!(
        !row.snapshot
            .comments
            .iter()
            .any(|comment| comment.text.starts_with(VERIFICATION_SUBMITTED_PREFIX))
    );
    assert!(
        fixture
            .store()
            .read(|tx| tx.pr_links(fixture.project()))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_green_attempt_closes_then_reaps_the_story() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "green", Priority::High, PR_ONE);
    let root = scratch_dir();
    let env = Environment::at(root.path());
    let actuator = FakeActuator::new(VerificationOutcome::Merged {
        tree: "abc123".into(),
        detail: "landed".into(),
        gate: GateCommand::DEFAULT.into(),
    });

    assert_eq!(
        tick_with(fixture.store(), &env, &actuator, fixture.project()).unwrap(),
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

/// The SH-652 straddle: `shipped` is the positionally first CLOSED state and
/// `abandoned` the alphabetically first, so either wrong search answers
/// wrong. Green must land in the required `done`, reap must be asked, and the
/// cleanup pass must find a green-but-unreaped story under this catalog —
/// the three reads that have to agree for the actuator's `reap-leased`
/// (which accepts only `done`) to succeed.
#[test]
fn a_green_attempt_lands_in_done_whatever_closed_state_sorts_first() {
    let fixture = ServiceFixture::new();
    let config_ctx = fixture.ctx();
    let config = ConfigService::new(&config_ctx);
    config
        .add_state("shipped", SuperState::Closed, None, None)
        .unwrap();
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
                "shipped",
                "abandoned",
                "done",
                "dropped",
            ]
            .map(str::to_string),
        )
        .unwrap();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "green under a straddle", Priority::High, PR_ONE);
    let root = scratch_dir();
    let env = Environment::at(root.path());
    let actuator = FakeActuator::new(VerificationOutcome::Merged {
        tree: "abc123".into(),
        detail: "landed".into(),
        gate: GateCommand::DEFAULT.into(),
    });

    assert_eq!(
        tick_with(fixture.store(), &env, &actuator, fixture.project()).unwrap(),
        TickResult::Completed
    );
    let story_no = StoryNo::parse_id("SH", &id).unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), story_no))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, COMPLETION_STATE_SLUG);
    assert!(row.archived);
    assert_eq!(actuator.reaped.lock().unwrap().as_slice(), [id]);

    // The cleanup pass reads the same answer: strip the completion marker the
    // reap wrote and the story is a cleanup candidate again, found by the
    // constant and not by whichever CLOSED state sorts first.
    let ctx = fixture.ctx();
    let unreaped = submitted(&fixture, "green, reap still owed", Priority::High, PR_TWO);
    StoryService::new(&ctx)
        .comment(
            &unreaped,
            &format!(
                "{VERIFICATION_GREEN_PREFIX} merge tree `def456` passed `make test` and pull request {PR_TWO} landed."
            ),
        )
        .unwrap();
    VerificationQueue::new(fixture.store())
        .record_merged(&ctx, &unreaped, PR_TWO)
        .unwrap();
    let candidate = VerificationQueue::new(fixture.store())
        .next_cleanup()
        .unwrap()
        .expect("a done story with GREEN and no CLEANUP COMPLETE is owed a reap");
    assert_eq!(candidate.story_id, unreaped);
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
    let actuator = FakeActuator::new(VerificationOutcome::InfrastructureFailure {
        detail: "verification must not run for cleanup".into(),
        disposition: storyhook::store::VerificationFailureDisposition::Retryable,
    });

    assert_eq!(
        tick_with(fixture.store(), &env, &actuator, fixture.project()).unwrap(),
        TickResult::Completed
    );
    assert_eq!(
        actuator.reaped.lock().unwrap().as_slice(),
        std::slice::from_ref(&id)
    );
    assert_eq!(
        tick_with(fixture.store(), &env, &actuator, fixture.project()).unwrap(),
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
fn a_durable_incident_marks_the_head_and_keeps_every_stalled_timestamp_fixed() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let head = submitted(&fixture, "broken head", Priority::High, PR_ONE);
    let tail = submitted(&fixture, "waiting tail", Priority::Low, PR_TWO);
    let candidate = VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap();
    let incident = VerificationIncident {
        incident_id: format!(
            "{}:{}",
            candidate.project.get(),
            candidate.verifying_generation.unwrap().get()
        ),
        project: candidate.project,
        story: StoryNo::parse_id("SH", &head).unwrap(),
        generation: candidate.verifying_generation.unwrap(),
        disposition: VerificationFailureDisposition::Permanent,
        halted: true,
        attempts: 1,
        detail: "not inside a git worktree".into(),
        first_failed_at: "2026-01-01T00:01:00Z".into(),
        last_failed_at: "2026-01-01T00:01:00Z".into(),
    };
    fixture
        .store()
        .write(|tx| tx.put_verification_incident(&incident))
        .unwrap();
    let activity = VerificationActivity::new();

    assert!(
        publish_once(
            fixture.store(),
            fixture.env(),
            "2026-01-01T00:02:00Z",
            &activity
        )
        .unwrap()
    );
    let head_first = last_comment(&fixture, &head);
    let tail_first = last_comment(&fixture, &tail);
    assert!(head_first.contains("Verification — HALTED"), "{head_first}");
    // SH-666: the tail names the incident as the verifier's own and the head
    // as where it was first hit — never as a blocker — and how to release it.
    assert!(
        tail_first.contains("Verifier HALTED since 2026-01-01T00:01:00Z on an infrastructure failure of the verifier itself"),
        "{tail_first}"
    );
    assert!(
        tail_first.contains("first hit while verifying SH-1 (SH-1 is not at fault)"),
        "{tail_first}"
    );
    assert!(
        tail_first.contains(&format!("story verifier ack {}", incident.incident_id)),
        "{tail_first}"
    );
    assert!(!tail_first.contains("blocked by"), "{tail_first}");
    assert!(head_first.contains("last evidence 2026-01-01T00:01:00Z"));

    assert!(
        !publish_once(
            fixture.store(),
            fixture.env(),
            "2026-01-01T00:20:00Z",
            &activity
        )
        .unwrap()
    );
    assert_eq!(last_comment(&fixture, &head), head_first);
    assert_eq!(last_comment(&fixture, &tail), tail_first);
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
    let actuator = FakeActuator::new(VerificationOutcome::Merged {
        tree: "abc123".into(),
        detail: "landed".into(),
        gate: GateCommand::DEFAULT.into(),
    });
    assert_eq!(
        tick_with(fixture.store(), &env, &actuator, fixture.project()).unwrap(),
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

/// A git-initialized scratch checkout with the origin the fixture PR belongs
/// to, and beside it a recording `verify-pr.sh` — in its own directory, the
/// bundle's stand-in, since the checkout holds no verifier of its own (SH-654)
/// — that writes its argv to `<checkout>/argv` and answers a merged verdict:
/// the SH-649 seam, where what reaches the script is the whole question.
fn recording_checkout() -> (tempfile::TempDir, tempfile::TempDir) {
    let checkout = scratch_dir();
    for args in [
        &["init", "-q"][..],
        &[
            "config",
            "remote.origin.url",
            "https://github.com/acme/widgets.git",
        ][..],
    ] {
        let out = Command::new("git")
            .args(args)
            .current_dir(checkout.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let tools = scratch_dir();
    std::fs::write(
        tools.path().join("verify-pr.sh"),
        "#!/bin/bash\nprintf '%s\\n' \"$@\" > argv\n\
         printf '{\"result\":\"merged\",\"tree\":\"t\",\"detail\":\"landed\"}\\n'\n",
    )
    .unwrap();
    (checkout, tools)
}

fn shell_actuator_candidate(checkout: &Path) -> (VerificationCandidate, storyhook::store::PrLink) {
    let fixture = ServiceFixture::new();
    let candidate = VerificationCandidate {
        project: fixture.project(),
        project_slug: "fixture".into(),
        story_id: "SH-1".into(),
        title: "configured gate".into(),
        priority: Priority::High,
        created_at: FIXTURE_NOW.into(),
        verifying_since: Some(FIXTURE_NOW.into()),
        verifying_generation: None,
        blocking_revision: None,
        checkout: checkout.to_path_buf(),
        cleanup_lease: None,
        pull_request: Err(VerificationProblem::MissingPullRequest),
    };
    let pull_request = storyhook::store::PrLink {
        owner: "acme".into(),
        repo: "widgets".into(),
        number: 1,
        url: PR_ONE.into(),
        close_on_merge: true,
        status: "open".into(),
        linked_at: FIXTURE_NOW.into(),
        last_checked_at: None,
    };
    (candidate, pull_request)
}

fn shell_actuator(
    daemon_env: &Environment,
    checkout: &Path,
    tools: &Path,
) -> ShellVerificationActuator {
    ShellVerificationActuator::with_paths(
        daemon_env.clone(),
        checkout.join("unused-helper"),
        PathBuf::from("/usr/bin/true"),
    )
    .with_verifier_script(tools.join("verify-pr.sh"))
}

/// The configured gate reaches `verify-pr.sh` as `<url> -- <argv…>`, one word
/// per element, and the verdict carries the same command so the GREEN and RED
/// comments name what actually ran rather than a literal.
#[test]
fn the_configured_gate_reaches_verify_pr_as_a_bare_argv_and_names_the_verdict() {
    let (checkout, tools) = recording_checkout();
    std::fs::write(
        checkout.path().join(".storyhook.toml"),
        "schema = 1\nuuid = \"291ea25f-3363-4b5d-9051-66636c1066f9\"\nprefix = \"SH\"\n\n\
         [verify]\ngate = \"cargo test --workspace\"\n",
    )
    .unwrap();
    let (candidate, pull_request) = shell_actuator_candidate(checkout.path());
    let env_root = scratch_dir();
    let actuator = shell_actuator(
        &Environment::at(env_root.path()),
        checkout.path(),
        tools.path(),
    );

    let outcome = actuator.verify(&candidate, &pull_request);
    assert_eq!(
        outcome,
        VerificationOutcome::Merged {
            tree: "t".into(),
            detail: "landed".into(),
            gate: "cargo test --workspace".into(),
        }
    );
    let argv = std::fs::read_to_string(checkout.path().join("argv")).unwrap();
    assert_eq!(
        argv,
        format!("{PR_ONE}\n--\ncargo\ntest\n--workspace\n"),
        "each word is its own argv element"
    );
}

/// No pointer at all is the default gate, spelled out to the script rather
/// than left for it to assume — the script carries no default of its own.
#[test]
fn a_checkout_without_a_pointer_runs_the_default_gate() {
    let (checkout, tools) = recording_checkout();
    let (candidate, pull_request) = shell_actuator_candidate(checkout.path());
    let env_root = scratch_dir();
    let actuator = shell_actuator(
        &Environment::at(env_root.path()),
        checkout.path(),
        tools.path(),
    );

    let outcome = actuator.verify(&candidate, &pull_request);
    assert!(
        matches!(outcome, VerificationOutcome::Merged { ref gate, .. } if gate == GateCommand::DEFAULT),
        "{outcome:?}"
    );
    let argv = std::fs::read_to_string(checkout.path().join("argv")).unwrap();
    assert_eq!(argv, format!("{PR_ONE}\n--\nmake\ntest\n"));
}

/// A gate that is not a plain argv is local configuration needing a person:
/// a permanent infrastructure failure naming the key and the character,
/// taken before the verifier is spawned and before a journal is written, and
/// never a red returned to the implementor as if the code were wrong.
#[test]
fn a_gate_that_is_not_a_plain_argv_halts_before_the_verifier_is_spawned() {
    let (checkout, tools) = recording_checkout();
    std::fs::write(
        checkout.path().join(".storyhook.toml"),
        "schema = 1\nuuid = \"291ea25f-3363-4b5d-9051-66636c1066f9\"\nprefix = \"SH\"\n\n\
         [verify]\ngate = \"make test && rm -rf /\"\n",
    )
    .unwrap();
    let (candidate, pull_request) = shell_actuator_candidate(checkout.path());
    let env_root = scratch_dir();
    let daemon_env = Environment::at(env_root.path());
    let actuator = shell_actuator(&daemon_env, checkout.path(), tools.path());

    let outcome = actuator.verify(&candidate, &pull_request);
    match outcome {
        VerificationOutcome::InfrastructureFailure {
            detail,
            disposition,
        } => {
            assert_eq!(
                disposition,
                storyhook::store::VerificationFailureDisposition::Permanent
            );
            assert!(detail.contains("[verify].gate"), "{detail}");
            assert!(detail.contains("`&`"), "{detail}");
            assert!(detail.contains(".storyhook.toml"), "{detail}");
        }
        other => panic!("a misconfigured gate is an infrastructure failure, got {other:?}"),
    }
    assert!(
        !checkout.path().join("argv").exists(),
        "the verifier must not have been spawned"
    );
    assert!(
        !journal_path(&daemon_env, &candidate).exists(),
        "no progress journal is written for a run that never started"
    );
}

/// The worker's comments are derived from the verdict's own gate, never from
/// a literal: a project whose gate is not `make test` reads its own command
/// in GREEN and RED.
#[test]
fn green_and_red_comments_name_the_gate_the_verdict_carries() {
    for (outcome, prefix, expected) in [
        (
            VerificationOutcome::Merged {
                tree: "abc123".into(),
                detail: "landed".into(),
                gate: "cargo test --workspace".into(),
            },
            VERIFICATION_GREEN_PREFIX,
            "merge tree `abc123` passed `cargo test --workspace`",
        ),
        (
            VerificationOutcome::TestsFailed {
                tree: "abc123".into(),
                log: "/tmp/red.log".into(),
                detail: "red".into(),
                gate: "make test-full".into(),
            },
            "CENTRAL VERIFICATION RED",
            "merge tree `abc123` failed `make test-full`",
        ),
    ] {
        let fixture = ServiceFixture::new();
        fixture.link_origin("https://github.com/acme/widgets");
        let id = submitted(&fixture, "named gate", Priority::High, PR_ONE);
        let root = scratch_dir();
        let env = Environment::at(root.path());
        let actuator = FakeActuator::new(outcome);
        tick_with(fixture.store(), &env, &actuator, fixture.project()).unwrap();
        let story_no = StoryNo::parse_id("SH", &id).unwrap();
        let row = fixture
            .store()
            .read(|tx| tx.story(fixture.project(), story_no))
            .unwrap()
            .unwrap();
        let comment = row
            .snapshot
            .comments
            .iter()
            .find(|comment| comment.text.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix} comment: {:?}", row.snapshot.comments));
        assert!(comment.text.contains(expected), "{}", comment.text);
        assert!(
            !comment.text.contains("`make test`"),
            "the literal must be gone: {}",
            comment.text
        );
    }
}

// ---------------------------------------------------------------------------
// One verifier per project (SH-648)
// ---------------------------------------------------------------------------

/// A story submitted in a project other than the fixture's seeded one.
fn submitted_in(
    fixture: &ServiceFixture,
    project: storyhook::store::ProjectId,
    title: &str,
    priority: Priority,
    url: &str,
) -> String {
    let ctx = fixture.ctx_for(project);
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

/// The second fixture project, with its own origin: a submission there links
/// a PR on a different repository, as a second registered checkout would.
fn second_project(fixture: &ServiceFixture) -> storyhook::store::ProjectId {
    let project = fixture.add_project("gadgets", "GD");
    fixture.link_origin_for(project, "https://github.com/acme/gadgets");
    project
}

const GADGETS_PR_ONE: &str = "https://github.com/acme/gadgets/pull/1";

/// An actuator that holds one project's verification open until released,
/// and lands every other project's on sight. Its `entered` channel names the
/// project whose worker reached the blocking call, so a test can prove two
/// workers are inside `verify` at once.
struct ProjectGateActuator {
    held: storyhook::store::ProjectId,
    entered: std::sync::mpsc::Sender<storyhook::store::ProjectId>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl VerificationActuator for ProjectGateActuator {
    fn verify(
        &self,
        candidate: &VerificationCandidate,
        _pull_request: &PrLink,
    ) -> VerificationOutcome {
        self.entered
            .send(candidate.project)
            .expect("the test observes every entry");
        if candidate.project == self.held {
            self.release
                .lock()
                .expect("locking the release channel")
                .recv_timeout(lifecycle::CONTROL_DEADLINE)
                .expect("the test must release the held verifier");
        }
        VerificationOutcome::Merged {
            tree: format!("tree-{}", candidate.project_slug),
            detail: "landed".into(),
            gate: GateCommand::DEFAULT.into(),
        }
    }

    fn submit(
        &self,
        candidate: &VerificationCandidate,
    ) -> Result<SubmittedPullRequest, SubmissionFailure> {
        adopt_linked(candidate)
    }

    fn notify(
        &self,
        _candidate: &VerificationCandidate,
        _message: &str,
    ) -> Result<NotifyDelivery, AppError> {
        Ok(NotifyDelivery::Delivered)
    }

    fn redispatch(
        &self,
        _candidate: &VerificationCandidate,
        _plan: &ResumePlan,
    ) -> Result<(), AppError> {
        panic!("a delivered notification never re-dispatches")
    }

    fn reap(&self, _candidate: &VerificationCandidate) -> Result<(), AppError> {
        Ok(())
    }
}

/// D-B: two projects' verifications overlap. Project A's worker is held
/// inside its actuator; project B's tick, sharing the SAME activity registry
/// and in-flight ledger, completes while A is held, and both attempts are
/// visible as owned at the same instant.
#[test]
fn two_projects_verify_concurrently() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let widgets = fixture.project();
    let gadgets = second_project(&fixture);
    let widgets_story = submitted(&fixture, "held", Priority::High, PR_ONE);
    let gadgets_story = submitted_in(
        &fixture,
        gadgets,
        "flows past",
        Priority::High,
        GADGETS_PR_ONE,
    );

    let activity = VerificationActivity::new();
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(fixture.env().clone());
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let actuator = ProjectGateActuator {
        held: widgets,
        entered: entered_tx,
        release: Mutex::new(release_rx),
    };

    let (widgets_result, gadgets_result) = thread::scope(|scope| {
        let widgets_worker = scope.spawn(|| {
            tick_with_activity(
                fixture.store(),
                fixture.env(),
                &actuator,
                &activity,
                &inflight,
                widgets,
            )
        });
        assert_eq!(
            entered_rx
                .recv_timeout(lifecycle::CONTROL_DEADLINE)
                .expect("widgets' worker must reach its actuator"),
            widgets
        );

        let gadgets_result = tick_with_activity(
            fixture.store(),
            fixture.env(),
            &actuator,
            &activity,
            &inflight,
            gadgets,
        )
        .unwrap();
        assert_eq!(
            entered_rx
                .recv_timeout(lifecycle::CONTROL_DEADLINE)
                .expect("gadgets' worker must reach its actuator while widgets is held"),
            gadgets
        );
        assert_eq!(
            activity
                .active_for(widgets)
                .map(|active| active.story_id)
                .as_deref(),
            Some(widgets_story.as_str()),
            "widgets is still owned while gadgets ran to completion"
        );
        assert_eq!(
            activity.active_for(gadgets),
            None,
            "gadgets' attempt released its own slot and nobody else's"
        );

        release_tx.send(()).unwrap();
        (widgets_worker.join().unwrap().unwrap(), gadgets_result)
    });

    assert_eq!(widgets_result, TickResult::Completed);
    assert_eq!(gadgets_result, TickResult::Completed);
    assert!(activity.active_all().is_empty());
    for (project, id, prefix) in [
        (widgets, &widgets_story, "SH"),
        (gadgets, &gadgets_story, "GD"),
    ] {
        let row = fixture
            .store()
            .read(|tx| tx.story(project, StoryNo::parse_id(prefix, id).unwrap()))
            .unwrap()
            .unwrap();
        assert_eq!(row.state, "done", "{id}");
    }
}

/// Both owned at once is the observable the previous test cannot assert
/// while one tick runs on the calling thread: two held workers, two entries
/// in `active_all`, two `verify` records in the in-flight ledger.
#[test]
fn two_held_verifications_are_both_visible_as_owned() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let widgets = fixture.project();
    let gadgets = second_project(&fixture);
    submitted(&fixture, "held one", Priority::High, PR_ONE);
    submitted_in(
        &fixture,
        gadgets,
        "held two",
        Priority::High,
        GADGETS_PR_ONE,
    );

    let activity = VerificationActivity::new();
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(fixture.env().clone());
    let mut gates = Vec::new();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    for project in [widgets, gadgets] {
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        gates.push((
            project,
            release_tx,
            ProjectGateActuator {
                held: project,
                entered: entered_tx.clone(),
                release: Mutex::new(release_rx),
            },
        ));
    }

    let fixture = &fixture;
    let activity = &activity;
    let inflight = &inflight;
    thread::scope(|scope| {
        let workers: Vec<_> = gates
            .iter()
            .map(|(project, _, actuator)| {
                scope.spawn(move || {
                    tick_with_activity(
                        fixture.store(),
                        fixture.env(),
                        actuator,
                        activity,
                        inflight,
                        *project,
                    )
                })
            })
            .collect();
        let mut entered = vec![
            entered_rx
                .recv_timeout(lifecycle::CONTROL_DEADLINE)
                .unwrap(),
            entered_rx
                .recv_timeout(lifecycle::CONTROL_DEADLINE)
                .unwrap(),
        ];
        entered.sort();
        assert_eq!(entered, [widgets, gadgets]);

        assert_eq!(activity.active_all().len(), 2);
        let published = lifecycle::read_inflight(fixture.env());
        assert_eq!(published.len(), 2, "{published:?}");
        assert!(published.iter().all(|entry| entry.command == "verify"));
        assert_ne!(published[0].request_id, published[1].request_id);

        for (_, release, _) in &gates {
            release.send(()).unwrap();
        }
        for worker in workers {
            assert_eq!(worker.join().unwrap().unwrap(), TickResult::Completed);
        }
    });
    assert!(activity.active_all().is_empty());
    assert!(lifecycle::read_inflight(fixture.env()).is_empty());
}

/// A halt is the project's own: widgets halts on a permanent infrastructure
/// failure and stays halted, gadgets drains, and each project's dashboard
/// and acknowledgement see only their own incident.
#[test]
fn a_halt_in_one_project_leaves_the_other_draining() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let widgets = fixture.project();
    let gadgets = second_project(&fixture);
    let widgets_story = submitted(&fixture, "broken verifier", Priority::High, PR_ONE);
    let gadgets_story = submitted_in(&fixture, gadgets, "healthy", Priority::High, GADGETS_PR_ONE);

    let halting = FakeActuator::new(VerificationOutcome::InfrastructureFailure {
        detail: "not inside a git worktree".into(),
        disposition: VerificationFailureDisposition::Permanent,
    });
    let landing = FakeActuator::new(VerificationOutcome::Merged {
        tree: "gadgets-tree".into(),
        detail: "landed".into(),
        gate: GateCommand::DEFAULT.into(),
    });

    assert_eq!(
        tick_with(fixture.store(), fixture.env(), &halting, widgets).unwrap(),
        TickResult::Halted
    );
    assert_eq!(
        tick_with(fixture.store(), fixture.env(), &halting, widgets).unwrap(),
        TickResult::Halted,
        "widgets stays halted until acknowledged"
    );
    assert_eq!(
        tick_with(fixture.store(), fixture.env(), &landing, gadgets).unwrap(),
        TickResult::Completed,
        "gadgets must drain while widgets is halted"
    );
    let incident = fixture
        .store()
        .read(|tx| tx.verification_incident(widgets))
        .unwrap()
        .expect("widgets' halt is durable");
    assert_eq!(incident.story.to_id("SH"), widgets_story);
    assert!(
        fixture
            .store()
            .read(|tx| tx.verification_incident(gadgets))
            .unwrap()
            .is_none(),
        "gadgets has no incident of its own"
    );
    let gadgets_row = fixture
        .store()
        .read(|tx| tx.story(gadgets, StoryNo::parse_id("GD", &gadgets_story).unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(gadgets_row.state, "done");

    let headers = [Header::from_bytes("Host", "127.0.0.1:3456").unwrap()];
    let data = |slug: &str| -> serde_json::Value {
        let response = rest::route(
            fixture.store(),
            fixture.env(),
            &Method::Get,
            &format!("/api/repos/{slug}/data"),
            &headers,
            "",
            &TrustedHosts::default(),
        );
        serde_json::from_str(response.reply.text_body().expect("UTF-8 text response")).unwrap()
    };
    assert_eq!(
        data("fixture")["verification_incident"]["story_id"],
        widgets_story
    );
    assert_eq!(
        data("gadgets")["verification_incident"],
        serde_json::Value::Null,
        "another project's halt must not be reported on this project's dashboard"
    );

    let ack_headers = [
        Header::from_bytes("Host", "127.0.0.1:3456").unwrap(),
        Header::from_bytes("X-Storyhook", "1").unwrap(),
        Header::from_bytes("Content-Type", "application/json").unwrap(),
    ];
    let body = serde_json::json!({"incident_id": incident.incident_id}).to_string();
    let wrong_project = rest::route(
        fixture.store(),
        fixture.env(),
        &Method::Post,
        "/api/repos/gadgets/verification/ack",
        &ack_headers,
        &body,
        &TrustedHosts::default(),
    );
    assert_eq!(
        wrong_project.reply.status,
        422,
        "widgets' incident cannot be acknowledged through gadgets: {}",
        wrong_project
            .reply
            .text_body()
            .expect("UTF-8 text response")
    );
    let ack = rest::route(
        fixture.store(),
        fixture.env(),
        &Method::Post,
        "/api/repos/fixture/verification/ack",
        &ack_headers,
        &body,
        &TrustedHosts::default(),
    );
    assert_eq!(ack.reply.status, 200);
    assert!(
        fixture
            .store()
            .read(|tx| tx.verification_incident(widgets))
            .unwrap()
            .is_none()
    );
}

/// The conflict queue-hold reserves ONE project's worker for the conflicted
/// story. Inside widgets' hold, gadgets' tick — the same activity registry —
/// runs to completion.
#[test]
fn a_conflict_hold_in_one_project_does_not_hold_the_other() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let widgets = fixture.project();
    let gadgets = second_project(&fixture);
    let widgets_story = submitted(&fixture, "conflicted", Priority::High, PR_ONE);
    let gadgets_story = submitted_in(
        &fixture,
        gadgets,
        "unrelated",
        Priority::High,
        GADGETS_PR_ONE,
    );

    let activity = VerificationActivity::new();
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(fixture.env().clone());
    let conflicting = FakeActuator::new(VerificationOutcome::Conflict {
        detail: "base moved".into(),
    });
    let landing = FakeActuator::new(VerificationOutcome::Merged {
        tree: "gadgets-tree".into(),
        detail: "landed".into(),
        gate: GateCommand::DEFAULT.into(),
    });
    let other_drained_during_hold = Mutex::new(None);

    let result = tick_with_reconciliation(
        fixture.store(),
        fixture.env(),
        &conflicting,
        &activity,
        &inflight,
        widgets,
        |reserved| {
            assert_eq!(reserved.story_id, widgets_story);
            assert_eq!(
                activity
                    .active_for(widgets)
                    .map(|active| active.story_id)
                    .as_deref(),
                Some(widgets_story.as_str()),
                "the hold keeps widgets' worker reserved"
            );
            let drained = tick_with_activity(
                fixture.store(),
                fixture.env(),
                &landing,
                &activity,
                &inflight,
                gadgets,
            )?;
            *other_drained_during_hold.lock().unwrap() = Some(drained);
            Ok(None)
        },
    )
    .unwrap();

    assert_eq!(result, TickResult::Returned);
    assert_eq!(
        *other_drained_during_hold.lock().unwrap(),
        Some(TickResult::Completed),
        "gadgets must land while widgets' worker is held for reconciliation"
    );
    let gadgets_row = fixture
        .store()
        .read(|tx| tx.story(gadgets, StoryNo::parse_id("GD", &gadgets_story).unwrap()))
        .unwrap()
        .unwrap();
    assert_eq!(gadgets_row.state, "done");
    assert!(activity.active_all().is_empty());
}

/// A queued story's position counts its own project's queue, never the
/// machine's: two projects with two stories each both report positions 1
/// and 2 in their progress comments.
#[test]
fn a_queued_candidate_position_counts_only_its_own_project() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let gadgets = second_project(&fixture);
    let widgets_first = submitted(&fixture, "widgets first", Priority::High, PR_ONE);
    let widgets_second = submitted(&fixture, "widgets second", Priority::Low, PR_TWO);
    let gadgets_first = submitted_in(
        &fixture,
        gadgets,
        "gadgets first",
        Priority::High,
        GADGETS_PR_ONE,
    );
    let gadgets_second = submitted_in(
        &fixture,
        gadgets,
        "gadgets second",
        Priority::Low,
        "https://github.com/acme/gadgets/pull/2",
    );

    publish_once(
        fixture.store(),
        fixture.env(),
        "2026-01-01T00:02:00Z",
        &VerificationActivity::new(),
    )
    .unwrap();

    assert!(last_comment(&fixture, &widgets_first).contains("QUEUED (position 1)"));
    assert!(last_comment(&fixture, &widgets_second).contains("QUEUED (position 2)"));
    let gadgets_comment = |id: &str| -> String {
        let row = fixture
            .store()
            .read(|tx| tx.story(gadgets, StoryNo::parse_id("GD", id).unwrap()))
            .unwrap()
            .unwrap();
        row.snapshot.comments.last().unwrap().text.clone()
    };
    assert!(
        gadgets_comment(&gadgets_first).contains("QUEUED (position 1)"),
        "{}",
        gadgets_comment(&gadgets_first)
    );
    assert!(
        gadgets_comment(&gadgets_second).contains("QUEUED (position 2)"),
        "{}",
        gadgets_comment(&gadgets_second)
    );
}

/// The supervisor itself: one worker per registered project from the start,
/// both inside their actuators at once; a project registered while the
/// daemon runs gets a worker on the catalog change; stop drains every worker.
#[test]
fn the_supervisor_runs_one_worker_per_project_and_follows_the_catalog() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use storyhook::daemon::bus::{Change, ChangeBus};
    use storyhook::daemon::verification::poll_verification_with;

    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let widgets = fixture.project();
    let gadgets = second_project(&fixture);
    submitted(&fixture, "widgets work", Priority::High, PR_ONE);
    submitted_in(
        &fixture,
        gadgets,
        "gadgets work",
        Priority::High,
        GADGETS_PR_ONE,
    );

    let activity = VerificationActivity::new();
    std::fs::create_dir_all(fixture.env().daemon_state_dir()).unwrap();
    let inflight = InFlight::new(fixture.env().clone());
    let bus = ChangeBus::new();
    let stop = AtomicBool::new(false);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let releases: Mutex<Vec<(storyhook::store::ProjectId, std::sync::mpsc::Sender<()>)>> =
        Mutex::new(Vec::new());

    /// Stops the supervisor when the scope unwinds, so an assertion that
    /// fails below turns this test red instead of joining a supervisor
    /// nobody told to stop (measured: without this, every mutation of the
    /// supervisor hung the binary rather than failing the case).
    struct StopOnDrop<'a> {
        stop: &'a AtomicBool,
        bus: &'a ChangeBus,
    }
    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            self.bus.publish(Change::Catalog);
        }
    }

    thread::scope(|scope| {
        let _stop_on_unwind = StopOnDrop {
            stop: &stop,
            bus: &bus,
        };
        let supervisor = scope.spawn(|| {
            poll_verification_with(
                fixture.store(),
                fixture.env(),
                &bus,
                &stop,
                &activity,
                &inflight,
                |project| {
                    let (release_tx, release_rx) = std::sync::mpsc::channel();
                    releases.lock().unwrap().push((project, release_tx));
                    ProjectGateActuator {
                        held: project,
                        entered: entered_tx.clone(),
                        release: Mutex::new(release_rx),
                    }
                },
            );
        });

        let mut entered = vec![
            entered_rx
                .recv_timeout(lifecycle::CONTROL_DEADLINE)
                .unwrap(),
            entered_rx
                .recv_timeout(lifecycle::CONTROL_DEADLINE)
                .unwrap(),
        ];
        entered.sort();
        assert_eq!(
            entered,
            [widgets, gadgets],
            "both projects' workers must be inside their actuators at once"
        );
        assert_eq!(activity.active_all().len(), 2);

        // A project registered while the daemon runs.
        let sprockets = fixture.add_project("sprockets", "SP");
        fixture.link_origin_for(sprockets, "https://github.com/acme/sprockets");
        submitted_in(
            &fixture,
            sprockets,
            "sprockets work",
            Priority::High,
            "https://github.com/acme/sprockets/pull/1",
        );
        bus.publish(Change::Catalog);
        assert_eq!(
            entered_rx
                .recv_timeout(lifecycle::CONTROL_DEADLINE)
                .expect("the catalog change must spawn sprockets' worker"),
            sprockets
        );
        assert_eq!(activity.active_all().len(), 3);

        for (_, release) in releases.lock().unwrap().iter() {
            release.send(()).unwrap();
        }
        // Every worker lands its story, then idles; stop drains them.
        let deadline = Instant::now() + lifecycle::CONTROL_DEADLINE;
        while Instant::now() < deadline && !activity.active_all().is_empty() {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            activity.active_all().is_empty(),
            "{:?}",
            activity.active_all()
        );
        drop(_stop_on_unwind);
        supervisor.join().unwrap();
    });

    for (project, prefix) in [(widgets, "SH"), (gadgets, "GD")] {
        let rows = fixture
            .store()
            .read(|tx| tx.stories(project, &storyhook::store::StoryQuery::all().state("done")))
            .unwrap();
        assert_eq!(rows.len(), 1, "{prefix}: {rows:?}");
    }
}
