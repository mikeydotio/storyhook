//! SH-642: the CLI control path wakes the real daemon without a timer tick.

use std::path::Path;
use std::time::{Duration, Instant};

use storyhook::api::dispatch::REQUIRED_DISPATCH_PROTOCOL;
use storyhook::store::{EngineRunState, ReadOps, SqliteStore, Store, WriteOps};
use storyhook_test_support::{DaemonGuard, TestEnv, scratch_dir};

fn command(env: &TestEnv, cwd: &Path, args: &[&str]) -> serde_json::Value {
    let output = env.story(cwd).args(args).arg("--json").output().unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn finished(store: &SqliteStore, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let run = store.read(|tx| tx.engine_run(id)).unwrap().unwrap();
        if run.state == EngineRunState::Finished {
            assert_eq!(run.stop_reason.as_deref(), Some("queue-drained"));
            return;
        }
        assert!(
            Instant::now() < deadline,
            "CLI control did not wake reconciliation: {run:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn cli_start_and_resume_wake_runs_created_after_the_daemon_began_waiting() {
    let env = TestEnv::isolated();
    let scratch = scratch_dir();
    let script = scratch.path().join("story.sh");
    std::fs::write(
        &script,
        format!("#!/usr/bin/env bash\nDISPATCH_PROTOCOL={REQUIRED_DISPATCH_PROTOCOL}\n"),
    )
    .unwrap();
    let _guard = DaemonGuard::new(&env, scratch.path());
    env.story(scratch.path())
        .args(["daemon", "start"])
        .env("STORYHOOK_DISPATCH_SCRIPT", &script)
        .env("STORYHOOK_RECONCILE_TICK_MS", "60000")
        .assert()
        .success();
    let project = env.project().prefix("CTL").build();
    let store = SqliteStore::open(env.store_path()).unwrap();

    // No ready stories: the production pass finishes immediately, providing
    // positive evidence of a wake without launching an agent or helper.
    let started = command(&env, project.path(), &["engine", "start", "--lanes", "1"]);
    let first_id = started["run"]["id"].as_str().unwrap();
    finished(&store, first_id);

    // Seed the second run already paused in one transaction: a separate
    // start then pause would race the real daemon's empty-queue completion.
    let mut paused = store.read(|tx| tx.engine_run(first_id)).unwrap().unwrap();
    paused.id = "paused-after-daemon-start".into();
    paused.state = EngineRunState::Paused;
    paused.stop_reason = None;
    let mut lane = store
        .read(|tx| tx.engine_lanes(first_id))
        .unwrap()
        .remove(0);
    lane.run_id = paused.id.clone();
    store
        .write(|tx| {
            tx.create_engine_run(&paused)?;
            tx.put_engine_lane(&lane)
        })
        .unwrap();
    command(
        &env,
        project.path(),
        &["engine", "resume", "--run", &paused.id],
    );
    finished(&store, &paused.id);
}
