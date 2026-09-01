//! Store-backed contracts for the SH-521 centralized verification queue.

use storyhook::daemon::verification::{
    ShellVerificationActuator, TickResult, VerificationActuator, VerificationOutcome, journal_path,
    tick_with,
};
use storyhook::daemon::verification_progress::publish_once;
use storyhook::domain::remote::RemoteUrl;
use storyhook::domain::{Priority, SuperState};
use storyhook::env::Environment;
use storyhook::error::AppError;
use storyhook::service::gate_progress::GATE_PROGRESS_PREFIX;
use storyhook::service::{
    Clock, ConfigService, NewStoryInput, PrLinkService, StoryService,
    VERIFICATION_CLEANUP_COMPLETE_PREFIX, VERIFICATION_GREEN_PREFIX, VerificationCandidate,
    VerificationProblem, VerificationQueue,
};
use storyhook::store::{PrLink, ReadOps, Store, StoryNo, WriteOps};
use storyhook_test_support::ServiceFixture;
use storyhook_test_support::{FIXTURE_NOW, scratch_dir};

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
        "{\"kind\":\"item\",\"path\":\"release gate/fmt\",\"status\":\"passed\",\"at\":\"t\",\"seconds\":2}\n\
         {\"kind\":\"item\",\"path\":\"release gate/rust-suite\",\"status\":\"running\",\"at\":\"t\"}\n\
         {\"kind\":\"case\",\"path\":\"release gate/rust-suite\",\"outcome\":\"pass\"}\n",
    )
    .unwrap();

    let moved = publish_once(fixture.store(), fixture.env(), "2026-01-01T00:02:00Z").unwrap();
    assert!(
        moved,
        "the first publish for a running candidate must write"
    );

    let comment = last_comment(&fixture, &id);
    assert!(comment.starts_with(GATE_PROGRESS_PREFIX), "{comment}");
    assert!(comment.contains("- [x] fmt (1/1, 2s)"), "{comment}");
    assert!(comment.contains("rust-suite (1/~1, running)"), "{comment}");
    assert_eq!(progress_comment_count(&fixture, &id), 1);
}

#[test]
fn a_queued_candidate_shows_its_position_and_what_is_ahead_of_it() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let high = submitted(&fixture, "running", Priority::High, PR_ONE);
    let low = submitted(&fixture, "queued", Priority::Low, PR_TWO);

    publish_once(fixture.store(), fixture.env(), "2026-01-01T00:02:00Z").unwrap();

    let running_comment = last_comment(&fixture, &high);
    assert!(
        running_comment.contains("Verification ("),
        "{running_comment}"
    );

    let queued_comment = last_comment(&fixture, &low);
    assert!(
        queued_comment.contains("QUEUED (position 2)"),
        "{queued_comment}"
    );
    assert!(
        queued_comment.contains("1 candidate of higher priority, 0 of equal priority and older"),
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
        "{\"kind\":\"item\",\"path\":\"release gate/fmt\",\"status\":\"running\",\"at\":\"t\"}\n",
    )
    .unwrap();

    let first = publish_once(fixture.store(), fixture.env(), "2026-01-01T00:01:00Z").unwrap();
    assert!(first);
    assert_eq!(progress_comment_count(&fixture, &id), 1);

    std::fs::write(
        &journal,
        "{\"kind\":\"item\",\"path\":\"release gate/fmt\",\"status\":\"passed\",\"at\":\"t\",\"seconds\":5}\n",
    )
    .unwrap();
    let second = publish_once(fixture.store(), fixture.env(), "2026-01-01T00:02:00Z").unwrap();
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
        "{\"kind\":\"item\",\"path\":\"release gate/fmt\",\"status\":\"running\",\"at\":\"t\"}\n",
    )
    .unwrap();

    // Same `now` both times, so even the header's own timestamp is identical
    // and the rendered body is byte-for-byte the same on the second call.
    assert!(publish_once(fixture.store(), fixture.env(), "2026-01-01T00:01:00Z").unwrap());
    let moved_again = publish_once(fixture.store(), fixture.env(), "2026-01-01T00:01:00Z").unwrap();
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
        "{\"kind\":\"item\",\"path\":\"release gate/fmt\",\"status\":\"running\",\"at\":\"t\"}\n",
    )
    .unwrap();
    assert!(publish_once(fixture.store(), fixture.env(), "2026-01-01T00:01:00Z").unwrap());
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
        "{\"kind\":\"item\",\"path\":\"release gate/fmt\",\"status\":\"passed\",\"at\":\"t\"}\n",
    )
    .unwrap();
    let moved = publish_once(fixture.store(), fixture.env(), "2026-01-01T00:05:00Z").unwrap();
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
