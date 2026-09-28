//! Startup budgets are shared deadlines, including adopted-lane subprobes.

use super::*;

fn observer(root: &Path, body: &str) -> ShellDispatcher {
    use std::os::unix::fs::PermissionsExt;
    let tmux = root.join("tmux");
    std::fs::write(&tmux, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut dispatcher = ShellDispatcher::new(root.join("story.sh"), Environment::at(root));
    dispatcher.tmux_program = tmux.into_os_string();
    dispatcher
}

fn adopted_lane(root: &Path) -> EngineLaneRecord {
    let mut lane = idle_lane("run", 0, "2026-09-27T00:00:00Z");
    lane.story_id = Some("SH-1".into());
    lane.pane_id = Some("%1".into());
    lane.cleanup_lease = Some(StoryCleanupLease {
        version: CLEANUP_LEASE_VERSION,
        project_slug: "fixture".into(),
        story_id: "SH-1".into(),
        repository_path: root.into(),
        worktree_path: root.into(),
        branch: "worktree-SH-1".into(),
        tmux: crate::domain::TmuxCleanupTarget {
            socket_path: root.join("socket"),
        },
    });
    lane.adopted_identity = Some(crate::store::AdoptedIdentity {
        provider: EngineAgent::Codex,
        pane_pid: std::process::id().try_into().unwrap(),
        window_id: "@1".into(),
    });
    lane
}

#[test]
fn expired_restart_budget_never_starts_regular_or_adopted_probes() {
    let root = storyhook_test_support::scratch_dir();
    let marker = root.path().join("invoked");
    let dispatcher = observer(root.path(), &format!("touch '{}'", marker.display()))
        .with_probe_deadline(Instant::now());
    for lane in [idle_lane("run", 0, "now"), adopted_lane(root.path())] {
        assert!(
            matches!(dispatcher.probe_lane(&lane, "%1"), WindowProbe::Unanswered { detail }
            if detail.contains("startup probe budget exhausted"))
        );
    }
    assert!(
        !marker.exists(),
        "expiration must be checked before spawning"
    );
}

#[test]
fn restart_probe_allowance_is_remaining_time_capped_at_tmux_timeout() {
    let now = Instant::now();
    assert_eq!(probe_timeout_at(None, now).unwrap(), TMUX_TIMEOUT);
    assert_eq!(
        probe_timeout_at(Some(now + TMUX_TIMEOUT * 2), now).unwrap(),
        TMUX_TIMEOUT
    );
    assert_eq!(
        probe_timeout_at(Some(now + TMUX_TIMEOUT / 4), now).unwrap(),
        TMUX_TIMEOUT / 4
    );
    assert!(probe_timeout_at(Some(now), now).is_err());
    assert!(probe_timeout_at(Some(now - TMUX_TIMEOUT), now).is_err());
    assert!(TMUX_TIMEOUT < crate::daemon::lifecycle::SPAWN_DEADLINE);
}

#[test]
fn a_timed_out_restart_probe_exhausts_the_next_dispatchers_budget() {
    let root = storyhook_test_support::scratch_dir();
    let marker = root.path().join("calls");
    let body = format!(
        "echo probe >> '{}'\nsleep {}",
        marker.display(),
        (TMUX_TIMEOUT * 2).as_secs()
    );
    let deadline = Instant::now() + TMUX_TIMEOUT;
    let first = observer(root.path(), &body).with_probe_deadline(deadline);
    let second = observer(root.path(), &body).with_probe_deadline(deadline);
    assert!(matches!(
        first.probe_window("%1"),
        WindowProbe::Unanswered { .. }
    ));
    assert!(
        matches!(second.probe_window("%2"), WindowProbe::Unanswered { detail }
        if detail.contains("startup probe budget exhausted"))
    );
    assert_eq!(std::fs::read_to_string(marker).unwrap().lines().count(), 1);
}

#[test]
fn adopted_activity_query_uses_the_identity_queries_remaining_budget() {
    let root = storyhook_test_support::scratch_dir();
    let marker = root.path().join("calls");
    // Each answer fits a normal per-call timeout; their sum exceeds the
    // restart budget. A renewed activity timeout would incorrectly answer Alive.
    let delay = TMUX_TIMEOUT.mul_f64(2.0 / 3.0).as_secs_f64();
    let body = format!(
        "echo \"$3\" >> '{}'\nsleep {delay}\nif [ \"$3\" = list-panes ]; then\n\
         printf 'SH-1\\t%%1\\t{}\\tcodex\\t0\\t@1\\t{}\\tcodex\\t1\\n'\n\
         else printf '1789066115\\n'; fi",
        marker.display(),
        std::process::id(),
        root.path().display(),
    );
    let dispatcher =
        observer(root.path(), &body).with_probe_deadline(Instant::now() + TMUX_TIMEOUT);
    let answer = dispatcher.probe_lane(&adopted_lane(root.path()), "%1");
    assert!(
        matches!(answer, WindowProbe::Unanswered { .. }),
        "{answer:?}"
    );
    let calls = std::fs::read_to_string(marker).unwrap();
    assert_eq!(
        calls, "list-panes\ndisplay-message\n",
        "both production subprobes must be exercised"
    );
}

#[test]
fn steady_probe_is_not_given_a_previous_restart_deadline() {
    let root = storyhook_test_support::scratch_dir();
    let body = format!(
        "printf '{}\\tcodex\\t0\\t1789066115\\n'",
        std::process::id()
    );
    let restart = observer(root.path(), &body).with_probe_deadline(Instant::now());
    assert!(matches!(
        restart.probe_window("%1"),
        WindowProbe::Unanswered { .. }
    ));
    assert!(matches!(
        observer(root.path(), &body).probe_window("%1"),
        WindowProbe::Alive { .. }
    ));
}
