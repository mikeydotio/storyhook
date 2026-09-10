//! `story cleanup` is the verifier's retry path and nothing more (SH-653,
//! decision D-H of SH-645).
//!
//! `CleanupService::run` had no end-to-end test before this file: its unit
//! tests drive `clean_candidate` on a lease they built by hand, which is
//! exactly the layer a story-state gate sits above. Every case here reaches
//! the service through the store — the story's real state transitions, the
//! lease event the `verifying` transition writes, the comments the verifier
//! writes — so what is pinned is the gate as a caller meets it.

use std::fs;

use storyhook::daemon::cleanup::tick;
use storyhook::domain::{
    CLEANUP_LEASE_MARKER, CLEANUP_LEASE_VERSION, StoryCleanupLease, TmuxCleanupTarget,
};
use storyhook::service::{
    CleanupReport, CleanupService, NewStoryInput, StoryService,
    VERIFICATION_CLEANUP_COMPLETE_PREFIX, VERIFICATION_CLEANUP_REQUIRED_PREFIX,
};
use storyhook::store::{Store, WriteOps};
use storyhook_test_support::{ServiceFixture, StoryWorkspace, scratch_dir};

/// A merged story workspace registered as the fixture project's checkout.
struct Leased {
    fixture: ServiceFixture,
    workspace: StoryWorkspace,
    id: String,
}

impl Leased {
    /// A story created and moved to `verifying` from a leased worktree, with
    /// the workspace merged into the default branch — the state every green
    /// verification leaves behind before its reap.
    fn new() -> Self {
        let fixture = ServiceFixture::new();
        let ctx = fixture.ctx();
        let id = StoryService::new(&ctx)
            .create(&NewStoryInput {
                title: "leased".into(),
                ..NewStoryInput::default()
            })
            .unwrap()
            .id;
        let workspace = StoryWorkspace::new(&id, true);
        fixture
            .store()
            .write(|tx| tx.set_checkout_path(fixture.project(), Some(&workspace.checkout)))
            .unwrap();
        StoryService::new(&ctx)
            .set_state(&id, "in-progress", None, None, None)
            .unwrap();
        StoryService::new(&ctx)
            .set_state(&id, "verifying", None, None, None)
            .unwrap();
        let leased = Self {
            fixture,
            workspace,
            id,
        };
        // Dispatch writes the marker into the worktree's private git dir and
        // `story move <id> verifying` copies it into history; a fixture that
        // omitted either half would be a shape production never produces.
        leased.write_marker(&leased.lease());
        leased
            .fixture
            .append_cleanup_lease(&leased.id, leased.lease());
        leased
    }

    fn write_marker(&self, lease: &StoryCleanupLease) {
        let marker = self.workspace.worktree_git_dir().join(CLEANUP_LEASE_MARKER);
        fs::write(&marker, serde_json::to_vec(lease).unwrap()).unwrap();
    }

    fn lease(&self) -> StoryCleanupLease {
        StoryCleanupLease {
            version: CLEANUP_LEASE_VERSION,
            project_slug: "fixture".into(),
            story_id: self.id.clone(),
            repository_path: self.workspace.checkout.clone(),
            worktree_path: self.workspace.worktree.clone(),
            branch: self.workspace.branch.clone(),
            tmux: TmuxCleanupTarget {
                socket_path: self.workspace.root.path().join("no-tmux.sock"),
            },
        }
    }

    fn move_to(&self, state: &str) {
        StoryService::new(&self.fixture.ctx())
            .set_state(&self.id, state, None, None, None)
            .unwrap();
    }

    fn comment(&self, text: &str) {
        StoryService::new(&self.fixture.ctx())
            .comment(&self.id, text)
            .unwrap();
    }

    fn run(&self, dry_run: bool) -> CleanupReport {
        CleanupService::new(&self.fixture.ctx())
            .run(dry_run)
            .unwrap()
    }
}

fn skip_reasons(report: &CleanupReport) -> Vec<(String, String)> {
    report
        .skipped
        .iter()
        .map(|skip| (skip.story_id.clone(), skip.reason.clone()))
        .collect()
}

#[test]
fn an_open_story_is_refused_before_any_git_work() {
    // The lease names a repository that does not exist. Ungated, cleanup's
    // first act on the candidate is to resolve it and report
    // `repository-unavailable`; the gate must answer first, from the store.
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "open".into(),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id;
    let checkout = scratch_dir();
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), Some(checkout.path())))
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&id, "in-progress", None, None, None)
        .unwrap();
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    let missing = checkout.path().join("never-created");
    fixture.append_cleanup_lease(
        &id,
        StoryCleanupLease {
            version: CLEANUP_LEASE_VERSION,
            project_slug: "fixture".into(),
            story_id: id.clone(),
            repository_path: missing.join("repo"),
            worktree_path: missing.join(&id),
            branch: format!("worktree-{id}"),
            tmux: TmuxCleanupTarget {
                socket_path: missing.join("tmux.sock"),
            },
        },
    );

    let report = CleanupService::new(&ctx).run(false).unwrap();

    assert_eq!(report.candidates, 1);
    assert!(report.removed.is_empty());
    assert!(report.failed.is_empty(), "{:?}", report.failed);
    assert_eq!(
        skip_reasons(&report),
        vec![(id.clone(), "story-open".to_string())]
    );
    assert!(
        report.skipped[0].detail.contains("verifying"),
        "the refusal names the state the story is in: {}",
        report.skipped[0].detail
    );
}

#[test]
fn a_closed_story_the_verifier_never_marked_is_refused() {
    let leased = Leased::new();
    leased.move_to("done");

    let report = leased.run(false);

    assert_eq!(
        skip_reasons(&report),
        vec![(leased.id.clone(), "not-verifier-released".to_string())]
    );
    assert!(leased.workspace.worktree.exists());
    assert!(leased.workspace.local_branch_exists());
}

#[test]
fn a_cleanup_required_marker_releases_the_workspace_and_dry_run_previews_it() {
    let leased = Leased::new();
    leased.move_to("done");
    leased.comment(&format!(
        "{VERIFICATION_CLEANUP_REQUIRED_PREFIX} the PR landed and the story is done, but automatic reap failed: tmux window open"
    ));

    let preview = leased.run(true);
    assert!(preview.dry_run);
    assert_eq!(preview.removed.len(), 1, "{preview:?}");
    assert_eq!(preview.removed[0].story_id, leased.id);
    assert!(preview.removed[0].removed_worktree);
    assert!(preview.removed[0].removed_local_branch);
    assert!(
        leased.workspace.worktree.exists(),
        "a dry run removes nothing"
    );
    assert!(leased.workspace.local_branch_exists());

    let real = leased.run(false);
    assert_eq!(real.removed.len(), 1, "{real:?}");
    assert!(!leased.workspace.worktree.exists());
    assert!(!leased.workspace.local_branch_exists());
}

#[test]
fn a_retry_that_finds_nothing_left_is_reported_as_already_clean_never_as_a_removal() {
    let leased = Leased::new();
    leased.move_to("done");
    leased.comment(&format!(
        "{VERIFICATION_CLEANUP_REQUIRED_PREFIX} reap failed: x"
    ));
    let first = leased.run(false);
    assert_eq!(first.removed.len(), 1, "{first:?}");

    let second = leased.run(false);

    assert_eq!(second.candidates, 1);
    assert!(second.removed.is_empty(), "{second:?}");
    assert_eq!(
        skip_reasons(&second),
        vec![(leased.id.clone(), "already-clean".to_string())]
    );
}

#[test]
fn a_verified_absent_workspace_is_not_a_candidate_unless_something_came_back() {
    let leased = Leased::new();
    leased.move_to("done");
    leased.comment(&format!(
        "{VERIFICATION_CLEANUP_COMPLETE_PREFIX} exact leased worktree, branch, and agent window were verified absent."
    ));
    // The verifier's own reap has already run; nothing is on disk.
    let reaped = leased.run(false);
    assert_eq!(reaped.removed.len(), 1, "{reaped:?}");

    let quiet = leased.run(true);
    assert!(quiet.removed.is_empty(), "{quiet:?}");
    assert!(
        quiet.skipped.is_empty(),
        "a story the verifier verified absent must not be listed on every pass: {:?}",
        quiet.skipped
    );

    // A worktree recreated on the same branch after the reap is the case
    // COMPLETE stays in scope for.
    let output = std::process::Command::new("git")
        .args([
            "worktree",
            "add",
            &leased.workspace.worktree.to_string_lossy(),
            "-b",
            &leased.workspace.branch,
            "dev",
        ])
        .current_dir(&leased.workspace.checkout)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let back = leased.run(true);
    assert_eq!(back.removed.len(), 1, "{back:?}");
}

#[test]
fn a_stale_complete_marker_from_an_earlier_generation_does_not_release_the_next() {
    let leased = Leased::new();
    leased.move_to("done");
    leased.comment(&format!(
        "{VERIFICATION_CLEANUP_COMPLETE_PREFIX} exact leased worktree, branch, and agent window were verified absent."
    ));
    // The story is reopened, re-verified from the same worktree, and closed
    // again; the verifier has not reaped this generation yet.
    StoryService::new(&leased.fixture.ctx())
        .reopen(&leased.id)
        .unwrap();
    leased.move_to("verifying");
    leased
        .fixture
        .append_cleanup_lease(&leased.id, leased.lease());
    leased.move_to("done");

    let report = leased.run(false);

    assert_eq!(
        skip_reasons(&report),
        vec![(leased.id.clone(), "not-verifier-released".to_string())]
    );
    assert!(leased.workspace.worktree.exists());
}

#[test]
fn a_disk_marker_for_a_story_this_project_does_not_have_is_refused() {
    // The worktree's private marker names a story id nothing in the store
    // knows. The disk source is gated exactly like history: no story, no
    // release — and no git work on its behalf.
    let leased = Leased::new();
    let mut misnamed = leased.lease();
    misnamed.story_id = "SH-999".into();
    leased.write_marker(&misnamed);

    let report = leased.run(false);

    let reasons = skip_reasons(&report);
    assert!(
        reasons.contains(&("SH-999".to_string(), "unknown-story".to_string())),
        "{reasons:?}"
    );
    assert!(
        reasons.contains(&(leased.id.clone(), "story-open".to_string())),
        "{reasons:?}"
    );
    assert!(leased.workspace.worktree.exists());
}

#[test]
fn the_daemon_runs_the_same_gate() {
    let leased = Leased::new();
    leased.move_to("done");
    let env = leased.fixture.env().clone();

    tick(leased.fixture.store(), &env);

    assert!(leased.workspace.worktree.exists());
    assert!(leased.workspace.local_branch_exists());
}
