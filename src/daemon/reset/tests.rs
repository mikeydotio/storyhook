//! The runtime resumes, survives panics and stands down (SH-886).
use super::*;
use crate::service::{NewStoryInput, StoryService};
use crate::store::{SqliteStore, WriteOps};
use storyhook_test_support::ServiceFixture;

/// Stops the runtime loop however the test ends, so a failed assertion
/// inside the scope cannot leave its poller running for ever.
struct StopOnDrop<'a> {
    stop: &'a AtomicBool,
    runtime: &'a ResetRuntime,
}

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.runtime.shutdown();
    }
}

/// A store with one reserved, unfinished reset of a story without a checkout.
fn reserved(fixture: &ServiceFixture) -> (SqliteStore, Environment, StoryReset) {
    let store = SqliteStore::open(fixture.env().store_path()).unwrap();
    let project = store
        .read(|tx| Ok(tx.project_by_slug("fixture")?.unwrap().id))
        .unwrap();
    store
        .write(|tx| tx.set_checkout_path(project, None))
        .unwrap();
    let env = Environment::at(fixture.env().home());
    let ctx = Ctx::new(&store, project, fixture.cwd(), env.clone()).no_hooks(true);
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Resume me".into(),
            state: Some("in-progress".into()),
            ..Default::default()
        })
        .unwrap();
    let reset = StoryResetService::new(&ctx)
        .reserve(&story.id, &story.id)
        .unwrap();
    (store, env, reset)
}

#[test]
fn an_unfinished_reset_is_resumed_and_finished_on_the_daemon_store() {
    let fixture = ServiceFixture::new();
    let (store, env, reset) = reserved(&fixture);
    let (bus, dispatch) = (ChangeBus::new(), DispatchRegistry::new());
    let (activity, inflight) = (VerificationActivity::new(), InFlight::new(env.clone()));
    let daemon = Daemon {
        store: &store,
        env: &env,
        bus: &bus,
        dispatch: &dispatch,
        activity: &activity,
        inflight: &inflight,
    };
    let runtime = ResetRuntime::new();
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let _guard = StopOnDrop {
            stop: &stop,
            runtime: &runtime,
        };
        scope.spawn(|| run(scope, &daemon, &runtime, &stop));
        let mut patience =
            storyhook_test_support::load_grace::Patience::new(Duration::from_secs(20));
        loop {
            let current = store
                .read(|tx| tx.story_reset(reset.project, reset.story))
                .unwrap()
                .unwrap();
            if current.completed {
                break;
            }
            assert!(
                !patience.expired(),
                "{patience}; the startup sweep never finished the reset"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let row = store
        .read(|tx| tx.story(reset.project, reset.story))
        .unwrap()
        .unwrap();
    assert_eq!(row.state, "todo");
    assert!(!runtime.is_active(&reset.token));
}

#[test]
fn a_panicking_attempt_releases_its_slot_for_the_next_sweep() {
    let fixture = ServiceFixture::new();
    let (_store, _env, reset) = reserved(&fixture);
    let runtime = ResetRuntime::new();
    runtime.request(&reset);
    let job = runtime.next().unwrap();
    let slot = Slot {
        runtime: &runtime,
        token: job.token.clone(),
    };
    let outcome = guarded(|| panic!("injected worker panic"));
    assert!(
        outcome
            .unwrap_err()
            .to_string()
            .contains("injected worker panic")
    );
    drop(slot);
    assert!(!runtime.is_active(&reset.token));
    runtime.request(&reset);
    assert!(
        runtime.is_active(&reset.token),
        "the next sweep can resume it"
    );
}

#[test]
fn a_requested_stand_down_ends_the_runtime_loop() {
    let fixture = ServiceFixture::new();
    let (store, env, _reset) = reserved(&fixture);
    let (bus, dispatch) = (ChangeBus::new(), DispatchRegistry::new());
    let (activity, inflight) = (VerificationActivity::new(), InFlight::new(env.clone()));
    let daemon = Daemon {
        store: &store,
        env: &env,
        bus: &bus,
        dispatch: &dispatch,
        activity: &activity,
        inflight: &inflight,
    };
    let runtime = ResetRuntime::new();
    runtime.shutdown();
    // The loop returns at once; the scope would otherwise never join.
    let never = AtomicBool::new(false);
    std::thread::scope(|scope| run(scope, &daemon, &runtime, &never));
}
