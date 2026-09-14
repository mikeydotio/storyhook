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
    storyhook::service::ConfigService::new(&ctx)
        .add_type("epic", None, None)
        .unwrap();
    let epic = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Folder".into(),
            story_type: Some("epic".into()),
            ..Default::default()
        })
        .unwrap();
    assert!(
        service
            .reserve(&epic.id, &epic.id)
            .unwrap_err()
            .to_string()
            .contains("epic")
    );
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
    assert!(
        fixture
            .store()
            .write(|tx| tx.delete_project(fixture.project()))
            .is_err()
    );
    assert!(
        fixture
            .store()
            .write(|tx| tx.set_prefix(fixture.project(), "NEW"))
            .is_err()
    );
    assert!(
        fixture
            .store()
            .write(|tx| tx.set_checkout_path(fixture.project(), None))
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
    fixture
        .store()
        .read(|tx| {
            let query = storyhook::service::QueryService::new(
                tx,
                fixture.project(),
                storyhook_test_support::FIXTURE_NOW,
            );
            assert!(!query.session_eligibility(&first.id).unwrap().eligible);
            assert_eq!(
                query.session_eligibility(&first.id).unwrap().reason,
                storyhook::service::query::EligibilityReason::Resetting
            );
            assert_eq!(query.summary().unwrap().ready_count, 1);
            let report = query.report_data().unwrap();
            assert!(!report.ready_ids.contains(&first.id));
            assert!(!report.next_ids.contains(&first.id));
            assert!(report.blocked_ids.contains(&first.id));
            let context: serde_json::Value =
                serde_json::from_str(&query.context(true).unwrap()).unwrap();
            assert_eq!(context["ready_count"], 1);
            let listed = query
                .list(&storyhook::service::query::ListFilters {
                    ready: true,
                    ..Default::default()
                })
                .unwrap();
            assert_eq!(listed.views.len(), 1);
            assert_eq!(listed.views[0].story.id, second.id);
            Ok(())
        })
        .unwrap();
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
    forced_worktree_reset(true);
}

#[test]
fn local_only_worktree_reset_does_not_require_an_origin() {
    forced_worktree_reset(false);
}

fn forced_worktree_reset(with_origin: bool) {
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
    if !with_origin {
        git(&repo, &["remote", "remove", "origin"]);
    }
    let service = StoryResetService::new(&ctx);
    let reset = service.reserve(&story.id, &story.id).unwrap();
    let mut pinned = reset.clone();
    pinned.resources = Some(
        storyhook::service::resources::ResourceService::new(&ctx)
            .resolve(&story.id, &Default::default())
            .unwrap(),
    );
    pinned.paths = pinned_paths(pinned.resources.as_ref().unwrap());
    fixture
        .store()
        .write(|tx| tx.put_story_reset(&pinned))
        .unwrap();
    git(&worktree, &["switch", "-c", "replacement"]);
    assert!(
        service
            .execute(&story.id, &reset.token, || Ok(()))
            .unwrap_err()
            .to_string()
            .contains("identity changed")
    );
    assert!(worktree.exists());
    git(&worktree, &["switch", "worktree-SH-1"]);
    if std::env::var_os("SH717_ARTIFACT_TEST_CHILD").is_some() {
        let manifest = storyhook::plugin::managed_paths_file().unwrap();
        std::fs::write(&manifest, format!("{}\n", worktree.display())).unwrap();
        assert!(
            service
                .execute(&story.id, &reset.token, || Ok(()))
                .unwrap_err()
                .to_string()
                .contains("installed artifact")
        );
        assert!(worktree.exists());
        std::fs::remove_file(&manifest).unwrap();
    }
    let saved = worktree.with_extension("original");
    std::fs::rename(&worktree, &saved).unwrap();
    std::fs::create_dir(&worktree).unwrap();
    std::fs::copy(saved.join(".git"), worktree.join(".git")).unwrap();
    assert!(
        service
            .execute(&story.id, &reset.token, || Ok(()))
            .unwrap_err()
            .to_string()
            .contains("filesystem identity changed")
    );
    std::fs::remove_file(worktree.join(".git")).unwrap();
    std::fs::remove_dir(&worktree).unwrap();
    std::fs::rename(&saved, &worktree).unwrap();
    let caller = storyhook::service::Ctx::new(
        fixture.store(),
        fixture.project(),
        worktree.clone(),
        fixture.env().clone(),
    )
    .no_hooks(true);
    assert!(
        StoryResetService::new(&caller)
            .execute(&story.id, &reset.token, || Ok(()))
            .unwrap_err()
            .to_string()
            .contains("calling worktree")
    );
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
fn concurrent_execution_cannot_enter_the_same_cleanup_operation() {
    let fixture = ServiceFixture::new();
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), None))
        .unwrap();
    let ctx = fixture.ctx();
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "One cleanup owner".into(),
            ..Default::default()
        })
        .unwrap();
    let service = StoryResetService::new(&ctx);
    let reset = service.reserve(&story.id, &story.id).unwrap();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let id = &story.id;
        let token = &reset.token;
        scope.spawn(|| {
            let service = StoryResetService::new(&ctx);
            service
                .execute(id, token, move || {
                    entered_tx.send(()).unwrap();
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                    Ok(())
                })
                .unwrap();
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert!(
            service
                .execute(id, token, || panic!("duplicate cleanup entered"))
                .unwrap_err()
                .to_string()
                .contains("already running")
        );
        release_tx.send(()).unwrap();
    });
    assert!(service.get(&story.id, &reset.token).unwrap().completed);
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

#[test]
fn installed_artifact_registry_is_independent_of_the_story_database() {
    if std::env::var_os("SH717_ARTIFACT_TEST_CHILD").is_some() {
        forced_worktree_reset(false);
        return;
    }
    let registry = storyhook_test_support::scratch_dir();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "card::installed_artifact_registry_is_independent_of_the_story_database",
            "--nocapture",
        ])
        .env("STORYHOOK_DATA_DIR", registry.path())
        .env("SH717_ARTIFACT_TEST_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn pinned_paths(
    report: &storyhook::service::resources::ResourceReport,
) -> Vec<storyhook::store::ResetPathIdentity> {
    use std::os::unix::fs::MetadataExt;
    let repo = report.repository.as_ref().unwrap();
    let common = storyhook::service::resources::git::text(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .unwrap();
    let worktree = report.worktree.as_ref().unwrap();
    let private =
        storyhook::service::resources::git::text(worktree, &["rev-parse", "--absolute-git-dir"])
            .unwrap();
    [
        (std::path::PathBuf::from(common.trim()), false),
        (worktree.clone(), true),
        (std::path::PathBuf::from(private.trim()), true),
    ]
    .into_iter()
    .map(|(path, removable)| {
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        storyhook::store::ResetPathIdentity {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            removable,
        }
    })
    .collect()
}

#[test]
fn reset_allows_an_existing_dispatch_to_release_its_lane() {
    use storyhook::service::engine::{EngineService, StartRequest};
    use storyhook::store::{EngineAgent, EngineLaneState, EngineScope};
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Dispatch race".into(),
            ..Default::default()
        })
        .unwrap();
    let dispatcher = storyhook_test_support::FakeDispatcher::new([]);
    let run = EngineService::new(&ctx, &dispatcher)
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 1,
            agent: EngineAgent::Codex,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap();
    fixture
        .store()
        .write(|tx| {
            let mut lane = tx.engine_lanes(&run.id)?.remove(0);
            lane.state = EngineLaneState::Dispatching;
            lane.story_id = Some(story.id.clone());
            tx.put_engine_lane(&lane)?;
            tx.set_checkout_path(fixture.project(), None)
        })
        .unwrap();
    let service = StoryResetService::new(&ctx);
    let reset = service.reserve(&story.id, &story.id).unwrap();
    service
        .execute(&story.id, &reset.token, || {
            fixture.store().write(|tx| {
                let mut lane = tx.engine_lanes(&run.id)?.remove(0);
                lane.state = EngineLaneState::Idle;
                lane.story_id = None;
                tx.put_engine_lane(&lane)
            })?;
            Ok(())
        })
        .unwrap();
    assert!(service.get(&story.id, &reset.token).unwrap().completed);
}
