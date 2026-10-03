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
    remove(&report, &[], fixture.env.home(), &fixture.env, None).unwrap();
    assert_eq!(windows(&fixture, &fixture.socket), "SH-1\n");
    assert_eq!(windows(&fixture, &fixture.endpoint), "keepalive\n");
}

#[test]
fn reset_cannot_redirect_old_numeric_identity_from_logical_to_private_endpoint() {
    let mut fixture = Fixture::new();
    fixture.start(&fixture.socket.clone(), "SH-1");
    fixture.start(&fixture.endpoint.clone(), "SH-1");
    let report = report(&fixture, &fixture.socket);
    let error = remove(&report, &[], fixture.env.home(), &fixture.env, None).unwrap_err();
    assert!(error.to_string().contains("generation changed"), "{error}");
    assert_eq!(windows(&fixture, &fixture.socket), "SH-1\n");
    assert_eq!(windows(&fixture, &fixture.endpoint), "SH-1\n");
}
