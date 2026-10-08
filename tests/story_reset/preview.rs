//! SH-891: an observation must never perform or reserve a reset.
use super::workspace::{Workspace, commit, git};
use storyhook::service::story_reset::StoryResetService;
use storyhook::service::{Ctx, StoryService};
use storyhook::store::{ReadOps, Store, StoryNo};

#[test]
fn preview_counts_dirty_work_and_unique_commits_without_changing_any_resource() {
    let w = Workspace::new(true);
    std::fs::write(w.worktree.join("tracked"), "committed").unwrap();
    git(&w.worktree, &["add", "tracked"]);
    commit(&w.worktree, "local work");
    let tip = git(&w.worktree, &["rev-parse", "HEAD"]);
    std::fs::write(w.worktree.join("tracked"), "uncommitted").unwrap();
    std::fs::write(w.worktree.join("untracked\nname"), "keep me").unwrap();
    git(&w.repo, &["worktree", "lock", w.worktree.to_str().unwrap()]);
    let monitor = w.repo.join("monitor");
    std::fs::write(&monitor, "#!/bin/sh\nprintf called > \"$0.ran\"\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&monitor, std::fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &w.repo,
        &["config", "core.fsmonitor", monitor.to_str().unwrap()],
    );
    let ctx = w.fixture.ctx().no_hooks(true);
    StoryService::new(&ctx)
        .set_awaiting(&w.id, "Need a decision")
        .unwrap();
    let before = w.story();
    let private = git(&w.worktree, &["rev-parse", "--absolute-git-dir"]);
    let index_path = std::path::Path::new(private.trim()).join("index");
    let index = std::fs::read(&index_path).unwrap();
    let events = w
        .fixture
        .store()
        .read(|tx| tx.events_for(w.fixture.project(), StoryNo::new(1)))
        .unwrap();
    let preview = StoryResetService::new(&ctx)
        .preview(&w.id, &Default::default())
        .unwrap();
    assert!(
        !w.repo.join("monitor.ran").exists(),
        "preview must not invoke a configured fsmonitor"
    );
    assert_eq!(preview.story_id, w.id);
    assert_eq!(preview.original_state, "in-progress");
    assert_eq!(preview.awaiting.as_deref(), Some("Need a decision"));
    assert_eq!(preview.worktree.as_ref(), Some(&w.worktree));
    assert_eq!(preview.branch.as_deref(), Some("worktree-SH-1"));
    assert_eq!(preview.recovery.tip.as_deref(), Some(tip.trim()));
    assert_eq!(preview.recovery.dirty, Some(1));
    assert_eq!(preview.recovery.untracked, Some(1));
    assert_eq!(preview.recovery.unpushed, Some(1));
    assert!(preview.residue.is_empty(), "{:?}", preview.residue);
    assert!(preview.existing_reset.is_none());
    let after = w.story();
    assert_eq!(after.head_seq, before.head_seq);
    assert_eq!(after.awaiting, before.awaiting);
    assert_eq!(after.state, before.state);
    assert_eq!(
        w.fixture
            .store()
            .read(|tx| tx.events_for(w.fixture.project(), StoryNo::new(1)))
            .unwrap(),
        events,
        "all prior event fields are unchanged"
    );
    assert!(
        w.fixture
            .store()
            .read(|tx| tx.story_reset(w.fixture.project(), StoryNo::new(1)))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        std::fs::read(&index_path).unwrap(),
        index,
        "git status must not refresh the index"
    );
    assert_eq!(
        std::fs::read_to_string(w.worktree.join("tracked")).unwrap(),
        "uncommitted"
    );
    assert_eq!(
        std::fs::read_to_string(w.worktree.join("untracked\nname")).unwrap(),
        "keep me"
    );
    assert_eq!(git(&w.worktree, &["rev-parse", "HEAD"]), tip);
    assert!(git(&w.repo, &["worktree", "list", "--porcelain"]).contains("locked"));
}

#[test]
fn preview_excludes_commits_reachable_from_any_other_branch_tag_or_remote() {
    let w = Workspace::new(true);
    let ctx = w.fixture.ctx().no_hooks(true);
    commit(&w.worktree, "unique");
    for reference in [
        "refs/heads/saved",
        "refs/tags/saved",
        "refs/remotes/origin/saved",
    ] {
        let tip = git(&w.worktree, &["rev-parse", "HEAD"]);
        git(&w.repo, &["update-ref", reference, tip.trim()]);
        let p = StoryResetService::new(&ctx)
            .preview(&w.id, &Default::default())
            .unwrap();
        assert_eq!(p.recovery.unpushed, Some(0), "{reference}");
        git(&w.repo, &["update-ref", "-d", reference]);
    }
    assert_eq!(
        StoryResetService::new(&ctx)
            .preview(&w.id, &Default::default())
            .unwrap()
            .recovery
            .unpushed,
        Some(1)
    );
}

#[test]
fn preview_uses_execution_caller_guards_and_predicts_remote_residue() {
    let w = Workspace::new(true);
    commit(&w.worktree, "remote work");
    git(&w.worktree, &["push", "origin", "worktree-SH-1"]);
    let ctx = Ctx::new(
        w.fixture.store(),
        w.fixture.project(),
        w.worktree.clone(),
        w.fixture.env().clone(),
    )
    .no_hooks(true);
    let p = StoryResetService::new(&ctx)
        .preview(&w.id, &Default::default())
        .unwrap();
    assert!(p.worktree.is_none());
    assert!(p.branch.is_none());
    assert!(
        p.residue
            .iter()
            .any(|r| r.reason.contains("caller") && r.blocks_dispatch)
    );
    assert!(
        p.residue
            .iter()
            .any(|r| r.resource == "remote branch origin/worktree-SH-1" && r.blocks_dispatch)
    );
    assert!(w.worktree.exists());
    assert!(w.branch_exists("worktree-SH-1"));
    assert!(
        w.fixture
            .store()
            .read(|tx| tx.story_reset(w.fixture.project(), StoryNo::new(1)))
            .unwrap()
            .is_none()
    );
}

#[test]
fn preview_of_pinned_ambiguous_reset_preserves_the_receipt_and_all_candidates() {
    let w = Workspace::new(true);
    let reset = w.reserve_pinned_with(|r| {
        r.status = "ambiguous".into();
        r.diagnostics = vec!["multiple candidates".into()];
    });
    let before = serde_json::to_value(&reset).unwrap();
    let ctx = w.fixture.ctx().no_hooks(true);
    let p = StoryResetService::new(&ctx)
        .preview(&w.id, &Default::default())
        .unwrap();
    assert_eq!(p.existing_reset.as_deref(), Some(reset.token.as_str()));
    assert!(p.worktree.is_none());
    assert!(p.branch.is_none());
    assert!(
        p.residue
            .iter()
            .any(|r| r.reason.contains("ambiguous") && r.blocks_dispatch)
    );
    assert_eq!(
        serde_json::to_value(
            StoryResetService::new(&ctx)
                .get(&w.id, &reset.token)
                .unwrap()
        )
        .unwrap(),
        before
    );
    assert_eq!(w.story().state, "in-progress");
    assert!(w.worktree.exists());
}

#[test]
fn native_dry_run_preserves_story_and_reports_structured_and_human_preview() {
    let project = storyhook_test_support::TestEnv::shared()
        .project()
        .git()
        .build();
    let id = project.new_story("Observe only");
    let show = || {
        let o = project
            .story()
            .args(["show", &id, "--json"])
            .output()
            .unwrap();
        assert!(o.status.success());
        serde_json::from_slice::<serde_json::Value>(&o.stdout).unwrap()["story"].clone()
    };
    let before = show();
    for json in [true, false] {
        let mut cmd = project.story();
        cmd.args(["reset", &id, "--force", "--dry-run"]);
        if json {
            cmd.arg("--json");
        }
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        if json {
            let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(v["dry_run"], true);
            assert_eq!(v["preview"]["story_id"], id);
            assert!(v["preview"]["existing_reset"].is_null());
            assert!(v["preview"]["worktree"].is_null());
            assert!(v["preview"]["worktree_registration"].is_null());
            assert!(v["preview"]["branch"].is_null());
        } else {
            let text = String::from_utf8(out.stdout).unwrap();
            assert!(text.contains("DRY RUN story reset"));
            assert!(text.contains("no changes"));
            assert!(!text.contains("Would discard worktree"));
            assert!(!text.contains("Would delete local branch"));
        }
        assert_eq!(show(), before);
    }
}

#[test]
fn reset_preview_parser_uses_a_distinct_wire_command_that_old_daemons_cannot_execute() {
    use storyhook::cli::{Invocation, parse_invocation};
    let args = ["reset", "SH-1", "--dry-run", "--force"].map(str::to_owned);
    let request = parse_invocation(&args).unwrap();
    assert!(matches!(request, Invocation::ResetPreview { .. }));
    let wire = serde_json::to_value(&request).unwrap();
    assert!(wire.get("ResetPreview").is_some());
    assert!(
        wire.get("Reset").is_none(),
        "an old daemon must not see an executable reset"
    );
    let decoded: Invocation = serde_json::from_value(wire.clone()).unwrap();
    assert!(matches!(decoded, Invocation::ResetPreview { .. }));
    #[derive(serde::Deserialize)]
    enum OldRequest {
        Reset {
            #[serde(rename = "id")]
            _id: String,
        },
    }
    assert!(serde_json::from_value::<OldRequest>(wire).is_err());
    let plain = parse_invocation(&["reset".into(), "SH-1".into()]).unwrap();
    assert!(matches!(plain, Invocation::Reset { .. }));
}

#[test]
fn preview_keeps_an_existing_resets_original_caller_worktree_protection() {
    let w = Workspace::new(true);
    let ctx = w.fixture.ctx().no_hooks(true);
    let origin = storyhook::store::ResetOrigin {
        cwd: Some(w.worktree.clone()),
        ..Default::default()
    };
    let reset = StoryResetService::new(&ctx)
        .reserve_from(&w.id, &w.id, &origin)
        .unwrap();
    let p = StoryResetService::new(&ctx)
        .preview(&w.id, &Default::default())
        .unwrap();
    assert!(p.worktree.is_none());
    assert!(p.branch.is_none());
    assert!(p.residue.iter().any(|r| r.reason.contains("caller")));
    assert_eq!(
        StoryResetService::new(&ctx)
            .get(&w.id, &reset.token)
            .unwrap()
            .origin,
        origin
    );
    assert!(w.worktree.exists());
}

#[test]
fn preview_pending_default_origin_uses_daemon_home_not_the_later_callers_worktree() {
    let w = Workspace::new(true);
    std::fs::write(w.worktree.join("keep-until-execution"), "local work").unwrap();
    let ctx = w.fixture.ctx().no_hooks(true);
    // Dashboard reservations carry no cwd; daemon/reset.rs executes them
    // from env.home(), regardless of where a later preview is requested.
    let reset = StoryResetService::new(&ctx).reserve(&w.id, &w.id).unwrap();
    assert!(reset.origin.cwd.is_none());
    let before = serde_json::to_value(&reset).unwrap();
    let events = w
        .fixture
        .store()
        .read(|tx| tx.events_for(w.fixture.project(), StoryNo::new(1)))
        .unwrap();
    let inside = Ctx::new(
        w.fixture.store(),
        w.fixture.project(),
        &w.worktree,
        w.fixture.env().clone(),
    )
    .no_hooks(true);
    let preview = StoryResetService::new(&inside)
        .preview(&w.id, &Default::default())
        .unwrap();
    assert_eq!(preview.worktree.as_ref(), Some(&w.worktree));
    assert_eq!(preview.branch.as_deref(), Some("worktree-SH-1"));
    assert!(preview.residue.is_empty(), "{:?}", preview.residue);
    assert_eq!(
        serde_json::to_value(
            StoryResetService::new(&ctx)
                .get(&w.id, &reset.token)
                .unwrap()
        )
        .unwrap(),
        before
    );
    assert_eq!(
        w.fixture
            .store()
            .read(|tx| tx.events_for(w.fixture.project(), StoryNo::new(1)))
            .unwrap(),
        events
    );
    assert_eq!(
        std::fs::read_to_string(w.worktree.join("keep-until-execution")).unwrap(),
        "local work"
    );
    assert!(w.branch_exists("worktree-SH-1"));
    let done = w.execute_from(w.fixture.env().home(), &reset);
    assert!(done.completed);
    assert!(!w.worktree.exists());
    assert!(!w.branch_exists("worktree-SH-1"));
}

#[test]
fn preview_partial_cleanup_reports_only_remaining_resources_and_keeps_the_receipt() {
    for stale_registration in [false, true] {
        let w = Workspace::new(true);
        let reset = w.reserve_pinned();
        if stale_registration {
            std::fs::remove_dir_all(&w.worktree).unwrap();
        } else {
            git(
                &w.repo,
                &[
                    "worktree",
                    "remove",
                    "--force",
                    w.worktree.to_str().unwrap(),
                ],
            );
            git(&w.repo, &["branch", "-D", "worktree-SH-1"]);
        }
        let before = serde_json::to_value(&reset).unwrap();
        let ctx = w.fixture.ctx().no_hooks(true);
        let service = StoryResetService::new(&ctx);
        let preview = service.preview(&w.id, &Default::default()).unwrap();
        assert!(preview.worktree.is_none());
        assert_eq!(
            preview.worktree_registration.as_ref(),
            stale_registration.then_some(&w.worktree)
        );
        assert_eq!(
            preview.branch.as_deref(),
            stale_registration.then_some("worktree-SH-1")
        );
        assert!(!preview.display().contains("Would discard worktree"));
        assert_eq!(
            preview
                .display()
                .contains("Would remove stale worktree registration"),
            stale_registration
        );
        assert_eq!(
            serde_json::to_value(service.get(&w.id, &reset.token).unwrap()).unwrap(),
            before
        );
        assert!(!w.worktree.exists());
        assert_eq!(w.branch_exists("worktree-SH-1"), stale_registration);
    }
}

#[test]
fn preview_rejects_closed_and_epic_targets_without_creating_reset_receipts() {
    let fixture = storyhook_test_support::ServiceFixture::new();
    let ctx = fixture.ctx().no_hooks(true);
    let service = StoryService::new(&ctx);
    for (kind, state, expected) in [(None, "done", "closed"), (Some("epic"), "todo", "epic")] {
        let story = service
            .create(&storyhook::service::NewStoryInput {
                title: "Cannot reset".into(),
                state: Some(state.into()),
                story_type: kind.map(str::to_owned),
                ..Default::default()
            })
            .unwrap();
        let error = StoryResetService::new(&ctx)
            .preview(&story.id, &Default::default())
            .unwrap_err();
        assert!(
            error.to_string().to_lowercase().contains(expected),
            "{error}"
        );
        let number = StoryNo::parse_id("SH", &story.id).unwrap();
        assert!(
            fixture
                .store()
                .read(|tx| tx.story_reset(fixture.project(), number))
                .unwrap()
                .is_none()
        );
    }
}
