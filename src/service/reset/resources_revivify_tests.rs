//! The older reset door must not bypass protected ownership discovery.
use super::*;
use crate::service::tmux_target::tests::Fixture;

fn setup(fixture: &Fixture, socket: &Path) -> (StoryCleanupLease, WorkspaceLock) {
    git(fixture.env.home(), &["init", "-b", "main"], None).unwrap();
    let lock = WorkspaceLock::acquire(fixture.env.home(), "SH-1").unwrap();
    let lease = StoryCleanupLease {
        version: CLEANUP_LEASE_VERSION,
        project_slug: "fixture".into(),
        story_id: "SH-1".into(),
        repository_path: fixture.env.home().into(),
        worktree_path: fixture.env.home().into(),
        branch: "worktree-SH-1".into(),
        tmux: crate::domain::TmuxCleanupTarget {
            revivify: None,
            socket_path: socket.into(),
        },
    };
    (lease, lock)
}

fn caller() -> ResetCaller {
    ResetCaller {
        pane: None,
        socket: None,
    }
}

fn inventory(
    fixture: &Fixture,
    lease: &StoryCleanupLease,
    caller: &ResetCaller,
    lock: &WorkspaceLock,
) -> Result<Vec<Pane>, AppError> {
    let target = resolve_target(&fixture.env, lease)?;
    panes(lease, caller, lock, &target)
}

#[test]
fn protected_missing_logical_socket_is_not_absence() {
    let fixture = Fixture::new();
    let (lease, lock) = setup(&fixture, &fixture.socket);
    assert!(inventory(&fixture, &lease, &caller(), &lock).is_err());
}

#[test]
fn protected_missing_private_endpoint_is_not_absence() {
    let fixture = Fixture::new();
    let (lease, lock) = setup(&fixture, &fixture.endpoint);
    assert!(inventory(&fixture, &lease, &caller(), &lock).is_err());
}

#[test]
fn caller_logical_alias_cannot_authorize_self_removal() {
    let mut fixture = Fixture::new();
    fixture.start(&fixture.endpoint.clone(), "SH-1");
    fixture.start(&fixture.socket.clone(), "foreign");
    let (lease, lock) = setup(&fixture, &fixture.endpoint);
    let caller = ResetCaller {
        pane: Some("%0".into()),
        socket: Some(fixture.socket.clone()),
    };
    let result = inventory(&fixture, &lease, &caller, &lock);
    assert!(result.is_err(), "{result:?}");
}

#[test]
fn cleanup_keeps_its_captured_endpoint_when_activation_changes() {
    let mut fixture = Fixture::new();
    fixture.start(&fixture.endpoint.clone(), "SH-1");
    fixture.start(&fixture.endpoint.clone(), "anchor");
    fixture.start(&fixture.socket.clone(), "SH-1");
    let (lease, lock) = setup(&fixture, &fixture.endpoint);
    let target = resolve_target(&fixture.env, &lease).unwrap();
    let before = panes(&lease, &caller(), &lock, &target).unwrap();
    assert_eq!(before.len(), 1);
    // A changed publication cannot redirect the already checked numeric target.
    std::fs::write(&fixture.activation, "{}").unwrap();
    tmux(&target, &["kill-window", "-t", &before[0].window], &lock).unwrap();
    assert!(panes(&lease, &caller(), &lock, &target).unwrap().is_empty());
    let mut command = Command::new("tmux");
    command.args(["-N", "-S"]).arg(&fixture.socket).args([
        "list-windows",
        "-a",
        "-F",
        "#{window_name}",
    ]);
    let answer = capture(command, Some(&lock)).unwrap();
    assert!(answer.status.success());
    assert_eq!(answer.stdout, b"SH-1\n");
}

#[test]
fn unmanaged_missing_socket_remains_confirmed_absence() {
    let fixture = Fixture::new();
    std::fs::remove_file(&fixture.activation).unwrap();
    let (lease, lock) = setup(&fixture, &fixture.socket);
    assert!(
        inventory(&fixture, &lease, &caller(), &lock)
            .unwrap()
            .is_empty()
    );
}
