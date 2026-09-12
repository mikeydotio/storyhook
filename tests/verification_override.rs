//! Completing a `verifying` story by hand (SH-692).
//!
//! PR #791 was merged by hand while its gate ran, and its story was dragged
//! to Done eleven seconds later with nothing recorded: no verdict, no reason.
//! The rule since is the operator's own: a hand completion of a story the
//! central verifier owns stays allowed, but it requires a stated reason,
//! recorded on the story as `CENTRAL VERIFICATION OVERRIDDEN — <why>`, and
//! the verifier then withdraws its attempt and moves on.
//!
//! Two layers are provoked here through the real store: the `set_state`
//! door every client shares (CLI, dashboard move, TUI, MCP), which refuses
//! a bare move naming the way out, and the backstop inside
//! `append_and_fold`, the one write path every service funnels through,
//! which catches every other producer of a completion — `story set
//! --state` is the case pinned below.
//!
//! # Mutation-checked (SH-295: a pin that cannot fail is not a pin)
//!
//! - the override branch removed from `set_state` (`overriding` forced
//!   false) → **2 of 9 red**: `completing_a_verifying_story_with_a_reason_
//!   records_the_override` and `an_overridden_story_whose_pull_request_merged_
//!   is_reap_eligible` — no OVERRIDDEN comment is written. The bare-move
//!   refusal stayed green because the backstop caught it with the same words:
//!   the two layers are deliberately redundant.
//! - `refuse_uncertified_completion` made a no-op → **2 of 9 red**:
//!   `story_set_state_done_on_a_verifying_story_hits_the_same_backstop` and
//!   `a_green_from_an_earlier_generation_does_not_certify_the_current_one`.
//! - the merged-link requirement dropped from the reap predicate → **1 of 9
//!   red**: `an_overridden_story_whose_pull_request_merged_is_reap_eligible`,
//!   whose first assertion is that an unmerged override is not reaped.

use storyhook::domain::StoryEvent;
use storyhook::domain::provenance::Provenance;
use storyhook::domain::{Priority, fold_story};
use storyhook::error::AppError;
use storyhook::service::{
    Clock, FieldEdits, NewStoryInput, PrLinkService, StoryService, VERIFICATION_GREEN_PREFIX,
    VERIFICATION_OVERRIDDEN_PREFIX, VerificationQueue,
};
use storyhook::store::{ExpectedSeq, ReadOps, Store, StoryNo, WriteOps, partition_known};
use storyhook_test_support::{FIXTURE_NOW, ServiceFixture};

const PR: &str = "https://github.com/acme/widgets/pull/1";

fn create(fixture: &ServiceFixture, title: &str) -> String {
    StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: title.into(),
            priority: Some(Priority::High.as_str().into()),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id
}

/// A story the verifier owns: linked and moved into `verifying`.
fn submitted(fixture: &ServiceFixture, title: &str) -> String {
    let id = create(fixture, title);
    PrLinkService::new(&fixture.ctx())
        .link(&id, PR, true)
        .unwrap();
    StoryService::new(&fixture.ctx())
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    id
}

fn row(fixture: &ServiceFixture, id: &str) -> storyhook::store::StoryRow {
    fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::parse_id("SH", id).unwrap()))
        .unwrap()
        .unwrap()
}

fn overrides(fixture: &ServiceFixture, id: &str) -> Vec<String> {
    row(fixture, id)
        .snapshot
        .comments
        .iter()
        .filter(|comment| comment.text.starts_with(VERIFICATION_OVERRIDDEN_PREFIX))
        .map(|comment| comment.text.clone())
        .collect()
}

/// Records the pull request as merged the way the poller does, without a
/// GitHub imitation: the event is the fact the store folds a `merged` link
/// status from.
fn mark_merged(fixture: &ServiceFixture, id: &str) {
    let project = fixture.project();
    let story = StoryNo::parse_id("SH", id).unwrap();
    fixture
        .store()
        .write(|tx| {
            let head = tx.append_events(
                project,
                story,
                ExpectedSeq::Any,
                &[StoryEvent::StoryPrMerged {
                    at: FIXTURE_NOW.into(),
                    url: PR.into(),
                }],
                &Provenance::unrecorded(),
            )?;
            let stored = tx.events_for(project, story)?;
            let (known, _) = partition_known(story, &stored);
            let states = tx.state_map(project)?;
            let snapshot =
                fold_story(id, &known, &states).map_err(storyhook::store::StoreError::from)?;
            tx.put_story(project, &snapshot, head)
        })
        .unwrap();
}

#[test]
fn completing_a_verifying_story_without_a_reason_is_refused_naming_the_override() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "under the gate");

    let error = StoryService::new(&fixture.ctx())
        .set_state(&id, "done", None, None, None)
        .expect_err("a bare completion of a verifying story is refused");

    let message = error.to_string();
    assert!(matches!(error, AppError::Validation(_)), "{error:?}");
    assert!(message.contains("under central verification"), "{message}");
    assert!(
        message.contains(&format!("story move {id} done \"<why>\"")),
        "the refusal names the override door: {message}"
    );
    assert!(
        message.contains(&format!("story move {id} in-progress")),
        "the refusal names the way back: {message}"
    );
    let after = row(&fixture, &id);
    assert_eq!(after.state, "verifying");
    assert!(!after.archived);
    assert!(overrides(&fixture, &id).is_empty());
}

#[test]
fn a_blank_reason_is_no_reason() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "under the gate");

    let error = StoryService::new(&fixture.ctx())
        .set_state(&id, "done", Some("   "), None, None)
        .expect_err("whitespace is not a reason");
    assert!(matches!(error, AppError::Validation(_)), "{error:?}");
    assert_eq!(row(&fixture, &id).state, "verifying");
}

#[test]
fn completing_a_verifying_story_with_a_reason_records_the_override() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "under the gate");

    StoryService::new(&fixture.ctx())
        .set_state(
            &id,
            "done",
            Some("  merged by hand; the gate was re-run locally  "),
            Some("verifying"),
            None,
        )
        .expect("a completion with a reason is an override");

    let after = row(&fixture, &id);
    assert_eq!(after.state, "done");
    assert!(after.archived, "done archives, override or not");
    let recorded = overrides(&fixture, &id);
    assert_eq!(
        recorded,
        vec![format!(
            "{VERIFICATION_OVERRIDDEN_PREFIX} merged by hand; the gate was re-run locally"
        )],
        "one marked comment carries the trimmed reason"
    );
    assert!(
        !after
            .snapshot
            .comments
            .iter()
            .any(|comment| comment.text == "merged by hand; the gate was re-run locally"),
        "the reason is recorded once, as the marked override, not as a second plain comment"
    );
}

/// `story set --state done` builds its own event batch rather than calling
/// `set_state`; the backstop in `append_and_fold` is what refuses it.
#[test]
fn story_set_state_done_on_a_verifying_story_hits_the_same_backstop() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "under the gate");

    let error = StoryService::new(&fixture.ctx())
        .set_fields(
            &id,
            &FieldEdits {
                state: Some("done".into()),
                ..FieldEdits::default()
            },
        )
        .expect_err("a completion through `story set` has no reason to give");

    let message = error.to_string();
    assert!(matches!(error, AppError::Validation(_)), "{error:?}");
    assert!(message.contains("under central verification"), "{message}");
    assert_eq!(row(&fixture, &id).state, "verifying");
}

#[test]
fn withdrawing_or_dropping_a_verifying_story_needs_no_reason() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let returned = submitted(&fixture, "handed back");
    let dropped = submitted(&fixture, "abandoned");

    StoryService::new(&fixture.ctx())
        .set_state(&returned, "in-progress", None, Some("verifying"), None)
        .expect("handing a story back is not an override");
    StoryService::new(&fixture.ctx())
        .set_state(
            &dropped,
            "dropped",
            Some("superseded"),
            Some("verifying"),
            None,
        )
        .expect("abandonment is not completion");

    assert_eq!(row(&fixture, &returned).state, "in-progress");
    assert_eq!(row(&fixture, &dropped).state, "dropped");
    assert!(overrides(&fixture, &returned).is_empty());
    assert!(overrides(&fixture, &dropped).is_empty());
}

#[test]
fn a_story_the_verifier_does_not_own_completes_without_a_reason_as_before() {
    let fixture = ServiceFixture::new();
    let id = create(&fixture, "ordinary");
    StoryService::new(&fixture.ctx())
        .set_state(&id, "done", None, None, None)
        .expect("only `verifying` is the verifier's");
    assert_eq!(row(&fixture, &id).state, "done");
    assert!(overrides(&fixture, &id).is_empty());
}

/// The verifier's own completion writes GREEN in the same batch; a GREEN
/// posted for THIS stay in `verifying` also counts. A GREEN from an earlier
/// generation certified an earlier tree and does not.
#[test]
fn a_green_from_an_earlier_generation_does_not_certify_the_current_one() {
    let mut fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "verified once, resubmitted since");
    StoryService::new(&fixture.ctx())
        .comment(
            &id,
            &format!("{VERIFICATION_GREEN_PREFIX} merge tree `old` passed `make test` and pull request {PR} landed."),
        )
        .unwrap();
    // The earlier generation: its GREEN is enough for a hand completion.
    fixture.set_clock(Clock::Fixed("2026-01-01T00:10:00Z".into()));
    StoryService::new(&fixture.ctx())
        .set_state(&id, "in-progress", None, Some("verifying"), None)
        .unwrap();
    fixture.set_clock(Clock::Fixed("2026-01-01T00:20:00Z".into()));
    StoryService::new(&fixture.ctx())
        .set_state(&id, "verifying", None, Some("in-progress"), None)
        .unwrap();
    fixture.set_clock(Clock::Fixed("2026-01-01T00:30:00Z".into()));

    let error = StoryService::new(&fixture.ctx())
        .set_fields(
            &id,
            &FieldEdits {
                state: Some("done".into()),
                ..FieldEdits::default()
            },
        )
        .expect_err("the old GREEN belongs to a generation that is gone");
    assert!(matches!(error, AppError::Validation(_)), "{error:?}");

    // A GREEN for the current stay satisfies the backstop with no override.
    StoryService::new(&fixture.ctx())
        .comment(
            &id,
            &format!("{VERIFICATION_GREEN_PREFIX} merge tree `new` passed `make test` and pull request {PR} landed."),
        )
        .unwrap();
    StoryService::new(&fixture.ctx())
        .set_fields(
            &id,
            &FieldEdits {
                state: Some("done".into()),
                ..FieldEdits::default()
            },
        )
        .expect("a verdict for this generation certifies the completion");
    assert_eq!(row(&fixture, &id).state, "done");
}

/// A story the verifier certified for this stay is not being overridden by
/// a hand `done`: the door lets it through with no reason and records no
/// override, the same rule the backstop applies.
#[test]
fn a_story_the_verifier_certified_completes_without_a_reason() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "green, closed by hand");
    StoryService::new(&fixture.ctx())
        .comment(
            &id,
            &format!("{VERIFICATION_GREEN_PREFIX} merge tree `abc` passed `make test` and pull request {PR} landed."),
        )
        .unwrap();

    StoryService::new(&fixture.ctx())
        .set_state(&id, "done", None, Some("verifying"), None)
        .expect("a certified story completes like any other");

    assert_eq!(row(&fixture, &id).state, "done");
    assert!(
        overrides(&fixture, &id).is_empty(),
        "nothing was overridden"
    );
}

#[test]
fn an_overridden_story_whose_pull_request_merged_is_reap_eligible() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    let id = submitted(&fixture, "merged by hand");
    StoryService::new(&fixture.ctx())
        .set_state(&id, "done", Some("merged by hand"), Some("verifying"), None)
        .unwrap();
    assert!(
        VerificationQueue::new(fixture.store())
            .next_cleanup_for(fixture.project())
            .unwrap()
            .is_none(),
        "no merged pull request, no reap: the branch may be the only copy of the work"
    );

    mark_merged(&fixture, &id);

    let candidate = VerificationQueue::new(fixture.store())
        .next_cleanup_for(fixture.project())
        .unwrap()
        .expect("an overridden story with a merged pull request is owed a reap");
    assert_eq!(candidate.story_id, id);
}
