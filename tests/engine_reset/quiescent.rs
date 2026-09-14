//! A successful reset leader cannot release authority while its effect child survives.
use super::*;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use storyhook::service::engine::ShellDispatcher;

struct Release(PathBuf);
impl Drop for Release {
    fn drop(&mut self) {
        fs::write(&self.0, "release").expect("release the reset effect child");
    }
}

#[test]
fn successful_helper_waits_for_its_effect_child_before_releasing_reset_authority() {
    let fixture = ServiceFixture::new();
    let run = setup(&fixture, &FakeDispatcher::default(), "todo");
    let script = fixture.env().home().join("reset-child.sh");
    fs::write(
        &script,
        r#"#!/usr/bin/env bash
exec python3 - <<'PY'
import json, os, pathlib, time
root = pathlib.Path.cwd()
request = json.loads(os.environ['STORYHOOK_ENGINE_RESET_V1'])
leader = os.getpid()
if os.fork() == 0:
    deadline = time.monotonic() + 20
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
print(json.dumps({'ok': True, 'token': request['token'], 'lease': request['lease'],
    'postconditions': {key: True for key in ['tmux_story_windows_absent',
        'worktree_registration_absent', 'worktree_path_absent', 'branch_absent']}}), flush=True)
os._exit(0)
PY
"#,
    )
    .unwrap();
    let dispatcher = ShellDispatcher::new(script, fixture.env().clone());
    let ctx = fixture.ctx();
    let engine = EngineService::new(&ctx, &dispatcher);
    let (sent, received) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| sent.send(engine.stop(&run, true)).unwrap());
        let release = Release(fixture.env().home().join("release-child"));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !fixture.env().home().join("leader-exited").exists() {
            assert!(
                Instant::now() < deadline,
                "reset helper did not reach its child barrier"
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
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap()
                .run
                .state,
            EngineRunState::Finished
        );
    });
    assert!(fixture.env().home().join("child-finished").exists());
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
