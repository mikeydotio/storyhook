//! `run_check`/`PrLinkService::check` — `story pr-check` (SH-49).
//!
//! Gated on `github-pr`: unlike `tests/service_pr_link.rs`'s `link`/
//! `unlink` tests, everything here talks to (a fake) GitHub.

#![cfg(feature = "github-pr")]

use storyhook::domain::remote::RemoteUrl;
use storyhook::domain::secret::GithubToken;
use storyhook::domain::{COMPLETION_STATE_SLUG, StoryEvent, SuperState};
use storyhook::service::pr_check::run_check;
use storyhook::service::{
    ConfigService, NewStoryInput, PrLinkService, RelationService, StoryService,
    VERIFICATION_UNCERTIFIED_MERGE_PREFIX,
};
use storyhook::store::{ReadOps, Store, WriteOps};
use storyhook_test_support::{FakeGithubApiFactory, RecordedCall, ServiceFixture, scratch_dir};

fn token() -> GithubToken {
    GithubToken::new("ghp_fake_token_value").expect("a usable token")
}

fn create(fixture: &ServiceFixture, title: &str) -> String {
    create_typed(fixture, title, None)
}

/// [`create`], with an explicit story type.
///
/// Needed since SH-499: epic-ness is the TYPE, not the presence of children, so
/// a test about an epic has to say so rather than manufacturing one out of a
/// `parent-of` edge.
fn create_typed(fixture: &ServiceFixture, title: &str, story_type: Option<&str>) -> String {
    StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: title.to_string(),
            story_type: story_type.map(str::to_string),
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

fn configure_remote_on(fixture: &ServiceFixture, host: &str, owner: &str, repo: &str) {
    fixture.link_origin(&format!("https://{host}/{owner}/{repo}"));
}

fn write_pointer_for(root: &std::path::Path, uuid: &str, github: Option<&str>) {
    let github = github
        .map(|api_url| format!("\n[github]\napi_url = \"{api_url}\"\n"))
        .unwrap_or_default();
    std::fs::write(
        root.join(".storyhook.toml"),
        format!("schema = 1\nuuid = \"{uuid}\"\nprefix = \"SH\"\n{github}"),
    )
    .expect("writing the project pointer");
}

fn write_pointer(root: &std::path::Path, github: Option<&str>) {
    write_pointer_for(root, "fixture-uuid", github);
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

    assert!(fake.recorded_calls().contains(&RecordedCall::Build {
        api_base: "https://api.github.com".into(),
        owner: "acme".into(),
        repo: "widgets".into(),
    }));

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

/// SH-692: a pull request merged outside central verification — by hand,
/// while the story sits in `verifying` — is recorded as a fact and never
/// completes the story. Nothing certified the merge tree; completing it is
/// either the verifier's own verdict or an operator's recorded override.
#[test]
fn check_records_an_uncertified_merge_on_a_verifying_story_without_closing_it() {
    let fixture = ServiceFixture::new();
    configure_remote(&fixture, "acme", "widgets");
    let id = create(&fixture, "Merged under a running gate");
    let ctx = fixture.ctx().with_github_token(Some(token()));
    PrLinkService::new(&ctx).link(&id, URL, true).unwrap();
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();

    let fake = FakeGithubApiFactory::new();
    fake.seed_pull_request(7, "closed", true);

    let response = run_check(&ctx, &fake, Some(id.as_str())).expect("checking pull requests");
    let message = format!("{response:?}");
    assert!(
        message.contains("left verifying"),
        "the check names what it did not do: {message}"
    );

    let project = fixture.project();
    let story_no = storyhook::store::StoryNo::parse_id("SH", &id).unwrap();
    let row = fixture
        .store()
        .read(|tx| tx.story(project, story_no))
        .unwrap()
        .expect("story exists");
    assert_eq!(
        row.state, "verifying",
        "the poller never completes a verifying story"
    );
    assert!(!row.archived);
    let notices: Vec<&str> = row
        .snapshot
        .comments
        .iter()
        .filter(|comment| {
            comment
                .text
                .starts_with(VERIFICATION_UNCERTIFIED_MERGE_PREFIX)
        })
        .map(|comment| comment.text.as_str())
        .collect();
    assert_eq!(notices.len(), 1, "{:?}", row.snapshot.comments);
    assert!(notices[0].contains(URL), "{}", notices[0]);
    assert!(
        notices[0].contains(&format!("story move {id} done")),
        "the notice names the override door: {}",
        notices[0]
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
        "the merge itself is still recorded"
    );
}

/// A merged close-on-merge PR closes its story into the completion state,
/// `done` — never the abandonment state (SH-652).
///
/// Before the fix the close searched `state_map`, a `BTreeMap`, for the first
/// CLOSED state, which is the *alphabetically* first: `closed` on every default
/// catalog. Three comments claimed catalog order kept it answering `done`;
/// `check_closes_the_story_when_a_close_on_merge_link_merges` above could not
/// see the misfiling because it asserts only `archived`. This catalog is the
/// straddle — `shipped` first by position, `abandoned` first by name — so
/// both wrong searches answer wrong here, and only the named slug passes.
#[test]
fn check_closes_a_merged_story_into_done_not_the_first_closed_state() {
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
    configure_remote(&fixture, "acme", "widgets");
    let id = create(&fixture, "Merges into done");
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
    assert!(row.archived);
    assert_eq!(
        row.state, COMPLETION_STATE_SLUG,
        "a merged PR completes its story; it never abandons it"
    );
}

#[test]
fn check_routes_an_enterprise_pr_to_the_registered_remotes_api() {
    let fixture = ServiceFixture::new();
    configure_remote_on(&fixture, "github.example.com", "acme", "widgets");
    let id = create(&fixture, "Enterprise merge");
    let ctx = fixture.ctx().with_github_token(Some(token()));
    PrLinkService::new(&ctx)
        .link(&id, "https://github.example.com/acme/widgets/pull/7", true)
        .unwrap();

    let fake = FakeGithubApiFactory::new();
    fake.seed_pull_request(7, "closed", true);
    run_check(&ctx, &fake, Some(id.as_str())).expect("checking the Enterprise pull request");

    assert_eq!(
        fake.recorded_calls(),
        vec![
            RecordedCall::Build {
                api_base: "https://github.example.com/api/v3".into(),
                owner: "acme".into(),
                repo: "widgets".into(),
            },
            RecordedCall::GetPullRequest(7),
        ]
    );
}

#[test]
fn check_routes_a_ghe_com_pr_to_its_dedicated_api_host() {
    let fixture = ServiceFixture::new();
    configure_remote_on(&fixture, "octocorp.ghe.com", "acme", "widgets");
    let id = create(&fixture, "Data-resident Enterprise merge");
    let ctx = fixture.ctx().with_github_token(Some(token()));
    PrLinkService::new(&ctx)
        .link(&id, "https://octocorp.ghe.com/acme/widgets/pull/7", true)
        .unwrap();

    let fake = FakeGithubApiFactory::new();
    fake.seed_pull_request(7, "closed", true);
    run_check(&ctx, &fake, Some(id.as_str())).expect("checking the GHE.com pull request");

    assert!(fake.recorded_calls().contains(&RecordedCall::Build {
        api_base: "https://api.octocorp.ghe.com".into(),
        owner: "acme".into(),
        repo: "widgets".into(),
    }));
}

#[test]
fn check_uses_the_current_checkouts_api_override() {
    let fixture = ServiceFixture::new();
    configure_remote_on(&fixture, "github.example.com", "acme", "widgets");
    write_pointer(fixture.cwd(), Some("http://api.github.internal/custom/"));
    let id = create(&fixture, "Custom Enterprise API");
    let ctx = fixture.ctx().with_github_token(Some(token()));
    PrLinkService::new(&ctx)
        .link(&id, "https://github.example.com/acme/widgets/pull/7", true)
        .unwrap();

    let fake = FakeGithubApiFactory::new();
    fake.seed_pull_request(7, "closed", true);
    run_check(&ctx, &fake, Some(id.as_str())).expect("checking through the override");

    assert!(fake.recorded_calls().contains(&RecordedCall::Build {
        api_base: "http://api.github.internal/custom".into(),
        owner: "acme".into(),
        repo: "widgets".into(),
    }));
}

#[test]
fn check_falls_back_to_the_registered_checkout_for_unattended_calls() {
    let fixture = ServiceFixture::new();
    configure_remote_on(&fixture, "github.example.com", "acme", "widgets");
    let checkout = scratch_dir();
    write_pointer(checkout.path(), Some("https://proxy.example.test/github"));
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), Some(checkout.path())))
        .unwrap();
    let id = create(&fixture, "Unattended Enterprise API");
    let link_ctx = fixture.ctx();
    PrLinkService::new(&link_ctx)
        .link(&id, "https://github.example.com/acme/widgets/pull/7", true)
        .unwrap();
    let unattended = storyhook::service::Ctx::new(
        fixture.store(),
        fixture.project(),
        fixture.env().home(),
        fixture.env().clone(),
    )
    .with_github_token(Some(token()));

    let fake = FakeGithubApiFactory::new();
    fake.seed_pull_request(7, "closed", true);
    run_check(&unattended, &fake, Some(id.as_str())).expect("checking from the daemon context");

    assert!(fake.recorded_calls().contains(&RecordedCall::Build {
        api_base: "https://proxy.example.test/github".into(),
        owner: "acme".into(),
        repo: "widgets".into(),
    }));
}

#[test]
fn check_refuses_a_registered_checkout_that_names_another_project() {
    let fixture = ServiceFixture::new();
    configure_remote(&fixture, "acme", "widgets");
    let checkout = scratch_dir();
    write_pointer_for(
        checkout.path(),
        "another-project-uuid",
        Some("https://stale.example.test/api"),
    );
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), Some(checkout.path())))
        .unwrap();
    let unattended = storyhook::service::Ctx::new(
        fixture.store(),
        fixture.project(),
        fixture.env().home(),
        fixture.env().clone(),
    )
    .with_github_token(Some(token()));
    let fake = FakeGithubApiFactory::new();

    let error = run_check(&unattended, &fake, None)
        .expect_err("configuration from another project must be refused");

    assert!(error.to_string().contains("another-project-uuid"));
    assert!(error.to_string().contains("fixture-uuid"));
    assert!(fake.recorded_calls().is_empty());
}

#[test]
fn a_matching_current_pointer_without_github_config_suppresses_stale_fallback_config() {
    let fixture = ServiceFixture::new();
    configure_remote_on(&fixture, "github.example.com", "acme", "widgets");
    write_pointer(fixture.cwd(), None);
    let checkout = scratch_dir();
    write_pointer(checkout.path(), Some("https://stale.example.test/api"));
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), Some(checkout.path())))
        .unwrap();
    let id = create(&fixture, "Current checkout wins");
    let ctx = fixture.ctx().with_github_token(Some(token()));
    PrLinkService::new(&ctx)
        .link(&id, "https://github.example.com/acme/widgets/pull/7", true)
        .unwrap();

    let fake = FakeGithubApiFactory::new();
    fake.seed_pull_request(7, "closed", true);
    run_check(&ctx, &fake, Some(id.as_str())).unwrap();

    assert!(fake.recorded_calls().contains(&RecordedCall::Build {
        api_base: "https://github.example.com/api/v3".into(),
        owner: "acme".into(),
        repo: "widgets".into(),
    }));
}

#[test]
fn malformed_api_override_refuses_before_building_a_client() {
    let fixture = ServiceFixture::new();
    configure_remote(&fixture, "acme", "widgets");
    write_pointer(fixture.cwd(), Some("api.example.test"));
    let ctx = fixture.ctx().with_github_token(Some(token()));
    let fake = FakeGithubApiFactory::new();

    let error = run_check(&ctx, &fake, None).expect_err("a relative API URL must be refused");
    assert!(error.to_string().contains("[github].api_url"));
    assert!(fake.recorded_calls().is_empty());
}

#[test]
fn one_override_is_refused_when_matching_links_span_multiple_hosts() {
    let fixture = ServiceFixture::new();
    configure_remote_on(&fixture, "github.one.test", "acme", "one");
    configure_remote_on(&fixture, "github.two.test", "acme", "two");
    write_pointer(fixture.cwd(), Some("https://proxy.example.test/api"));
    let first = create(&fixture, "First host");
    let second = create(&fixture, "Second host");
    let ctx = fixture.ctx().with_github_token(Some(token()));
    PrLinkService::new(&ctx)
        .link(&first, "https://github.one.test/acme/one/pull/1", true)
        .unwrap();
    PrLinkService::new(&ctx)
        .link(&second, "https://github.two.test/acme/two/pull/2", true)
        .unwrap();
    let fake = FakeGithubApiFactory::new();

    let error = run_check(&ctx, &fake, None).expect_err("one override cannot route two hosts");
    assert!(error.to_string().contains("multiple GitHub hosts"));
    assert!(fake.recorded_calls().is_empty());
}

#[test]
fn check_records_an_epics_merge_without_closing_its_computed_state() {
    let fixture = ServiceFixture::new();
    configure_remote(&fixture, "acme", "widgets");
    // This fixture's project ships `bug, feature`; the type has to exist before
    // a story can carry it. SH-499's rule in miniature: a project that does not
    // define `epic` has no epics.
    ConfigService::new(&fixture.ctx())
        .add_type("epic", None, None)
        .expect("adding the epic type");
    let epic_id = create_typed(&fixture, "Computed epic", Some("epic"));
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
