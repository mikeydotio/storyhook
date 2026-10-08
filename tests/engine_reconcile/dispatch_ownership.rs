//! A live dispatch owns its incomplete lane until its result is published.
use super::*;
use fs4::FileExt;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use storyhook::error::AppError;
use storyhook::service::engine::{DispatchRequest, UnclaimRequest};

fn lock_path(fixture: &ServiceFixture, run: &str) -> PathBuf {
    let key: String = run.bytes().map(|b| format!("{b:02x}")).collect();
    fixture
        .env()
        .store_path()
        .with_extension(format!("dispatch-{key}.lock"))
}
fn pending(fixture: &ServiceFixture, run: &str, index: u32, story: &str) -> EngineLaneRecord {
    occupy(fixture, run, index, story);
    let mut lane = lane_at(fixture, run, index);
    lane.state = EngineLaneState::Dispatching;
    lane.window_name = None;
    lane.worktree_path = None;
    fixture
        .store()
        .write(|tx| tx.put_engine_lane(&lane))
        .unwrap();
    lane
}
fn paused(fixture: &ServiceFixture, lanes: u32) -> String {
    let run = started_run(fixture, &FakeDispatcher::default(), lanes);
    EngineService::new(&fixture.ctx(), &FakeDispatcher::default())
        .pause(&run)
        .unwrap();
    run
}

#[test]
fn a_sibling_reconcile_cannot_quarantine_the_real_dispatch_callback() {
    struct Live<'a> {
        fixture: &'a ServiceFixture,
        run: &'a str,
        called: AtomicBool,
    }
    impl Dispatcher for Live<'_> {
        fn dispatch(&self, _: DispatchRequest) -> Result<DispatchOutcome, AppError> {
            let before = lane_at(self.fixture, self.run, 0);
            assert_eq!(before.state, EngineLaneState::Dispatching);
            assert!(before.pane_id.is_none() && before.window_name.is_none());
            let fake = FakeDispatcher::default();
            let ctx = self.fixture.ctx();
            let sibling = EngineService::new(&ctx, &fake);
            for restart in [false, true] {
                let report = if restart {
                    sibling.reconcile_after_restart(&self.run.into())
                } else {
                    sibling.reconcile(&self.run.into())
                }
                .unwrap();
                assert!(report.quarantined.is_empty());
                assert_eq!(report.deferred.len(), 1);
                assert_eq!(lane_at(self.fixture, self.run, 0), before);
                assert!(fake.calls().is_empty());
            }
            self.called.store(true, Ordering::SeqCst);
            Ok(DispatchOutcome::from_payload(
                serde_json::json!({"ok":false,"display":"controlled callback completion"}),
            ))
        }
        fn unclaim(&self, _: UnclaimRequest) -> Result<DispatchOutcome, AppError> {
            panic!("no unclaim")
        }
        fn probe_window(&self, _: &str) -> WindowProbe {
            panic!("no completed dispatch")
        }
        fn kill_window(&self, _: &str) -> Result<(), AppError> {
            panic!("no window")
        }
        fn census(&self) -> WindowCensus {
            WindowCensus::Counted { windows: vec![] }
        }
    }
    let fixture = ServiceFixture::new();
    new_story(&fixture, "live handoff", &[]);
    let run = started_run(&fixture, &FakeDispatcher::default(), 1);
    let live = Live {
        fixture: &fixture,
        run: &run,
        called: AtomicBool::new(false),
    };
    EngineService::new(&fixture.ctx(), &live)
        .reconcile(&run)
        .unwrap();
    assert!(live.called.load(Ordering::SeqCst));
}

#[test]
fn busy_and_unprobeable_dispatch_ownership_leave_lane_metadata_untouched() {
    for unprobeable in [false, true] {
        let fixture = ServiceFixture::new();
        let story = new_story(&fixture, "retained handoff", &[]);
        let run = paused(&fixture, 1);
        let before = pending(&fixture, &run, 0, &story);
        let path = lock_path(&fixture, &run);
        let _held = if unprobeable {
            std::fs::create_dir(&path).unwrap();
            None
        } else {
            let file = std::fs::File::create(&path).unwrap();
            file.lock_exclusive().unwrap();
            Some(file)
        };
        let fake = FakeDispatcher::default();
        let ctx = fixture.ctx();
        let engine = EngineService::new(&ctx, &fake);
        for restart in [false, true] {
            let report = if restart {
                engine.reconcile_after_restart(&run)
            } else {
                engine.reconcile(&run)
            }
            .unwrap();
            assert_eq!(lane_at(&fixture, &run, 0), before);
            assert!(report.quarantined.is_empty());
            assert_eq!(report.deferred.len(), 1);
            assert!(report.deferred[0].1.contains("dispatch"));
            assert!(fake.calls().is_empty());
        }
    }
}

#[test]
fn an_acquired_dispatch_lock_still_allows_genuine_orphan_classification() {
    for restart in [false, true] {
        let fixture = ServiceFixture::new();
        let story = new_story(&fixture, "orphan handoff", &[]);
        let run = paused(&fixture, 1);
        pending(&fixture, &run, 0, &story);
        let fake = FakeDispatcher::default();
        let ctx = fixture.ctx();
        let engine = EngineService::new(&ctx, &fake);
        let report = if restart {
            engine.reconcile_after_restart(&run)
        } else {
            engine.reconcile(&run)
        }
        .unwrap();
        assert_eq!(
            report.quarantined,
            vec![(
                0,
                if restart {
                    HardStopKind::Interrupted
                } else {
                    HardStopKind::WindowGone
                }
            )]
        );
        assert_eq!(
            lane_at(&fixture, &run, 0).state,
            EngineLaneState::Quarantined
        );
        let file = std::fs::File::open(lock_path(&fixture, &run)).unwrap();
        assert!(
            file.try_lock_exclusive().is_ok(),
            "observation custody must end on return"
        );
    }
}

#[test]
fn a_busy_dispatch_does_not_suppress_normal_working_lane_probes() {
    let fixture = ServiceFixture::new();
    let first = new_story(&fixture, "dispatch handoff", &[]);
    let second = new_story(&fixture, "working lane", &[]);
    let run = paused(&fixture, 2);
    let before = pending(&fixture, &run, 0, &first);
    occupy(&fixture, &run, 1, &second);
    let lock = std::fs::File::create(lock_path(&fixture, &run)).unwrap();
    lock.lock_exclusive().unwrap();
    let fake = FakeDispatcher::new([DispatcherStep::WindowAlive {
        window: "=fixture:=story-SH-2".into(),
        alive: false,
    }]);
    let report = EngineService::new(&fixture.ctx(), &fake)
        .reconcile(&run)
        .unwrap();
    assert_eq!(lane_at(&fixture, &run, 0), before);
    assert_eq!(report.quarantined, vec![(1, HardStopKind::WindowGone)]);
    assert_eq!(
        fake.calls(),
        vec![DispatcherCall::WindowAlive("=fixture:=story-SH-2".into())]
    );
}

#[test]
fn orphan_observation_retains_the_dispatch_lock_while_probing() {
    struct Probe {
        path: PathBuf,
        called: AtomicBool,
    }
    impl Dispatcher for Probe {
        fn dispatch(&self, _: DispatchRequest) -> Result<DispatchOutcome, AppError> {
            panic!("paused")
        }
        fn unclaim(&self, _: UnclaimRequest) -> Result<DispatchOutcome, AppError> {
            panic!("no unclaim")
        }
        fn probe_window(&self, _: &str) -> WindowProbe {
            let file = std::fs::File::open(&self.path).unwrap();
            let error = file.try_lock_exclusive().expect_err(
                "another dispatcher must not acquire orphan ownership during observation",
            );
            assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
            self.called.store(true, Ordering::SeqCst);
            WindowProbe::Gone {
                detail: "fixture orphan window gone".into(),
            }
        }
        fn kill_window(&self, _: &str) -> Result<(), AppError> {
            panic!("no window kill")
        }
        fn census(&self) -> WindowCensus {
            WindowCensus::Counted { windows: vec![] }
        }
    }
    let fixture = ServiceFixture::new();
    let story = new_story(&fixture, "orphan with old window name", &[]);
    let run = paused(&fixture, 1);
    let mut lane = pending(&fixture, &run, 0, &story);
    lane.window_name = Some("old-window".into());
    fixture
        .store()
        .write(|tx| tx.put_engine_lane(&lane))
        .unwrap();
    let probe = Probe {
        path: lock_path(&fixture, &run),
        called: AtomicBool::new(false),
    };
    let report = EngineService::new(&fixture.ctx(), &probe)
        .reconcile(&run)
        .unwrap();
    assert!(probe.called.load(Ordering::SeqCst));
    assert_eq!(report.quarantined, vec![(0, HardStopKind::WindowGone)]);
}
