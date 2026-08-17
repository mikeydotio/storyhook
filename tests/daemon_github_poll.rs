//! `daemon::github_poll::tick` — the daemon's unattended GitHub poll (SH-212).
//!
//! Gated on `github-sync`, the same as `tests/service_pr_check.rs`, whose
//! `check_closes_the_story_when_a_close_on_merge_link_merges` scenario the
//! third test here mirrors — driven through `tick` instead of `run_check`
//! directly, to prove the credential-read-and-iterate wiring between them,
//! not to re-prove `run_check`'s own merge-detection logic.

#![cfg(feature = "github-sync")]

use std::sync::Arc;

use keyring_core::CredentialStore;
use storyhook::daemon::github_poll::tick;
use storyhook::domain::remote::RemoteUrl;
use storyhook::domain::secret::GithubToken;
use storyhook::github::credential_store;
use storyhook::service::{NewStoryInput, PrLinkService, StoryService};
use storyhook::store::{NewProject, ReadOps, Store, StoryNo, WriteOps};
use storyhook_test_support::{FakeGithubApiFactory, ServiceFixture, default_states, default_types};

/// A fresh mock keychain — isolated per test, unlike
/// `keyring_core::set_default_store`'s process-wide static, which would make
/// every test in this binary share one credential store.
fn mock_store() -> Arc<CredentialStore> {
    keyring_core::mock::Store::new().expect("a mock store never fails to build")
}

fn token() -> GithubToken {
    GithubToken::new("ghp_fake_token_value").expect("a usable token")
}

/// `tick` with nothing stored does nothing — no panic, no project read that
/// could fail loudly for a feature nobody has opted into.
#[test]
fn a_tick_with_no_stored_credential_does_nothing() {
    let credential_store = mock_store();
    let fixture = ServiceFixture::new();
    let factory = FakeGithubApiFactory::new();

    tick(fixture.store(), fixture.env(), &credential_store, &factory);

    assert!(
        factory.recorded_calls().is_empty(),
        "no stored credential means no GitHub call"
    );
}

/// A project with no GitHub remote registered is skipped, not treated as an
/// error that could abort the tick for a sibling project — the same
/// per-project isolation `run_check` itself already guarantees, exercised
/// here through the poll path.
#[test]
fn a_project_with_no_github_remote_is_skipped_quietly() {
    let credential_store = mock_store();
    let fixture = ServiceFixture::new();
    let account = fixture.env().store().key();
    credential_store::login(&credential_store, &account, &token()).unwrap();

    let factory = FakeGithubApiFactory::new();
    tick(fixture.store(), fixture.env(), &credential_store, &factory);

    assert!(
        factory.recorded_calls().is_empty(),
        "a project with no configured remote must never reach GitHub"
    );
}

/// End to end through the poll path: a stored credential, a configured
/// remote, a close-on-merge link, and a merged pull request.
#[test]
fn a_tick_closes_a_story_whose_linked_pr_merged() {
    let credential_store = mock_store();
    let fixture = ServiceFixture::new();
    let account = fixture.env().store().key();
    credential_store::login(&credential_store, &account, &token()).unwrap();

    let ctx = fixture.ctx();
    fixture.link_origin("https://github.com/acme/widgets");

    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Merges via the poll".to_string(),
            ..NewStoryInput::default()
        })
        .expect("creating a story")
        .id;
    PrLinkService::new(&ctx)
        .link(&id, "https://github.com/acme/widgets/pull/7", true)
        .expect("linking never spends a credential");

    let factory = FakeGithubApiFactory::new();
    factory.seed_pull_request(7, "closed", true);

    tick(fixture.store(), fixture.env(), &credential_store, &factory);

    let project = fixture.project();
    let story_no = StoryNo::parse_id("SH", &id).unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(project, story_no))
        .unwrap()
        .expect("story exists");
    assert!(
        row.archived,
        "the poll must close the story its merged, close-on-merge PR asked to"
    );
}

/// The credential a poll tick spends is scoped to the *store*
/// (`StoreLocation::key()`), not to a single project — a second project in
/// the same store, with its own configured remote and its own linked PR,
/// gets checked in the same tick from the one stored token, alongside the
/// fixture's own default project.
#[test]
fn one_stored_credential_covers_every_project_in_the_store() {
    let credential_store = mock_store();
    let fixture = ServiceFixture::new();
    let account = fixture.env().store().key();
    credential_store::login(&credential_store, &account, &token()).unwrap();

    let second_project = fixture
        .store()
        .write(|tx| {
            let project = tx.create_project(&NewProject {
                uuid: "second-project-uuid".into(),
                slug: "second".into(),
                name: "second".into(),
                prefix: "SEC".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            })?;
            tx.put_states(project, &default_states())?;
            tx.put_types(project, &default_types())?;
            Ok(project)
        })
        .expect("seeding a second project in the same store");
    let second_ctx = storyhook::service::Ctx::new(
        fixture.store(),
        second_project,
        fixture.cwd(),
        fixture.env().clone(),
    );
    fixture
        .store()
        .write(|tx| {
            tx.link_remote(
                second_project,
                &RemoteUrl::normalize("https://github.com/other-org/other-repo")
                    .expect("a well-formed remote url"),
                "2026-01-01T00:00:00Z",
            )
        })
        .expect("registering the second project's origin");
    let second_id = StoryService::new(&second_ctx)
        .create(&NewStoryInput {
            title: "In the second project".to_string(),
            ..NewStoryInput::default()
        })
        .expect("creating a story in the second project")
        .id;
    PrLinkService::new(&second_ctx)
        .link(
            &second_id,
            "https://github.com/other-org/other-repo/pull/3",
            true,
        )
        .expect("linking never spends a credential");

    let factory = FakeGithubApiFactory::new();
    factory.seed_pull_request(3, "closed", true);

    tick(fixture.store(), fixture.env(), &credential_store, &factory);

    let story_no = StoryNo::parse_id("SEC", &second_id).unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(second_project, story_no))
        .unwrap()
        .expect("story exists");
    assert!(
        row.archived,
        "the single store-scoped credential must reach a second project's linked PRs too"
    );
}
