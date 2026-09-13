//! SH-700: partial CLI configuration preserves the live run's other settings.
use storyhook::cli;

#[test]
fn configure_accepts_partial_flags_and_rejects_empty_or_invalid_requests() {
    for arguments in [
        vec!["engine", "configure", "--lanes", "6"],
        vec![
            "engine",
            "configure",
            "--run",
            "run-id",
            "--model",
            "model-x",
            "--effort",
            "high",
            "--speed",
            "fast",
        ],
    ] {
        let args = arguments.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(cli::parse_invocation(&args).is_ok(), "{args:?}");
    }
    for arguments in [
        vec!["engine", "configure"],
        vec!["engine", "configure", "--run", "run-id"],
        vec!["engine", "configure", "--lanes", "0"],
        vec!["engine", "configure", "--lanes", "256"],
        vec!["engine", "configure", "--speed", "unknown"],
        vec!["engine", "configure", "--model"],
    ] {
        let args = arguments.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(cli::parse_invocation(&args).is_err(), "{args:?}");
    }
}

use storyhook::service::engine::{ConfigurePatch, EngineService, StartRequest};
use storyhook::store::{EngineAgent, EngineScope, EngineSpeed};
use storyhook_test_support::{FakeDispatcher, ServiceFixture};

#[test]
fn partial_updates_preserve_configuration_and_run_identity() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);
    let run = service
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 2,
            agent: EngineAgent::Codex,
            model: Some("model-x".into()),
            effort: Some("high".into()),
            speed: Some(EngineSpeed::Fast),
        })
        .unwrap();
    service.pause(&run.id).unwrap();
    let view = service
        .configure_patch(
            &run.id,
            ConfigurePatch {
                lanes: Some(6),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(view.run.id, run.id);
    assert_eq!(view.run.created_at, run.created_at);
    assert_eq!(view.run.agent, run.agent);
    assert_eq!(view.run.model, run.model);
    assert_eq!(view.run.effort, run.effort);
    assert_eq!(view.run.speed, run.speed);
    assert_eq!(view.lanes.len(), 6);
    assert!(
        service
            .configure_patch(&run.id, ConfigurePatch::default())
            .is_err()
    );
    assert!(
        service
            .configure_patch(
                &run.id,
                ConfigurePatch {
                    lanes: Some(0),
                    ..Default::default()
                }
            )
            .is_err()
    );
    assert_eq!(service.status(Some(&run.id)).unwrap()[0], view);
}

#[test]
fn concurrent_disjoint_patches_are_both_retained() {
    let fixture = ServiceFixture::new();
    let fake = FakeDispatcher::default();
    let ctx = fixture.ctx();
    let service = EngineService::new(&ctx, &fake);
    let run = service
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 1,
            agent: EngineAgent::Claude,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap();
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        for patch in [
            ConfigurePatch {
                model: Some("new-model".into()),
                ..Default::default()
            },
            ConfigurePatch {
                lanes: Some(6),
                ..Default::default()
            },
        ] {
            let service = &service;
            let id = &run.id;
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                service.configure_patch(id, patch).unwrap();
            });
        }
    });
    let view = service.status(Some(&run.id)).unwrap().remove(0);
    assert_eq!(view.run.model.as_deref(), Some("new-model"));
    assert_eq!(view.run.lanes, 6);
}
