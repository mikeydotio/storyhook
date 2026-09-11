//! The production engine wait, driven by a real subscriber mailbox.

use super::*;

#[test]
fn project_and_catalog_changes_wake_a_pass() {
    for change in [Change::Project("new-project".into()), Change::Catalog] {
        let bus = ChangeBus::new();
        let subscription = bus.subscribe();
        bus.publish(change);
        let deadline = Instant::now() + Duration::from_secs(5);
        assert!(wait_for_reconcile(
            &subscription,
            &mut 0,
            deadline,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        ));
        assert!(
            Instant::now() < deadline,
            "project/catalog work must not wait for the fallback"
        );
        assert_eq!(subscription.recv(Duration::ZERO), None);
    }
}

#[test]
fn stop_and_drain_take_precedence_over_queued_work_and_expired_deadlines() {
    for (stop, draining) in [(true, false), (false, true), (true, true)] {
        let bus = ChangeBus::new();
        let subscription = bus.subscribe();
        bus.publish(Change::Project("work".into()));
        assert!(!wait_for_reconcile(
            &subscription,
            &mut 0,
            Instant::now(),
            &AtomicBool::new(stop),
            &AtomicBool::new(draining),
        ));
        assert_eq!(
            subscription.recv(Duration::ZERO),
            Some(Change::Project("work".into()))
        );
    }
}

#[test]
fn pings_do_not_shorten_the_deadline() {
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    bus.publish(Change::Ping);
    let deadline = Instant::now() + Duration::from_millis(20);
    assert!(wait_for_reconcile(
        &subscription,
        &mut 0,
        deadline,
        &AtomicBool::new(false),
        &AtomicBool::new(false),
    ));
    assert!(Instant::now() >= deadline);
}

#[test]
fn an_observation_write_refreshes_the_ui_without_reconciling_again() {
    use crate::daemon::watch::ChangeWatcher;
    use crate::service::engine::{StartRequest, StoreOnlyDispatcher};
    use crate::store::{EngineAgent, EngineScope, NewProject, SqliteStore, WriteOps};

    let dir = tempfile::Builder::new()
        .prefix("sh642-wait-")
        .tempdir_in("/private/tmp")
        .unwrap();
    let env = Environment::at(dir.path());
    let store = SqliteStore::open(env.store_path()).unwrap();
    store.migrate().unwrap();
    let project = store
        .write(|tx| {
            let project = tx.create_project(&NewProject {
                uuid: "wait-project".into(),
                slug: "wait-project".into(),
                name: "wait-project".into(),
                prefix: "WAIT".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            })?;
            tx.set_checkout_path(project, Some(dir.path()))?;
            Ok(project)
        })
        .unwrap();
    let ctx = Ctx::new(&store, project, dir.path(), env);
    let run = EngineService::new(&ctx, &StoreOnlyDispatcher)
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 1,
            agent: EngineAgent::Codex,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap();
    let watcher = ChangeWatcher::new(&store);
    let bus = ChangeBus::new();
    let engine = bus.subscribe();
    let ui = bus.subscribe();
    let mut lane = store.read(|tx| tx.engine_lanes(&run.id)).unwrap().remove(0);
    lane.last_observed_at = "2026-01-01T00:00:01Z".into();
    store.write(|tx| tx.put_engine_lane(&lane)).unwrap();
    watcher.notice(&store, &bus);
    assert_eq!(ui.recv(Duration::ZERO), Some(Change::Resync));
    let deadline = Instant::now() + Duration::from_millis(20);
    assert!(wait_for_reconcile(
        &engine,
        &mut 0,
        deadline,
        &AtomicBool::new(false),
        &AtomicBool::new(false),
    ));
    assert!(
        Instant::now() >= deadline,
        "a lane write must not trigger another pass before the fallback"
    );
}

#[test]
fn resync_and_reload_do_not_shorten_the_fallback() {
    for change in [Change::Resync, Change::Reload] {
        let bus = ChangeBus::new();
        let subscription = bus.subscribe();
        bus.publish(change);
        let deadline = Instant::now() + Duration::from_millis(20);
        assert!(wait_for_reconcile(
            &subscription,
            &mut 0,
            deadline,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        ));
        assert!(Instant::now() >= deadline);
    }
}

#[test]
fn sustained_ignored_notifications_cannot_postpone_the_deadline() {
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    let (cancel, cancelled) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(move || {
            for _ in 0..100 {
                bus.publish(Change::Ping);
                bus.publish(Change::Resync);
                if cancelled.recv_timeout(Duration::from_millis(10)).is_ok() {
                    return;
                }
            }
        });
        let start = Instant::now();
        let deadline = start + Duration::from_millis(80);
        let result = wait_for_reconcile(
            &subscription,
            &mut 0,
            deadline,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
        );
        let elapsed = start.elapsed();
        cancel.send(()).unwrap();
        assert!(result);
        assert!(
            Instant::now() >= deadline,
            "noise cannot trigger a pass early"
        );
        assert!(
            elapsed < Duration::from_millis(500),
            "noise must not keep resetting the wait budget: {elapsed:?}"
        );
    });
}

#[test]
fn overflow_recovers_a_discarded_project_once_then_returns_to_the_deadline() {
    let bus = ChangeBus::new();
    let subscription = bus.subscribe();
    let stop = AtomicBool::new(false);
    let draining = AtomicBool::new(false);
    let mut observed_drops = 0;
    for _ in 0..2 {
        bus.publish(Change::Project("lost-control".into()));
        for _ in 0..64 {
            bus.publish(Change::Ping);
        }
        assert!(subscription.dropped() > observed_drops);
        let recovery_deadline = Instant::now() + Duration::from_secs(5);
        assert!(wait_for_reconcile(
            &subscription,
            &mut observed_drops,
            recovery_deadline,
            &stop,
            &draining,
        ));
        assert!(
            Instant::now() < recovery_deadline,
            "lost controls must recover before the fallback"
        );
        assert_eq!(observed_drops, subscription.dropped());
        let deadline = Instant::now() + Duration::from_millis(20);
        assert!(wait_for_reconcile(
            &subscription,
            &mut observed_drops,
            deadline,
            &stop,
            &draining
        ));
        assert!(
            Instant::now() >= deadline,
            "an already recovered overflow must not feed another pass"
        );
    }
}

#[test]
fn reload_wakes_a_waiter_to_observe_shutdown_without_scheduling_work() {
    for drain in [false, true] {
        let bus = ChangeBus::new();
        let subscription = bus.subscribe();
        let stop = AtomicBool::new(false);
        let draining = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                wait_for_reconcile(
                    &subscription,
                    &mut 0,
                    Instant::now() + Duration::from_secs(1),
                    &stop,
                    &draining,
                )
            });
            if drain {
                draining.store(true, Ordering::Relaxed);
            } else {
                stop.store(true, Ordering::Relaxed);
            }
            bus.publish(Change::Reload);
            assert!(!waiter.join().unwrap());
        });
    }
}
