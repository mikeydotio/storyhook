//! SH-889: real separate SQLite handles contend inside one daemon.
use super::*;
use crate::store::{EngineRunState, StoreError, WriteOps};
use std::sync::mpsc;
use std::time::Duration;

fn fixture() -> (
    storyhook_test_support::ServiceFixture,
    EngineController,
    SqliteStore,
) {
    let fixture = storyhook_test_support::ServiceFixture::new();
    let env = Environment::at(fixture.env().home()).busy_timeout(Duration::from_millis(10));
    let controller = EngineController::open(&env).unwrap();
    let daemon = crate::invoke::open_store(&env).unwrap();
    daemon
        .write(|tx| {
            let project = tx.project_by_slug("fixture")?.unwrap().id;
            tx.set_checkout_path(project, Some(fixture.cwd()))
        })
        .unwrap();
    (fixture, controller, daemon)
}

fn contended<T: Send + std::fmt::Debug>(
    daemon: &SqliteStore,
    controller: &EngineController,
    change: impl FnOnce(&mut <SqliteStore as Store>::WriteTx<'_>) -> Result<(), StoreError>,
    action: impl FnOnce() -> Result<T, AppError> + Send,
) -> Result<T, AppError> {
    std::thread::scope(|scope| {
        let (send, receive) = mpsc::channel();
        let mut early = None;
        daemon
            .write(|tx| {
                scope.spawn(move || {
                    let _ = send.send(action());
                });
                // Observe the control inside its own local admission before
                // measuring patience or committing a manual-mode change. This
                // distinguishes contention from a thread not scheduled yet.
                let observed = Instant::now()
                    + storyhook_test_support::load_grace::graced_now(Duration::from_secs(5));
                loop {
                    match controller.store.try_write(|_| Ok(())) {
                        Err(StoreError::Busy(detail))
                            if detail == "project write admission is occupied" =>
                        {
                            break;
                        }
                        Err(StoreError::Busy(_)) => {}
                        other => panic!("the daemon writer must exclude admission: {other:?}"),
                    }
                    assert!(
                        Instant::now() < observed,
                        "the control never attempted admission"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
                // More than one SQLite busy timeout: an unpatient control returns
                // Busy here. Release the transaction before reporting any failure.
                early = Some(receive.recv_timeout(Duration::from_millis(150)));
                change(tx)
            })
            .unwrap();
        assert!(
            matches!(early.unwrap(), Err(mpsc::RecvTimeoutError::Timeout)),
            "the control must wait for admission instead of returning Busy"
        );
        receive
            .recv_timeout(storyhook_test_support::load_grace::PATIENCE_CEILING)
            .expect("the control must finish after the contending transaction commits")
    })
}

#[test]
fn engine_controls_wait_for_a_contending_writer_without_replaying() {
    let (_fixture, controller, daemon) = fixture();
    let run = contended(
        &daemon,
        &controller,
        |_| Ok(()),
        || controller.start("fixture", "{}"),
    )
    .unwrap()
    .run
    .id;
    assert_eq!(controller.status("fixture", None).unwrap().len(), 1);
    let body = serde_json::json!({"run":run,"lanes":3,"agent":"codex"}).to_string();
    let configured = contended(
        &daemon,
        &controller,
        |_| Ok(()),
        || controller.configure("fixture", &body),
    )
    .unwrap();
    assert_eq!(configured.run.lanes, 3);
    assert_eq!(configured.lanes.len(), 3);
    assert_eq!(configured.run.agent, EngineAgent::Codex);
    let body = serde_json::json!({"run":run}).to_string();
    let inflight = InFlight::new(controller.env.clone());
    for (action, state) in [
        (EngineAction::Pause, EngineRunState::Paused),
        (EngineAction::Resume, EngineRunState::Running),
        (EngineAction::Stop, EngineRunState::Finished),
    ] {
        let result = contended(
            &daemon,
            &controller,
            |_| Ok(()),
            || controller.action("fixture", action, &body, &inflight),
        )
        .unwrap();
        assert_eq!(result.run.state, state);
    }
    let ack = contended(
        &daemon,
        &controller,
        |_| Ok(()),
        || controller.action("fixture", EngineAction::Ack, &body, &inflight),
    )
    .unwrap();
    assert!(ack.run.acknowledged_at.is_some());
    let next = controller.start("fixture", "{}").unwrap();
    let body = serde_json::json!({"run":next.run.id,"now":true}).to_string();
    let stopped = contended(
        &daemon,
        &controller,
        |_| Ok(()),
        || controller.action("fixture", EngineAction::Stop, &body, &inflight),
    )
    .unwrap();
    assert_eq!(stopped.run.state, EngineRunState::Finished);
    assert!(
        stopped
            .lanes
            .iter()
            .all(|lane| lane.state == EngineLaneState::Idle)
    );
    assert_eq!(controller.status("fixture", None).unwrap().len(), 2);
}

#[test]
fn delayed_engine_admission_rechecks_manual_mode_before_start_and_resume() {
    let (_fixture, controller, daemon) = fixture();
    let project = controller.context("fixture").unwrap().project();
    let disable = |tx: &mut <SqliteStore as Store>::WriteTx<'_>| {
        let mut settings = tx.settings(project)?;
        settings.automations_enabled = Some(false);
        tx.put_settings(project, &settings)
    };
    let result = contended(&daemon, &controller, disable, || {
        controller.start("fixture", "{}")
    });
    assert!(
        matches!(result, Err(AppError::Validation(ref detail)) if detail.contains("Enable project automations"))
    );
    assert!(controller.status("fixture", None).unwrap().is_empty());
    crate::service::SettingsService::new(&controller.context("fixture").unwrap())
        .set("automations.enabled", "true")
        .unwrap();
    let run = controller.start("fixture", "{}").unwrap().run.id;
    let inflight = InFlight::new(controller.env.clone());
    let body = serde_json::json!({"run":run}).to_string();
    controller
        .action("fixture", EngineAction::Pause, &body, &inflight)
        .unwrap();
    let result = contended(&daemon, &controller, disable, || {
        controller.action("fixture", EngineAction::Resume, &body, &inflight)
    });
    assert!(
        matches!(result, Err(AppError::Validation(ref detail)) if detail.contains("Enable project automations"))
    );
    assert_eq!(
        controller.status("fixture", Some(&run)).unwrap()[0]
            .run
            .state,
        EngineRunState::Paused
    );
}

#[test]
fn engine_controller_caps_the_connection_timeout_and_preserves_shorter_policy() {
    let fixture = storyhook_test_support::ServiceFixture::new();
    for (configured, expected) in [
        (Duration::from_secs(120), crate::env::DEFAULT_BUSY_TIMEOUT),
        (Duration::from_millis(20), Duration::from_millis(20)),
    ] {
        let env = Environment::at(fixture.env().home()).busy_timeout(configured);
        let controller = EngineController::open(&env).unwrap();
        // Read SQLite's actual connection setting, not the Environment or a
        // duplicate of the cap expression. Removing the cap must fail this.
        assert_eq!(
            controller
                .store
                .read(|tx| tx.busy_timeout_for_test())
                .unwrap(),
            expected
        );
        // The cap belongs to HTTP engine controls, not ordinary store callers.
        let ordinary = crate::invoke::open_store(&env).unwrap();
        assert_eq!(
            ordinary.read(|tx| tx.busy_timeout_for_test()).unwrap(),
            configured
        );
    }
}

#[test]
fn engine_control_patience_stays_below_the_dashboard_mutation_deadline() {
    let html = include_str!("../../web_dashboard.html");
    let prefix = "intFromQuery(\"mutationTimeoutMs\", ";
    let milliseconds: String = html
        .split(prefix)
        .nth(1)
        .unwrap()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let dashboard = Duration::from_millis(milliseconds.parse().unwrap());
    assert!(CONTROL_PATIENCE + crate::env::DEFAULT_BUSY_TIMEOUT < dashboard);
    assert_eq!(
        CONTROL_PATIENCE,
        crate::service::story_reset::RESERVE_PATIENCE
    );
}
