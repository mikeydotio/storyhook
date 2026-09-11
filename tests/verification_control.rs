//! SH-668: manual verifier controls exercise the real REST service and store.

use serde_json::{Value, json};
use std::sync::{Barrier, Mutex, mpsc};
use std::time::Duration;
use storyhook::api::{http::TrustedHosts, rest};
use storyhook::daemon::http1::{Header, Method};
use storyhook::daemon::lifecycle::InFlight;
use storyhook::daemon::verification::{
    NotifyDelivery, ResumePlan, SubmissionFailure, TickResult, VerificationActivity,
    VerificationActuator, VerificationControlState, VerificationOutcome, tick_with_activity,
};
use storyhook::error::AppError;
use storyhook::service::verification_control::VerificationAction;
use storyhook::service::{
    NewStoryInput, PrLinkService, StoryService, VerificationCandidate, VerificationQueue,
    acknowledge_verification_incident,
};
use storyhook::store::{
    PrLink, ProjectId, ReadOps, SqliteStore, Store, StoryNo, VerificationFailureDisposition,
    VerificationIncident, WriteOps,
};
use storyhook_test_support::ServiceFixture;

fn request(fixture: &ServiceFixture, method: Method, suffix: &str, body: Value) -> (u16, Value) {
    let headers = [
        Header::from_bytes("Host", "127.0.0.1:3456").unwrap(),
        Header::from_bytes("X-Storyhook", "1").unwrap(),
        Header::from_bytes("Content-Type", "application/json").unwrap(),
    ];
    let response = rest::route(
        fixture.store(),
        fixture.env(),
        &method,
        &format!("/api/repos/fixture/{suffix}"),
        &headers,
        &body.to_string(),
        &TrustedHosts::default(),
    )
    .reply;
    let text = response.text_body().unwrap();
    (
        response.status,
        serde_json::from_str(text).unwrap_or_else(|_| json!({"text": text})),
    )
}

#[test]
fn manual_stop_is_durable_and_start_reopens_admission() {
    let fixture = ServiceFixture::new();
    for action in ["drain", "stop"] {
        let (code, stopped) = request(
            &fixture,
            Method::Post,
            "verification/control",
            json!({"action": action}),
        );
        assert_eq!(code, 200, "{stopped}");
        assert_eq!(stopped["state"], "stopped");
        // A fresh router registry models daemon restart: only the store survives.
        let (_, data) = request(&fixture, Method::Get, "data", json!({}));
        assert_eq!(data["verification_control"]["state"], "stopped");
        let (code, running) = request(
            &fixture,
            Method::Post,
            "verification/control",
            json!({"action": "start"}),
        );
        assert_eq!(code, 200, "{running}");
        assert_eq!(running["state"], "running");
    }
}

#[test]
fn invalid_controls_do_not_change_admission() {
    let fixture = ServiceFixture::new();
    for (body, expected) in [
        (json!({}), 400),
        (json!({"action": "oops"}), 422),
        (json!({"action": false}), 400),
    ] {
        let (code, response) = request(&fixture, Method::Post, "verification/control", body);
        assert_eq!(code, expected, "{response}");
        let (_, data) = request(&fixture, Method::Get, "data", json!({}));
        assert_eq!(data["verification_control"]["state"], "running");
    }
}

fn candidate(fixture: &ServiceFixture, project: ProjectId) -> VerificationCandidate {
    let ctx = fixture.ctx_for(project);
    let id = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Verify operator controls".into(),
            ..NewStoryInput::default()
        })
        .unwrap()
        .id;
    StoryService::new(&ctx)
        .set_state(&id, "verifying", None, None, None)
        .unwrap();
    VerificationQueue::new(fixture.store())
        .ordered_for(project)
        .unwrap()
        .pop()
        .unwrap()
}

fn halted(fixture: &ServiceFixture) -> VerificationIncident {
    let candidate = candidate(fixture, fixture.project());
    let incident = VerificationIncident {
        incident_id: "operator-controls:1".into(),
        project: fixture.project(),
        story: StoryNo::new(1),
        generation: candidate.verifying_generation.unwrap(),
        disposition: VerificationFailureDisposition::Permanent,
        halted: true,
        attempts: 1,
        detail: "verifier executable unavailable".into(),
        first_failed_at: fixture.env().now(),
        last_failed_at: fixture.env().now(),
    };
    fixture
        .store()
        .write(|tx| tx.put_verification_incident(&incident))
        .unwrap();
    incident
}

#[test]
fn leave_stopped_acknowledges_atomically_and_explicit_retry_resumes() {
    for action in ["leave-stopped", "retry"] {
        let fixture = ServiceFixture::new();
        let incident = halted(&fixture);
        request(
            &fixture,
            Method::Post,
            "verification/control",
            json!({"action":"stop"}),
        );
        let (code, body) = request(
            &fixture,
            Method::Post,
            "verification/ack",
            json!({
                "incident_id": incident.incident_id, "action": action,
            }),
        );
        assert_eq!(code, 200, "{body}");
        let reopened = SqliteStore::open(fixture.store().path()).unwrap();
        reopened
            .read(|tx| {
                assert!(tx.verification_incident(fixture.project())?.is_none());
                assert_eq!(
                    tx.verification_enabled(fixture.project())?,
                    action == "retry"
                );
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn stale_and_invalid_acknowledgements_change_neither_incident_nor_permission() {
    for enabled in [false, true] {
        let fixture = ServiceFixture::new();
        let incident = halted(&fixture);
        fixture
            .store()
            .write(|tx| tx.put_verification_enabled(fixture.project(), enabled))
            .unwrap();
        for body in [
            json!({"incident_id":"stale", "action":"leave-stopped"}),
            json!({"incident_id":"stale", "action":"retry"}),
            json!({"incident_id":incident.incident_id, "action":"oops"}),
            json!({"incident_id":incident.incident_id, "action":null}),
        ] {
            let (code, response) = request(&fixture, Method::Post, "verification/ack", body);
            assert_eq!(code, 422, "{response}");
            fixture
                .store()
                .read(|tx| {
                    assert_eq!(
                        tx.verification_incident(fixture.project())?,
                        Some(incident.clone())
                    );
                    assert_eq!(tx.verification_enabled(fixture.project())?, enabled);
                    Ok(())
                })
                .unwrap();
        }
    }
}

#[test]
fn start_never_acknowledges_and_legacy_ack_never_resumes_manual_stop() {
    let fixture = ServiceFixture::new();
    let incident = halted(&fixture);
    request(
        &fixture,
        Method::Post,
        "verification/control",
        json!({"action":"start"}),
    );
    assert!(
        fixture
            .store()
            .read(|tx| tx.verification_incident(fixture.project()))
            .unwrap()
            .is_some()
    );
    request(
        &fixture,
        Method::Post,
        "verification/control",
        json!({"action":"stop"}),
    );
    acknowledge_verification_incident(&fixture.ctx(), &incident.incident_id).unwrap();
    assert!(
        !fixture
            .store()
            .read(|tx| tx.verification_enabled(fixture.project()))
            .unwrap()
    );
}

#[test]
fn draining_can_escalate_but_restart_cannot_erase_an_owned_cancellation() {
    let fixture = ServiceFixture::new();
    let candidate = candidate(&fixture, fixture.project());
    let activity = VerificationActivity::new();
    let guard = activity.acquire(&candidate, fixture.env().now());
    assert_eq!(
        activity
            .control(
                fixture.store(),
                fixture.project(),
                VerificationAction::Drain
            )
            .unwrap(),
        VerificationControlState::Draining
    );
    assert!(!guard.is_cancelled());
    assert!(
        activity
            .control(
                fixture.store(),
                fixture.project(),
                VerificationAction::Start
            )
            .is_err()
    );
    assert_eq!(
        activity
            .control(fixture.store(), fixture.project(), VerificationAction::Stop)
            .unwrap(),
        VerificationControlState::Stopping
    );
    assert!(guard.is_cancelled());
    assert!(
        activity
            .control(
                fixture.store(),
                fixture.project(),
                VerificationAction::Start
            )
            .is_err()
    );
    assert_eq!(
        activity
            .control(
                fixture.store(),
                fixture.project(),
                VerificationAction::Drain
            )
            .unwrap(),
        VerificationControlState::Stopping
    );
    assert!(guard.is_cancelled());
    drop(guard);
    assert_eq!(
        activity
            .control(
                fixture.store(),
                fixture.project(),
                VerificationAction::Start
            )
            .unwrap(),
        VerificationControlState::Running
    );
    let next = activity.acquire(&candidate, fixture.env().now());
    assert!(
        !next.is_cancelled(),
        "a fresh attempt must have its own token"
    );
}

#[test]
fn stopping_one_project_does_not_cancel_or_disable_another() {
    let fixture = ServiceFixture::new();
    let other = fixture.add_project("gadgets", "GD");
    let first = candidate(&fixture, fixture.project());
    let second = candidate(&fixture, other);
    let activity = VerificationActivity::new();
    let first_guard = activity.acquire(&first, fixture.env().now());
    let second_guard = activity.acquire(&second, fixture.env().now());
    activity
        .control(fixture.store(), fixture.project(), VerificationAction::Stop)
        .unwrap();
    assert!(first_guard.is_cancelled());
    assert!(!second_guard.is_cancelled());
    assert!(
        fixture
            .store()
            .read(|tx| tx.verification_enabled(other))
            .unwrap()
    );
}

struct Gate {
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
    outcome: VerificationOutcome,
}

impl VerificationActuator for Gate {
    fn verify(&self, _: &VerificationCandidate, _: &PrLink) -> VerificationOutcome {
        self.entered.send(()).unwrap();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        self.outcome.clone()
    }
    fn submit(
        &self,
        _: &VerificationCandidate,
    ) -> Result<storyhook::domain::SubmittedPullRequest, SubmissionFailure> {
        panic!("unleased fixture must not submit")
    }
    fn notify(&self, _: &VerificationCandidate, _: &str) -> Result<NotifyDelivery, AppError> {
        panic!("operator stop must not request repair")
    }
    fn redispatch(&self, _: &VerificationCandidate, _: &ResumePlan) -> Result<(), AppError> {
        panic!("operator stop must not redispatch")
    }
    fn reap(&self, _: &VerificationCandidate) -> Result<(), AppError> {
        Ok(())
    }
}

fn linked_candidate(fixture: &ServiceFixture) -> VerificationCandidate {
    fixture.link_origin("https://github.com/acme/widgets");
    let candidate = candidate(fixture, fixture.project());
    PrLinkService::new(&fixture.ctx())
        .link(
            &candidate.story_id,
            "https://github.com/acme/widgets/pull/1",
            true,
        )
        .unwrap();
    VerificationQueue::new(fixture.store())
        .next()
        .unwrap()
        .unwrap()
}

#[test]
fn stop_prevents_admission_and_does_not_turn_cancellation_into_an_incident() {
    let fixture = ServiceFixture::new();
    let candidate = linked_candidate(&fixture);
    let activity = VerificationActivity::new();
    let inflight = InFlight::new(fixture.env().clone());
    let (entered, observed) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let gate = Gate {
        entered,
        release: Mutex::new(released),
        outcome: VerificationOutcome::InfrastructureFailure {
            detail: "child interrupted".into(),
            disposition: VerificationFailureDisposition::Permanent,
        },
    };
    activity
        .control(fixture.store(), fixture.project(), VerificationAction::Stop)
        .unwrap();
    assert_eq!(
        tick_with_activity(
            fixture.store(),
            fixture.env(),
            &gate,
            &activity,
            &inflight,
            fixture.project()
        )
        .unwrap(),
        TickResult::Stopped
    );
    assert!(
        observed.try_recv().is_err(),
        "stopped permission must gate actual worker admission"
    );
    activity
        .control(
            fixture.store(),
            fixture.project(),
            VerificationAction::Start,
        )
        .unwrap();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            tick_with_activity(
                fixture.store(),
                fixture.env(),
                &gate,
                &activity,
                &inflight,
                fixture.project(),
            )
        });
        observed.recv_timeout(Duration::from_secs(10)).unwrap();
        activity
            .control(fixture.store(), fixture.project(), VerificationAction::Stop)
            .unwrap();
        release.send(()).unwrap();
        assert_eq!(worker.join().unwrap().unwrap(), TickResult::Stopped);
    });
    assert!(activity.active_for(fixture.project()).is_none());
    assert!(
        fixture
            .store()
            .read(|tx| tx.verification_incident(fixture.project()))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        VerificationQueue::new(fixture.store())
            .next()
            .unwrap()
            .unwrap()
            .verifying_generation,
        candidate.verifying_generation
    );
}

#[test]
fn completed_merge_wins_a_concurrent_stop_and_drain_allows_completion() {
    for action in [VerificationAction::Drain, VerificationAction::Stop] {
        let fixture = ServiceFixture::new();
        linked_candidate(&fixture);
        let activity = VerificationActivity::new();
        let inflight = InFlight::new(fixture.env().clone());
        let (entered, observed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let gate = Gate {
            entered,
            release: Mutex::new(released),
            outcome: VerificationOutcome::Merged {
                tree: "certified-tree".into(),
                detail: "remote merge completed".into(),
                gate: "fixture-gate".into(),
            },
        };
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                tick_with_activity(
                    fixture.store(),
                    fixture.env(),
                    &gate,
                    &activity,
                    &inflight,
                    fixture.project(),
                )
            });
            observed.recv_timeout(Duration::from_secs(10)).unwrap();
            activity
                .control(fixture.store(), fixture.project(), action)
                .unwrap();
            release.send(()).unwrap();
            assert_eq!(worker.join().unwrap().unwrap(), TickResult::Completed);
        });
        assert!(
            VerificationQueue::new(fixture.store())
                .next()
                .unwrap()
                .is_none()
        );
        assert!(
            !fixture
                .store()
                .read(|tx| tx.verification_enabled(fixture.project()))
                .unwrap()
        );
    }
}

#[test]
fn concurrent_acknowledgements_have_one_winner_and_no_partial_permission_write() {
    let fixture = ServiceFixture::new();
    let incident = halted(&fixture);
    let activity = VerificationActivity::new();
    let barrier = Barrier::new(2);
    std::thread::scope(|scope| {
        let acknowledge = |action| {
            barrier.wait();
            activity.acknowledge(&fixture.ctx(), &incident.incident_id, Some(action))
        };
        use storyhook::service::verification_control::VerificationAcknowledgement::{
            LeaveStopped, Retry,
        };
        let stopped = scope.spawn(move || acknowledge(LeaveStopped));
        let retry = scope.spawn(move || acknowledge(Retry));
        let stopped = stopped.join().unwrap();
        let retry = retry.join().unwrap();
        assert_ne!(stopped.is_ok(), retry.is_ok());
        assert_eq!(
            fixture
                .store()
                .read(|tx| tx.verification_enabled(fixture.project()))
                .unwrap(),
            retry.is_ok()
        );
    });
}

#[test]
fn real_shell_cancellation_reaches_the_owned_process_and_preserves_the_queue() {
    use storyhook::daemon::verification::{ShellVerificationActuator, journal_path};
    let fixture = ServiceFixture::new();
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), Some(fixture.cwd())))
        .unwrap();
    for args in [
        vec!["init", "-q"],
        vec![
            "config",
            "remote.origin.url",
            "https://github.com/acme/widgets",
        ],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(fixture.cwd())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    let candidate = linked_candidate(&fixture);
    let activity = VerificationActivity::new();
    let inflight = InFlight::new(fixture.env().clone());
    let script = fixture.cwd().join("verification-probe.sh");
    std::fs::write(&script, "trap 'printf done > \"$STORYHOOK_GATE_PROGRESS.terminated\"; exit 0' TERM\nprintf ready > \"$STORYHOOK_GATE_PROGRESS.started\"\nwhile :; do sleep 30; done\n").unwrap();
    let journal = journal_path(fixture.env(), &candidate);
    let started = std::path::PathBuf::from(format!("{}.started", journal.display()));
    let terminated = std::path::PathBuf::from(format!("{}.terminated", journal.display()));
    let actuator = ShellVerificationActuator::new(fixture.env().clone())
        .with_verifier_script(script)
        .with_activity(activity.clone());
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            tick_with_activity(
                fixture.store(),
                fixture.env(),
                &actuator,
                &activity,
                &inflight,
                fixture.project(),
            )
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !started.exists() {
            assert!(
                !worker.is_finished(),
                "verification exited before startup: {:?}",
                fixture
                    .store()
                    .read(|tx| tx.verification_incident(fixture.project()))
                    .unwrap()
            );
            assert!(
                std::time::Instant::now() < deadline,
                "verification never started"
            );
            std::thread::yield_now();
        }
        activity
            .control(fixture.store(), fixture.project(), VerificationAction::Stop)
            .unwrap();
        assert_eq!(worker.join().unwrap().unwrap(), TickResult::Stopped);
    });
    assert!(
        terminated.exists(),
        "TERM must reach the production actuator's child"
    );
    assert!(activity.active_for(fixture.project()).is_none());
    assert!(storyhook::daemon::lifecycle::read_owned_processes(fixture.env()).is_empty());
    assert_eq!(
        VerificationQueue::new(fixture.store())
            .next()
            .unwrap()
            .unwrap()
            .verifying_generation,
        candidate.verifying_generation
    );
    assert!(
        fixture
            .store()
            .read(|tx| tx.verification_incident(fixture.project()))
            .unwrap()
            .is_none()
    );
}
