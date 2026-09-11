//! SH-651: verification age follows the latest submission, not story creation.

use storyhook::domain::Priority;
use storyhook::service::{
    Clock, NewStoryInput, PrLinkService, StoryService, VERIFICATION_GREEN_PREFIX, VerificationQueue,
};
use storyhook_test_support::ServiceFixture;

const ORIGIN: &str = "https://github.com/acme/widgets";
const PR_ONE: &str = "https://github.com/acme/widgets/pull/1";
const PR_TWO: &str = "https://github.com/acme/widgets/pull/2";

fn create(fixture: &ServiceFixture, title: &str, priority: Priority, url: &str) -> String {
    let ctx = fixture.ctx();
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: title.into(),
            priority: Some(priority.as_str().into()),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id;
    PrLinkService::new(&ctx).link(&id, url, true).unwrap();
    id
}

fn move_at(fixture: &mut ServiceFixture, id: &str, state: &str, at: &str) {
    fixture.set_clock(Clock::Fixed(at.into()));
    StoryService::new(&fixture.ctx())
        .set_state(id, state, None, None, None)
        .unwrap();
}

fn assert_order(fixture: &ServiceFixture, expected: &[&str]) {
    let queue = VerificationQueue::new(fixture.store());
    for candidates in [
        queue.ordered().unwrap(),
        queue.ordered_for(fixture.project()).unwrap(),
    ] {
        let ids: Vec<_> = candidates
            .iter()
            .map(|candidate| candidate.story_id.as_str())
            .collect();
        assert_eq!(ids, expected);
    }
    assert_eq!(queue.next().unwrap().unwrap().story_id, expected[0]);
}

#[test]
fn equal_priority_follows_submission_order_when_creation_order_disagrees() {
    let mut fixture = ServiceFixture::new();
    fixture.link_origin(ORIGIN);
    let older = create(&fixture, "created first", Priority::Medium, PR_ONE);
    fixture.set_clock(Clock::Fixed("2026-01-01T00:01:00Z".into()));
    let newer = create(&fixture, "submitted first", Priority::Medium, PR_TWO);
    move_at(&mut fixture, &newer, "verifying", "2026-01-01T00:02:00Z");
    move_at(&mut fixture, &older, "verifying", "2026-01-01T00:03:00Z");
    assert_order(&fixture, &[&newer, &older]);

    // Progress and ordinary comments must not move a waiting story backwards.
    fixture.set_clock(Clock::Fixed("2026-01-01T00:04:00Z".into()));
    StoryService::new(&fixture.ctx())
        .comment(&newer, "Still waiting.")
        .unwrap();
    assert_order(&fixture, &[&newer, &older]);
}

#[test]
fn resubmission_moves_an_older_story_behind_its_waiting_peer() {
    let mut fixture = ServiceFixture::new();
    fixture.link_origin(ORIGIN);
    let older = create(&fixture, "first attempt", Priority::Medium, PR_ONE);
    move_at(&mut fixture, &older, "verifying", "2026-01-01T00:01:00Z");
    fixture.set_clock(Clock::Fixed("2026-01-01T00:02:00Z".into()));
    let newer = create(&fixture, "waiting peer", Priority::Medium, PR_TWO);
    move_at(&mut fixture, &newer, "verifying", "2026-01-01T00:03:00Z");
    assert_order(&fixture, &[&older, &newer]);

    for (returned, resubmitted) in [
        ("2026-01-01T00:04:00Z", "2026-01-01T00:05:00Z"),
        ("2026-01-01T00:06:00Z", "2026-01-01T00:07:00Z"),
    ] {
        move_at(&mut fixture, &older, "in-progress", returned);
        move_at(&mut fixture, &older, "verifying", resubmitted);
        assert_order(&fixture, &[&newer, &older]);
        let candidates = VerificationQueue::new(fixture.store()).ordered().unwrap();
        assert_eq!(candidates[1].verifying_since.as_deref(), Some(resubmitted));
    }
}

#[test]
fn priority_precedes_queue_age() {
    let mut fixture = ServiceFixture::new();
    fixture.link_origin(ORIGIN);
    let low = create(&fixture, "waiting low", Priority::Low, PR_ONE);
    move_at(&mut fixture, &low, "verifying", "2026-01-01T00:01:00Z");
    fixture.set_clock(Clock::Fixed("2026-01-01T00:02:00Z".into()));
    let high = create(&fixture, "arriving high", Priority::High, PR_TWO);
    move_at(&mut fixture, &high, "verifying", "2026-01-01T00:03:00Z");
    assert_order(&fixture, &[&high, &low]);
}

#[test]
fn equal_submission_times_use_identity_even_when_creation_times_disagree() {
    let mut fixture = ServiceFixture::new();
    fixture.link_origin(ORIGIN);
    let first = create(&fixture, "first identity", Priority::Medium, PR_ONE);
    fixture.set_clock(Clock::Fixed("2025-12-31T23:59:00Z".into()));
    let second = create(&fixture, "older timestamp", Priority::Medium, PR_TWO);
    move_at(&mut fixture, &second, "verifying", "2026-01-01T00:01:00Z");
    move_at(&mut fixture, &first, "verifying", "2026-01-01T00:01:00Z");
    assert_order(&fixture, &[&first, &second]);
}

#[test]
fn cleanup_keeps_creation_order_when_identity_and_submission_order_disagree() {
    let mut fixture = ServiceFixture::new();
    fixture.link_origin(ORIGIN);
    let first = create(&fixture, "first identity", Priority::Medium, PR_ONE);
    fixture.set_clock(Clock::Fixed("2025-12-31T23:59:00Z".into()));
    let older = create(&fixture, "older timestamp", Priority::Medium, PR_TWO);
    move_at(&mut fixture, &first, "verifying", "2026-01-01T00:01:00Z");
    move_at(&mut fixture, &older, "verifying", "2026-01-01T00:02:00Z");
    for (id, url) in [(&first, PR_ONE), (&older, PR_TWO)] {
        let ctx = fixture.ctx();
        StoryService::new(&ctx)
            .comment(id, VERIFICATION_GREEN_PREFIX)
            .unwrap();
        VerificationQueue::new(fixture.store())
            .record_merged(&ctx, id, url)
            .unwrap();
    }
    let queue = VerificationQueue::new(fixture.store());
    for candidate in [
        queue.next_cleanup().unwrap(),
        queue.next_cleanup_for(fixture.project()).unwrap(),
    ] {
        let candidate = candidate.unwrap();
        assert_eq!(candidate.story_id, older);
        assert_eq!(candidate.verifying_since, None);
    }
}
