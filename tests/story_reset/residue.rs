//! A resource reset cannot prove it owns stays in place, is reported, and
//! never stops the story from returning to todo (SH-886).
use super::workspace::{Workspace, commit, git};
use storyhook::store::ResetResidue;

/// The residue entry for `resource`, which must exist.
fn entry<'a>(residue: &'a [ResetResidue], resource: &str) -> &'a ResetResidue {
    residue
        .iter()
        .find(|entry| entry.resource == resource)
        .unwrap_or_else(|| panic!("no residue for {resource}: {residue:?}"))
}

fn worktree_resource(workspace: &Workspace) -> String {
    format!("worktree {}", workspace.worktree.display())
}

/// The story is todo, and held out of dispatch only when residue collides.
fn assert_released(workspace: &Workspace, held: bool) {
    let story = workspace.story();
    assert_eq!(story.state, "todo");
    assert_eq!(
        story.awaiting.is_some(),
        held,
        "awaiting: {:?}",
        story.awaiting
    );
}

#[test]
fn a_changed_worktree_branch_is_left_with_its_branch_and_the_story_is_held() {
    let workspace = Workspace::new(true);
    let reset = workspace.reserve_pinned();
    git(&workspace.worktree, &["switch", "-c", "replacement"]);
    let done = workspace.execute(&reset);
    let left = entry(&done.residue, &worktree_resource(&workspace));
    assert!(left.reason.contains("branch changed"), "{left:?}");
    assert!(left.blocks_dispatch);
    assert!(entry(&done.residue, "local branch worktree-SH-1").blocks_dispatch);
    assert!(workspace.worktree.exists());
    assert!(workspace.branch_exists("worktree-SH-1"));
    assert_released(&workspace, true);
    assert!(
        workspace
            .story()
            .awaiting
            .unwrap()
            .contains("next dispatch would collide")
    );
}

#[test]
fn a_replaced_worktree_directory_is_never_removed() {
    let workspace = Workspace::new(true);
    let reset = workspace.reserve_pinned();
    let saved = workspace.worktree.with_extension("original");
    std::fs::rename(&workspace.worktree, &saved).unwrap();
    std::fs::create_dir(&workspace.worktree).unwrap();
    std::fs::copy(saved.join(".git"), workspace.worktree.join(".git")).unwrap();
    std::fs::write(workspace.worktree.join("keep.txt"), "not the story's").unwrap();
    let done = workspace.execute(&reset);
    let left = entry(&done.residue, &worktree_resource(&workspace));
    assert!(
        left.reason.contains("filesystem identity changed"),
        "{left:?}"
    );
    assert!(workspace.worktree.join("keep.txt").exists());
    assert!(workspace.branch_exists("worktree-SH-1"));
    assert_released(&workspace, true);
}

#[test]
fn the_callers_own_worktree_is_left_in_place() {
    let workspace = Workspace::new(true);
    let reset = workspace.reserve_pinned();
    let done = workspace.execute_from(&workspace.worktree.clone(), &reset);
    let left = entry(&done.residue, &worktree_resource(&workspace));
    assert!(left.reason.contains("caller"), "{left:?}");
    assert!(workspace.worktree.exists());
    assert!(workspace.branch_exists("worktree-SH-1"));
    assert_released(&workspace, true);
}

#[test]
fn an_unidentifiable_workspace_is_left_whole_and_holds_the_story() {
    let workspace = Workspace::new(true);
    let reset = workspace.reserve_pinned_with(|resources| {
        resources.status = "ambiguous".into();
        resources.diagnostics = vec!["two worktrees claim SH-1".into()];
    });
    let done = workspace.execute(&reset);
    // Each named resource is reported, so nothing claims a removal.
    for resource in [
        worktree_resource(&workspace),
        "local branch worktree-SH-1".into(),
    ] {
        let left = entry(&done.residue, &resource);
        assert!(left.reason.contains("two worktrees claim SH-1"), "{left:?}");
        assert!(left.blocks_dispatch, "{left:?}");
    }
    assert!(
        !workspace.last_comment().contains("Removed"),
        "{}",
        workspace.last_comment()
    );
    assert!(workspace.worktree.exists());
    assert!(workspace.branch_exists("worktree-SH-1"));
    assert_released(&workspace, true);
}

#[test]
fn an_ambiguous_identity_names_every_candidate_it_leaves() {
    let workspace = Workspace::new(true);
    let other = workspace.repo.join(".codex/worktrees/SH-1");
    let reset = workspace.reserve_pinned_with(|resources| {
        // Two registrations claim the story, so none is selected.
        let mut second = resources.candidates[0].clone();
        second.worktree = Some(other.clone());
        resources.candidates.push(second);
        resources.status = "ambiguous".into();
        resources.worktree = None;
        resources.diagnostics =
            vec!["multiple resources claim this story; none was selected".into()];
    });
    let done = workspace.execute(&reset);
    // The record names everything left, so a person knows what collides.
    for resource in [
        worktree_resource(&workspace),
        format!("worktree {}", other.display()),
        "local branch worktree-SH-1".into(),
    ] {
        let left = entry(&done.residue, &resource);
        assert!(left.reason.contains("none was selected"), "{left:?}");
        assert!(left.blocks_dispatch, "{left:?}");
    }
    assert!(workspace.worktree.exists());
    assert!(workspace.branch_exists("worktree-SH-1"));
    assert_released(&workspace, true);
}

#[test]
fn a_branch_that_is_origins_cached_default_survives_while_the_worktree_goes() {
    let workspace = Workspace::new(true);
    git(&workspace.worktree, &["push", "origin", "worktree-SH-1"]);
    git(
        &workspace.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/worktree-SH-1",
        ],
    );
    let reset = workspace.reserve_pinned();
    let done = workspace.execute(&reset);
    let left = entry(&done.residue, "local branch worktree-SH-1");
    assert!(left.reason.contains("default branch"), "{left:?}");
    assert!(left.blocks_dispatch);
    assert!(!workspace.worktree.exists());
    assert!(workspace.branch_exists("worktree-SH-1"));
    assert_released(&workspace, true);
}

#[test]
fn an_unmerged_origin_branch_holds_the_story_after_everything_local_is_removed() {
    let workspace = Workspace::new(true);
    commit(&workspace.worktree, "pushed work");
    git(&workspace.worktree, &["push", "origin", "worktree-SH-1"]);
    let reset = workspace.reserve_pinned();
    let done = workspace.execute(&reset);
    let left = entry(&done.residue, "remote branch origin/worktree-SH-1");
    assert!(left.blocks_dispatch, "{left:?}");
    assert!(left.reason.contains("rejected"), "{left:?}");
    assert_eq!(done.residue.len(), 1, "{:?}", done.residue);
    assert!(!workspace.worktree.exists());
    assert!(!workspace.branch_exists("worktree-SH-1"));
    assert_released(&workspace, true);
    assert!(
        workspace
            .story()
            .awaiting
            .unwrap()
            .contains("origin/worktree-SH-1")
    );
}

#[test]
fn a_merged_origin_branch_does_not_hold_the_story() {
    let workspace = Workspace::new(true);
    git(&workspace.worktree, &["push", "origin", "worktree-SH-1"]);
    let reset = workspace.reserve_pinned();
    let done = workspace.execute(&reset);
    assert_eq!(done.residue, vec![]);
    assert!(!workspace.worktree.exists());
    assert_released(&workspace, false);
}

#[test]
fn an_unregistered_directory_with_its_pinned_identity_is_removed() {
    let workspace = Workspace::new(true);
    let reset = workspace.reserve_pinned();
    let admin = workspace.repo.join(".git/worktrees").join(&workspace.id);
    std::fs::remove_dir_all(&admin).unwrap();
    let done = workspace.execute(&reset);
    assert_eq!(done.residue, vec![]);
    assert!(!workspace.worktree.exists());
    assert!(!workspace.branch_exists("worktree-SH-1"));
    assert_released(&workspace, false);
}

#[test]
fn a_registration_whose_directory_is_gone_is_removed() {
    let workspace = Workspace::new(true);
    let reset = workspace.reserve_pinned();
    std::fs::remove_dir_all(&workspace.worktree).unwrap();
    let done = workspace.execute(&reset);
    assert_eq!(done.residue, vec![]);
    let listed = git(&workspace.repo, &["worktree", "list", "--porcelain"]);
    assert!(!listed.contains(&workspace.id), "{listed}");
    assert!(!workspace.branch_exists("worktree-SH-1"));
    assert_released(&workspace, false);
}
