//! Real Git cleanup retains workspace exclusion after its reset executor dies.
use fs4::FileExt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use storyhook::service::story_reset::StoryResetService;
use storyhook::service::{Ctx, NewStoryInput, StoryService};
use storyhook::store::{ReadOps, SqliteStore, Store, WriteOps};
use storyhook_test_support::{ChildGuard, ServiceFixture};

fn git(repo: &std::path::Path, args: &[&str]) {
    let result = storyhook::env::git_env::command(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn orphaned_git_cleanup_keeps_the_card_reservation_exclusive_until_retry() {
    if let Some(home) = std::env::var_os("SH718_CARD_ORPHAN_HOME") {
        let env = storyhook::env::Environment::at(home);
        let store = SqliteStore::open(env.store_path()).unwrap();
        let project = store
            .read(|tx| Ok(tx.project_by_slug("fixture")?.unwrap().id))
            .unwrap();
        let ctx = Ctx::new(&store, project, env.home(), env.clone()).no_hooks(true);
        let service = StoryResetService::new(&ctx);
        let reset = service.reserve("SH-1", "SH-1").unwrap();
        service.execute("SH-1", &reset.token, || Ok(())).unwrap();
        return;
    }

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
            title: "Recover an orphaned cleanup child".into(),
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
import pathlib
import sys
import time
root = pathlib.Path(__file__).resolve().parent.parent
if sys.argv[1] == 'prepared':
    (root / 'reset-hook-entered').touch()
    deadline = time.monotonic() + 20
    while not (root / 'reset-hook-release').exists():
        if time.monotonic() >= deadline:
            sys.exit(1)
        time.sleep(0.01)
"#,
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    let mut worker = ChildGuard::spawn(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "orphan::orphaned_git_cleanup_keeps_the_card_reservation_exclusive_until_retry",
                "--nocapture",
            ])
            .env("SH718_CARD_ORPHAN_HOME", fixture.env().home())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit()),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while !repo.join(".git/reset-hook-entered").exists() {
        assert!(
            worker.try_wait().is_none(),
            "reset executor exited before the real Git hook"
        );
        assert!(
            Instant::now() < deadline,
            "Git branch cleanup never reached its hook"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !worktree.exists(),
        "the production reset must reach branch cleanup after worktree removal"
    );
    worker.kill_and_reap();
    assert!(
        worker.try_wait().is_some(),
        "reset executor survived forced termination"
    );
    let release = repo.join(".git/reset-hook-release");
    let cleanup_result = || {
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(repo.join(format!(".git/storyhook/workspace-locks/{}.lock", story.id)))
            .unwrap();
        assert!(
            lock.try_lock_exclusive().is_err(),
            "orphaned Git child lost workspace ownership"
        );
        let service = StoryResetService::new(&ctx);
        let reset = service.reserve(&story.id, &story.id).unwrap();
        let error = service
            .execute(&story.id, &reset.token, || Ok(()))
            .unwrap_err();
        assert!(error.to_string().contains("workspace is busy"), "{error}");
        assert!(!service.get(&story.id, &reset.token).unwrap().completed);
        fs::write(&release, "continue").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while lock.try_lock_exclusive().is_err() {
            assert!(
                Instant::now() < deadline,
                "orphaned Git never released workspace ownership"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(lock);
        assert!(
            service
                .execute(&story.id, &reset.token, || Ok(()))
                .unwrap()
                .completed
        );
    };
    // Let the real hook exit even if an assertion fails; no orphan survives the fixture.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(cleanup_result));
    fs::write(release, "continue").unwrap();
    if let Err(error) = result {
        std::panic::resume_unwind(error);
    }
}
