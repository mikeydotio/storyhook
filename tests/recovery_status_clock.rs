//! Recovery elapsed time uses the same explicit clock as its service context.

use storyhook::daemon::verification::VerificationActivity;
use storyhook::service::project_fault::{ProjectFault, ReceiptRefusal};
use storyhook::service::project_recovery::ProjectRecoveryService;
use storyhook::service::{Clock, NewStoryInput, PrLinkService, StoryService, VerificationQueue};
use storyhook_test_support::ServiceFixture;

fn enrolled() -> (ServiceFixture, String) {
    let f = ServiceFixture::new();
    f.github_checkout("https://github.com/acme/widgets");
    let ctx = f.ctx();
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Restore missing gate certification".into(),
            ..Default::default()
        })
        .unwrap();
    PrLinkService::new(&ctx)
        .link(&story.id, "https://github.com/acme/widgets/pull/1", true)
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&story.id, "verifying", None, None, None)
        .unwrap();
    let candidate = VerificationQueue::new(f.store())
        .ordered_for(f.project())
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.story_id == story.id)
        .unwrap();
    let recovery = ProjectRecoveryService::new(&ctx)
        .observe(
            &candidate,
            &ProjectFault::MissingCertification {
                locus: ".storyhook.toml#verify.gate".into(),
                tree: "a".repeat(40),
                base: "b".repeat(40),
                head: "c".repeat(40),
                head_tree: "d".repeat(40),
                gate: "make test".into(),
                log: "/retained/gate.log".into(),
                execution: "/retained/execution.json".into(),
                execution_status: 0,
                receipt: ReceiptRefusal::Missing,
                detail: "successful gate omitted certification".into(),
            },
            "missing-certificate-attempt",
        )
        .unwrap()
        .unwrap();

    (f, recovery.record.id)
}

#[test]
fn recovery_status_elapsed_respects_the_context_clock() {
    let (f, recovery_id) = enrolled();
    let activity = VerificationActivity::new();
    // Explicit instants replace scheduler-dependent time between reads.
    // Expected durations are fixed examples, not a second elapsed calculator.
    let elapsed = [
        "2026-01-01T00:00:01.250Z",
        "2026-01-01T00:00:01.250Z",
        "2026-01-01T01:00:01.250+01:00",
        "2026-01-01T00:00:02.750Z",
        "2025-12-31T23:59:59Z",
    ]
    .map(|now| {
        let status = activity
            .status(&f.ctx().clock(Clock::Fixed(now.into())))
            .unwrap();
        let row = status
            .project_recoveries
            .into_iter()
            .find(|row| row.id == recovery_id)
            .expect("the enrolled recovery remains visible");
        assert_eq!(row.started_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        row.elapsed_milliseconds
    });
    assert_eq!(
        elapsed,
        [Some(1_250), Some(1_250), Some(1_250), Some(2_750), Some(0)],
        "recovery elapsed milliseconds must follow the supplied observation clock"
    );
}

#[test]
fn recovery_status_rejects_malformed_observation_for_a_live_row() {
    let (f, _) = enrolled();
    let error = VerificationActivity::new()
        .status(&f.ctx().clock(Clock::Fixed("not-a-timestamp".into())))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("project recovery timestamp not-a-timestamp"),
        "{error}"
    );
}
