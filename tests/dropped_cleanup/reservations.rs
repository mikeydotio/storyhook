//! Restart and lifecycle races use the real durable ownership fence.
use super::*;
use std::os::unix::fs::MetadataExt;
use storyhook::domain::StoryEvent;
use storyhook::service::resources::{ResourceOptions, ResourceService};
use storyhook::store::{
    DroppedCleanup, DroppedCleanupPhase as Phase, ResetPathIdentity, SqliteStore,
};

fn reserve(f: &Dropped, phase: Phase) -> DroppedCleanup {
    let report = ResourceService::new(&f.fixture.ctx())
        .resolve(
            &f.lease.story_id,
            &ResourceOptions {
                lease_json: Some(serde_json::to_string(&f.lease).unwrap()),
                ..Default::default()
            },
        )
        .unwrap();
    let generation = f.fixture.store().read(|tx| tx.events_for(f.fixture.project(), StoryNo::new(1))).unwrap()
        .into_iter().rev().find(|e| matches!(e.known(), Some(StoryEvent::StoryStateChanged { state, .. }) if state == "dropped")).unwrap().global_seq;
    let paths = [
        (f.workspace.checkout.join(".git"), false),
        (f.workspace.worktree.clone(), true),
        (f.workspace.worktree_git_dir(), true),
    ]
    .into_iter()
    .map(|(path, removable)| {
        let metadata = fs::symlink_metadata(&path).unwrap();
        ResetPathIdentity {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            removable,
        }
    })
    .collect();
    let record = DroppedCleanup {
        project: f.fixture.project(),
        story: StoryNo::new(1),
        token: "fixture-drop".into(),
        generation,
        lease: f.lease.clone(),
        resources: report,
        paths,
        process_start: None,
        phase,
        released: false,
        failure: Some("interrupted fixture".into()),
    };
    f.fixture
        .store()
        .write(|tx| tx.put_dropped_cleanup(&record))
        .unwrap();
    record
}

#[test]
fn reservation_prevents_reopen_delete_reset_and_project_transfer_but_allows_comments() {
    let f = Dropped::new();
    let record = reserve(&f, Phase::Prepared);
    let ctx = f.fixture.ctx();
    let stories = StoryService::new(&ctx);
    assert!(
        stories
            .reopen(&f.lease.story_id)
            .unwrap_err()
            .to_string()
            .contains("cleanup")
    );
    assert!(
        stories
            .set_state(&f.lease.story_id, "todo", None, None, None)
            .is_err()
    );
    assert!(stories.delete(&f.lease.story_id).is_err());
    assert!(
        storyhook::service::story_reset::StoryResetService::new(&ctx)
            .reserve(&f.lease.story_id, &f.lease.story_id)
            .is_err()
    );
    assert!(
        f.fixture
            .store()
            .write(|tx| tx.set_checkout_path(f.fixture.project(), Some(f.fixture.cwd())))
            .is_err()
    );
    stories
        .comment(&f.lease.story_id, "Cleanup is still in progress.")
        .unwrap();
    let reopened = SqliteStore::open(f.fixture.env().store_path()).unwrap();
    assert_eq!(
        reopened
            .read(|tx| tx.dropped_cleanup(f.fixture.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .token,
        record.token
    );
    let report = CleanupService::new(&ctx).run(false).unwrap();
    assert_eq!(report.removed.len(), 1, "{report:?}");
    assert!(
        f.fixture
            .store()
            .read(|tx| tx.dropped_cleanup(f.fixture.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .released
    );
    stories.reopen(&f.lease.story_id).unwrap();
}

#[test]
fn restart_reconciles_each_absent_pane_stage_and_releases_the_reservation() {
    for phase in [
        Phase::Prepared,
        Phase::Stopping,
        Phase::Quiescent,
        Phase::Removing,
    ] {
        let f = Dropped::new();
        reserve(&f, phase);
        let reopened = SqliteStore::open(f.fixture.env().store_path()).unwrap();
        let ctx = storyhook::service::Ctx::new(
            &reopened,
            f.fixture.project(),
            f.fixture.cwd(),
            f.fixture.env().clone(),
        )
        .no_hooks(true);
        let report = CleanupService::new(&ctx).run(false).unwrap();
        assert_eq!(report.removed.len(), 1, "{phase:?}: {report:?}");
        let record = reopened
            .read(|tx| tx.dropped_cleanup(f.fixture.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap();
        assert_eq!(record.phase, Phase::Removed);
        assert!(record.released);
        assert!(!f.workspace.worktree.exists());
        assert!(f.workspace.local_branch_exists());
    }
}

#[test]
fn settled_dirty_refusal_releases_ownership_without_discarding_work() {
    for phase in [Phase::Prepared, Phase::Quiescent] {
        let f = Dropped::new();
        reserve(&f, phase);
        fs::write(f.workspace.worktree.join("work"), "keep changes").unwrap();
        let report = CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
        assert!(report.removed.is_empty(), "{report:?}");
        assert_eq!(
            fs::read_to_string(f.workspace.worktree.join("work")).unwrap(),
            "keep changes"
        );
        let record = f
            .fixture
            .store()
            .read(|tx| tx.dropped_cleanup(f.fixture.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap();
        assert!(record.released, "{phase:?}: {report:?}");
        StoryService::new(&f.fixture.ctx())
            .reopen(&f.lease.story_id)
            .unwrap();
    }
}

#[test]
fn retry_after_worktree_removal_before_receipt_is_idempotent() {
    let f = Dropped::new();
    reserve(&f, Phase::Quiescent);
    git(
        &f,
        &["worktree", "remove", f.workspace.worktree.to_str().unwrap()],
    );
    let report = CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
    assert!(
        report.failed.is_empty() && report.skipped.is_empty(),
        "{report:?}"
    );
    let record = f
        .fixture
        .store()
        .read(|tx| tx.dropped_cleanup(f.fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert!(record.released);
    assert_eq!(record.phase, Phase::Removed);
    assert!(f.workspace.local_branch_exists());
}

#[test]
fn pinned_paths_cannot_be_replaced_during_a_retry() {
    let f = Dropped::new();
    reserve(&f, Phase::Quiescent);
    let saved = f.workspace.root.path().join("retained-worktree");
    fs::rename(&f.workspace.worktree, &saved).unwrap();
    fs::create_dir(&f.workspace.worktree).unwrap();
    fs::write(f.workspace.worktree.join("valuable"), "replacement").unwrap();
    let report = CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
    assert!(report.removed.is_empty(), "{report:?}");
    assert!(
        report.skipped.iter().any(|s| s.detail.contains("identity")),
        "{report:?}"
    );
    assert_eq!(
        fs::read_to_string(f.workspace.worktree.join("valuable")).unwrap(),
        "replacement"
    );
    assert!(saved.exists());
}

#[test]
fn a_released_old_generation_cannot_remove_a_reopened_workspace() {
    let f = Dropped::new();
    let mut record = reserve(&f, Phase::Prepared);
    record.released = true;
    f.fixture
        .store()
        .write(|tx| tx.put_dropped_cleanup(&record))
        .unwrap();
    StoryService::new(&f.fixture.ctx())
        .reopen(&f.lease.story_id)
        .unwrap();
    let report = CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
    assert!(report.removed.is_empty(), "{report:?}");
    assert!(f.workspace.worktree.exists());
    StoryService::new(&f.fixture.ctx())
        .set_state(&f.lease.story_id, "dropped", None, None, None)
        .unwrap();
    let report = CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
    assert_eq!(report.removed.len(), 1, "{report:?}");
    let next = f
        .fixture
        .store()
        .read(|tx| tx.dropped_cleanup(f.fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert_ne!(record.generation, next.generation);
    assert_ne!(record.token, next.token);
}

#[test]
fn reservation_identity_cannot_change_and_competing_reset_cannot_acquire_it() {
    let f = Dropped::new();
    let record = reserve(&f, Phase::Prepared);
    for field in ["token", "lease", "paths", "start", "resources"] {
        let mut changed = record.clone();
        match field {
            "token" => changed.token = "replacement".into(),
            "lease" => changed.lease.branch = "different".into(),
            "paths" => changed.paths.clear(),
            "start" => changed.process_start = Some("fake".into()),
            _ => changed.resources.window_name = "different".into(),
        }
        assert!(
            f.fixture
                .store()
                .write(|tx| tx.put_dropped_cleanup(&changed))
                .is_err(),
            "{field}"
        );
    }
    let reset = storyhook::store::StoryReset {
        project: f.fixture.project(),
        story: StoryNo::new(1),
        story_id: f.lease.story_id.clone(),
        token: "reset".into(),
        original_state: "dropped".into(),
        lanes: vec![],
        resources: None,
        paths: vec![],
        completed: false,
        failure: None,
    };
    assert!(
        f.fixture
            .store()
            .write(|tx| tx.put_story_reset(&reset))
            .is_err()
    );
}

#[test]
fn raw_state_writes_and_unsettled_release_cannot_bypass_ownership() {
    let f = Dropped::new();
    let record = reserve(&f, Phase::Stopping);
    let mut released = record.clone();
    released.released = true;
    assert!(
        f.fixture
            .store()
            .write(|tx| tx.put_dropped_cleanup(&released))
            .is_err()
    );
    let mut backwards = record;
    backwards.phase = Phase::Prepared;
    assert!(
        f.fixture
            .store()
            .write(|tx| tx.put_dropped_cleanup(&backwards))
            .is_err()
    );
    let row = f
        .fixture
        .store()
        .read(|tx| tx.story(f.fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    let mut snapshot = row.snapshot;
    snapshot.state = "todo".into();
    assert!(
        f.fixture
            .store()
            .write(|tx| tx.put_story(f.fixture.project(), &snapshot, row.head_seq))
            .is_err()
    );
    assert_eq!(
        f.fixture
            .store()
            .read(|tx| tx.story(f.fixture.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .state,
        "dropped"
    );
}
