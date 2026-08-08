//! `run_check`/`PrLinkService::check` — `story pr-check` (SH-49).
//!
//! Gated on `github-sync`: unlike `tests/service_pr_link.rs`'s `link`/
//! `unlink` tests, everything here talks to (a fake) GitHub.

#![cfg(feature = "github-sync")]

use storyhook::domain::StoryEvent;
use storyhook::domain::secret::GithubToken;
use storyhook::github::storage::SyncStorage;
use storyhook::github::sync_state::{GithubRepo, GithubSyncConfig, SyncMode, SyncSettings};
use storyhook::service::pr_check::run_check;
use storyhook::service::{NewStoryInput, PrLinkService, StoreSyncStorage, StoryService};
use storyhook::store::{ReadOps, Store};
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

/// Configures the fixture's project to sync with `owner/repo`.
fn configure_remote(fixture: &ServiceFixture, owner: &str, repo: &str) {
    let ctx = fixture.ctx();
    StoreSyncStorage::new(&ctx)
        .save_config(&GithubSyncConfig {
            github: GithubRepo {
                owner: owner.to_string(),
                repo: repo.to_string(),
            },
            sync: SyncSettings {
                mode: SyncMode::Manual,
                last_sync_at: None,
                last_full_sync_at: None,
            },
            etags: Default::default(),
            mappings: Vec::new(),
        })
        .expect("saving github-sync config");
}

const URL: &str = "https://github.com/acme/widgets/pull/7";

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
    // control exists to close.
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
