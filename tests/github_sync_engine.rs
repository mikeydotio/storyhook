//! The sync engine's orchestration, exercised in-process against
//! `FakeGithubApiFactory` — no network, no `story` subprocess.
//!
//! Before SH-158, `GithubClient` was a concrete `ureq` struct with no seam a
//! test could stand in front of, so `run_sync_with` and `run_initial_setup`
//! were called only through the real CLI (`tests/github_sync_setup.rs`,
//! `tests/github_sync_conflicts.rs`), which can drive only the refusals that
//! fire before any network call. Everything past that point — the pull
//! phase, the push phase, initial-setup wiring, and a conflict actually
//! reaching `SyncReport` — had zero coverage. This file is what closes that
//! gap, calling `run_initial_setup`/`run_sync_with` directly the way
//! `tests/service_github.rs` calls `StoreSyncStorage` directly.
//!
//! Design: `.council/sh158-githubapi-trait-seam/DECISION.md`. Notably: no
//! per-call error-injection mechanism exists on the fake, by decision —
//! [`an_error_syncing_one_story_does_not_abort_the_rest_of_the_sync`] below
//! provokes a real error (a mapping pointing at an issue number the fake
//! never seeded, as if deleted on GitHub between syncs) with nothing but
//! ordinary seeding, which is what made the toggle unnecessary.

#![cfg(feature = "github-sync")]

use std::process::Command;

use storyhook::domain::StoryEvent;
use storyhook::domain::secret::GithubToken;
use storyhook::error::AppError;
use storyhook::github::initial::{
    InitialSetupOutcome, InitialStrategy, SetupAnswers, run_initial_setup,
};
use storyhook::github::run_sync_with;
use storyhook::github::storage::SyncStorage;
use storyhook::github::sync_state::{StoryIssueMapping, SyncMode};
use storyhook::output::Response;
use storyhook::service::{NewStoryInput, StoreSyncStorage, StoryService};
use storyhook_test_support::{FakeGithubApiFactory, RecordedCall, ServiceFixture};

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

/// Gives the fixture's checkout a `git remote origin` GitHub URL —
/// `run_initial_setup` refuses without one, and nothing else in
/// `ServiceFixture` gives its `cwd` a git repository at all.
fn add_github_remote(fixture: &ServiceFixture) {
    let cwd = fixture.cwd();
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("running git");
        assert!(status.success(), "`git {}` failed", args.join(" "));
    };
    run(&["init", "--quiet"]);
    run(&[
        "remote",
        "add",
        "origin",
        "https://github.com/acme/widgets.git",
    ]);
}

// ---------------------------------------------------------------------------
// SH-153's council acceptance criteria — the wiring gap SH-158 closes
// ---------------------------------------------------------------------------

#[test]
fn run_initial_setup_returns_a_plan_for_an_unanswered_setup() {
    let fixture = ServiceFixture::new();
    add_github_remote(&fixture);
    create(&fixture, "Local only");
    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);
    let fake = FakeGithubApiFactory::new();
    fake.seed_issue("Local only");
    fake.seed_issue("Remote only");

    let outcome =
        run_initial_setup(&storage, &fake, Some(&token()), None).expect("computing the plan");

    let InitialSetupOutcome::Plan(plan) = outcome else {
        panic!("an unanswered setup must return a plan, not proceed");
    };
    assert_eq!(plan.owner, "acme");
    assert_eq!(plan.repo, "widgets");
    assert_eq!(plan.local_story_count, 1);
    assert_eq!(plan.open_issue_count, 2);
    assert_eq!(plan.exact_match_count, 1, "\"Local only\" matches by title");
}

#[test]
fn an_unanswered_setup_answers_with_setup_required_over_run_sync_with() {
    let fixture = ServiceFixture::new();
    add_github_remote(&fixture);
    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);
    let fake = FakeGithubApiFactory::new();

    let response = run_sync_with(
        &storage,
        &fake,
        Some(&token()),
        None,
        false,
        None,
        None,
        None,
    )
    .expect("an unanswered setup is a plan, not an error");

    assert!(
        matches!(response, Response::SetupRequired(_)),
        "{response:?}"
    );
    assert!(
        storage.load_config().expect("loading").is_none(),
        "a plan must not write configuration"
    );
}

#[test]
fn stated_answers_write_the_configuration() {
    let fixture = ServiceFixture::new();
    add_github_remote(&fixture);
    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);
    let fake = FakeGithubApiFactory::new();

    assert!(storage.load_config().expect("loading").is_none());

    run_sync_with(
        &storage,
        &fake,
        Some(&token()),
        None,
        false,
        None,
        Some(InitialStrategy::PushOnly),
        Some(SyncMode::Manual),
    )
    .expect("a stated setup proceeds");

    let config = storage
        .load_config()
        .expect("loading")
        .expect("configuration was saved");
    assert_eq!(config.github.owner, "acme");
    assert_eq!(config.github.repo, "widgets");
    assert_eq!(config.sync.mode, SyncMode::Manual);
    assert!(
        config.mappings.is_empty(),
        "push-only with nothing local creates no mappings"
    );
}

#[test]
fn dry_run_writes_no_configuration_on_first_setup() {
    let fixture = ServiceFixture::new();
    add_github_remote(&fixture);
    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);
    let fake = FakeGithubApiFactory::new();

    run_sync_with(
        &storage,
        &fake,
        Some(&token()),
        None,
        true, // dry_run
        None,
        Some(InitialStrategy::PushOnly),
        Some(SyncMode::Manual),
    )
    .expect("a dry run still proceeds");

    assert!(
        storage.load_config().expect("loading").is_none(),
        "a dry run must not write configuration to an unconfigured project"
    );
}

#[test]
fn unique_title_pairs_are_linked_end_to_end() {
    let fixture = ServiceFixture::new();
    add_github_remote(&fixture);
    let matched_id = create(&fixture, "Fix the parser");
    let unrelated_id = create(&fixture, "Totally unrelated");
    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);
    let fake = FakeGithubApiFactory::new();
    let matched_issue = fake.seed_issue("Fix the parser");
    fake.seed_issue("Nothing like it locally");

    let outcome = run_initial_setup(
        &storage,
        &fake,
        Some(&token()),
        Some(SetupAnswers {
            strategy: InitialStrategy::MatchTitles,
            mode: SyncMode::Manual,
        }),
    )
    .expect("computing the match");

    let InitialSetupOutcome::Configured { config, notes } = outcome else {
        panic!("stated answers must proceed, not return a plan");
    };
    assert!(
        notes.is_empty(),
        "no ambiguous pairs were seeded: {notes:?}"
    );
    assert_eq!(config.mappings.len(), 1, "{:?}", config.mappings);
    assert_eq!(config.mappings[0].story_id, matched_id);
    assert_eq!(config.mappings[0].issue_number, matched_issue.number);
    assert!(
        config.mappings.iter().all(|m| m.story_id != unrelated_id),
        "the unrelated story/issue must not be linked: {:?}",
        config.mappings
    );
}

// ---------------------------------------------------------------------------
// Orchestration — the pull phase, the push phase, and error accumulation
// ---------------------------------------------------------------------------

#[test]
fn pull_phase_creates_a_local_story_from_an_unmapped_issue() {
    let fixture = ServiceFixture::new();
    add_github_remote(&fixture);
    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);
    let fake = FakeGithubApiFactory::new();

    run_sync_with(
        &storage,
        &fake,
        Some(&token()),
        None,
        false,
        None,
        Some(InitialStrategy::PushOnly),
        Some(SyncMode::Manual),
    )
    .expect("initial setup");

    let issue = fake.seed_issue("Reported on GitHub");

    let response = run_sync_with(
        &storage,
        &fake,
        Some(&token()),
        None,
        false,
        None,
        None,
        None,
    )
    .expect("syncing");
    assert!(matches!(response, Response::Message(_)), "{response:?}");

    let stories = storage.open_stories().expect("open stories");
    assert_eq!(stories.len(), 1, "{stories:?}");
    assert_eq!(stories[0].title, "Reported on GitHub");

    let config = storage.load_config().expect("loading").expect("configured");
    assert_eq!(config.mappings.len(), 1);
    assert_eq!(config.mappings[0].issue_number, issue.number);
}

#[test]
fn push_phase_creates_a_github_issue_from_an_unmapped_local_story() {
    let fixture = ServiceFixture::new();
    add_github_remote(&fixture);
    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);
    let fake = FakeGithubApiFactory::new();

    run_sync_with(
        &storage,
        &fake,
        Some(&token()),
        None,
        false,
        None,
        Some(InitialStrategy::PushOnly),
        Some(SyncMode::Manual),
    )
    .expect("initial setup");

    let story_id = create(&fixture, "Never synced yet");

    run_sync_with(
        &storage,
        &fake,
        Some(&token()),
        None,
        false,
        None,
        None,
        None,
    )
    .expect("syncing");

    let created = fake.created_issues();
    assert_eq!(created.len(), 1, "{created:?}");
    assert_eq!(created[0].title, "Never synced yet");

    let config = storage.load_config().expect("loading").expect("configured");
    assert_eq!(config.mappings.len(), 1);
    assert_eq!(config.mappings[0].story_id, story_id);
}

#[test]
fn a_conflict_between_local_and_remote_is_reported_as_sync_conflict() {
    let fixture = ServiceFixture::new();
    add_github_remote(&fixture);
    let story_id = create(&fixture, "Fix the bug");
    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);
    let fake = FakeGithubApiFactory::new();
    let issue = fake.seed_issue("Fix the bug");

    // Link them, then capture a base snapshot at "Fix the bug" on both sides
    // -- the shared ancestor the next edits diverge from.
    let outcome = run_initial_setup(
        &storage,
        &fake,
        Some(&token()),
        Some(SetupAnswers {
            strategy: InitialStrategy::MatchTitles,
            mode: SyncMode::Manual,
        }),
    )
    .expect("linking");
    let InitialSetupOutcome::Configured { config, .. } = outcome else {
        panic!("stated answers must proceed");
    };
    storage.save_config(&config).expect("saving the link");
    let base = storage.story(&story_id).expect("reading");
    storage
        .save_base(&story_id, &base)
        .expect("saving the base");

    // Diverge both sides from that base, on the same field, to different
    // values -- a genuine conflict, not a one-sided update.
    storage
        .write_events(
            &story_id,
            &[StoryEvent::StoryTitleSet {
                at: "2026-01-02T00:00:00Z".to_string(),
                title: "Fix the bug (local edit)".to_string(),
            }],
        )
        .expect("editing locally");
    fake.set_issue_title(issue.number, "Fix the bug (remote edit)");

    let error = run_sync_with(
        &storage,
        &fake,
        Some(&token()),
        Some(&story_id),
        false,
        None,
        None,
        None,
    )
    .expect_err("a conflict must not answer with a success");

    let AppError::SyncConflict(detail) = error else {
        panic!("a merge conflict must answer with SyncConflict: {error}");
    };
    assert!(detail.contains(&story_id), "{detail}");
    assert!(detail.contains("title"), "{detail}");
    assert_eq!(
        AppError::SyncConflict(String::new()).exit_code(),
        8,
        "the exit code contract is pinned in tests/error_contract.rs; this test only \
         needs SyncConflict to be reachable, which it now is, in-process"
    );
}

#[test]
fn an_error_syncing_one_story_does_not_abort_the_rest_of_the_sync() {
    let fixture = ServiceFixture::new();
    add_github_remote(&fixture);
    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);
    let fake = FakeGithubApiFactory::new();

    run_sync_with(
        &storage,
        &fake,
        Some(&token()),
        None,
        false,
        None,
        Some(InitialStrategy::PushOnly),
        Some(SyncMode::Manual),
    )
    .expect("initial setup");

    let broken_id = create(&fixture, "Points at a vanished issue");
    create(&fixture, "Never synced yet");

    // As if the mapped issue were deleted on GitHub between syncs: a mapping
    // to an issue number the fake never seeded. No error-injection mechanism
    // needed -- an absent issue is a real, ordinary NotFound.
    let mut config = storage.load_config().expect("loading").expect("configured");
    config.mappings.push(StoryIssueMapping {
        story_id: broken_id.clone(),
        issue_number: 999,
        last_synced_at: "2026-01-01T00:00:00Z".to_string(),
        last_local_event_index: None,
    });
    storage
        .save_config(&config)
        .expect("seeding the broken mapping");

    let error = run_sync_with(
        &storage,
        &fake,
        Some(&token()),
        None,
        false,
        None,
        None,
        None,
    )
    .expect_err(
        "one story's error must still fail the run's exit code (SH-159), even though \
         processing continues past it -- that's the point of the next two assertions",
    );

    let AppError::SyncErrors(detail) = error else {
        panic!("a per-story sync error must answer with SyncErrors: {error}");
    };
    assert!(detail.contains(&broken_id), "{detail}");
    assert!(detail.contains("Errors"), "{detail}");

    // Processing the rest of the batch is unaffected by the error becoming
    // fatal to the *exit code* -- the healthy story still pushed, proven
    // against the fake's own record rather than the returned `Result`.
    let created = fake.created_issues();
    assert_eq!(
        created.len(),
        1,
        "the healthy story still pushes: {created:?}"
    );
    assert_eq!(created[0].title, "Never synced yet");
}

// ---------------------------------------------------------------------------
// SH-179 — a push must not rename a comma-bearing remote label
// ---------------------------------------------------------------------------

#[test]
fn pushing_a_genuinely_new_label_leaves_an_existing_comma_bearing_one_alone() {
    // SH-164 renders a GitHub label like "backend,urgent" as the storyhook
    // label "backend & urgent" on the way in, since splitting on the comma
    // would invent a label GitHub does not have. Left alone, a later push
    // triggered by a genuine local label change used to send that rendering
    // straight back to GitHub verbatim, renaming "backend,urgent" itself.
    let fixture = ServiceFixture::new();
    add_github_remote(&fixture);
    let story_id = create(&fixture, "Label test");
    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);
    let fake = FakeGithubApiFactory::new();
    let issue = fake.seed_issue("Label test");
    fake.set_issue_labels(issue.number, &["backend,urgent"]);

    let outcome = run_initial_setup(
        &storage,
        &fake,
        Some(&token()),
        Some(SetupAnswers {
            strategy: InitialStrategy::MatchTitles,
            mode: SyncMode::Manual,
        }),
    )
    .expect("linking");
    let InitialSetupOutcome::Configured { config, .. } = outcome else {
        panic!("stated answers must proceed");
    };
    storage.save_config(&config).expect("saving the link");
    let base = storage.story(&story_id).expect("reading");
    storage
        .save_base(&story_id, &base)
        .expect("saving the base");

    // A genuine local label change -- the bug's actual trigger. Nothing
    // about the pre-existing "backend,urgent" label changed locally or
    // remotely; the merge only needs to notice "feature" is new.
    storage
        .write_events(
            &story_id,
            &[StoryEvent::StoryLabelsSet {
                at: "2026-01-02T00:00:00Z".to_string(),
                labels: vec!["feature".to_string()],
            }],
        )
        .expect("adding a local label");

    run_sync_with(
        &storage,
        &fake,
        Some(&token()),
        Some(&story_id),
        false,
        None,
        None,
        None,
    )
    .expect("syncing");

    let update = fake
        .recorded_calls()
        .into_iter()
        .find_map(|call| match call {
            RecordedCall::UpdateIssue(number, req) if number == issue.number => Some(req),
            _ => None,
        })
        .expect("the label change pushes an update to the issue");

    assert_eq!(
        update.labels,
        Some(vec!["backend,urgent".to_string(), "feature".to_string()]),
        "the pre-existing label must go back out under its real GitHub name, \
         not the storyhook rendering: {update:?}"
    );
}
