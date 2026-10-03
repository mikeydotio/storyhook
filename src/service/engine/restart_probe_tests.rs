//! Startup budgets are shared deadlines, including adopted-lane subprobes.
//!
//! Each test declares how it reads the tmux bound (SH-836): proof where the
//! production value itself is the claim, patience where a fixture must
//! answer. A patience test derives its shared deadline and its fixture delays
//! from the one bound its `Environment` grants, so the ratios it proves hold
//! at any contention.

use super::*;

/// A shell dispatcher whose `tmux` is a fixture script running `body`.
pub(super) fn observer(root: &Path, body: &str, env: Environment) -> ShellDispatcher {
    use std::os::unix::fs::PermissionsExt;
    let tmux = root.join("tmux");
    std::fs::write(&tmux, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut dispatcher = ShellDispatcher::new(root.join("story.sh"), env);
    dispatcher.tmux_program = tmux.into_os_string();
    dispatcher
}

/// An environment for a test whose tmux fixture must answer.
pub(super) fn patient(root: &Path) -> Environment {
    Environment::at(root).with_subprocess_patience()
}

/// An environment for a test that proves the production tmux bound.
fn proven(root: &Path) -> Environment {
    Environment::at(root).with_subprocess_proof()
}

/// A lane adopted from an existing Codex dispatch on `root`'s socket, whose
/// pane is this test process.
pub(super) fn adopted_lane(root: &Path) -> EngineLaneRecord {
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
            revivify: None,
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

/// The `list-panes` row that proves `adopted_lane(root)`'s identity, with
/// `dead` as its `#{pane_dead}`, as a `printf` format.
pub(super) fn adopted_row(root: &Path, dead: u8) -> String {
    format!(
        "SH-1\\t%%1\\t{}\\tcodex\\t{dead}\\t@1\\t{}\\tcodex\\t1\\n",
        std::process::id(),
        root.display()
    )
}

#[test]
fn expired_restart_budget_never_starts_regular_or_adopted_probes() {
    let root = storyhook_test_support::scratch_dir();
    let marker = root.path().join("invoked");
    let dispatcher = observer(
        root.path(),
        &format!("touch '{}'", marker.display()),
        proven(root.path()),
    )
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
    let within = |deadline| TmuxBudget::new(TMUX_TIMEOUT, deadline).timeout_at(now);
    assert_eq!(within(None).unwrap(), TMUX_TIMEOUT);
    assert_eq!(within(Some(now + TMUX_TIMEOUT * 2)).unwrap(), TMUX_TIMEOUT);
    assert_eq!(
        within(Some(now + TMUX_TIMEOUT / 4)).unwrap(),
        TMUX_TIMEOUT / 4
    );
    assert!(within(Some(now)).is_err());
    assert!(within(Some(now - TMUX_TIMEOUT)).is_err());
    assert!(TMUX_TIMEOUT < crate::daemon::lifecycle::SPAWN_DEADLINE);

    // The production wiring: a dispatcher's probe budget is TMUX_TIMEOUT per
    // call, within the restart sweep's deadline when it carries one.
    let root = storyhook_test_support::scratch_dir();
    let steady = observer(root.path(), "exit 0", proven(root.path()));
    assert_eq!(steady.probe_budget(), TmuxBudget::new(TMUX_TIMEOUT, None));
    let deadline = now + TMUX_TIMEOUT;
    assert_eq!(
        steady.with_probe_deadline(deadline).probe_budget(),
        TmuxBudget::new(TMUX_TIMEOUT, Some(deadline))
    );
}

#[test]
fn a_timed_out_restart_probe_exhausts_the_next_dispatchers_budget() {
    let root = storyhook_test_support::scratch_dir();
    let env = patient(root.path());
    let bound = env.subprocess_bound(TMUX_TIMEOUT);
    let marker = root.path().join("calls");
    // The fixture records its call, then outlasts the whole shared budget.
    let body = format!(
        "echo probe >> '{}'\nsleep {}",
        marker.display(),
        (bound * 2).as_secs_f64()
    );
    let deadline = Instant::now() + bound;
    let first = observer(root.path(), &body, env.clone()).with_probe_deadline(deadline);
    let second = observer(root.path(), &body, env).with_probe_deadline(deadline);
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
    let env = patient(root.path());
    let bound = env.subprocess_bound(TMUX_TIMEOUT);
    let marker = root.path().join("calls");
    // Each answer fits a per-call bound; their sum exceeds the shared
    // restart budget of one bound, so a renewed activity timeout would
    // incorrectly answer Alive. The identity answer is the quick one: its
    // three quarters of the budget are the spawn slack this test must not
    // lose under load, and the activity answer (seven eighths) then outlasts
    // whatever remains.
    let identity = (bound / 4).as_secs_f64();
    let activity = (bound * 7 / 8).as_secs_f64();
    let body = format!(
        "echo \"$4\" >> '{}'\nif [ \"$4\" = list-panes ]; then\n\
         sleep {identity}\nprintf '{}'\n\
         else sleep {activity}\nprintf '1789066115\\n'; fi",
        marker.display(),
        adopted_row(root.path(), 0),
    );
    let dispatcher = observer(root.path(), &body, env).with_probe_deadline(Instant::now() + bound);
    let answer = dispatcher.probe_lane(&adopted_lane(root.path()), "%1");
    assert!(
        matches!(answer, WindowProbe::Unanswered { .. }),
        "{answer:?}"
    );
    let calls = std::fs::read_to_string(marker).unwrap();
    assert_eq!(
        calls, "list-panes\ndisplay-message\n",
        "both production subprobes must be exercised; the probe answered {answer:?}"
    );
}

#[test]
fn steady_probe_is_not_given_a_previous_restart_deadline() {
    let root = storyhook_test_support::scratch_dir();
    let body = format!(
        "printf '{}\\tcodex\\t0\\t1789066115\\n'",
        std::process::id()
    );
    let restart =
        observer(root.path(), &body, patient(root.path())).with_probe_deadline(Instant::now());
    assert!(matches!(
        restart.probe_window("%1"),
        WindowProbe::Unanswered { .. }
    ));
    assert!(matches!(
        observer(root.path(), &body, patient(root.path())).probe_window("%1"),
        WindowProbe::Alive { .. }
    ));
}
