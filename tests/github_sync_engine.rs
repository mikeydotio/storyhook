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
//! Design: SH-158's council verdict (`story show SH-158`). Notably: no
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
use storyhook::github::api::GithubApiFactory;
use storyhook::github::initial::{
    InitialSetupOutcome, InitialStrategy, SetupAnswers, run_initial_setup,
};
use storyhook::github::run_sync_with;
use storyhook::github::storage::SyncStorage;
use storyhook::github::sync_state::{StoryIssueMapping, SyncMode};
use storyhook::github::types::UpdateIssueRequest;
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

fn create_with_priority(fixture: &ServiceFixture, title: &str, priority: &str) -> String {
    StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: title.to_string(),
            priority: Some(priority.to_string()),
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
    // `MessageWithWarnings`, not `Message`, since SH-358: the seeded issue
    // carries no storyhook block, so the pulled story lands unassessed and the
    // sync says so — see `pulling_an_issue_with_no_priority_warns_unassessed`
    // and its sibling below for the dedicated coverage of that behaviour.
    assert!(
        matches!(response, Response::MessageWithWarnings(..)),
        "{response:?}"
    );

    let stories = storage.open_stories().expect("open stories");
    assert_eq!(stories.len(), 1, "{stories:?}");
    assert_eq!(stories[0].title, "Reported on GitHub");

    let config = storage.load_config().expect("loading").expect("configured");
    assert_eq!(config.mappings.len(), 1);
    assert_eq!(config.mappings[0].issue_number, issue.number);
}

// ---------------------------------------------------------------------------
// SH-358 -- a pulled issue with no priority lands unassessed, and the sync
// says so; one with a storyhook priority block does not need to be told.
// ---------------------------------------------------------------------------

#[test]
fn pulling_an_issue_with_no_priority_warns_unassessed() {
    // `create_story_from_issue` (this file) only writes `StoryPrioritySet` when
    // the issue's storyhook block names a priority other than `none` — an
    // issue with no block at all (`seed_issue`'s default) carries
    // `RemoteSnapshot::priority == Priority::None`, so the pulled story gets no
    // priority event and folds unassessed, exactly like `story new` with no
    // `--priority` (SH-354/SH-359).
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

    fake.seed_issue("No priority named on GitHub");

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

    let stories = storage.open_stories().expect("open stories");
    assert_eq!(stories.len(), 1, "{stories:?}");
    assert!(
        !stories[0].priority_assessed,
        "an issue with no storyhook block must not silently claim its story was assessed"
    );

    let Response::MessageWithWarnings(_, warnings) = response else {
        panic!("a sync that pulled an unassessed story must warn: {response:?}");
    };
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains(&stories[0].id));
    assert!(warnings[0].contains("story list --unassessed"));
}

#[test]
fn pulling_an_issue_with_a_stated_priority_stays_silent() {
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

    // `story_id` names a story that does not exist locally, so
    // `already_has_a_local_story` still treats this as unmapped
    // (`already_has_a_local_story`'s own doc — SH-191's shape) and the pull
    // phase creates a new one, carrying the block's stated priority.
    let body = "Filed on GitHub, priority already known.\n\n\
                ---\n\n\
                ```storyhook\n\
                story_id: SH-999\n\
                priority: high\n\
                ```\n";
    fake.seed_issue_with_body("Priority already named on GitHub", Some(body));

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

    let stories = storage.open_stories().expect("open stories");
    assert_eq!(stories.len(), 1, "{stories:?}");
    assert!(
        stories[0].priority_assessed,
        "a storyhook block naming a priority must be honoured, not treated as unassessed"
    );
    assert_eq!(stories[0].priority.as_str(), "high");

    assert!(
        matches!(response, Response::Message(_)),
        "an already-assessed pull must not warn: {response:?}"
    );
}

// ---------------------------------------------------------------------------
// SH-191 -- import-all's placeholder mapping must not re-duplicate a story
// its own issue body already names
// ---------------------------------------------------------------------------

#[test]
fn import_all_does_not_duplicate_a_story_the_issue_body_already_names() {
    // `handle_import_all` gives every open issue a placeholder mapping
    // (`story_id` empty) regardless of whether the issue's own body already
    // carries a storyhook block naming a story that exists locally -- the
    // shape left behind by a sync config that was reset and re-run through
    // "Import all". The pull phase's truly-unmapped branch guards against
    // exactly this (skips when `remote_snap.story_id` already exists
    // locally); the placeholder-mapping branch a few lines above it had no
    // equivalent, so it duplicated the story instead of recognising it.
    let fixture = ServiceFixture::new();
    add_github_remote(&fixture);
    let existing_id = create(&fixture, "Already tracked locally");
    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);
    let fake = FakeGithubApiFactory::new();
    let body = format!(
        "Filed on GitHub, but this issue already belongs to a local story.\n\n\
         ---\n\n\
         ```storyhook\n\
         story_id: {existing_id}\n\
         ```\n"
    );
    fake.seed_issue_with_body("Already tracked locally", Some(&body));

    run_sync_with(
        &storage,
        &fake,
        Some(&token()),
        None,
        false,
        None,
        Some(InitialStrategy::ImportAll),
        Some(SyncMode::Manual),
    )
    .expect("import-all setup and its first sync");

    let stories = storage.open_stories().expect("open stories");
    assert_eq!(
        stories.len(),
        1,
        "the issue's own storyhook block already names {existing_id}; import-all's \
         placeholder mapping must not spawn a duplicate: {stories:?}"
    );
    assert_eq!(stories[0].id, existing_id);
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

// ---------------------------------------------------------------------------
// SH-372 -- a deliberately parked priority survives a push and a pull back,
// on a second, independent clone syncing the same repo. This is the story's
// own concrete failure scenario, steps 1-5.
// ---------------------------------------------------------------------------

#[test]
fn a_parked_priority_pushes_the_key_and_pulls_back_parked_not_unassessed() {
    let pusher = ServiceFixture::new();
    add_github_remote(&pusher);
    let pusher_ctx = pusher.ctx();
    let pusher_storage = StoreSyncStorage::new(&pusher_ctx);
    let fake = FakeGithubApiFactory::new();

    run_sync_with(
        &pusher_storage,
        &fake,
        Some(&token()),
        None,
        false,
        None,
        Some(InitialStrategy::PushOnly),
        Some(SyncMode::Manual),
    )
    .expect("initial setup");

    create_with_priority(&pusher, "Deliberately parked", "none");

    run_sync_with(
        &pusher_storage,
        &fake,
        Some(&token()),
        None,
        false,
        None,
        None,
        None,
    )
    .expect("pushing");

    let created = fake.created_issues();
    assert_eq!(created.len(), 1, "{created:?}");
    let body = created[0].body.as_deref().unwrap_or_default();
    assert!(
        body.contains("priority: none"),
        "a deliberately parked story's push must state the key: {body:?}"
    );

    // A second, independent clone syncing the same repo -- the story's own
    // step 3.
    let puller = ServiceFixture::new();
    add_github_remote(&puller);
    let puller_ctx = puller.ctx();
    let puller_storage = StoreSyncStorage::new(&puller_ctx);

    let response = run_sync_with(
        &puller_storage,
        &fake,
        Some(&token()),
        None,
        false,
        None,
        Some(InitialStrategy::ImportAll),
        Some(SyncMode::Manual),
    )
    .expect("pulling on a fresh clone");

    let stories = puller_storage.open_stories().expect("open stories");
    assert_eq!(stories.len(), 1, "{stories:?}");
    assert!(
        stories[0].priority_assessed,
        "a pulled `priority: none` must read as deliberately parked, not unassessed"
    );
    assert_eq!(stories[0].priority.as_str(), "none");
    assert!(
        matches!(response, Response::Message(_)),
        "a story that was actually assessed must not draw SH-358's unassessed warning: {response:?}"
    );
}

// ---------------------------------------------------------------------------
// SH-372 -- a merge base written before SH-359 has no `priority_assessed`
// key and deserializes to `false` even for a real assessed level. The base
// is normalized against `fold_story`'s own invariant before any merge, or a
// stale base would look like a real remote change on every mapped,
// prioritized story's first sync after upgrade.
// ---------------------------------------------------------------------------

#[test]
fn a_stale_pre_sh359_base_does_not_spuriously_push_or_pull_an_assessed_priority() {
    let fixture = ServiceFixture::new();
    add_github_remote(&fixture);
    let story_id = create_with_priority(&fixture, "Already assessed", "high");
    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);
    let fake = FakeGithubApiFactory::new();

    // The remote already correctly carries the level -- a real level always
    // rendered its key, even before this fix; only `none` was ever dropped.
    let body = "Filed on GitHub, priority already high.\n\n\
                ---\n\n\
                ```storyhook\n\
                story_id: SH-BOGUS\n\
                priority: high\n\
                ```\n";
    let issue = fake.seed_issue_with_body("Already assessed", Some(body));

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

    // Simulate a `github_bases` row written by a pre-SH-359 binary: the
    // stored snapshot's `priority` is real but its JSON has no
    // `priority_assessed` key at all, so it deserializes to `false`.
    let mut stale_base = storage.story(&story_id).expect("reading");
    assert!(
        stale_base.priority_assessed,
        "fixture must start genuinely assessed"
    );
    stale_base.priority_assessed = false;
    storage
        .save_base(&story_id, &stale_base)
        .expect("seeding a stale base");

    let response = run_sync_with(
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

    assert!(
        matches!(response, Response::Message(_)),
        "a stale base must not manufacture a conflict or an error: {response:?}"
    );
    assert!(
        fake.recorded_calls()
            .iter()
            .all(|c| !matches!(c, RecordedCall::UpdateIssue(n, _) if *n == issue.number)),
        "an already-agreeing priority must not be spuriously pushed: {:?}",
        fake.recorded_calls()
    );
    let after = storage.story(&story_id).expect("reading");
    assert!(after.priority_assessed, "must not be silently cleared");
    assert_eq!(after.priority.as_str(), "high");
}

// ---------------------------------------------------------------------------
// SH-403 -- MatchTitles never saves a base, so the first real sync used to
// fall back to `base = story.clone()`; a blockless remote then read as a
// remote change and silently parked the story's priority. `RemotePriority::
// Unknown` resolving against that same fallback base closes it as a side
// effect of this fix.
// ---------------------------------------------------------------------------

#[test]
fn match_titles_first_sync_does_not_silently_park_an_assessed_priority() {
    let fixture = ServiceFixture::new();
    add_github_remote(&fixture);
    let story_id = create_with_priority(&fixture, "Matched by title", "high");
    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);
    let fake = FakeGithubApiFactory::new();
    // An ordinary GitHub-native issue -- no storyhook block at all, the
    // exact shape MatchTitles exists to link.
    fake.seed_issue("Matched by title");

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

    // No `save_base` call -- MatchTitles's own gap (SH-403). The first real
    // sync must not treat the blockless remote as an authoritative clear.
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
    .expect("the first real sync after MatchTitles");

    let after = storage.story(&story_id).expect("reading");
    assert!(
        after.priority_assessed,
        "MatchTitles's missing base must not silently park an assessed priority"
    );
    assert_eq!(after.priority.as_str(), "high");
}

// ---------------------------------------------------------------------------
// SH-372 scope item 5 -- a remote priority edited away to nothing (by hand,
// on GitHub) must not be read as an authoritative statement either way: not
// a false "un-assess" of a parked story.
// ---------------------------------------------------------------------------

#[test]
fn a_hand_deleted_remote_priority_key_is_silence_not_a_statement() {
    let fixture = ServiceFixture::new();
    add_github_remote(&fixture);
    let story_id = create_with_priority(&fixture, "Parked, then edited by hand", "none");
    let ctx = fixture.ctx();
    let storage = StoreSyncStorage::new(&ctx);
    let fake = FakeGithubApiFactory::new();

    // Push it for real, so the remote and the base both correctly agree the
    // story is parked -- the state a human would actually be editing.
    let outcome = run_initial_setup(
        &storage,
        &fake,
        Some(&token()),
        Some(SetupAnswers {
            strategy: InitialStrategy::PushOnly,
            mode: SyncMode::Manual,
        }),
    )
    .expect("initial setup");
    let InitialSetupOutcome::Configured { config, .. } = outcome else {
        panic!("stated answers must proceed");
    };
    storage.save_config(&config).expect("saving the config");
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
    .expect("the initial push");

    let created = fake.created_issues();
    assert_eq!(created.len(), 1, "{created:?}");
    let config = storage
        .load_config()
        .expect("loading")
        .expect("configured");
    let issue_number = config
        .mappings
        .iter()
        .find(|m| m.story_id == story_id)
        .expect("mapped")
        .issue_number;

    let synced = storage.story(&story_id).expect("reading");
    assert!(synced.priority_assessed, "must have pushed as parked");
    assert_eq!(synced.priority.as_str(), "none");

    // A human deletes the `priority:` line on GitHub directly -- bypassing
    // the sync engine's own renderer entirely, the way an actual hand edit
    // would.
    let client = fake.build("token".to_string(), "acme".to_string(), "widgets".to_string());
    let edited_body = format!(
        "Parked, then edited by hand.\n\n---\n\n```storyhook\nstory_id: {story_id}\n```\n"
    );
    client
        .update_issue(
            issue_number,
            &UpdateIssueRequest {
                body: Some(edited_body),
                ..Default::default()
            },
        )
        .expect("simulating a hand edit");

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
    .expect("syncing after the hand edit");

    let after = storage.story(&story_id).expect("reading");
    assert!(
        after.priority_assessed,
        "a hand-deleted remote key must not silently un-park the story"
    );
    assert_eq!(after.priority.as_str(), "none");
}
