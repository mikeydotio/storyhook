//! Commit notifications persist repair publication independently of verification.
use std::path::{Path, PathBuf};
use storyhook::domain::{CLEANUP_LEASE_MARKER, StoryCleanupLease, TmuxCleanupTarget};
use storyhook::service::{Ctx, GitService, NewStoryInput, PrLinkService, StoryService};
use storyhook_test_support::ServiceFixture;

fn git(root: &Path, args: &[&str]) -> String {
    let out = storyhook::env::git_env::command(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}

fn lane(f: &ServiceFixture) -> (String, PathBuf) {
    f.github_checkout_at(f.project(), f.cwd(), "https://github.com/acme/widgets");
    git(f.cwd(), &["config", "user.name", "test"]);
    git(f.cwd(), &["config", "user.email", "test@example.com"]);
    git(f.cwd(), &["add", "."]);
    git(f.cwd(), &["commit", "-qm", "initial"]);
    let id = StoryService::new(&f.ctx())
        .create(&NewStoryInput {
            title: "repair publication".into(),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id;
    StoryService::new(&f.ctx())
        .set_state(&id, "in-progress", None, None, None)
        .unwrap();
    let worktree = f.cwd().join("lane");
    git(
        f.cwd(),
        &[
            "worktree",
            "add",
            "-b",
            "repair",
            worktree.to_str().unwrap(),
        ],
    );
    let private = PathBuf::from(git(&worktree, &["rev-parse", "--absolute-git-dir"]));
    let lease = StoryCleanupLease {
        version: 1,
        project_slug: "fixture".into(),
        story_id: id.clone(),
        repository_path: f.cwd().canonicalize().unwrap(),
        worktree_path: worktree.canonicalize().unwrap(),
        branch: "repair".into(),
        tmux: TmuxCleanupTarget {
            socket_path: f.cwd().join("tmux.sock"),
            revivify: None,
        },
    };
    std::fs::write(
        private.join(CLEANUP_LEASE_MARKER),
        serde_json::to_vec(&lease).unwrap(),
    )
    .unwrap();
    (id, worktree)
}

fn requests(f: &ServiceFixture) -> Vec<serde_json::Value> {
    let root = f.env().daemon_state_dir().join("repair-publication");
    if !root.exists() {
        return vec![];
    }
    std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())
        .collect()
}

#[test]
fn commit_sync_queues_a_dirty_repair_without_story_references_once() {
    let f = ServiceFixture::new();
    let (id, worktree) = lane(&f);
    PrLinkService::new(&f.ctx())
        .link(&id, "https://github.com/acme/widgets/pull/1", true)
        .unwrap();
    git(
        &worktree,
        &[
            "commit",
            "--allow-empty",
            "-qm",
            "repair without a story reference",
        ],
    );
    let head = git(&worktree, &["rev-parse", "HEAD"]);
    std::fs::write(worktree.join("unfinished"), "dirty").unwrap();
    let ctx = Ctx::new(f.store(), f.project(), &worktree, f.env().clone());
    GitService::new(&ctx).commit_sync(None).unwrap();
    GitService::new(&ctx).commit_sync(None).unwrap();
    let pending = requests(&f);
    assert_eq!(
        pending.len(),
        1,
        "one durable request, independent of verification admission"
    );
    assert_eq!(pending[0]["head"], head);
    assert_eq!(pending[0]["lease"]["story_id"], id);
    assert_eq!(
        pending[0]["pull_request"],
        "https://github.com/acme/widgets/pull/1"
    );
    assert!(worktree.join("unfinished").exists());
}

#[test]
fn commits_before_the_first_pr_do_not_request_publication() {
    let f = ServiceFixture::new();
    let (_, worktree) = lane(&f);
    let ctx = Ctx::new(f.store(), f.project(), &worktree, f.env().clone());
    GitService::new(&ctx).commit_sync(None).unwrap();
    assert!(requests(&f).is_empty());
}

#[test]
fn malformed_receipt_is_retained_reported_and_not_retried_on_each_tick() {
    use std::os::unix::fs::PermissionsExt;
    use storyhook::daemon::{
        repair_publication::tick_with, verification::ShellVerificationActuator,
    };
    let f = ServiceFixture::new();
    let (id, worktree) = lane(&f);
    PrLinkService::new(&f.ctx())
        .link(&id, "https://github.com/acme/widgets/pull/1", true)
        .unwrap();
    let ctx = Ctx::new(f.store(), f.project(), &worktree, f.env().clone());
    GitService::new(&ctx).commit_sync(None).unwrap();
    let helper = f.cwd().join("malformed-receipt.sh");
    let witness = f.cwd().join("helper-called");
    std::fs::write(
        &helper,
        format!(
            "#!/bin/sh\nprintf 'called\\n' >> '{}'\nprintf 'invalid receipt\\n'\n",
            witness.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let actuator = ShellVerificationActuator::with_paths(
        storyhook_test_support::subprocess_patience(f.env().clone()),
        helper,
        "/bin/true".into(),
    );
    let cancellation = storyhook::daemon::verification::VerificationCancellation::default();
    let error = tick_with(f.store(), f.env(), &actuator, &cancellation).unwrap_err();
    assert!(error.to_string().contains("invalid receipt"), "{error}");
    let pending = requests(&f);
    assert_eq!(pending.len(), 1);
    assert!(pending[0]["retry_at"].is_string());
    assert!(
        pending[0]["error"]
            .as_str()
            .unwrap()
            .contains("invalid receipt")
    );
    GitService::new(&ctx).commit_sync(None).unwrap();
    tick_with(f.store(), f.env(), &actuator, &cancellation).unwrap();
    assert_eq!(std::fs::read_to_string(witness).unwrap(), "called\n");
    assert_eq!(
        requests(&f),
        pending,
        "duplicate notification must not erase failure evidence"
    );
}

#[test]
fn reassigned_worktree_cannot_publish_an_old_stories_request() {
    use storyhook::daemon::{
        repair_publication::tick_with, verification::ShellVerificationActuator,
    };
    let f = ServiceFixture::new();
    let (id, worktree) = lane(&f);
    PrLinkService::new(&f.ctx())
        .link(&id, "https://github.com/acme/widgets/pull/1", true)
        .unwrap();
    let ctx = Ctx::new(f.store(), f.project(), &worktree, f.env().clone());
    GitService::new(&ctx).commit_sync(None).unwrap();
    let marker = PathBuf::from(git(&worktree, &["rev-parse", "--absolute-git-dir"]))
        .join(CLEANUP_LEASE_MARKER);
    let mut lease: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
    lease["story_id"] = "SH-999".into();
    std::fs::write(marker, serde_json::to_vec(&lease).unwrap()).unwrap();
    let actuator = ShellVerificationActuator::with_paths(
        storyhook_test_support::subprocess_patience(f.env().clone()),
        "/must-not-run".into(),
        "/bin/true".into(),
    );
    let error = tick_with(
        f.store(),
        f.env(),
        &actuator,
        &storyhook::daemon::verification::VerificationCancellation::default(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("worktree marker"), "{error}");
    assert_eq!(requests(&f).len(), 1);
}

#[test]
fn missing_worktree_does_not_acknowledge_an_unpublished_commit() {
    use storyhook::daemon::{
        repair_publication::tick_with, verification::ShellVerificationActuator,
    };
    let f = ServiceFixture::new();
    let (id, worktree) = lane(&f);
    PrLinkService::new(&f.ctx())
        .link(&id, "https://github.com/acme/widgets/pull/1", true)
        .unwrap();
    let ctx = Ctx::new(f.store(), f.project(), &worktree, f.env().clone());
    GitService::new(&ctx).commit_sync(None).unwrap();
    git(f.cwd(), &["worktree", "remove", worktree.to_str().unwrap()]);
    let actuator = ShellVerificationActuator::with_paths(
        storyhook_test_support::subprocess_patience(f.env().clone()),
        "/must-not-run".into(),
        "/bin/true".into(),
    );
    let error = tick_with(
        f.store(),
        f.env(),
        &actuator,
        &storyhook::daemon::verification::VerificationCancellation::default(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("without merged PR proof"),
        "{error}"
    );
    assert_eq!(requests(&f).len(), 1);
    assert!(!git(f.cwd(), &["rev-parse", "refs/heads/repair"]).is_empty());
}
