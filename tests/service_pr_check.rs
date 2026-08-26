//! `run_check`/`PrLinkService::check` — `story pr-check` (SH-49).
//!
//! Gated on `github-pr`: unlike `tests/service_pr_link.rs`'s `link`/
//! `unlink` tests, everything here talks to (a fake) GitHub.

#![cfg(feature = "github-pr")]

use storyhook::domain::StoryEvent;
use storyhook::domain::remote::RemoteUrl;
use storyhook::domain::secret::GithubToken;
use storyhook::service::pr_check::run_check;
use storyhook::service::{NewStoryInput, PrLinkService, RelationService, StoryService};
use storyhook::store::{ReadOps, Store, WriteOps};
use storyhook_test_support::{FakeGithubApiFactory, ServiceFixture};

fn token() -> GithubToken {
    GithubToken::new("ghp_fake_token_value").expect("a usable token")
}

fn create(fixture: &ServiceFixture, title: &str) -> String {
    StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: title.to_string(),
            ..NewStoryInput::default()
        })
        .expect("creating a story")
        .id
}

/// Registers `https://github.com/{owner}/{repo}` as one of the fixture's
/// project's origins — `ServiceFixture::link_origin` under the hood.
fn configure_remote(fixture: &ServiceFixture, owner: &str, repo: &str) {
    fixture.link_origin(&format!("https://github.com/{owner}/{repo}"));
}

/// Unregisters a previously-configured origin — the store-level counterpart
/// to [`configure_remote`], for a test that repoints a project's remote
/// rather than adding a second one alongside it.
fn unconfigure_remote(fixture: &ServiceFixture, owner: &str, repo: &str) {
    let project = fixture.project();
    let remote = RemoteUrl::normalize(&format!("https://github.com/{owner}/{repo}"))
        .expect("a well-formed remote url");
    fixture
        .store()
        .write(|tx| tx.unlink_remote(project, &remote))
        .expect("unregistering an origin");
}

const URL: &str = "https://github.com/acme/widgets/pull/7";

/// No registered GitHub remote at all is a refusal naming the fix, not a
/// silent no-op and not "checked 0" — see the module doc on
/// `storyhook::service::pr_check`.
#[test]
fn check_refuses_when_the_project_has_no_registered_github_remote() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx().with_github_token(Some(token()));
    let fake = FakeGithubApiFactory::new();

    let error = run_check(&ctx, &fake, None).expect_err("no registered remote must be a refusal");
    let message = error.to_string();
    assert!(
        message.contains("story project link origin"),
        "the refusal must name the fix: {message}"
    );
}

#[test]
fn check_closes_the_story_when_a_close_on_merge_link_merges() {
    let fixture = ServiceFixture::new();
    configure_remote(&fixture, "acme", "widgets");
    let id = create(&fixture, "Merges and closes");
    let ctx = fixture.ctx().with_github_token(Some(token()));
    PrLinkService::new(&ctx).link(&id, URL, true).unwrap();

    let fake = FakeGithubApiFactory::new();
    fake.seed_pull_request(7, "closed", true);

    run_check(&ctx, &fake, Some(id.as_str())).expect("checking pull requests");

    let project = fixture.project();
    let story_no = storyhook::store::StoryNo::parse_id("SH", &id).unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(project, story_no))
        .unwrap()
        .expect("story exists");
    assert!(
        row.archived,
        "the story must close when its merged PR asked to"
    );

    let events = fixture
        .store()
        .read(|tx| tx.events_for(project, story_no))
        .unwrap();
    assert!(
        events
            .iter()
            .filter_map(storyhook::store::StoredEvent::known)
            .any(|e| matches!(e, StoryEvent::StoryPrMerged { url, .. } if url == URL)),
        "a StoryPrMerged event must have been appended"
    );
}

#[test]
fn check_records_an_epics_merge_without_closing_its_computed_state() {
    let fixture = ServiceFixture::new();
    configure_remote(&fixture, "acme", "widgets");
    let epic_id = create(&fixture, "Computed epic");
    let child_id = create(&fixture, "Actionable child");
    let ctx = fixture.ctx().with_github_token(Some(token()));
    RelationService::new(&ctx)
        .relate(&epic_id, "parent-of", &child_id, false)
        .expect("making the story structural");
    PrLinkService::new(&ctx).link(&epic_id, URL, true).unwrap();

    let fake = FakeGithubApiFactory::new();
    fake.seed_pull_request(7, "closed", true);

    run_check(&ctx, &fake, Some(epic_id.as_str())).expect("checking the epic's pull request");

    let project = fixture.project();
    let story_no = storyhook::store::StoryNo::parse_id("SH", &epic_id).unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(project, story_no))
        .unwrap()
        .expect("epic exists");
    assert!(!row.archived, "a merged PR must not directly close an epic");
    assert!(
        row.snapshot.state_computed,
        "the epic must retain computed-state authority"
    );

    let events = fixture
        .store()
        .read(|tx| tx.events_for(project, story_no))
        .unwrap();
    let known: Vec<_> = events
        .iter()
        .filter_map(storyhook::store::StoredEvent::known)
        .collect();
    assert!(
        known
            .iter()
            .any(|event| matches!(event, StoryEvent::StoryPrMerged { url, .. } if url == URL)),
        "the external merge observation must still be recorded"
    );
    assert!(
        !known.iter().any(|event| matches!(
            event,
            StoryEvent::StoryStateChanged { .. } | StoryEvent::StoryClosedAndArchived { .. }
        )),
        "the observation must not append a direct state transition"
    );
}

#[test]
fn check_leaves_the_story_open_when_close_on_merge_is_false() {
    let fixture = ServiceFixture::new();
    configure_remote(&fixture, "acme", "widgets");
    let id = create(&fixture, "Merges but stays open");
    let ctx = fixture.ctx().with_github_token(Some(token()));
    PrLinkService::new(&ctx).link(&id, URL, false).unwrap();

    let fake = FakeGithubApiFactory::new();
    fake.seed_pull_request(7, "closed", true);

    run_check(&ctx, &fake, Some(id.as_str())).expect("checking pull requests");

    let project = fixture.project();
    let story_no = storyhook::store::StoryNo::parse_id("SH", &id).unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(project, story_no))
        .unwrap()
        .expect("story exists");
    assert!(
        !row.archived,
        "close_on_merge: false must never close the story, merged or not"
    );
}

#[test]
fn check_records_pr_closed_not_merged_for_a_pr_closed_without_merging() {
    let fixture = ServiceFixture::new();
    configure_remote(&fixture, "acme", "widgets");
    let id = create(&fixture, "Closed without merging");
    let ctx = fixture.ctx().with_github_token(Some(token()));
    PrLinkService::new(&ctx).link(&id, URL, true).unwrap();

    let fake = FakeGithubApiFactory::new();
    fake.seed_pull_request(7, "closed", false);

    run_check(&ctx, &fake, Some(id.as_str())).expect("checking pull requests");

    let project = fixture.project();
    let story_no = storyhook::store::StoryNo::parse_id("SH", &id).unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(project, story_no))
        .unwrap()
        .expect("story exists");
    assert!(
        !row.archived,
        "a PR closed without merging must never close the story"
    );

    let status: String = rusqlite::Connection::open(fixture.store().path())
        .unwrap()
        .query_row("SELECT status FROM story_pr_links", [], |r| r.get(0))
        .unwrap();
    assert_eq!(status, "closed");
}

#[test]
fn check_skips_a_link_whose_repository_no_longer_matches_the_configured_remote() {
    let fixture = ServiceFixture::new();
    configure_remote(&fixture, "acme", "widgets");
    let id = create(&fixture, "Repo repointed after linking");
    let ctx = fixture.ctx().with_github_token(Some(token()));
    PrLinkService::new(&ctx).link(&id, URL, true).unwrap();

    // The project's remote is repointed to a different repository between
    // link time and check time — the TOCTOU window the second security
    // control exists to close. Unregistering the old one is what makes this
    // a repoint rather than an addition (SH-408: a project may legitimately
    // hold more than one registered remote, so leaving the old one in place
    // would still match the link).
    unconfigure_remote(&fixture, "acme", "widgets");
    configure_remote(&fixture, "acme", "some-other-repo");

    let fake = FakeGithubApiFactory::new();
    fake.seed_pull_request(7, "closed", true);

    run_check(&ctx, &fake, Some(id.as_str())).expect("checking pull requests");

    assert!(
        fake.recorded_calls().is_empty(),
        "a mismatched link must never reach the GitHub client at all"
    );

    let project = fixture.project();
    let story_no = storyhook::store::StoryNo::parse_id("SH", &id).unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(project, story_no))
        .unwrap()
        .expect("story exists");
    assert!(
        !row.archived,
        "a link skipped for repository mismatch must not close the story"
    );
    let links = fixture
        .store()
        .read(|tx| tx.open_pr_links_for_story(project, story_no))
        .unwrap();
    assert_eq!(
        links.len(),
        1,
        "the link itself is untouched — skipped, not deleted or modified"
    );
    assert_eq!(links[0].status, "open");
}

/// SH-408's membership design: a project with two registered GitHub remotes
/// checks links against BOTH — never resolves to a single one and refuses
/// the rest. See `storyhook::service::pr_link`'s module doc and the council
/// verdict it cites.
#[test]
fn check_closes_stories_across_two_registered_repositories() {
    let fixture = ServiceFixture::new();
    configure_remote(&fixture, "acme", "widgets");
    configure_remote(&fixture, "acme", "widgets-upstream");
    let first_id = create(&fixture, "First registered repo");
    let second_id = create(&fixture, "Second registered repo");
    let ctx = fixture.ctx().with_github_token(Some(token()));
    PrLinkService::new(&ctx).link(&first_id, URL, true).unwrap();
    PrLinkService::new(&ctx)
        .link(
            &second_id,
            "https://github.com/acme/widgets-upstream/pull/9",
            true,
        )
        .unwrap();

    let fake = FakeGithubApiFactory::new();
    fake.seed_pull_request(7, "closed", true);
    fake.seed_pull_request(9, "closed", true);

    run_check(&ctx, &fake, None).expect("checking pull requests across both repositories");

    let project = fixture.project();
    for id in [&first_id, &second_id] {
        let story_no = storyhook::store::StoryNo::parse_id("SH", id).unwrap();
        let row = fixture
            .store()
            .read(|tx| tx.story(project, story_no))
            .unwrap()
            .expect("story exists");
        assert!(row.archived, "{id} must close: its repo is registered");
    }
}

/// A GitHub API failure against one registered repository's links must not
/// prevent another registered repository's links from being checked in the
/// same call — and the failure must surface as a real error, never folded
/// into a "successful" message at exit 0 (the same doctrine SH-159
/// established for the sync engine this file survived).
#[test]
fn check_isolates_one_repositorys_failure_from_another_repositorys_links() {
    let fixture = ServiceFixture::new();
    configure_remote(&fixture, "acme", "widgets");
    configure_remote(&fixture, "acme", "widgets-upstream");
    let healthy_id = create(&fixture, "Healthy repo");
    let failing_id = create(&fixture, "Failing repo");
    let ctx = fixture.ctx().with_github_token(Some(token()));
    PrLinkService::new(&ctx)
        .link(&healthy_id, URL, true)
        .unwrap();
    PrLinkService::new(&ctx)
        .link(
            &failing_id,
            "https://github.com/acme/widgets-upstream/pull/9",
            true,
        )
        .unwrap();

    let fake = FakeGithubApiFactory::new();
    // PR #7 (widgets) is seeded and merges; PR #9 (widgets-upstream) is
    // never seeded, so the fake answers it with a 404 — the failure to
    // isolate.
    fake.seed_pull_request(7, "closed", true);

    let error = run_check(&ctx, &fake, None)
        .expect_err("a per-link failure must turn the call into an error");
    let message = error.to_string();
    assert!(
        message.contains("widgets-upstream"),
        "the error must name what failed: {message}"
    );

    let project = fixture.project();
    let healthy_no = storyhook::store::StoryNo::parse_id("SH", &healthy_id).unwrap();
    let healthy_row = fixture
        .store()
        .read(|tx| tx.story(project, healthy_no))
        .unwrap()
        .expect("story exists");
    assert!(
        healthy_row.archived,
        "the healthy repository's link must still close its story, despite the other \
         repository's failure"
    );

    let failing_no = storyhook::store::StoryNo::parse_id("SH", &failing_id).unwrap();
    let failing_row = fixture
        .store()
        .read(|tx| tx.story(project, failing_no))
        .unwrap()
        .expect("story exists");
    assert!(
        !failing_row.archived,
        "the failing repository's own story must not close"
    );
}
