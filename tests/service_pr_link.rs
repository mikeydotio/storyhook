//! `PrLinkService::link`/`unlink` — `story link-pr` / `story unlink-pr`
//! (SH-49).
//!
//! Deliberately unconditional (no `#![cfg(feature = "github-pr")]`):
//! linking and unlinking a pull request never talk to GitHub, so they must
//! work whether or not that feature is compiled in — see the module doc on
//! `storyhook::service::pr_link` and SH-49's council verdict.
//! `run_check`/`check`'s own tests are in `tests/service_pr_check.rs`, which
//! *is* gated, because those do talk to GitHub.

use storyhook::domain::StoryEvent;
use storyhook::error::AppError;
use storyhook::service::{NewStoryInput, PrLinkService, StoryService};
use storyhook::store::{ReadOps, Store};
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

/// Registers `https://github.com/{owner}/{repo}` as the fixture's origin —
/// `ServiceFixture::link_origin` under the hood, which is what
/// `configured_github_repos` (`PrLinkService`'s cross-repo guard) reads
/// since SH-408.
fn configure_remote(fixture: &ServiceFixture, owner: &str, repo: &str) {
    fixture.github_checkout(&format!("https://github.com/{owner}/{repo}"));
}

fn configure_remote_on(fixture: &ServiceFixture, host: &str, owner: &str, repo: &str) {
    fixture.github_checkout(&format!("https://{host}/{owner}/{repo}"));
}

const URL: &str = "https://github.com/acme/widgets/pull/7";

#[test]
fn historical_registration_does_not_authorize_a_close_on_merge_link() {
    let fixture = ServiceFixture::new();
    fixture.link_origin("https://github.com/acme/widgets");
    fixture.github_checkout("git@github.example.com:acme/current.git");
    let id = create(&fixture, "Origin moved");
    assert!(
        PrLinkService::new(&fixture.ctx())
            .link(&id, URL, true)
            .is_err()
    );
}

#[test]
fn registered_checkout_with_foreign_project_pointer_is_refused() {
    let fixture = ServiceFixture::new();
    let root = fixture.github_checkout("https://github.com/acme/widgets");
    std::fs::write(
        root.join(".storyhook.toml"),
        "schema = 1\nuuid = 'foreign'\nprefix = 'XX'\n",
    )
    .unwrap();
    let id = create(&fixture, "Foreign pointer");
    assert!(
        PrLinkService::new(&fixture.ctx())
            .link(&id, URL, true)
            .is_err()
    );
}

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
fn link_accepts_a_pr_on_the_registered_enterprise_host() {
    let fixture = ServiceFixture::new();
    configure_remote_on(&fixture, "github.example.com", "acme", "widgets");
    let id = create(&fixture, "Enterprise PR");
    let ctx = fixture.ctx();

    PrLinkService::new(&ctx)
        .link(&id, "https://github.example.com/acme/widgets/pull/7", true)
        .expect("the registered Enterprise host must be accepted");
}

#[test]
fn link_rejects_the_same_repo_name_on_a_different_host() {
    let fixture = ServiceFixture::new();
    configure_remote_on(&fixture, "github.example.com", "acme", "widgets");
    let id = create(&fixture, "Wrong GitHub host");
    let ctx = fixture.ctx();

    let error = PrLinkService::new(&ctx)
        .link(&id, "https://github.com/acme/widgets/pull/7", true)
        .expect_err("host identity must participate in the cross-repository guard");
    assert!(
        error
            .to_string()
            .contains("github.example.com/acme/widgets")
    );
}

#[test]
fn link_refuses_close_on_merge_without_a_registered_checkout() {
    // An absent authority cannot authorize a future automatic transition.
    let fixture = ServiceFixture::new();
    let id = create(&fixture, "No configured remote");
    let ctx = fixture.ctx();

    PrLinkService::new(&ctx)
        .link(&id, URL, true)
        .expect_err("close-on-merge needs a current checkout origin");
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

/// SH-408's membership design: a project may have more than one registered
/// GitHub remote (a fork-and-upstream project, or one mid-move), and a
/// `close_on_merge` link is accepted against **any** of them, not refused
/// for ambiguity — see `storyhook::service::pr_link`'s module doc and the
/// council verdict it cites.
#[test]
fn link_accepts_the_current_origin_despite_historical_registrations() {
    let fixture = ServiceFixture::new();
    configure_remote(&fixture, "acme", "widgets");
    configure_remote(&fixture, "acme", "widgets-upstream");
    let id = create(&fixture, "Second registered repo");
    let ctx = fixture.ctx();

    PrLinkService::new(&ctx)
        .link(&id, "https://github.com/acme/widgets-upstream/pull/9", true)
        .expect("a close_on_merge link matching the SECOND registered repo must be accepted");
}

/// The refusal, when it fires with several repositories registered, names
/// all of them — not just one — so the message tells the caller what would
/// have been accepted.
#[test]
fn link_rejects_a_cross_repo_url_and_names_the_current_origin() {
    let fixture = ServiceFixture::new();
    configure_remote(&fixture, "acme", "widgets");
    configure_remote(&fixture, "acme", "widgets-upstream");
    let id = create(&fixture, "Cross-repo with two registered");
    let ctx = fixture.ctx();

    let error = PrLinkService::new(&ctx)
        .link(
            &id,
            "https://github.com/someone-else/other-repo/pull/3",
            true,
        )
        .expect_err("a close_on_merge link to neither registered repo must be refused");
    match error {
        AppError::Validation(message) => {
            assert!(message.contains("acme/widgets"), "{message}");
            assert!(message.contains("acme/widgets-upstream"), "{message}");
        }
        other => panic!("expected a Validation error, got {other:?}"),
    }
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
    configure_remote(&fixture, "acme", "widgets");
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
/// `github-pr` feature. Only meaningful in a `--no-default-features`
/// build; under the default feature set it is redundant with the tests
/// above and still passes.
#[test]
fn link_and_unlink_work_without_the_github_pr_feature() {
    let fixture = ServiceFixture::new();
    configure_remote(&fixture, "acme", "widgets");
    let id = create(&fixture, "No github-pr feature needed");
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

/// A project pointer identifies the lane but cannot override checkout authority.
#[test]
fn a_project_lane_with_a_different_origin_cannot_authorize_links() {
    let fixture = ServiceFixture::new();
    let authority = fixture.github_checkout("https://github.com/acme/widgets.git");
    let pointer = std::fs::read(authority.join(".storyhook.toml")).unwrap();
    std::fs::write(fixture.cwd().join(".storyhook.toml"), pointer).unwrap();
    for args in [
        vec!["init", "--quiet"],
        vec![
            "config",
            "remote.origin.url",
            "https://github.example.com/acme/widgets.git",
        ],
    ] {
        assert!(
            storyhook::env::git_env::command(fixture.cwd())
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let id = create(&fixture, "Conflicting lane origin");
    let error = PrLinkService::new(&fixture.ctx())
        .link(&id, URL, true)
        .unwrap_err();
    assert!(error.to_string().contains("different origin"), "{error}");
}
