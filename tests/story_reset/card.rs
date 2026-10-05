//! A reset owns the story until it finishes; what it cannot remove is reported.
use storyhook::service::story_reset::StoryResetService;
use storyhook::service::{NewStoryInput, StoryService};
use storyhook::store::{ReadOps, Store, StoryNo, WriteOps};
use storyhook_test_support::ServiceFixture;

use super::workspace::{Workspace, commit, git};

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
fn no_checkout_reset_clears_awaiting_preserves_metadata_and_never_waits_on_a_stuck_worker() {
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
    assert_eq!(
        service.reserve(&before.id, &before.id).unwrap().token,
        reset.token
    );
    // A worker that does not stop is reported; it never refuses the reset.
    let done = service
        .execute(&before.id, &reset.token, || {
            Err(storyhook::error::AppError::Validation(
                "worker did not stop".into(),
            ))
        })
        .unwrap();
    assert!(done.completed);
    assert!(
        done.residue
            .iter()
            .any(|entry| entry.resource == "running work"
                && entry.reason.contains("worker did not stop")),
        "{:?}",
        done.residue
    );
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

#[test]
fn dirty_locked_unpushed_worktree_is_removed_before_story_becomes_todo() {
    forced_worktree_reset(true);
}

#[test]
fn local_only_worktree_reset_does_not_require_an_origin() {
    forced_worktree_reset(false);
}

/// Removes dirty, locked and unpushed local work and records how to recover it.
fn forced_worktree_reset(with_origin: bool) {
    let workspace = Workspace::new(with_origin);
    commit(&workspace.worktree, "unpushed");
    let tip = git(&workspace.worktree, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    std::fs::write(workspace.worktree.join("uncommitted.txt"), "destroy me").unwrap();
    git(
        &workspace.repo,
        &["worktree", "lock", workspace.worktree.to_str().unwrap()],
    );
    let reset = workspace.reserve_pinned();
    let done = workspace.execute(&reset);
    assert_eq!(done.residue, vec![]);
    assert!(!workspace.worktree.exists());
    assert!(!workspace.branch_exists("worktree-SH-1"));
    assert!(workspace.branch_exists("main"));
    let recovery = done.recovery.unwrap();
    assert_eq!(recovery.branch.as_deref(), Some("worktree-SH-1"));
    assert_eq!(recovery.tip.as_deref(), Some(tip.as_str()));
    assert_eq!(recovery.unpushed, Some(1));
    assert_eq!((recovery.dirty, recovery.untracked), (Some(0), Some(1)));
    let comment = workspace.last_comment();
    assert!(
        comment.contains(&format!("git branch worktree-SH-1 {tip}")),
        "{comment}"
    );
    assert!(comment.contains("1 untracked"), "{comment}");
    let story = workspace.story();
    assert_eq!(story.state, "todo");
    assert_eq!(story.awaiting, None);
}

/// A worktree that holds installed StoryHook artifacts is never removed.
fn installed_artifact_worktree_is_left_in_place() {
    let workspace = Workspace::new(false);
    let reset = workspace.reserve_pinned();
    let manifest = storyhook::plugin::managed_paths_file().unwrap();
    std::fs::write(&manifest, format!("{}\n", workspace.worktree.display())).unwrap();
    let done = workspace.execute(&reset);
    std::fs::remove_file(&manifest).unwrap();
    let resource = format!("worktree {}", workspace.worktree.display());
    let left = done
        .residue
        .iter()
        .find(|entry| entry.resource == resource)
        .unwrap_or_else(|| panic!("{:?}", done.residue));
    assert!(left.reason.contains("installed artifact"), "{left:?}");
    assert!(workspace.worktree.exists());
    assert!(workspace.branch_exists("worktree-SH-1"));
    assert_eq!(workspace.story().state, "todo");
}

#[test]
fn a_concurrent_execution_joins_the_running_cleanup_operation() {
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
        let release_tx = release_tx;
        let id = &story.id;
        let token = &reset.token;
        scope.spawn(|| {
            let service = StoryResetService::new(&ctx);
            service
                .execute(id, token, move || {
                    entered_tx.send(()).unwrap();
                    release_rx
                        .recv_timeout(storyhook_test_support::load_grace::graced_now(
                            std::time::Duration::from_secs(5),
                        ))
                        .unwrap();
                    Ok(())
                })
                .unwrap();
        });
        entered_rx
            .recv_timeout(storyhook_test_support::load_grace::graced_now(
                std::time::Duration::from_secs(5),
            ))
            .unwrap();
        // The second caller joins: it sees the unfinished receipt and never
        // enters the cleanup the first executor owns.
        assert!(
            !service
                .execute(id, token, || panic!("duplicate cleanup entered"))
                .unwrap()
                .completed
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
        installed_artifact_worktree_is_left_in_place();
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
