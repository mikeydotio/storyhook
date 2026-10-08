//! Native Stop Now cannot release authority while a successful Git leader's effect child survives.
use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;
use storyhook::error::AppError;
use storyhook::lane_budget::WindowCensus;
use storyhook::service::engine::{DispatchRequest, Dispatcher, UnclaimRequest, WindowProbe};

struct Native;
impl Dispatcher for Native {
    fn dispatch(&self, _: DispatchRequest) -> Result<DispatchOutcome, AppError> {
        panic!("Stop Now must not dispatch")
    }
    fn unclaim(&self, _: UnclaimRequest) -> Result<DispatchOutcome, AppError> {
        panic!("ordinary lane must not unclaim")
    }
    fn probe_window(&self, _: &str) -> WindowProbe {
        panic!("stopping must not probe ordinary liveness")
    }
    fn kill_window(&self, _: &str) -> Result<(), AppError> {
        panic!("native cleanup owns window removal")
    }
    fn census(&self) -> WindowCensus {
        WindowCensus::Counted { windows: vec![] }
    }
    // The default reset invokes the real native actuator under engine ownership.
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = storyhook::env::git_env::command(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap()
}

struct Release(PathBuf);
impl Drop for Release {
    fn drop(&mut self) {
        fs::write(&self.0, "release").expect("release the reset effect child");
    }
}

#[test]
fn successful_helper_waits_for_its_effect_child_before_releasing_reset_authority() {
    let fixture = ServiceFixture::new().with_subprocess_patience();
    let run = setup(&fixture, &FakeDispatcher::default(), "todo");
    let repo = fixture.cwd().canonicalize().unwrap();
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
    let mut lease = fixture.store().read(|tx| tx.engine_lanes(&run)).unwrap()[0]
        .cleanup_lease
        .clone()
        .unwrap();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            &lease.branch,
            lease.worktree_path.to_str().unwrap(),
        ],
    );
    lease.tmux.socket_path = repo.join("absent-tmux-socket");
    let private = git(&lease.worktree_path, &["rev-parse", "--absolute-git-dir"]);
    fs::write(
        Path::new(private.trim()).join(storyhook::domain::CLEANUP_LEASE_MARKER),
        serde_json::to_vec(&lease).unwrap(),
    )
    .unwrap();
    fixture
        .store()
        .write(|tx| {
            let mut lane = tx.engine_lanes(&run)?.remove(0);
            lane.cleanup_lease = Some(lease.clone());
            tx.put_engine_lane(&lane)
        })
        .unwrap();

    // Native reset deletes this real leased branch. A Git hook, rather than the
    // removed shell-reset entrypoint, leaves an effect child in its owned group.
    // All IPC paths live outside the worktree being removed and ignore helper cwd.
    let ipc = repo.join(".git");
    let hook = ipc.join("hooks/reference-transaction");
    git(
        &repo,
        &[
            "config",
            "core.hooksPath",
            hook.parent().unwrap().to_str().unwrap(),
        ],
    );
    let child_patience = storyhook_test_support::load_grace::graced_now(Duration::from_secs(20));
    fs::write(
        ipc.join("child-patience"),
        child_patience.as_secs_f64().to_string(),
    )
    .unwrap();
    fs::write(
        &hook,
        r#"#!/usr/bin/env python3
import os, pathlib, sys, time
if sys.argv[1] != 'committed':
    sys.exit(0)
updates = [line.split() for line in sys.stdin]
if not any(len(row) == 3 and row[2] == 'refs/heads/worktree-SH-1'
           and set(row[1]) == {'0'} for row in updates):
    sys.exit(0)
root = pathlib.Path(__file__).resolve().parent.parent
leader = os.getppid()
if os.fork() != 0:
    os._exit(0)
deadline = time.monotonic() + float((root / 'child-patience').read_text())
# Git owns the hook; capture reaps that exact Git leader before checking group
# quiescence. A zombie is still present, so this barrier waits for its reaping.
# Signal zero observes only; the fixture never signals an inferred PID or group.
while time.monotonic() < deadline:
    try:
        os.kill(leader, 0)
    except ProcessLookupError:
        break
    time.sleep(0.005)
else:
    os._exit(1)
(root / 'leader-exited').touch()
while not (root / 'release-child').exists():
    if time.monotonic() >= deadline:
        os._exit(1)
    time.sleep(0.005)
(root / 'child-finished').touch()
os._exit(0)
"#,
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let ctx = fixture.ctx();
    let engine = EngineService::new(&ctx, &Native);
    let (sent, received) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| sent.send(engine.stop(&run, true)).unwrap());
        let release = Release(ipc.join("release-child"));
        let mut deadline =
            storyhook_test_support::load_grace::Patience::new(Duration::from_secs(10));
        while !ipc.join("leader-exited").exists() {
            assert!(
                !deadline.expired(),
                "{deadline}; native Git cleanup did not reach its child barrier"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            matches!(
                received.recv_timeout(Duration::from_millis(500)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "reset returned while its effect child remained alive"
        );
        assert!(
            fixture
                .store()
                .read(|tx| tx.engine_reset(fixture.project(), StoryNo::new(1)))
                .unwrap()
                .is_some()
        );
        assert!(
            fixture
                .store()
                .write(|tx| tx
                    .set_checkout_path(fixture.project(), Some(&fixture.cwd().join("retargeted"))))
                .is_err()
        );
        assert_eq!(
            fixture
                .store()
                .read(|tx| Ok(tx.story(fixture.project(), StoryNo::new(1))?.unwrap().state))
                .unwrap(),
            "in-progress"
        );
        drop(release);
        assert_eq!(
            received
                .recv_timeout(storyhook_test_support::load_grace::graced_now(
                    Duration::from_secs(10)
                ))
                .unwrap()
                .unwrap()
                .run
                .state,
            EngineRunState::Finished
        );
    });
    assert!(ipc.join("child-finished").exists());
    assert!(!lease.worktree_path.exists());
    assert!(
        git(&repo, &["branch", "--list", &lease.branch])
            .trim()
            .is_empty()
    );
    assert!(
        fixture
            .store()
            .read(|tx| tx.engine_reset(fixture.project(), StoryNo::new(1)))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fixture
            .store()
            .read(|tx| Ok(tx.story(fixture.project(), StoryNo::new(1))?.unwrap().state))
            .unwrap(),
        "todo"
    );
}
