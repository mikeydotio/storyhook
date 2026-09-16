//! Project-isolated polling consent and lifecycle behavior through the production tick.
#![cfg(feature = "github-pr")]
use storyhook::daemon::github_poll::tick;
use storyhook::service::{NewStoryInput, PrLinkService, StoryService};
use storyhook::store::{ProjectId, ReadOps, Store, StoryNo};
use storyhook_test_support::{FakeGithubApiFactory, RecordedCall, ServiceFixture};

fn seed(
    fixture: &ServiceFixture,
    project: ProjectId,
    host: &str,
    number: u64,
    poll: bool,
) -> String {
    let root = fixture.github_checkout_for(project, &format!("https://{host}/acme/widgets"));
    if poll {
        std::fs::OpenOptions::new()
            .append(true)
            .open(root.join(".storyhook.toml"))
            .map(|mut file| {
                use std::io::Write;
                writeln!(file, "\n[github]\npoll = true").unwrap();
            })
            .unwrap();
    }
    let ctx = fixture.ctx_for(project);
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Observed PR".into(),
            ..Default::default()
        })
        .unwrap()
        .id;
    PrLinkService::new(&ctx)
        .link(
            &id,
            &format!("https://{host}/acme/widgets/pull/{number}"),
            true,
        )
        .unwrap();
    id
}

fn closed(fixture: &ServiceFixture, project: ProjectId, id: &str) -> bool {
    let prefix = fixture
        .store()
        .read(|tx| tx.project(project))
        .unwrap()
        .unwrap()
        .prefix;
    let number = StoryNo::parse_id(&prefix, id).unwrap();
    fixture
        .store()
        .read(|tx| tx.story(project, number))
        .unwrap()
        .unwrap()
        .archived
}

#[test]
fn polling_defaults_off_and_reloads_explicit_consent() {
    let fixture = ServiceFixture::new();
    let id = seed(
        &fixture,
        fixture.project(),
        "github.example.com",
        7,
        false,
    );
    let fake = FakeGithubApiFactory::new();
    fake.seed_pull_request(7, "closed", true);
    tick(fixture.store(), fixture.env(), &fake);
    assert!(fake.recorded_calls().is_empty());
    assert!(!closed(&fixture, fixture.project(), &id));
    let root = fixture
        .store()
        .read(|tx| tx.checkout_path(fixture.project()))
        .unwrap()
        .unwrap();
    use std::io::Write;
    writeln!(
        std::fs::OpenOptions::new()
            .append(true)
            .open(root.join(".storyhook.toml"))
            .unwrap(),
        "\n[github]\npoll = true"
    )
    .unwrap();
    tick(fixture.store(), fixture.env(), &fake);
    assert!(closed(&fixture, fixture.project(), &id));
}

#[test]
fn different_hosts_are_isolated_and_one_failure_does_not_starve_another_project() {
    let fixture = ServiceFixture::new();
    let first = seed(&fixture, fixture.project(), "github.com", 7, true);
    let other = fixture.add_project("enterprise", "ENT");
    let second = seed(&fixture, other, "github.example.com", 9, true);
    let fake = FakeGithubApiFactory::new();
    fake.seed_pull_request(9, "closed", true);
    tick(fixture.store(), fixture.env(), &fake);
    assert!(!closed(&fixture, fixture.project(), &first));
    assert!(closed(&fixture, other, &second));
    for host in ["github.com", "github.example.com"] {
        assert!(fake.recorded_calls().contains(&RecordedCall::Build {
            host: host.into(),
            owner: "acme".into(),
            repo: "widgets".into()
        }));
    }
}

#[test]
fn projects_without_links_do_not_need_github_configuration() {
    let fixture = ServiceFixture::new();
    let fake = FakeGithubApiFactory::new();
    tick(fixture.store(), fixture.env(), &fake);
    assert!(fake.recorded_calls().is_empty());
}
