//! Real resources change after an injected failed child, before cleanup retries.
use super::*;
use crate::service::tmux_target::tests::Fixture;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Removal {
    Worktree,
    Branch,
    Window,
}

type Failure = (Removal, Box<dyn FnOnce()>);
thread_local! {
    // A single child failure on this test thread. No environment switch and
    // no release-build seam: every subsequent attempt uses the real actuator.
    static FAILED_CHILD: RefCell<Option<Failure>> = const { RefCell::new(None) };
}

struct FailureGuard;
impl Drop for FailureGuard {
    fn drop(&mut self) {
        FAILED_CHILD.with(|slot| *slot.borrow_mut() = None);
    }
}

fn fail_first_child(kind: Removal, after_failure: impl FnOnce() + 'static) -> FailureGuard {
    FAILED_CHILD.with(|slot| {
        assert!(slot.borrow().is_none());
        *slot.borrow_mut() = Some((kind, Box::new(after_failure)));
    });
    FailureGuard
}

pub(super) fn before_removal(kind: Removal) -> Result<(), AppError> {
    let replace = FAILED_CHILD.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|(selected, _)| *selected == kind) {
            slot.take().map(|(_, callback)| callback)
        } else {
            None
        }
    });
    if let Some(replace) = replace {
        // Model a child returning an error without changing its target, with
        // another owner replacing that target before the next retry. The real
        // child is used on retries, so missing proof would destroy replacement.
        replace();
        return Err(AppError::Validation("injected first child failure".into()));
    }
    Ok(())
}

fn checked_git(root: &Path, args: &[&str]) -> String {
    git::text(root, args).unwrap()
}

fn repository(fixture: &Fixture) {
    checked_git(fixture.env.home(), &["init", "-b", "main"]);
    storyhook_test_support::approve_fixture_identity(
        fixture.env.home(),
        "Reset retry fixture",
        "reset-retry@example.test",
    );
    checked_git(
        fixture.env.home(),
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "base",
        ],
    );
}

fn absent_window_report(fixture: &Fixture) -> ResourceReport {
    ResourceReport {
        location_only: false,
        project: "fixture".into(),
        story_id: "SH-1".into(),
        status: "resolved".into(),
        repository: Some(fixture.env.home().into()),
        worktree: None,
        branch: Some("worktree-SH-1".into()),
        window_name: "SH-1".into(),
        socket_path: Some(fixture.env.home().join("unmanaged-absent-socket")),
        pane: None,
        provider: None,
        candidates: vec![],
        observations: vec![],
        diagnostics: vec![],
    }
}

#[test]
fn sh890_worktree_retry_preserves_replacement_with_the_same_marker_and_branch_tip() {
    let fixture = Fixture::new();
    repository(&fixture);
    let worktree = fixture.env.home().join("lane");
    checked_git(
        fixture.env.home(),
        &[
            "worktree",
            "add",
            "-b",
            "worktree-SH-1",
            worktree.to_str().unwrap(),
        ],
    );
    let private = checked_git(&worktree, &["rev-parse", "--absolute-git-dir"]);
    let marker = Path::new(private.trim()).join(crate::domain::CLEANUP_LEASE_MARKER);
    let lease = crate::domain::StoryCleanupLease {
        version: crate::domain::CLEANUP_LEASE_VERSION,
        project_slug: "fixture".into(),
        story_id: "SH-1".into(),
        repository_path: fixture.env.home().into(),
        worktree_path: worktree.clone(),
        branch: "worktree-SH-1".into(),
        tmux: crate::domain::TmuxCleanupTarget {
            revivify: None,
            socket_path: fixture.env.home().join("unmanaged-absent-socket"),
        },
    };
    let marker_bytes = serde_json::to_vec(&lease).unwrap();
    std::fs::write(&marker, &marker_bytes).unwrap();
    let tip = checked_git(&worktree, &["rev-parse", "HEAD"]);
    let mut report = absent_window_report(&fixture);
    report.worktree = Some(worktree.clone());
    let paths = super::super::identity::capture(&report).unwrap();
    let authority = Authority {
        repository: report.repository.clone(),
        worktree: true,
        branch: true,
        orphan_directory: false,
    };
    let changed = Rc::new(std::cell::Cell::new(false));
    let observed = changed.clone();
    let target = worktree.clone();
    let _fault = fail_first_child(Removal::Worktree, move || {
        // Keep the old inode alive, so inode reuse cannot make the test pass.
        let previous = target.with_file_name("previous-lane");
        std::fs::rename(&target, &previous).unwrap();
        std::fs::create_dir(&target).unwrap();
        std::fs::copy(previous.join(".git"), target.join(".git")).unwrap();
        std::fs::write(target.join("keep"), "replacement work").unwrap();
        observed.set(true);
    });
    let mut residue = Residue::default();
    remove_checked(
        &report,
        &paths,
        &authority,
        &ResetCaller::default(),
        &fixture.env,
        None,
        &mut residue,
        Some(&|| Ok(())),
    )
    .unwrap();
    dispatch_overlap(&report, &authority, &fixture.env, &mut residue);
    assert!(
        changed.get(),
        "the first removal must reach the injected child failure"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("keep")).unwrap(),
        "replacement work"
    );
    assert_eq!(std::fs::read(&marker).unwrap(), marker_bytes);
    assert_eq!(checked_git(&worktree, &["rev-parse", "HEAD"]), tip);
    assert!(
        residue
            .into_entries()
            .iter()
            .any(|entry| entry.blocks_dispatch
                && entry.reason.contains("filesystem identity changed"))
    );
}

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let to = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to);
        } else {
            assert!(entry.file_type().unwrap().is_file());
            std::fs::copy(entry.path(), to).unwrap();
        }
    }
}

#[test]
fn sh890_branch_retry_preserves_a_replaced_repository_with_the_same_tip() {
    let fixture = Fixture::new();
    repository(&fixture);
    checked_git(fixture.env.home(), &["branch", "worktree-SH-1"]);
    let tip = checked_git(fixture.env.home(), &["rev-parse", "worktree-SH-1"]);
    let report = absent_window_report(&fixture);
    let paths = super::super::identity::capture(&report).unwrap();
    let authority = Authority {
        repository: report.repository.clone(),
        branch: true,
        ..Default::default()
    };
    let common = fixture.env.home().join(".git");
    let changed = Rc::new(std::cell::Cell::new(false));
    let observed = changed.clone();
    let _fault = fail_first_child(Removal::Branch, move || {
        let previous = common.with_file_name("previous-git");
        std::fs::rename(&common, &previous).unwrap();
        copy_tree(&previous, &common);
        observed.set(true);
    });
    let mut residue = Residue::default();
    remove_checked(
        &report,
        &paths,
        &authority,
        &ResetCaller::default(),
        &fixture.env,
        None,
        &mut residue,
        Some(&|| Ok(())),
    )
    .unwrap();
    dispatch_overlap(&report, &authority, &fixture.env, &mut residue);
    assert!(changed.get());
    assert_eq!(
        checked_git(fixture.env.home(), &["rev-parse", "worktree-SH-1"]),
        tip
    );
    assert!(
        residue
            .into_entries()
            .iter()
            .any(|entry| entry.blocks_dispatch
                && entry.reason.contains("filesystem identity changed"))
    );
}

#[test]
fn sh890_window_retry_preserves_a_respawned_pane_and_its_git_resources() {
    let mut fixture = Fixture::new();
    repository(&fixture);
    fixture.start(&fixture.endpoint.clone(), "SH-1");
    let worktree = fixture.env.home().join("lane");
    checked_git(
        fixture.env.home(),
        &[
            "worktree",
            "add",
            "-b",
            "worktree-SH-1",
            worktree.to_str().unwrap(),
        ],
    );
    std::fs::write(worktree.join("keep"), "retained work").unwrap();
    let mut report = absent_window_report(&fixture);
    report.worktree = Some(worktree.clone());
    report.socket_path = Some(fixture.endpoint.clone());
    report.pane = tmux::panes(
        &fixture.env,
        &fixture.endpoint,
        &BTreeSet::from(["SH-1".into()]),
    )
    .unwrap()
    .pop();
    let original = report.pane.clone().unwrap();
    let paths = super::super::identity::capture(&report).unwrap();
    let authority = Authority {
        repository: report.repository.clone(),
        worktree: true,
        branch: true,
        orphan_directory: false,
    };
    let endpoint = fixture.endpoint.clone();
    let pane = original.pane_id.clone();
    let env = fixture.env.clone();
    let replaced = Rc::new(RefCell::new(None));
    let observed = replaced.clone();
    let _fault = fail_first_child(Removal::Window, move || {
        let mut command = std::process::Command::new("tmux");
        command.args(["-N", "-S"]).arg(&endpoint).args([
            "respawn-pane",
            "-k",
            "-t",
            &pane,
            "sleep 600",
        ]);
        let output = crate::process::run_captured(
            command,
            env.subprocess_bound(crate::service::engine::TMUX_TIMEOUT),
        )
        .unwrap_or_else(|error| panic!("respawning fixture pane: {}", error.detail()));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        *observed.borrow_mut() = tmux::panes(&env, &endpoint, &BTreeSet::from(["SH-1".into()]))
            .unwrap()
            .pop();
    });
    let mut residue = Residue::default();
    remove_checked(
        &report,
        &paths,
        &authority,
        &ResetCaller::default(),
        &fixture.env,
        None,
        &mut residue,
        Some(&|| Ok(())),
    )
    .unwrap();
    dispatch_overlap(&report, &authority, &fixture.env, &mut residue);
    let replacement = replaced
        .borrow()
        .clone()
        .expect("first kill failed and pane respawned");
    assert_eq!(replacement.window_id, original.window_id);
    assert_eq!(replacement.pane_id, original.pane_id);
    assert_ne!(replacement.pid, original.pid);
    let survivors = tmux::panes(
        &fixture.env,
        &fixture.endpoint,
        &BTreeSet::from(["SH-1".into()]),
    )
    .unwrap();
    assert_eq!(survivors.len(), 1);
    assert_eq!(survivors[0].pid, replacement.pid);
    assert!(!survivors[0].dead);
    assert_eq!(
        std::fs::read_to_string(worktree.join("keep")).unwrap(),
        "retained work"
    );
    assert!(git::branch_exists(fixture.env.home(), "worktree-SH-1").unwrap());
    assert!(
        residue
            .into_entries()
            .iter()
            .any(|entry| entry.blocks_dispatch && entry.reason.contains("identity changed"))
    );
}
