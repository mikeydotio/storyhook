//! Startup budgets are shared deadlines, including adopted-lane subprobes.
//!
//! Budget arithmetic uses the production probe path with a logical clock and
//! scripted external observations (SH-855); it never depends on fork/exec
//! winning a race against the deadline. Real-shell identity/liveness tests
//! retain fixture patience, while production-value assertions declare proof.

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

/// One logical clock shared by distinct dispatchers and nested probes. The
/// runner times out exactly when the requested external work cannot fit, so
/// renewing a timeout changes both the recorded allowance and the verdict.
struct ScriptedProbe {
    now: std::cell::Cell<Instant>,
    discovery_time: Duration,
    steps: std::cell::RefCell<std::collections::VecDeque<(String, Duration, String)>>,
    calls: std::cell::RefCell<Vec<(String, Duration)>>,
}

impl ScriptedProbe {
    fn new(now: Instant, discovery_time: Duration, steps: Vec<(&str, Duration, String)>) -> Self {
        Self {
            now: std::cell::Cell::new(now),
            discovery_time,
            steps: std::cell::RefCell::new(
                steps
                    .into_iter()
                    .map(|(verb, elapsed, output)| (verb.into(), elapsed, output))
                    .collect(),
            ),
            calls: Default::default(),
        }
    }

    fn assert_finished(&self, expected: &[(&str, Duration)]) {
        assert!(
            self.steps.borrow().is_empty(),
            "every planned subprobe must run"
        );
        let expected: Vec<_> = expected
            .iter()
            .map(|(verb, bound)| (verb.to_string(), *bound))
            .collect();
        assert_eq!(*self.calls.borrow(), expected);
    }
}

impl ProbeRuntime for ScriptedProbe {
    fn now(&self) -> Instant {
        self.now.get()
    }

    fn inspect(
        &self,
        _env: &Environment,
        socket: Option<&Path>,
        deadline: Instant,
    ) -> Result<super::super::tmux_target::Target, AppError> {
        let remaining = self.remaining(deadline)?;
        self.calls.borrow_mut().push(("inspect".into(), remaining));
        self.now
            .set(self.now.get() + self.discovery_time.min(remaining));
        self.remaining(deadline)?;
        let socket = socket.unwrap_or_else(|| Path::new("unused-scripted-socket"));
        Ok(serde_json::from_value(serde_json::json!({
            "protected": false,
            "socket": socket,
            "endpoint": socket,
            "requested_socket": socket,
        }))
        .unwrap())
    }

    fn capture(&self, command: Command, timeout: Duration) -> Result<Captured, CaptureError> {
        use std::os::unix::process::ExitStatusExt;
        let (verb, elapsed, output) = self
            .steps
            .borrow_mut()
            .pop_front()
            .expect("an exhausted deadline must not start another subprocess");
        assert!(
            command.get_args().any(|arg| arg == verb.as_str()),
            "unexpected probe command: {command:?}"
        );
        self.calls.borrow_mut().push((verb, timeout));
        self.now.set(self.now.get() + elapsed.min(timeout));
        if elapsed >= timeout {
            return Err(CaptureError::Timeout(
                crate::process::TimeoutTermination::Killed,
            ));
        }
        Ok(Captured {
            status: std::process::ExitStatus::from_raw(0),
            stdout: output.into_bytes(),
            stderr: Vec::new(),
            stdout_truncated: false,
        })
    }
}

fn logical_dispatcher(root: &Path, deadline: Instant) -> ShellDispatcher {
    // No shell file is needed: only the external boundary is scripted.
    ShellDispatcher::new(root.join("unused-story.sh"), proven(root)).with_probe_deadline(deadline)
}

fn logical_adopted_row(root: &Path) -> String {
    adopted_row(root, 0)
        .replace("\\t", "\t")
        .replace("\\n", "\n")
        .replace("%%", "%")
}

#[test]
fn a_timed_out_restart_probe_exhausts_the_next_dispatchers_budget() {
    let root = storyhook_test_support::scratch_dir();
    let now = Instant::now();
    let deadline = now + TMUX_TIMEOUT;
    let runtime = ScriptedProbe::new(
        now,
        Duration::ZERO,
        vec![("display-message", TMUX_TIMEOUT * 2, String::new())],
    );
    let first = logical_dispatcher(root.path(), deadline);
    let second = logical_dispatcher(root.path(), deadline);
    assert!(matches!(
        first.probe_window_at_with("%1", None, &runtime),
        WindowProbe::Unanswered { .. }
    ));
    assert!(
        matches!(second.probe_window_at_with("%2", None, &runtime), WindowProbe::Unanswered { detail }
        if detail.contains("startup probe budget exhausted"))
    );
    runtime.assert_finished(&[("inspect", TMUX_TIMEOUT), ("display-message", TMUX_TIMEOUT)]);
}

#[test]
fn adopted_activity_query_uses_the_identity_queries_remaining_budget() {
    let root = storyhook_test_support::scratch_dir();
    let now = Instant::now();
    let runtime = ScriptedProbe::new(
        now,
        Duration::ZERO,
        vec![
            (
                "list-panes",
                TMUX_TIMEOUT / 4,
                logical_adopted_row(root.path()),
            ),
            (
                "display-message",
                TMUX_TIMEOUT * 7 / 8,
                "1789066115\n".into(),
            ),
        ],
    );
    let dispatcher = logical_dispatcher(root.path(), now + TMUX_TIMEOUT);
    let answer = adoption::probe_with(
        &adopted_lane(root.path()),
        dispatcher.probe_budget(),
        &dispatcher.tmux_program,
        &dispatcher.env,
        &runtime,
    );
    assert!(
        matches!(answer, WindowProbe::Unanswered { .. }),
        "{answer:?}"
    );
    runtime.assert_finished(&[
        ("inspect", TMUX_TIMEOUT),
        ("list-panes", TMUX_TIMEOUT),
        ("display-message", TMUX_TIMEOUT * 3 / 4),
    ]);
}

#[test]
fn restart_discovery_time_reduces_regular_and_adopted_capture_allowance() {
    let root = storyhook_test_support::scratch_dir();
    for adopted in [false, true] {
        let now = Instant::now();
        let runtime = ScriptedProbe::new(
            now,
            TMUX_TIMEOUT / 4,
            vec![(
                if adopted {
                    "list-panes"
                } else {
                    "display-message"
                },
                TMUX_TIMEOUT,
                String::new(),
            )],
        );
        let dispatcher = logical_dispatcher(root.path(), now + TMUX_TIMEOUT);
        let answer = if adopted {
            adoption::probe_with(
                &adopted_lane(root.path()),
                dispatcher.probe_budget(),
                &dispatcher.tmux_program,
                &dispatcher.env,
                &runtime,
            )
        } else {
            dispatcher.probe_window_at_with("%1", None, &runtime)
        };
        assert!(
            matches!(answer, WindowProbe::Unanswered { .. }),
            "{answer:?}"
        );
        runtime.assert_finished(&[
            ("inspect", TMUX_TIMEOUT),
            (
                if adopted {
                    "list-panes"
                } else {
                    "display-message"
                },
                TMUX_TIMEOUT * 3 / 4,
            ),
        ]);
    }
}

#[test]
fn adopted_activity_answers_inside_the_shared_remaining_budget() {
    let root = storyhook_test_support::scratch_dir();
    let now = Instant::now();
    let runtime = ScriptedProbe::new(
        now,
        TMUX_TIMEOUT / 4,
        vec![
            (
                "list-panes",
                TMUX_TIMEOUT / 4,
                logical_adopted_row(root.path()),
            ),
            ("display-message", TMUX_TIMEOUT / 4, "1789066115\n".into()),
        ],
    );
    let dispatcher = logical_dispatcher(root.path(), now + TMUX_TIMEOUT);
    let answer = adoption::probe_with(
        &adopted_lane(root.path()),
        dispatcher.probe_budget(),
        &dispatcher.tmux_program,
        &dispatcher.env,
        &runtime,
    );
    assert!(
        matches!(
            answer,
            WindowProbe::Alive {
                last_output_at: Some(1789066115)
            }
        ),
        "{answer:?}"
    );
    runtime.assert_finished(&[
        ("inspect", TMUX_TIMEOUT),
        ("list-panes", TMUX_TIMEOUT * 3 / 4),
        ("display-message", TMUX_TIMEOUT / 2),
    ]);
}

#[test]
fn adopted_identity_change_never_spends_budget_on_activity() {
    let root = storyhook_test_support::scratch_dir();
    let now = Instant::now();
    let runtime = ScriptedProbe::new(
        now,
        Duration::ZERO,
        vec![(
            "list-panes",
            TMUX_TIMEOUT / 4,
            logical_adopted_row(root.path()),
        )],
    );
    let dispatcher = logical_dispatcher(root.path(), now + TMUX_TIMEOUT);
    let mut lane = adopted_lane(root.path());
    lane.adopted_identity.as_mut().unwrap().window_id = "@different".into();
    let answer = adoption::probe_with(
        &lane,
        dispatcher.probe_budget(),
        &dispatcher.tmux_program,
        &dispatcher.env,
        &runtime,
    );
    assert!(matches!(answer, WindowProbe::Gone { detail } if detail.contains("identity changed")));
    runtime.assert_finished(&[("inspect", TMUX_TIMEOUT), ("list-panes", TMUX_TIMEOUT)]);
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
