//! Production project reader and fixture containment, on private tmux sockets.

use std::process::Command;
use storyhook_test_support::{ChildGuard, STORY_COMMAND_DEADLINE, daemon_containment};

#[test]
fn project_readers_reconcile_and_fixture_servers_are_owned() {
    let env = storyhook_test_support::TestEnv::isolated();
    let project = env.project().prefix("VIEW").build();
    let slug = project.slug();
    env.stop_daemon();
    let mut command = Command::new("python3");
    env.apply(&mut command);
    command
        .args([
            "-m",
            "unittest",
            "scripts.tests.test_verification_view",
            "scripts.tests.test_cleanup_verifier_fixtures",
            "-v",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .envs(daemon_containment())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env(
            "STORY_VIEW_TEST_BINARY",
            storyhook_test_support::story_binary(),
        )
        .env("STORY_VIEW_TEST_PROJECT", project.path())
        .env("STORY_VIEW_TEST_SLUG", slug)
        .env("STORY_VIEW_TEST_STORE", env.environment().store_path());
    let output = ChildGuard::spawn_with_output(&mut command)
        .unwrap()
        .wait_with_output_within(STORY_COMMAND_DEADLINE * 4, || {
            "private project-view regressions did not finish".into()
        });
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn phase_scripts_emit_diagnostics_without_contacting_tmux() {
    let fixture = storyhook_test_support::scratch_dir();
    let tmux = fixture.path().join("tmux");
    std::fs::write(&tmux, "#!/bin/sh\nexit 99\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(tmux, std::fs::Permissions::from_mode(0o700)).unwrap();
    for action in ["banner", "tail"] {
        let mut command = Command::new("/bin/bash");
        command
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/scripts/verify-window.sh"
            ))
            .args([action, "literal ' $(not-code) ; text"])
            .envs(daemon_containment())
            .env("STORYHOOK_VERIFIER_MIRROR", "1")
            .env("PATH", fixture.path());
        let output = ChildGuard::spawn_with_output(&mut command)
            .unwrap()
            .wait_with_output_within(STORY_COMMAND_DEADLINE, || {
                "phase diagnostics did not finish".into()
            });
        assert!(output.status.success(), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("literal ' $(not-code) ; text"));
        assert!(output.stdout.is_empty());
    }
}
