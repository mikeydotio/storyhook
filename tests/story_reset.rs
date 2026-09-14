//! A reset owns the story until every resource is gone.
use storyhook::service::story_reset::StoryResetService;
use storyhook::service::{NewStoryInput, StoryService};
use storyhook::store::{ReadOps, Store, StoryNo, WriteOps};
use storyhook_test_support::ServiceFixture;

#[test]
fn reservation_survives_reopen_deduplicates_and_prevents_state_changes() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let stories = StoryService::new(&ctx);
    let story = stories
        .create(&NewStoryInput {
            title: "Reset me".into(),
            state: Some("in-progress".into()),
            ..Default::default()
        })
        .unwrap();
    let service = StoryResetService::new(&ctx);
    let first = service.reserve(&story.id, &story.id).unwrap();
    assert_eq!(
        first.token,
        service.reserve(&story.id, &story.id).unwrap().token
    );
    assert!(
        stories
            .set_state(&story.id, "todo", None, None, None)
            .unwrap_err()
            .to_string()
            .contains("reset")
    );
    let reopened = storyhook::store::SqliteStore::open(fixture.env().store_path()).unwrap();
    assert_eq!(
        reopened
            .read(|tx| tx.story_reset(fixture.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .token,
        first.token
    );
}

#[test]
fn confirmation_and_closed_or_epic_targets_are_rejected_without_reserving() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Ordinary".into(),
            ..Default::default()
        })
        .unwrap();
    let service = StoryResetService::new(&ctx);
    assert!(service.reserve(&story.id, "wrong").is_err());
    assert!(
        fixture
            .store()
            .read(|tx| tx.story_reset(fixture.project(), StoryNo::new(1)))
            .unwrap()
            .is_none()
    );
    StoryService::new(&ctx)
        .set_state(&story.id, "done", None, None, None)
        .unwrap();
    assert!(service.reserve(&story.id, &story.id).is_err());
}

#[test]
fn no_checkout_reset_clears_awaiting_preserves_metadata_and_can_be_repeated() {
    let fixture = ServiceFixture::new();
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), None))
        .unwrap();
    let ctx = fixture.ctx();
    let stories = StoryService::new(&ctx);
    let before = stories
        .create(&NewStoryInput {
            title: "Keep title".into(),
            description: Some("Keep description".into()),
            state: Some("in-progress".into()),
            ..Default::default()
        })
        .unwrap();
    stories.set_awaiting(&before.id, "human").unwrap();
    let service = StoryResetService::new(&ctx);
    let reset = service.reserve(&before.id, &before.id).unwrap();
    assert!(
        service
            .execute(&before.id, &reset.token, || Err(
                storyhook::error::AppError::Validation("worker did not stop".into())
            ))
            .is_err()
    );
    assert!(
        service
            .get(&before.id, &reset.token)
            .unwrap()
            .failure
            .unwrap()
            .contains("worker did not stop")
    );
    assert_eq!(
        service.reserve(&before.id, &before.id).unwrap().token,
        reset.token
    );
    let done = service
        .execute(&before.id, &reset.token, || Ok(()))
        .unwrap();
    assert!(done.completed);
    service
        .execute(&before.id, &reset.token, || {
            panic!("completed cleanup must not run twice")
        })
        .unwrap();
    let after = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap()
        .snapshot;
    assert_eq!(after.state, "todo");
    assert_eq!(after.title, before.title);
    assert_eq!(after.description, before.description);
    assert_eq!(after.awaiting, None);
    assert_ne!(
        service.reserve(&before.id, &before.id).unwrap().token,
        reset.token
    );
}

#[test]
fn reset_reservation_cannot_be_rebound_and_is_excluded_from_claim_next() {
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let stories = StoryService::new(&ctx);
    let first = stories
        .create(&NewStoryInput {
            title: "Reserved".into(),
            ..Default::default()
        })
        .unwrap();
    let second = stories
        .create(&NewStoryInput {
            title: "Ready".into(),
            ..Default::default()
        })
        .unwrap();
    let reset = StoryResetService::new(&ctx)
        .reserve(&first.id, &first.id)
        .unwrap();
    let mut forged = reset.clone();
    forged.token = "another-owner".into();
    assert!(
        fixture
            .store()
            .write(|tx| tx.put_story_reset(&forged))
            .is_err()
    );
    let next = fixture
        .store()
        .read(|tx| {
            Ok(storyhook::service::QueryService::new(
                tx,
                fixture.project(),
                storyhook_test_support::FIXTURE_NOW,
            )
            .next(1, None)
            .unwrap())
        })
        .unwrap();
    assert_eq!(next[0].story.id, second.id);
    assert!(
        fixture
            .store()
            .write(|tx| tx.purge_story(fixture.project(), StoryNo::new(1)))
            .is_err()
    );
}

fn git(cwd: &std::path::Path, args: &[&str]) {
    let output = storyhook::env::git_env::command(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn dirty_locked_unpushed_worktree_is_removed_before_story_becomes_todo() {
    let fixture = ServiceFixture::new();
    let root = storyhook_test_support::scratch_dir();
    let repo = root.path().join("repo");
    let remote = root.path().join("remote.git");
    std::fs::create_dir(&repo).unwrap();
    git(
        root.path(),
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            remote.to_str().unwrap(),
        ],
    );
    git(&repo, &["init", "--initial-branch=main"]);
    storyhook_test_support::approve_fixture_identity(&repo, "Reset fixture", "reset@example.test");
    git(
        &repo,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "base",
        ],
    );
    git(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo, &["push", "origin", "main"]);
    let repo = repo.canonicalize().unwrap();
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), Some(&repo)))
        .unwrap();
    let ctx = fixture.ctx();
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Discard local work".into(),
            state: Some("in-progress".into()),
            ..Default::default()
        })
        .unwrap();
    let worktree = repo.join(".codex/worktrees").join(&story.id);
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "worktree-SH-1",
            worktree.to_str().unwrap(),
        ],
    );
    git(
        &worktree,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "unpushed",
        ],
    );
    std::fs::write(worktree.join("uncommitted.txt"), "destroy me").unwrap();
    git(&repo, &["worktree", "lock", worktree.to_str().unwrap()]);
    let service = StoryResetService::new(&ctx);
    let reset = service.reserve(&story.id, &story.id).unwrap();
    assert!(
        service
            .execute(&story.id, &reset.token, || Ok(()))
            .unwrap()
            .completed
    );
    assert!(!worktree.exists());
    assert!(!storyhook::service::resources::git::branch_exists(&repo, "worktree-SH-1").unwrap());
    assert!(storyhook::service::resources::git::branch_exists(&repo, "main").unwrap());
    let after = fixture
        .store()
        .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_eq!(after.state, "todo");
}

#[test]
fn resetting_one_engine_story_preserves_the_other_lane_and_run() {
    use storyhook::service::engine::{EngineService, StartRequest};
    use storyhook::store::{EngineAgent, EngineLaneState, EngineScope};
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let stories = StoryService::new(&ctx);
    let mut ids = Vec::new();
    for title in ["Reset this lane", "Keep this lane"] {
        ids.push(
            stories
                .create(&NewStoryInput {
                    title: title.into(),
                    state: Some("in-progress".into()),
                    ..Default::default()
                })
                .unwrap()
                .id,
        );
    }
    let dispatcher = storyhook_test_support::FakeDispatcher::new([]);
    let run = EngineService::new(&ctx, &dispatcher)
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 2,
            agent: EngineAgent::Codex,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap();
    fixture
        .store()
        .write(|tx| {
            for (index, mut lane) in tx.engine_lanes(&run.id)?.into_iter().enumerate() {
                lane.state = EngineLaneState::Working;
                lane.story_id = Some(ids[index].clone());
                tx.put_engine_lane(&lane)?;
            }
            tx.set_checkout_path(fixture.project(), None)
        })
        .unwrap();
    let before = fixture.store().read(|tx| tx.engine_lanes(&run.id)).unwrap();
    let service = StoryResetService::new(&ctx);
    let reset = service.reserve(&ids[0], &ids[0]).unwrap();
    assert!(
        fixture
            .store()
            .write(|tx| tx.put_engine_lane(&before[0]))
            .is_err()
    );
    service.execute(&ids[0], &reset.token, || Ok(())).unwrap();
    let after = fixture.store().read(|tx| tx.engine_lanes(&run.id)).unwrap();
    assert_eq!(after[0].state, EngineLaneState::Idle);
    assert_eq!(after[1], before[1]);
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.engine_run(&run.id))
            .unwrap()
            .unwrap(),
        run
    );
}
