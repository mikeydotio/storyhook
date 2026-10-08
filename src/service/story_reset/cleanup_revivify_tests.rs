//! Cleanup keeps the endpoint that supplied its pane evidence (SH-825).
use super::*;
use crate::service::tmux_target::tests::Fixture;
use std::process::Command;

fn report(fixture: &Fixture, socket: &Path) -> ResourceReport {
    let mut init = crate::env::git_env::command(fixture.env.home());
    init.args(["init", "-b", "main"]);
    assert!(
        crate::process::run_captured(
            init,
            fixture
                .env
                .subprocess_bound(crate::service::engine::TMUX_TIMEOUT)
        )
        .unwrap_or_else(|error| panic!("{}", error.detail()))
        .status
        .success()
    );
    let pane = tmux::panes(
        &fixture.env,
        &fixture.socket,
        &BTreeSet::from(["SH-1".into()]),
    )
    .unwrap()
    .pop();
    ResourceReport {
        location_only: false,
        project: "fixture".into(),
        story_id: "SH-1".into(),
        status: "resolved".into(),
        repository: Some(fixture.env.home().into()),
        worktree: None,
        branch: None,
        window_name: "SH-1".into(),
        socket_path: Some(socket.into()),
        pane,
        provider: None,
        candidates: vec![],
        observations: vec![],
        diagnostics: vec![],
    }
}

fn windows(fixture: &Fixture, socket: &Path) -> String {
    let mut command = Command::new("tmux");
    command
        .args(["-N", "-S"])
        .arg(socket)
        .args(["list-windows", "-a", "-F", "#{window_name}"]);
    let output = crate::process::run_captured(
        command,
        fixture
            .env
            .subprocess_bound(crate::service::engine::TMUX_TIMEOUT),
    )
    .unwrap_or_else(|error| panic!("{}", error.detail()));
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn reset_uses_observed_endpoint_and_preserves_the_foreign_public_window() {
    let mut fixture = Fixture::new();
    fixture.start(&fixture.socket.clone(), "SH-1");
    fixture.start(&fixture.endpoint.clone(), "SH-1");
    fixture.start(&fixture.endpoint.clone(), "keepalive");
    let report = report(&fixture, &fixture.endpoint);
    let mut residue = Residue::default();
    remove(
        &report,
        &[],
        &Authority::default(),
        &ResetCaller::default(),
        &fixture.env,
        None,
        &mut residue,
    );
    assert_eq!(residue.into_entries(), vec![]);
    assert_eq!(windows(&fixture, &fixture.socket), "SH-1\n");
    assert_eq!(windows(&fixture, &fixture.endpoint), "keepalive\n");
}

#[test]
fn reset_cannot_redirect_old_numeric_identity_from_logical_to_private_endpoint() {
    let mut fixture = Fixture::new();
    fixture.start(&fixture.socket.clone(), "SH-1");
    fixture.start(&fixture.endpoint.clone(), "SH-1");
    let report = report(&fixture, &fixture.socket);
    let mut residue = Residue::default();
    remove(
        &report,
        &[],
        &Authority::default(),
        &ResetCaller::default(),
        &fixture.env,
        None,
        &mut residue,
    );
    // The redirected window is left in place and reported; the reset goes on.
    let residue = residue.into_entries();
    assert_eq!(residue.len(), 1, "{residue:?}");
    assert_eq!(residue[0].resource, "tmux window SH-1");
    assert!(
        residue[0].reason.contains("generation changed"),
        "{residue:?}"
    );
    assert_eq!(windows(&fixture, &fixture.socket), "SH-1\n");
    assert_eq!(windows(&fixture, &fixture.endpoint), "SH-1\n");
}

#[test]
fn a_window_that_appeared_after_reservation_is_left_and_reported() {
    let mut fixture = Fixture::new();
    fixture.start(&fixture.endpoint.clone(), "SH-1");
    let mut report = report(&fixture, &fixture.endpoint);
    // Reset pinned no pane: whatever runs in this window now is not proven its.
    report.pane = None;
    let mut residue = Residue::default();
    remove(
        &report,
        &[],
        &Authority::default(),
        &ResetCaller::default(),
        &fixture.env,
        None,
        &mut residue,
    );
    let residue = residue.into_entries();
    assert_eq!(residue.len(), 1, "{residue:?}");
    assert!(residue[0].reason.contains("new tmux window"), "{residue:?}");
    assert_eq!(windows(&fixture, &fixture.endpoint), "SH-1\n");
}

fn engine_orphan(
    fixture: &Fixture,
    report: &mut ResourceReport,
) -> (PathBuf, Vec<crate::store::ResetPathIdentity>, Authority) {
    use std::os::unix::fs::MetadataExt;
    // A partial prior Git removal can leave a directory with pinned identity.
    let orphan = fixture.env.home().join("engine-orphan");
    std::fs::create_dir(&orphan).unwrap();
    std::fs::write(orphan.join("keep"), "owned work").unwrap();
    let metadata = std::fs::symlink_metadata(&orphan).unwrap();
    report.worktree = Some(orphan.clone());
    let paths = vec![crate::store::ResetPathIdentity {
        path: orphan.clone(),
        device: metadata.dev(),
        inode: metadata.ino(),
        removable: true,
    }];
    let authority = Authority {
        repository: Some(fixture.env.home().into()),
        orphan_directory: true,
        ..Default::default()
    };
    (orphan, paths, authority)
}

#[test]
fn sh890_changed_server_generation_with_empty_replacement_preserves_git_resources() {
    let mut fixture = Fixture::new();
    fixture.start(&fixture.socket.clone(), "SH-1");
    fixture.start(&fixture.endpoint.clone(), "keepalive");
    let mut report = report(&fixture, &fixture.socket);
    let (orphan, paths, authority) = engine_orphan(&fixture, &mut report);
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
    assert_eq!(
        std::fs::read_to_string(orphan.join("keep")).unwrap(),
        "owned work"
    );
    assert_eq!(windows(&fixture, &fixture.socket), "SH-1\n");
    assert!(
        residue
            .into_entries()
            .iter()
            .any(|entry| entry.reason.contains("generation"))
    );
}

#[test]
fn sh890_callers_live_window_withholds_git_removal() {
    let mut fixture = Fixture::new();
    fixture.start(&fixture.endpoint.clone(), "SH-1");
    let mut report = report(&fixture, &fixture.endpoint);
    let (orphan, paths, authority) = engine_orphan(&fixture, &mut report);
    let caller = ResetCaller {
        pane: Some(report.pane.as_ref().unwrap().pane_id.clone()),
        socket: Some(fixture.endpoint.clone()),
    };
    let mut residue = Residue::default();
    remove_checked(
        &report,
        &paths,
        &authority,
        &caller,
        &fixture.env,
        None,
        &mut residue,
        Some(&|| Ok(())),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(orphan.join("keep")).unwrap(),
        "owned work"
    );
    assert_eq!(windows(&fixture, &fixture.endpoint), "SH-1\n");
    assert!(
        residue
            .into_entries()
            .iter()
            .any(|entry| entry.reason.contains("caller's own"))
    );
}
