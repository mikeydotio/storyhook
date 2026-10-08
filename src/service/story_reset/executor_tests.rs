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
        gate.set_read_timeout(Some(storyhook_test_support::load_grace::graced_now(
            Duration::from_secs(5),
        )))
        .unwrap();
        // Compute patience before fork: the child performs only async-signal-safe calls.
        let release_patience_ms = i32::try_from(
            storyhook_test_support::load_grace::graced_now(Duration::from_secs(10)).as_millis(),
        )
        .unwrap();
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
                    if libc::poll(&mut poll, 1, release_patience_ms) != 1 {
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
                .wait_within(
                    storyhook_test_support::load_grace::graced_now(Duration::from_secs(5)),
                    || { "unrelated child did not exit after exec".into() }
                )
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
fn a_stuck_worker_or_an_unwind_never_strands_the_card_while_an_unrelated_fork_survives() {
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
                title: "Retry the interrupted reset".into(),
                ..Default::default()
            })
            .unwrap();
        let service = StoryResetService::new(&ctx);
        let reset = service.reserve(&story.id, &story.id).unwrap();
        let mut fork = None;
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            service.execute(&story.id, &reset.token, || {
                fork = Some(ForkBarrier::enter());
                // A concurrent controller joins without entering cleanup.
                let concurrent = service
                    .execute(&story.id, &reset.token, || {
                        panic!("concurrent controller must stay excluded")
                    })
                    .unwrap();
                assert!(!concurrent.completed);
                if unwind {
                    panic!("controller stopped before cleanup");
                }
                Err(AppError::Validation("worker did not stop".into()))
            })
        }));
        let mut fork = fork.unwrap();
        assert!(
            !fork.spawn.as_ref().unwrap().is_finished(),
            "incidental fork must still own the copied descriptor"
        );
        if unwind {
            assert!(outcome.is_err());
            assert!(
                service
                    .execute(&story.id, &reset.token, || Ok(()))
                    .expect("controller retry must not wait for an unrelated fork to exec")
                    .completed
            );
        } else {
            let done = outcome.unwrap().unwrap();
            assert!(done.completed);
            assert!(
                done.residue
                    .iter()
                    .any(|entry| entry.reason.contains("worker did not stop")),
                "{:?}",
                done.residue
            );
        }
        fork.release();
    }
}

/// A repository whose reset runs the real artifact guard, and its story.
fn repository_story(
    fixture: &ServiceFixture,
) -> (SqliteStore, crate::store::ProjectId, std::path::PathBuf) {
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
    (store, project, repo)
}

/// Starts `script` holding the story's workspace lock, as a surviving
/// dispatch or verification child does.
fn effect(repo: &std::path::Path, id: &str, script: &str) -> ChildGuard {
    let workspace = WorkspaceLock::acquire(repo, id).unwrap();
    let mut command = Command::new("sh");
    command.args(["-c", script]).stdin(Stdio::piped());
    workspace.command(&mut command);
    ChildGuard::spawn(&mut command).unwrap()
}

#[test]
fn reset_waits_for_a_live_workspace_owner_then_takes_exclusion() {
    let fixture = ServiceFixture::new();
    let (store, project, repo) = repository_story(&fixture);
    // The reset runs the real python3 artifact guard, which must answer.
    let env = crate::env::Environment::at(fixture.env().home()).with_subprocess_patience();
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
    let mut owner = None;
    let done = service
        .execute(&story.id, &reset.token, || {
            // Exits by itself well inside the reset's workspace patience.
            owner = Some(effect(&repo, &story.id, "sleep 0.3"));
            Ok(())
        })
        .unwrap();
    assert!(done.completed);
    assert!(
        !done
            .residue
            .iter()
            .any(|entry| entry.resource == "workspace lock"),
        "the reset must have waited for exclusion: {:?}",
        done.residue
    );
    assert!(
        owner
            .unwrap()
            .wait_within(
                storyhook_test_support::load_grace::graced_now(Duration::from_secs(5)),
                || "owner did not exit".into()
            )
            .success()
    );
}

#[test]
fn reset_proceeds_without_exclusion_once_its_patience_is_spent() {
    let fixture = ServiceFixture::new();
    let (store, project, repo) = repository_story(&fixture);
    let env = crate::env::Environment::at(fixture.env().home()).with_subprocess_patience();
    let ctx = Ctx::new(&store, project, fixture.env().home(), env).no_hooks(true);
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Ignore a wedged workspace owner".into(),
            state: Some("in-progress".into()),
            ..Default::default()
        })
        .unwrap();
    let service = StoryResetService::new(&ctx).with_workspace_patience(Duration::ZERO);
    let reset = service.reserve(&story.id, &story.id).unwrap();
    let mut owner = None;
    let done = service
        .execute(&story.id, &reset.token, || {
            owner = Some(effect(&repo, &story.id, "read answer"));
            Ok(())
        })
        .unwrap();
    assert!(done.completed);
    let held = done
        .residue
        .iter()
        .find(|entry| entry.resource == "workspace lock")
        .unwrap_or_else(|| panic!("{:?}", done.residue));
    assert!(held.reason.contains("without exclusion"), "{held:?}");
    assert_eq!(
        store
            .read(|tx| Ok(tx.story(project, StoryNo::new(1))?.unwrap().state))
            .unwrap(),
        "todo"
    );
    let mut owner = owner.unwrap();
    assert!(
        owner.try_wait().is_none(),
        "reset never kills a foreign owner"
    );
    writeln!(owner.stdin().unwrap(), "finish").unwrap();
    assert!(
        owner
            .wait_within(
                storyhook_test_support::load_grace::graced_now(Duration::from_secs(5)),
                || "owner did not exit".into()
            )
            .success()
    );
}

#[test]
fn a_finish_that_keeps_failing_degrades_instead_of_stranding_the_story() {
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
            title: "Degrade the finish".into(),
            state: Some("in-progress".into()),
            ..Default::default()
        })
        .unwrap();
    let service = StoryResetService::new(&ctx);
    let reset = service.reserve(&story.id, &story.id).unwrap();
    let done = service
        .finish_degraded(
            &reset,
            None,
            &StoreError::Invariant("injected finish failure".into()),
        )
        .unwrap();
    assert!(done.completed);
    assert!(
        done.failure
            .as_deref()
            .is_some_and(|note| note.contains("injected finish failure")),
        "{done:?}"
    );
    let row = store
        .read(|tx| tx.story(project, StoryNo::new(1)))
        .unwrap()
        .unwrap();
    // Ownership is released even though the state could not change.
    assert_eq!(row.state, "in-progress");
    assert!(
        row.snapshot
            .comments
            .last()
            .unwrap()
            .text
            .contains("finished without returning the story to todo")
    );
    assert!(
        StoryService::new(&ctx)
            .set_state(&story.id, "todo", None, None, None)
            .is_ok(),
        "the completed receipt no longer reserves the story"
    );
}

#[test]
fn an_adopted_pre_upgrade_reservation_keeps_its_branch_and_only_forced_dirty_work_goes() {
    for force in [false, true] {
        let fixture = ServiceFixture::new();
        let (store, project, repo) = repository_story(&fixture);
        let env = crate::env::Environment::at(fixture.env().home()).with_subprocess_patience();
        let ctx = Ctx::new(&store, project, fixture.env().home(), env).no_hooks(true);
        let story = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: "Adopted reservation".into(),
                state: Some("in-progress".into()),
                ..Default::default()
            })
            .unwrap();
        let worktree = repo.join(".codex/worktrees").join(&story.id);
        let added = crate::env::git_env::command(&repo)
            .args(["worktree", "add", "-b", "worktree-SH-1"])
            .arg(&worktree)
            .output()
            .unwrap();
        assert!(added.status.success(), "{added:?}");
        std::fs::write(worktree.join("unfinished.txt"), "uncommitted").unwrap();
        let service = StoryResetService::new(&ctx);
        let reset = service.adopt_legacy(&story.id, force).unwrap();
        let done = service.execute(&story.id, &reset.token, || Ok(())).unwrap();
        assert!(done.completed);
        let left = |resource: &str| {
            done.residue.iter().any(|entry| {
                entry.resource == resource && entry.reason.contains("before this upgrade")
            })
        };
        assert!(
            left("local branch worktree-SH-1"),
            "force={force}: {:?}",
            done.residue
        );
        let branch = crate::service::resources::git::branch_exists(&repo, "worktree-SH-1").unwrap();
        assert!(
            branch,
            "the pre-upgrade request promised to keep its branch"
        );
        assert_eq!(
            left(&format!("worktree {}", worktree.display())),
            !force,
            "force={force}: {:?}",
            done.residue
        );
        assert_eq!(worktree.exists(), !force, "force={force}");
    }
}
