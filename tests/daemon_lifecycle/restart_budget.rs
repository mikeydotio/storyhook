//! A restart's external waits must fit one budget, across projects and lanes.

use std::os::unix::fs::PermissionsExt;

use super::*;
use storyhook::api::dispatch::REQUIRED_DISPATCH_PROTOCOL;
use storyhook::service::engine::{EngineService, StartRequest, TMUX_TIMEOUT};
use storyhook::service::{Ctx, NewStoryInput, StoryService};
use storyhook::store::{
    EngineAgent, EngineLaneState, EngineScope, NewProject, ReadOps, SqliteStore, Store, WriteOps,
};
use storyhook_test_support::{FakeDispatcher, default_states, default_types};

fn executable(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn slow_restart_probes_share_one_budget_across_projects() {
    let env = TestEnv::isolated();
    let root = scratch_dir();
    let _guard = DaemonGuard(&env);
    let environment = env.environment();
    let store = SqliteStore::open(env.store_path()).unwrap();
    store.migrate().unwrap();
    let mut runs = Vec::new();
    for (slug, prefix) in [("first", "AA"), ("second", "BB")] {
        let project = store
            .write(|tx| {
                let id = tx.create_project(&NewProject {
                    uuid: slug.into(),
                    slug: slug.into(),
                    name: slug.into(),
                    prefix: prefix.into(),
                    created_at: environment.now(),
                })?;
                tx.put_states(id, &default_states())?;
                tx.put_types(id, &default_types())?;
                tx.set_checkout_path(id, Some(root.path()))?;
                Ok(id)
            })
            .unwrap();
        let ctx = Ctx::new(&store, project, root.path(), environment.clone()).no_hooks(true);
        let fake = FakeDispatcher::default();
        let engine = EngineService::new(&ctx, &fake);
        let run = engine
            .start(StartRequest {
                scope: EngineScope::Project,
                lanes: 2,
                agent: EngineAgent::Codex,
                model: None,
                effort: None,
                speed: None,
            })
            .unwrap();
        for mut lane in store.read(|tx| tx.engine_lanes(&run.id)).unwrap() {
            let story = StoryService::new(&ctx)
                .create(&NewStoryInput {
                    title: format!("occupied lane {}", lane.lane_index),
                    ..NewStoryInput::default()
                })
                .unwrap();
            lane.state = EngineLaneState::Working;
            lane.story_id = Some(story.id.clone());
            lane.window_name = Some(story.id);
            lane.pane_id = Some(format!("%{}", lane.lane_index));
            lane.worktree_path = Some(root.path().to_string_lossy().into_owned());
            store.write(|tx| tx.put_engine_lane(&lane)).unwrap();
        }
        // Polling may observe these lanes after readiness but cannot dispatch.
        engine.pause(&run.id).unwrap();
        runs.push(run.id);
    }
    let tmux = root.path().join("tmux");
    executable(
        &tmux,
        &format!("exec sleep {}", (TMUX_TIMEOUT * 2).as_secs()),
    );
    let dispatch = root.path().join("story.sh");
    executable(
        &dispatch,
        &format!("DISPATCH_PROTOCOL={REQUIRED_DISPATCH_PROTOCOL}\nexit 42"),
    );
    let path = std::env::join_paths(
        std::iter::once(root.path().to_path_buf())
            .chain(std::env::split_paths(&env.path_with_binary())),
    )
    .unwrap();
    let output = env
        .story(root.path())
        .env("PATH", path)
        .env("STORYHOOK_DISPATCH_SCRIPT", dispatch)
        .args(["daemon", "start"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "startup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let info = env.daemon().expect("published daemon");
    lifecycle::hello(&info).expect("authenticated readiness after bounded restart reconciliation");
    for run in runs {
        let lanes = store.read(|tx| tx.engine_lanes(&run)).unwrap();
        assert!(
            lanes
                .iter()
                .all(|lane| lane.state == EngineLaneState::Working)
        );
        assert!(
            lanes.iter().all(|lane| lane.last_progress_at.is_some()),
            "every lane must be reconciled before readiness"
        );
        assert!(
            lanes.iter().all(|lane| lane.probe_detail.is_some()),
            "unanswered observations must remain visible"
        );
        assert_eq!(
            store
                .read(|tx| tx.engine_run(&run))
                .unwrap()
                .unwrap()
                .consecutive_hard_stops,
            0
        );
    }
}
