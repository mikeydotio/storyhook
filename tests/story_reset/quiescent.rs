//! A completed Git leader cannot release card reset authority while its hook child survives.
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use storyhook::service::story_reset::StoryResetService;
use storyhook::service::{NewStoryInput, StoryService};
use storyhook::store::{ReadOps, Store, StoryNo, WriteOps};
use storyhook_test_support::ServiceFixture;

struct Release(PathBuf);
impl Drop for Release {
    fn drop(&mut self) {
        fs::write(&self.0, "release").expect("release the Git effect child");
    }
}

fn git(repo: &Path, args: &[&str]) {
    let output = storyhook::env::git_env::command(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

#[test]
fn successful_git_waits_for_its_effect_child_before_completing_card_reset() {
    let fixture = ServiceFixture::new();
    let scratch = storyhook_test_support::scratch_dir();
    let repo = scratch.path().canonicalize().unwrap();
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
    let worktree = repo.join(".codex/worktrees/SH-1");
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
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), Some(&repo)))
        .unwrap();
    let ctx = fixture.ctx();
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Wait for the cleanup child".into(),
            state: Some("in-progress".into()),
            ..Default::default()
        })
        .unwrap();
    let hook = repo.join(".git/hooks/reference-transaction");
    git(
        &repo,
        &[
            "config",
            "core.hooksPath",
            hook.parent().unwrap().to_str().unwrap(),
        ],
    );
    fs::write(
        &hook,
        r#"#!/usr/bin/env python3
import os, pathlib, sys, time
if sys.argv[1] != 'committed':
    sys.exit(0)
root = pathlib.Path(__file__).resolve().parent.parent
leader = os.getppid()
if os.fork() != 0:
    os._exit(0)
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
"#,
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let service = StoryResetService::new(&ctx);
    let reset = service.reserve(&story.id, &story.id).unwrap();
    let (sent, received) = mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            sent.send(service.execute(&story.id, &reset.token, || Ok(())))
                .unwrap()
        });
        let release = Release(repo.join(".git/release-child"));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !repo.join(".git/leader-exited").exists() {
            assert!(
                Instant::now() < deadline,
                "Git cleanup did not reach its child barrier"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            matches!(
                received.recv_timeout(Duration::from_millis(500)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "card reset returned while its effect child remained alive"
        );
        assert!(!service.get(&story.id, &reset.token).unwrap().completed);
        assert!(
            fixture
                .store()
                .write(|tx| tx.set_checkout_path(fixture.project(), Some(&repo.join("retargeted"))))
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
        assert!(
            received
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap()
                .completed
        );
    });
    assert!(repo.join(".git/child-finished").exists());
    assert!(!worktree.exists());
    assert_eq!(
        fixture
            .store()
            .read(|tx| Ok(tx.story(fixture.project(), StoryNo::new(1))?.unwrap().state))
            .unwrap(),
        "todo"
    );
}
