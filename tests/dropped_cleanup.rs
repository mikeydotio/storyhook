//! Dropped stories release their workspace without discarding committed work.
use std::fs;
use storyhook::domain::{
    CLEANUP_LEASE_MARKER, CLEANUP_LEASE_VERSION, StoryCleanupLease, StoryEvent, TmuxCleanupTarget,
};
use storyhook::service::{CleanupService, NewStoryInput, StoryService};
use storyhook::store::{ReadOps, Store, StoryNo, WriteOps};
use storyhook_test_support::{ServiceFixture, StoryWorkspace};

struct Dropped {
    fixture: ServiceFixture,
    workspace: StoryWorkspace,
    lease: StoryCleanupLease,
}

#[test]
fn direct_completion_before_submission_releases_workspace_but_retains_work() {
    let f = Dropped::new();
    let ctx = f.fixture.ctx();
    StoryService::new(&ctx).reopen(&f.lease.story_id).unwrap();
    StoryService::new(&ctx)
        .set_state(&f.lease.story_id, "done", None, None, None)
        .unwrap();
    let report = CleanupService::new(&ctx).run(false).unwrap();
    assert_eq!(report.removed.len(), 1, "{report:?}");
    assert!(!f.workspace.worktree.exists());
    assert!(f.workspace.local_branch_exists());
    assert!(f.workspace.origin_has_branch());
}

#[test]
fn closure_worker_runs_with_verification_and_periodic_cleanup_disabled() {
    let f = Dropped::new();
    let ctx = f.fixture.ctx();
    let stories = StoryService::new(&ctx);
    stories.reopen(&f.lease.story_id).unwrap();
    stories
        .set_labels(
            &f.lease.story_id,
            &["human-only".into(), "no-auto".into()],
            &[],
        )
        .unwrap();
    stories
        .set_state(&f.lease.story_id, "dropped", None, None, None)
        .unwrap();
    f.fixture
        .store()
        .write(|tx| {
            let mut settings = tx.settings(f.fixture.project())?;
            settings.cleanup_auto = Some(false);
            tx.put_verification_enabled(f.fixture.project(), false)?;
            tx.put_settings(f.fixture.project(), &settings)
        })
        .unwrap();
    storyhook::daemon::cleanup::tick_closures(f.fixture.store(), f.fixture.env()).unwrap();
    assert!(!f.workspace.worktree.exists());
    assert!(f.workspace.local_branch_exists());
    let request = f
        .fixture
        .store()
        .read(|tx| tx.closure_cleanup(f.fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert!(request.completed);
    storyhook::daemon::cleanup::tick_closures(f.fixture.store(), f.fixture.env()).unwrap();
    assert_eq!(
        request,
        f.fixture
            .store()
            .read(|tx| tx.closure_cleanup(f.fixture.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
    );
}

#[test]
fn failed_closure_cleanup_is_durable_and_comments_do_not_repeat() {
    let f = Dropped::new();
    fs::write(f.workspace.worktree.join("uncommitted"), "preserve this").unwrap();
    let first = CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
    assert!(first.skipped.iter().any(|i| i.reason == "dirty-worktree"));
    let request = f
        .fixture
        .store()
        .read(|tx| tx.closure_cleanup(f.fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert!(!request.completed);
    assert!(request.retry_at.is_some());
    assert!(request.detail.as_ref().unwrap().contains("dirty-worktree"));
    let before = f
        .fixture
        .store()
        .read(|tx| tx.events_for(f.fixture.project(), StoryNo::new(1)))
        .unwrap()
        .len();
    CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
    let after = f
        .fixture
        .store()
        .read(|tx| tx.events_for(f.fixture.project(), StoryNo::new(1)))
        .unwrap()
        .len();
    assert_eq!(before, after);
    assert!(f.workspace.worktree.join("uncommitted").exists());
}

impl Dropped {
    fn new() -> Self {
        Self::with_merge(false)
    }

    fn with_merge(merged: bool) -> Self {
        let fixture = ServiceFixture::new();
        let id = StoryService::new(&fixture.ctx())
            .create(&NewStoryInput {
                title: "Abandoned work".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        let workspace = StoryWorkspace::new(&id, merged);
        fixture
            .store()
            .write(|tx| tx.set_checkout_path(fixture.project(), Some(&workspace.checkout)))
            .unwrap();
        let lease = StoryCleanupLease {
            version: CLEANUP_LEASE_VERSION,
            project_slug: "fixture".into(),
            story_id: id.clone(),
            repository_path: workspace.checkout.clone(),
            worktree_path: workspace.worktree.clone(),
            branch: workspace.branch.clone(),
            tmux: TmuxCleanupTarget {
                revivify: None,
                socket_path: workspace.root.path().join("tmux.sock"),
            },
        };
        fs::write(
            workspace.worktree_git_dir().join(CLEANUP_LEASE_MARKER),
            serde_json::to_vec(&lease).unwrap(),
        )
        .unwrap();
        StoryService::new(&fixture.ctx())
            .set_state(&id, "dropped", None, None, None)
            .unwrap();
        Self {
            fixture,
            workspace,
            lease,
        }
    }
}

#[test]
fn dropped_before_submission_removes_worktree_and_retains_unmerged_branch() {
    let f = Dropped::new();
    let preview = CleanupService::new(&f.fixture.ctx()).run(true).unwrap();
    assert_eq!(preview.removed.len(), 1, "{preview:?}");
    assert!(f.workspace.worktree.exists());
    let report = CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
    assert_eq!(report.removed.len(), 1, "{report:?}");
    assert_eq!(report.removed[0].story_id, f.lease.story_id);
    assert!(!f.workspace.worktree.exists());
    assert!(f.workspace.local_branch_exists());
    assert!(f.workspace.origin_has_branch());
    let again = CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
    assert!(again.removed.is_empty(), "{again:?}");
    assert!(again.failed.is_empty(), "{again:?}");
    assert!(again.skipped.is_empty(), "{again:?}");
}

#[test]
fn dropped_dirty_worktree_is_preserved() {
    let f = Dropped::new();
    fs::write(f.workspace.worktree.join("uncommitted"), "keep me").unwrap();
    let report = CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
    assert!(report.removed.is_empty(), "{report:?}");
    assert!(
        report.skipped.iter().any(|s| s.reason == "dirty-worktree"),
        "{report:?}"
    );
    assert_eq!(
        fs::read_to_string(f.workspace.worktree.join("uncommitted")).unwrap(),
        "keep me"
    );
}

fn git(f: &Dropped, args: &[&str]) -> String {
    let output = storyhook::env::git_env::command(&f.workspace.checkout)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

struct Terminal(std::path::PathBuf);
impl Terminal {
    fn call(&self, args: &[&str]) -> String {
        let output = std::process::Command::new("tmux")
            .arg("-S")
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().into()
    }
    fn start(f: &Dropped) -> Self {
        let terminal = Self(f.lease.tmux.socket_path.clone());
        terminal.call(&[
            "new-session",
            "-d",
            "-s",
            "test",
            "-n",
            "OTHER",
            "sleep 120",
        ]);
        terminal.call(&[
            "new-window",
            "-n",
            &f.lease.story_id,
            "-c",
            f.workspace.worktree.to_str().unwrap(),
            "sleep 120",
        ]);
        terminal
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = std::process::Command::new("tmux")
            .arg("-S")
            .arg(&self.0)
            .arg("kill-server")
            .output();
    }
}

#[test]
fn exact_window_and_worktree_are_removed_while_other_window_survives() {
    let f = Dropped::new();
    let terminal = Terminal::start(&f);
    let preview = CleanupService::new(&f.fixture.ctx()).run(true).unwrap();
    assert_eq!(preview.removed.len(), 1, "{preview:?}");
    assert!(preview.removed[0].removed_tmux_window);
    assert_eq!(
        terminal
            .call(&["list-windows", "-a", "-F", "#{window_name}"])
            .lines()
            .count(),
        2
    );
    assert!(
        f.fixture
            .store()
            .read(|tx| tx.dropped_cleanup(f.fixture.project(), StoryNo::new(1)))
            .unwrap()
            .is_none()
    );
    let report = CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
    assert_eq!(report.removed.len(), 1, "{report:?}");
    assert!(report.removed[0].removed_tmux_window);
    assert!(report.removed[0].retained_local_branch);
    assert_eq!(
        terminal.call(&["list-windows", "-a", "-F", "#{window_name}"]),
        "OTHER"
    );
    assert!(!f.workspace.worktree.exists());
    assert!(f.workspace.local_branch_exists());
    let again = CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
    assert!(
        again.removed.is_empty() && again.failed.is_empty() && again.skipped.is_empty(),
        "{again:?}"
    );
}

/// An uncaught helper exception is a traceback that names the helper's own
/// files. Run from a directory that changed on every attempt, each retry of
/// the same failure read as a new one and posted another comment: more than
/// 240 on each of SH-838 and SH-840 (SH-881).
#[test]
fn a_helper_crash_is_reported_once_across_retries() {
    let f = Dropped::new();
    let _terminal = Terminal::start(&f);
    let request = |f: &Dropped| {
        f.fixture
            .store()
            .read(|tx| tx.closure_cleanup(f.fixture.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
    };
    // The helper reads a process journal that is not a record before it
    // signals anything, and raises a TypeError that it does not catch.
    let journals = f.fixture.env().daemon_state_dir().join("dropped-cleanup");
    let token = request(&f).token;
    fs::create_dir_all(&journals).unwrap();
    fs::write(journals.join(format!("{token}.json")), "[]").unwrap();
    for _ in 0..2 {
        let report = CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
        assert!(report.removed.is_empty(), "{report:?}");
        assert!(!journals.join(format!("{token}.bundle")).exists());
    }
    let detail = request(&f).detail.unwrap();
    assert!(
        detail.contains("TypeError") && detail.contains("dropped-cleanup-pane.py"),
        "{detail}"
    );
    let comments = f
        .fixture
        .store()
        .read(|tx| tx.events_for(f.fixture.project(), StoryNo::new(1)))
        .unwrap()
        .into_iter()
        .filter(|event| {
            matches!(event.known(), Some(StoryEvent::StoryCommentAdded { text, .. })
                if text.starts_with("STORY RESOURCE CLEANUP REQUIRED"))
        })
        .count();
    assert_eq!(comments, 1, "{detail}");
    assert!(f.workspace.worktree.exists());
}

#[test]
fn dirty_work_does_not_even_close_the_terminal() {
    let f = Dropped::new();
    let terminal = Terminal::start(&f);
    fs::write(f.workspace.worktree.join("work"), "unfinished").unwrap();
    let report = CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
    assert!(report.removed.is_empty(), "{report:?}");
    assert!(
        terminal
            .call(&["list-windows", "-a", "-F", "#{window_name}"])
            .contains(&f.lease.story_id)
    );
}

#[test]
fn duplicate_windows_and_inactive_panes_are_preserved() {
    for split in [false, true] {
        let f = Dropped::new();
        let terminal = Terminal::start(&f);
        if split {
            terminal.call(&[
                "split-window",
                "-t",
                &format!("test:{}", f.lease.story_id),
                "-c",
                f.workspace.worktree.to_str().unwrap(),
                "sleep 120",
            ]);
        } else {
            terminal.call(&[
                "new-window",
                "-n",
                &f.lease.story_id,
                "-c",
                f.workspace.worktree.to_str().unwrap(),
                "sleep 120",
            ]);
        }
        let report = CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
        assert!(
            report.removed.is_empty() && !report.skipped.is_empty(),
            "{report:?}"
        );
        assert!(f.workspace.worktree.exists());
        assert_eq!(terminal.call(&["list-panes", "-a"]).lines().count(), 3);
    }
}

#[test]
fn cleanup_is_local_and_does_not_require_origin() {
    let f = Dropped::new();
    git(&f, &["remote", "remove", "origin"]);
    let report = CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
    assert_eq!(report.removed.len(), 1, "{report:?}");
    assert!(f.workspace.local_branch_exists());
}

#[test]
fn reopened_stories_and_locked_or_detached_worktrees_are_preserved() {
    for case in [
        "reopened",
        "locked",
        "detached",
        "missing-marker",
        "invalid-marker",
    ] {
        let f = Dropped::new();
        match case {
            "reopened" => {
                StoryService::new(&f.fixture.ctx())
                    .reopen(&f.lease.story_id)
                    .unwrap();
            }
            "locked" => {
                git(
                    &f,
                    &["worktree", "lock", f.workspace.worktree.to_str().unwrap()],
                );
            }
            "detached" => {
                git(
                    &f,
                    &[
                        "-C",
                        f.workspace.worktree.to_str().unwrap(),
                        "checkout",
                        "--detach",
                    ],
                );
            }
            "missing-marker" => {
                fs::remove_file(f.workspace.worktree_git_dir().join(CLEANUP_LEASE_MARKER)).unwrap()
            }
            _ => fs::write(
                f.workspace.worktree_git_dir().join(CLEANUP_LEASE_MARKER),
                "bad json",
            )
            .unwrap(),
        }
        let report = CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
        assert!(report.removed.is_empty(), "{case}: {report:?}");
        assert!(f.workspace.worktree.exists());
        assert!(f.workspace.local_branch_exists());
    }
}

#[test]
fn daemon_uses_dropped_policy_and_honors_disabled_cleanup() {
    for disabled in [false, true] {
        let f = Dropped::new();
        if disabled {
            f.fixture
                .store()
                .write(|tx| {
                    tx.put_settings(
                        f.fixture.project(),
                        &storyhook::store::ProjectSettings {
                            cleanup_auto: Some(false),
                            ..Default::default()
                        },
                    )
                })
                .unwrap();
        }
        storyhook::daemon::cleanup::tick(f.fixture.store(), f.fixture.env());
        assert_eq!(f.workspace.worktree.exists(), disabled);
        assert!(f.workspace.local_branch_exists());
    }
}

#[test]
fn active_workspace_lock_preserves_resources() {
    use fs4::FileExt;
    let f = Dropped::new();
    let directory = f.workspace.checkout.join(".git/storyhook/workspace-locks");
    fs::create_dir_all(&directory).unwrap();
    let lock = fs::File::create(directory.join(format!("{}.lock", f.lease.story_id))).unwrap();
    lock.lock_exclusive().unwrap();
    let report = CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
    assert!(
        report.skipped.iter().any(|s| s.reason == "workspace-busy"),
        "{report:?}"
    );
    assert!(f.workspace.worktree.exists());
}

#[path = "dropped_cleanup/reservations.rs"]
mod reservations;

#[test]
fn conflicting_and_misdirected_history_never_authorizes_cleanup() {
    for mismatched_story in [false, true] {
        let f = Dropped::new();
        let mut other = f.lease.clone();
        if mismatched_story {
            other.story_id = "SH-999".into();
        } else {
            other.tmux.socket_path = f.workspace.root.path().join("different.sock");
        }
        f.fixture.append_cleanup_lease(&f.lease.story_id, other);
        let report = CleanupService::new(&f.fixture.ctx()).run(false).unwrap();
        assert!(
            report.removed.is_empty() && !report.skipped.is_empty(),
            "{report:?}"
        );
        assert!(f.workspace.worktree.exists());
    }
}

#[test]
fn calling_worktree_cannot_be_removed() {
    let f = Dropped::new();
    let ctx = storyhook::service::Ctx::new(
        f.fixture.store(),
        f.fixture.project(),
        &f.workspace.worktree,
        f.fixture.env().clone(),
    )
    .no_hooks(true);
    let report = CleanupService::new(&ctx).run(false).unwrap();
    assert!(
        report
            .skipped
            .iter()
            .any(|s| s.reason == "protected-worktree"),
        "{report:?}"
    );
    assert!(f.workspace.worktree.exists());
}

#[test]
fn verified_completion_reclaims_a_live_window_and_previews_branch_removal() {
    let f = Dropped::with_merge(true);
    let ctx = f.fixture.ctx();
    let stories = StoryService::new(&ctx);
    stories.reopen(&f.lease.story_id).unwrap();
    stories
        .set_state(&f.lease.story_id, "verifying", None, None, None)
        .unwrap();
    f.fixture
        .append_cleanup_lease(&f.lease.story_id, f.lease.clone());
    stories
        .set_state(
            &f.lease.story_id,
            "done",
            Some("merged fixture"),
            None,
            None,
        )
        .unwrap();
    stories
        .comment(
            &f.lease.story_id,
            &format!("{} landed", storyhook::service::VERIFICATION_GREEN_PREFIX),
        )
        .unwrap();
    let terminal = Terminal::start(&f);
    let preview = CleanupService::new(&ctx).run(true).unwrap();
    assert_eq!(preview.removed.len(), 1, "{preview:?}");
    assert!(preview.removed[0].removed_tmux_window);
    assert!(preview.removed[0].removed_local_branch);
    assert!(f.workspace.worktree.exists());
    let report = CleanupService::new(&ctx).run(false).unwrap();
    assert_eq!(report.removed.len(), 1, "{report:?}");
    assert!(report.removed[0].removed_local_branch);
    assert!(!f.workspace.local_branch_exists());
    assert!(!f.workspace.worktree.exists());
    assert_eq!(
        terminal.call(&["list-windows", "-a", "-F", "#{window_name}"]),
        "OTHER"
    );
}
