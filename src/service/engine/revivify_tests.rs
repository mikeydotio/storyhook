//! Server ownership failures never prove that an engine lane is gone.
use super::restart_probe_tests::{adopted_lane, adopted_row, observer};
use super::*;
use crate::service::tmux_target::tests::{Fixture, private_json};

#[test]
fn protected_unknown_ownership_is_unanswered_before_tmux_is_queried() {
    let fixture = Fixture::new();
    let mut report = fixture.record.clone();
    report["ownership_state"] = serde_json::json!("unknown");
    private_json(&fixture.root.path().join("report.json"), &report);
    let marker = fixture.root.path().join("tmux-called");
    let dispatcher = observer(
        fixture.root.path(),
        &format!(
            "touch '{}'\nprintf 'no server running on old\\n' >&2\nexit 1",
            marker.display()
        ),
        fixture.env.clone(),
    );
    let answer = dispatcher.probe_window_at("%1", Some(&fixture.socket));
    assert!(
        matches!(answer, WindowProbe::Unanswered { .. }),
        "{answer:?}"
    );
    assert!(!marker.exists());
}

/// Real private and foreign servers observed by a client forced into the C locale.
pub(super) fn c_locale_dispatch() -> (Fixture, ShellDispatcher) {
    let mut fixture = Fixture::new();
    fixture.start(&fixture.endpoint.clone(), "SH-1");
    fixture.start(&fixture.socket.clone(), "foreign");
    let program = resolve_executable("tmux").expect("real tmux is required");
    let mut command = Command::new(&program);
    command.args(["-N", "-S"]).arg(&fixture.endpoint).args([
        "set-option",
        "-w",
        "-t",
        "SH-1",
        "@storyhook-agent",
        "codex",
    ]);
    let output = run_captured(command, fixture.env.subprocess_bound(TMUX_TIMEOUT))
        .unwrap_or_else(|error| panic!("{}", error.detail()));
    assert!(output.status.success());
    let dispatcher = observer(
        fixture.root.path(),
        &format!("export LC_ALL=C\nexec '{}' \"$@\"", program.display()),
        fixture.env.clone(),
    );
    (fixture, dispatcher)
}

#[test]
fn c_locale_liveness_preserves_fields_on_the_protected_server() {
    let (fixture, dispatcher) = c_locale_dispatch();
    let answer = dispatcher.probe_window_at("%0", Some(&fixture.endpoint));
    assert!(
        matches!(answer, WindowProbe::Gone { ref detail } if detail.contains("runs `sleep`")),
        "{answer:?}"
    );
    assert!(matches!(
        dispatcher.probe_window_at("%999", Some(&fixture.endpoint)),
        WindowProbe::Gone { .. }
    ));
}

#[test]
fn c_locale_census_counts_the_protected_server() {
    let (fixture, dispatcher) = c_locale_dispatch();
    let target = crate::service::tmux_target::inspect(
        &fixture.env,
        Some(&fixture.socket),
        fixture.deadline(),
        &Default::default(),
    )
    .unwrap();
    let mut command = dispatcher.tmux();
    target.apply(&mut command, Some(&fixture.socket));
    assert_eq!(
        crate::lane_budget::census_through(command, fixture.env.subprocess_bound(TMUX_TIMEOUT)),
        WindowCensus::Counted {
            windows: vec!["SH-1:SH-1".into()]
        }
    );
}

#[test]
fn protected_logical_socket_cannot_rebind_a_numeric_lane_implicitly() {
    let fixture = Fixture::new();
    let marker = fixture.root.path().join("tmux-called");
    let dispatcher = observer(
        fixture.root.path(),
        &format!(
            "touch '{}'\nprintf '{}\\tcodex\\t0\\t1\\n'",
            marker.display(),
            std::process::id()
        ),
        fixture.env.clone(),
    );
    let answer = dispatcher.probe_window_at("%1", Some(&fixture.socket));
    assert!(
        matches!(answer, WindowProbe::Unanswered { .. }),
        "{answer:?}"
    );
    assert!(!marker.exists());
}

#[test]
fn bound_numeric_probe_is_pinned_to_the_private_endpoint() {
    let fixture = Fixture::new();
    let marker = fixture.root.path().join("tmux-args");
    let dispatcher = observer(
        fixture.root.path(),
        &format!(
            "printf '%s\\n' \"$*\" > '{}'\nprintf '{}\\tcodex\\t0\\t1\\n'",
            marker.display(),
            std::process::id()
        ),
        fixture.env.clone(),
    );
    assert_eq!(
        dispatcher.probe_window_at("%1", Some(&fixture.endpoint)),
        WindowProbe::Alive {
            last_output_at: Some(1)
        }
    );
    assert!(
        std::fs::read_to_string(marker)
            .unwrap()
            .starts_with(&format!(
                "-N -S {} -u display-message",
                fixture.endpoint.display()
            ))
    );
}

#[test]
fn protected_transport_refusal_is_not_a_gone_numeric_lane() {
    let fixture = Fixture::new();
    let dispatcher = observer(
        fixture.root.path(),
        "printf 'no server running on private\\n' >&2\nexit 1",
        fixture.env.clone(),
    );
    let answer = dispatcher.probe_window_at("%1", Some(&fixture.endpoint));
    assert!(
        matches!(answer, WindowProbe::Unanswered { .. }),
        "{answer:?}"
    );
}

#[test]
fn adopted_lane_rejects_ownership_failure_before_missing_target_classification() {
    let fixture = Fixture::new();
    let mut report = fixture.record.clone();
    report["restore_ready"] = serde_json::json!(false);
    private_json(&fixture.root.path().join("report.json"), &report);
    let marker = fixture.root.path().join("tmux-called");
    let dispatcher = observer(
        fixture.root.path(),
        &format!("touch '{}'\nexit 0", marker.display()),
        fixture.env.clone(),
    );
    let mut lane = adopted_lane(fixture.root.path());
    lane.cleanup_lease.as_mut().unwrap().tmux.socket_path = fixture.endpoint.clone();
    let answer = dispatcher.probe_lane(&lane, "%1");
    assert!(
        matches!(answer, WindowProbe::Unanswered { .. }),
        "{answer:?}"
    );
    assert!(!marker.exists());
}

#[test]
fn adopted_identity_and_activity_queries_keep_one_endpoint() {
    let fixture = Fixture::new();
    let marker = fixture.root.path().join("tmux-args");
    let dispatcher = observer(
        fixture.root.path(),
        &format!(
            "printf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in *list-panes*) printf '{}' ;; *) printf '1\\n' ;; esac",
            marker.display(),
            adopted_row(fixture.root.path(), 0)
        ),
        fixture.env.clone(),
    );
    let mut lane = adopted_lane(fixture.root.path());
    lane.cleanup_lease.as_mut().unwrap().tmux.socket_path = fixture.endpoint.clone();
    assert_eq!(
        dispatcher.probe_lane(&lane, "%1"),
        WindowProbe::Alive {
            last_output_at: Some(1)
        }
    );
    let calls = std::fs::read_to_string(marker).unwrap();
    assert_eq!(calls.lines().count(), 2);
    assert!(
        calls
            .lines()
            .all(|line| line.starts_with(&format!("-N -S {} ", fixture.endpoint.display()))),
        "{calls}"
    );
}
