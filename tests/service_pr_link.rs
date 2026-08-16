//! `PrLinkService::link`/`unlink` — `story link-pr` / `story unlink-pr`
//! (SH-49).
//!
//! Deliberately unconditional (no `#![cfg(feature = "github-sync")]`):
//! linking and unlinking a pull request never talk to GitHub, so they must
//! work whether or not that feature is compiled in — see the module doc on
//! `storyhook::service::pr_link` and SH-49's council verdict.
//! `run_check`/`check`'s own tests are in `tests/service_pr_check.rs`, which
//! *is* gated, because those do talk to GitHub.

use storyhook::domain::StoryEvent;
use storyhook::error::AppError;
use storyhook::service::{NewStoryInput, PrLinkService, StoryService};
use storyhook::store::{ProjectSettings, ReadOps, Store, WriteOps};
use storyhook_test_support::ServiceFixture;

fn create(fixture: &ServiceFixture, title: &str) -> String {
    StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: title.to_string(),
            ..NewStoryInput::default()
        })
        .expect("creating a story")
        .id
}

/// Configures the fixture's project to sync with `owner/repo`, writing the
/// raw settings JSON document directly rather than through
/// `github-sync`'s (gated) `StoreSyncStorage`/`GithubSyncConfig` — this file
/// must build without that feature.
fn configure_remote(fixture: &ServiceFixture, owner: &str, repo: &str) {
    let project = fixture.project();
    fixture
        .store()
        .write(|tx| {
            tx.put_settings(
                project,
                &ProjectSettings {
                    github_sync: Some(serde_json::json!({
                        "github": { "owner": owner, "repo": repo },
                        "sync": { "mode": "manual" },
                    })),
                    ..ProjectSettings::default()
                },
            )
        })
        .expect("writing project settings");
}

const URL: &str = "https://github.com/acme/widgets/pull/7";

#[test]
fn link_happy_path_records_the_link() {
    let fixture = ServiceFixture::new();
    configure_remote(&fixture, "acme", "widgets");
    let id = create(&fixture, "Linked to a PR");
    let ctx = fixture.ctx();

    let snapshot = PrLinkService::new(&ctx)
        .link(&id, URL, true)
        .expect("linking a pull request");
    assert_eq!(snapshot.id, id);

    let project = fixture.project();
    let story_no = storyhook::store::StoryNo::parse_id("SH", &id).unwrap();
    let links = fixture
        .store()
        .read(|tx| tx.open_pr_links_for_story(project, story_no))
        .unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].owner, "acme");
    assert_eq!(links[0].repo, "widgets");
    assert_eq!(links[0].number, 7);
    assert!(links[0].close_on_merge);
}

#[test]
fn link_defaults_close_on_merge_uninvolved_when_no_remote_is_configured() {
    // No `configure_remote` call: nothing to validate a `close_on_merge: true`
    // link against, so it is accepted as given rather than refused.
    let fixture = ServiceFixture::new();
    let id = create(&fixture, "No configured remote");
    let ctx = fixture.ctx();

    PrLinkService::new(&ctx)
        .link(&id, URL, true)
        .expect("linking with no configured remote must not be refused");
}

#[test]
fn link_rejects_a_cross_repo_url_when_close_on_merge_is_true() {
    let fixture = ServiceFixture::new();
    configure_remote(&fixture, "acme", "widgets");
    let id = create(&fixture, "Cross-repo link");
    let ctx = fixture.ctx();

    let error = PrLinkService::new(&ctx)
        .link(
            &id,
            "https://github.com/someone-else/other-repo/pull/3",
            true,
        )
        .expect_err("a close_on_merge link to another repository must be refused");
    match error {
        AppError::Validation(message) => {
            assert!(
                message.contains("--no-close-on-merge"),
                "the refusal must name the way out: {message}"
            );
        }
        other => panic!("expected a Validation error, got {other:?}"),
    }
}

#[test]
fn link_allows_a_cross_repo_url_when_close_on_merge_is_false() {
    let fixture = ServiceFixture::new();
    configure_remote(&fixture, "acme", "widgets");
    let id = create(&fixture, "Cross-repo bookmark");
    let ctx = fixture.ctx();

    PrLinkService::new(&ctx)
        .link(
            &id,
            "https://github.com/someone-else/other-repo/pull/3",
            false,
        )
        .expect("a close_on_merge: false link to another repository is a legitimate bookmark");
}

#[test]
fn link_rejects_a_cross_repo_url_against_a_malformed_settings_document() {
    // A settings document missing the fields `configured_remote` needs (here,
    // `repo`) is treated the same as "nothing configured" — a no-op pass,
    // not a refusal and not an error. `refuse_cross_repo` is not the layer
    // that reports a malformed document.
    let fixture = ServiceFixture::new();
    let project = fixture.project();
    fixture
        .store()
        .write(|tx| {
            tx.put_settings(
                project,
                &ProjectSettings {
                    github_sync: Some(serde_json::json!({ "github": { "owner": "acme" } })),
                    ..ProjectSettings::default()
                },
            )
        })
        .expect("writing a malformed settings document");
    let id = create(&fixture, "Malformed settings document");
    let ctx = fixture.ctx();

    PrLinkService::new(&ctx)
        .link(&id, URL, true)
        .expect("a malformed settings document must not be treated as a refusal");
}

#[test]
fn re_linking_the_same_pr_upserts_close_on_merge() {
    let fixture = ServiceFixture::new();
    configure_remote(&fixture, "acme", "widgets");
    let id = create(&fixture, "Toggle close_on_merge");
    let ctx = fixture.ctx();
    let service = PrLinkService::new(&ctx);

    service.link(&id, URL, true).unwrap();
    service.link(&id, URL, false).unwrap();

    let project = fixture.project();
    let story_no = storyhook::store::StoryNo::parse_id("SH", &id).unwrap();
    let links = fixture
        .store()
        .read(|tx| tx.open_pr_links_for_story(project, story_no))
        .unwrap();
    assert_eq!(links.len(), 1, "re-linking upserts rather than duplicating");
    assert!(!links[0].close_on_merge);
}

#[test]
fn unlink_removes_the_row() {
    let fixture = ServiceFixture::new();
    let id = create(&fixture, "Unlink me");
    let ctx = fixture.ctx();
    let service = PrLinkService::new(&ctx);
    service.link(&id, URL, true).unwrap();

    service.unlink(&id, URL).expect("unlinking");

    let project = fixture.project();
    let story_no = storyhook::store::StoryNo::parse_id("SH", &id).unwrap();
    let links = fixture
        .store()
        .read(|tx| tx.open_pr_links_for_story(project, story_no))
        .unwrap();
    assert!(links.is_empty());
}

/// The compile-time proof that would have caught the original deviation:
/// `PrLinkService::link`/`unlink` must be present and callable without the
/// `github-sync` feature. Only meaningful in a `--no-default-features`
/// build; under the default feature set it is redundant with the tests
/// above and still passes.
#[test]
fn link_and_unlink_work_without_the_github_sync_feature() {
    let fixture = ServiceFixture::new();
    let id = create(&fixture, "No github-sync feature needed");
    let ctx = fixture.ctx();
    let service = PrLinkService::new(&ctx);

    service.link(&id, URL, true).expect("linking");
    let project = fixture.project();
    let story_no = storyhook::store::StoryNo::parse_id("SH", &id).unwrap();
    let events = fixture
        .store()
        .read(|tx| tx.events_for(project, story_no))
        .unwrap();
    assert!(
        events
            .iter()
            .filter_map(storyhook::store::StoredEvent::known)
            .any(|e| matches!(e, StoryEvent::StoryPrLinked { url, .. } if url == URL))
    );

    service.unlink(&id, URL).expect("unlinking");
}
