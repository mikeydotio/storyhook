//! Controller retries must not inherit unrelated fork lifetimes or bypass owned effects.
use super::*;
use crate::service::NewStoryInput;
use crate::store::SqliteStore;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::thread::JoinHandle;
use std::time::Duration;
use storyhook_test_support::{ChildGuard, ServiceFixture};

/// Keeps an unrelated child between fork and exec, where CLOEXEC has no effect yet.
struct ForkBarrier {
    gate: UnixStream,
    spawn: Option<JoinHandle<std::io::Result<ChildGuard>>>,
}

impl ForkBarrier {
    fn enter() -> Self {
        let (gate, child_gate) = UnixStream::pair().unwrap();
        gate.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let spawn = std::thread::spawn(move || {
            let mut command = Command::new("sh");
            command.args(["-c", "exit 0"]);
            // SAFETY: the child uses only async-signal-safe write, poll and read.
            // The socket remains alive in the closure until exec closes it.
            unsafe {
                command.pre_exec(move || {
                    let fd = child_gate.as_raw_fd();
                    let byte = [1_u8];
                    if libc::write(fd, byte.as_ptr().cast(), 1) != 1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    let mut poll = libc::pollfd {
                        fd,
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    if libc::poll(&mut poll, 1, 10_000) != 1 {
                        return Err(std::io::Error::from_raw_os_error(libc::ETIMEDOUT));
                    }
                    let mut release = [0_u8];
                    if libc::read(fd, release.as_mut_ptr().cast(), 1) != 1 {
                        return Err(std::io::Error::from_raw_os_error(libc::EPIPE));
                    }
                    Ok(())
                });
            }
            ChildGuard::spawn(&mut command)
        });
        let mut barrier = Self {
            gate,
            spawn: Some(spawn),
        };
        let mut ready = [0_u8];
        barrier
            .gate
            .read_exact(&mut ready)
            .expect("child reached its pre-exec barrier");
        barrier
    }

    fn release(&mut self) {
        self.gate.write_all(&[1]).unwrap();
        let mut child = self.spawn.take().unwrap().join().unwrap().unwrap();
        assert!(
            child
                .wait_within(Duration::from_secs(5), || {
                    "unrelated child did not exit after exec".into()
                })
                .success()
        );
    }
}

impl Drop for ForkBarrier {
    fn drop(&mut self) {
        if let Some(spawn) = self.spawn.take() {
            // On assertion failure, release before joining and let ChildGuard reap.
            let _ = self.gate.write_all(&[1]);
            drop(spawn.join());
        }
    }
}

#[test]
fn refusal_and_unwind_allow_immediate_card_retry_while_an_unrelated_fork_survives() {
    for unwind in [false, true] {
        let fixture = ServiceFixture::new();
        let store = SqliteStore::open(fixture.env().store_path()).unwrap();
        let project = store
            .read(|tx| Ok(tx.project_by_slug("fixture")?.unwrap().id))
            .unwrap();
        store
            .write(|tx| tx.set_checkout_path(project, None))
            .unwrap();
        let env = crate::env::Environment::at(fixture.env().home());
        let ctx = Ctx::new(&store, project, fixture.cwd(), env).no_hooks(true);
        let story = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: "Retry the refused reset".into(),
                ..Default::default()
            })
            .unwrap();
        let service = StoryResetService::new(&ctx);
        let reset = service.reserve(&story.id, &story.id).unwrap();
        let mut fork = None;
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            service.execute(&story.id, &reset.token, || {
                fork = Some(ForkBarrier::enter());
                let concurrent = service
                    .execute(&story.id, &reset.token, || {
                        panic!("concurrent controller must stay excluded")
                    })
                    .unwrap_err();
                assert!(
                    concurrent.to_string().contains("already running"),
                    "{concurrent}"
                );
                if unwind {
                    panic!("controller stopped before cleanup");
                }
                Err(AppError::Validation("caller worktree refused".into()))
            })
        }));
        if unwind {
            assert!(refused.is_err());
        } else {
            let error = refused.unwrap().unwrap_err();
            assert!(
                error.to_string().contains("caller worktree refused"),
                "{error}"
            );
        }
        let mut fork = fork.unwrap();
        assert!(
            !fork.spawn.as_ref().unwrap().is_finished(),
            "incidental fork must still own the copied descriptor"
        );
        assert!(
            service
                .execute(&story.id, &reset.token, || Ok(()))
                .expect("controller retry must not wait for an unrelated fork to exec")
                .completed
        );
        fork.release();
    }
}

#[test]
fn controller_refusal_allows_retry_but_live_effect_retains_workspace_and_reservation() {
    let fixture = ServiceFixture::new();
    let repo = fixture.cwd().canonicalize().unwrap();
    let init = crate::env::git_env::command(&repo)
        .args(["init", "--initial-branch=main"])
        .output()
        .unwrap();
    assert!(init.status.success(), "{init:?}");
    storyhook_test_support::approve_fixture_identity(&repo, "Reset fixture", "reset@example.test");
    let commit = crate::env::git_env::command(&repo)
        .args([
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "base",
        ])
        .output()
        .unwrap();
    assert!(commit.status.success(), "{commit:?}");
    let store = SqliteStore::open(fixture.env().store_path()).unwrap();
    let project = store
        .read(|tx| Ok(tx.project_by_slug("fixture")?.unwrap().id))
        .unwrap();
    store
        .write(|tx| tx.set_checkout_path(project, Some(&repo)))
        .unwrap();
    let env = crate::env::Environment::at(fixture.env().home());
    let ctx = Ctx::new(&store, project, fixture.env().home(), env).no_hooks(true);
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Wait for the cleanup child".into(),
            state: Some("in-progress".into()),
            ..Default::default()
        })
        .unwrap();
    let service = StoryResetService::new(&ctx);
    let reset = service.reserve(&story.id, &story.id).unwrap();
    let mut effect = None;
    let error = service
        .execute(&story.id, &reset.token, || {
            let workspace = WorkspaceLock::acquire(&repo, &story.id)?;
            let mut command = Command::new("sh");
            command.args(["-c", "read answer"]).stdin(Stdio::piped());
            workspace.command(&mut command);
            effect = Some(ChildGuard::spawn(&mut command)?);
            Err(AppError::Storage("owned effect is still settling".into()))
        })
        .unwrap_err();
    assert!(error.to_string().contains("still settling"), "{error}");
    let mut effect = effect.unwrap();
    assert!(effect.try_wait().is_none());
    let mut retried = false;
    let error = service
        .execute(&story.id, &reset.token, || {
            retried = true;
            Ok(())
        })
        .unwrap_err();
    assert!(retried, "controller exclusion must already be released");
    assert!(error.to_string().contains("workspace is busy"), "{error}");
    assert!(!service.get(&story.id, &reset.token).unwrap().completed);
    assert_eq!(
        store
            .read(|tx| Ok(tx.story(project, StoryNo::new(1))?.unwrap().state))
            .unwrap(),
        "in-progress"
    );
    assert!(
        store
            .write(|tx| tx.set_checkout_path(project, Some(&repo.join("retargeted"))))
            .is_err()
    );
    writeln!(effect.stdin().unwrap(), "finish").unwrap();
    assert!(
        effect
            .wait_within(Duration::from_secs(5), || {
                "owned effect did not exit after release".into()
            })
            .success()
    );
    assert!(
        service
            .execute(&story.id, &reset.token, || Ok(()))
            .unwrap()
            .completed
    );
}
