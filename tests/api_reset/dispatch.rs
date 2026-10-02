//! Card reset joins live dispatchers but cannot wait forever on an orphaned lane.
use fs4::FileExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use storyhook::service::engine::{EngineService, StartRequest};
use storyhook::service::{NewStoryInput, StoryService};
use storyhook::store::{
    EngineAgent, EngineLaneState, EngineScope, ReadOps, SqliteStore, Store, StoryNo, WriteOps,
};
use storyhook_test_support::{FIXTURE_NOW, FakeDispatcher, ServiceFixture, TestServer, serve};

fn run(fixture: &ServiceFixture) -> String {
    EngineService::new(&fixture.ctx(), &FakeDispatcher::default())
        .start(StartRequest {
            scope: EngineScope::Project,
            lanes: 2,
            agent: EngineAgent::Codex,
            model: None,
            effort: None,
            speed: None,
        })
        .unwrap()
        .id
}

fn seed_dispatch(fixture: &ServiceFixture, run: &str, index: u32, story: &str) {
    fixture
        .store()
        .write(|tx| {
            let mut lane = tx
                .engine_lanes(run)?
                .into_iter()
                .find(|l| l.lane_index == index)
                .unwrap();
            lane.state = EngineLaneState::Dispatching;
            lane.story_id = Some(story.into());
            lane.dispatched_at = Some(FIXTURE_NOW.into());
            tx.put_engine_lane(&lane)
        })
        .unwrap();
}

fn active_story(fixture: &ServiceFixture) -> String {
    StoryService::new(&fixture.ctx())
        .create(&NewStoryInput {
            title: "Reset a dispatch".into(),
            state: Some("in-progress".into()),
            ..Default::default()
        })
        .unwrap()
        .id
}

fn lock_path(fixture: &ServiceFixture, run: &str) -> PathBuf {
    let key: String = run.bytes().map(|b| format!("{b:02x}")).collect();
    fixture
        .env()
        .store_path()
        .with_extension(format!("dispatch-{key}.lock"))
}

struct HttpReset {
    server: TestServer,
    agent: ureq::Agent,
    url: String,
    handle: String,
}

impl HttpReset {
    fn start(fixture: &ServiceFixture, story: &str) -> Self {
        for run in fixture
            .store()
            .read(|tx| tx.engine_runs("fixture"))
            .unwrap()
        {
            EngineService::new(&fixture.ctx(), &FakeDispatcher::default())
                .pause(&run.id)
                .unwrap();
        }
        fixture
            .store()
            .write(|tx| tx.set_checkout_path(fixture.project(), None))
            .unwrap();
        let server = serve(
            Arc::new(SqliteStore::open(fixture.env().store_path()).unwrap()),
            fixture.env(),
        );
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(storyhook_test_support::load_grace::graced_now(
                Duration::from_secs(10),
            )))
            .build()
            .into();
        let url = format!(
            "http://127.0.0.1:{}/api/repos/fixture/story/{story}/reset",
            server.port()
        );
        let mut response = agent
            .post(&url)
            .header("X-Storyhook", "1")
            .header("X-Storyhook-Token", &server.token)
            .send_json(serde_json::json!({"confirmation": story}))
            .unwrap();
        assert_eq!(response.status(), 202);
        let body: serde_json::Value = response.body_mut().read_json().unwrap();
        Self {
            server,
            agent,
            url,
            handle: body["reset"]["handle"].as_str().unwrap().into(),
        }
    }

    fn poll(&self) -> serde_json::Value {
        self.agent
            .get(format!("{}/{}", self.url, self.handle))
            .header("X-Storyhook-Token", &self.server.token)
            .call()
            .unwrap()
            .body_mut()
            .read_json()
            .unwrap()
    }

    fn finished(&self) -> serde_json::Value {
        let mut patience =
            storyhook_test_support::load_grace::Patience::new(Duration::from_secs(5));
        loop {
            let body = self.poll();
            if body["reset"]["state"] != "running" {
                return body;
            }
            assert!(
                !patience.expired(),
                "reset did not finish: {body}; {patience}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn assert_waiting(&self) {
        // The lock is held across this observation interval. Patience scales
        // scheduling opportunity, not the production quiescence deadline.
        let mut patience =
            storyhook_test_support::load_grace::Patience::new(Duration::from_millis(200));
        loop {
            let body = self.poll();
            assert_eq!(
                body["reset"]["state"], "running",
                "cleanup passed a live dispatcher: {body}"
            );
            if patience.expired() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn orphaned_dispatch_does_not_stall_http_card_reset() {
    let fixture = ServiceFixture::new();
    let story = active_story(&fixture);
    let run = run(&fixture);
    seed_dispatch(&fixture, &run, 0, &story);
    let reset = HttpReset::start(&fixture, &story);
    let body = reset.finished();
    assert_eq!(body["reset"]["state"], "ok", "{body}");
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .state,
        "todo"
    );
    let lanes = fixture.store().read(|tx| tx.engine_lanes(&run)).unwrap();
    assert_eq!(lanes[0].state, EngineLaneState::Idle);
    assert_eq!(lanes[0].outcome.as_deref(), Some("story-reset"));
}

#[test]
fn unavailable_dispatch_lock_fails_reset_without_releasing_ownership() {
    let fixture = ServiceFixture::new();
    let story = active_story(&fixture);
    let run = run(&fixture);
    seed_dispatch(&fixture, &run, 0, &story);
    let path = lock_path(&fixture, &run);
    std::fs::create_dir(&path).unwrap();
    let reset = HttpReset::start(&fixture, &story);
    let body = reset.finished();
    assert_eq!(body["reset"]["state"], "error", "{body}");
    let detail = body["reset"]["detail"].as_str().unwrap();
    assert!(
        detail.contains("dispatch lock") && detail.contains(path.to_str().unwrap()),
        "{body}"
    );
    let receipt = fixture
        .store()
        .read(|tx| tx.story_reset(fixture.project(), StoryNo::new(1)))
        .unwrap()
        .unwrap();
    assert!(!receipt.completed);
    assert!(receipt.resources.is_none());
    assert_eq!(
        fixture.store().read(|tx| tx.engine_lanes(&run)).unwrap()[0].state,
        EngineLaneState::Dispatching
    );
}

#[test]
fn unrelated_dispatch_does_not_hold_a_story_without_a_lane() {
    let fixture = ServiceFixture::new();
    let story = active_story(&fixture);
    let other = active_story(&fixture);
    let run = run(&fixture);
    seed_dispatch(&fixture, &run, 1, &other);
    let lock = std::fs::File::create(lock_path(&fixture, &run)).unwrap();
    lock.lock_exclusive().unwrap();
    let before = fixture.store().read(|tx| tx.engine_lanes(&run)).unwrap();
    let body = HttpReset::start(&fixture, &story).finished();
    assert_eq!(body["reset"]["state"], "ok", "{body}");
    assert_eq!(
        fixture.store().read(|tx| tx.engine_lanes(&run)).unwrap(),
        before
    );
}

#[test]
fn live_engine_dispatch_is_joined_before_http_reset_completes() {
    use storyhook::error::AppError;
    use storyhook::lane_budget::WindowCensus;
    use storyhook::service::engine::{
        DispatchOutcome, DispatchRequest, Dispatcher, UnclaimRequest, WindowProbe,
    };
    struct Live<'a> {
        fixture: &'a ServiceFixture,
        reset: std::sync::Mutex<Option<HttpReset>>,
    }
    impl Dispatcher for Live<'_> {
        fn dispatch(&self, request: DispatchRequest) -> Result<DispatchOutcome, AppError> {
            let reset = HttpReset::start(self.fixture, &request.story);
            reset.assert_waiting();
            let receipt = self
                .fixture
                .store()
                .read(|tx| tx.story_reset(self.fixture.project(), StoryNo::new(1)))
                .unwrap()
                .unwrap();
            assert!(
                receipt.resources.is_none(),
                "discovery must follow quiescence"
            );
            *self.reset.lock().unwrap() = Some(reset);
            Ok(DispatchOutcome::from_payload(
                serde_json::json!({"ok": false, "display": "controlled dispatch refusal"}),
            ))
        }
        fn census(&self) -> WindowCensus {
            WindowCensus::Counted { windows: vec![] }
        }
        fn unclaim(&self, _: UnclaimRequest) -> Result<DispatchOutcome, AppError> {
            panic!("reset owns the story")
        }
        fn kill_window(&self, _: &str) -> Result<(), AppError> {
            panic!("no window was created")
        }
        fn probe_window(&self, _: &str) -> WindowProbe {
            panic!("no working lane")
        }
    }
    let fixture = ServiceFixture::new();
    let ctx = fixture.ctx();
    StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "Live dispatch".into(),
            ..Default::default()
        })
        .unwrap();
    let run = run(&fixture);
    let dispatcher = Live {
        fixture: &fixture,
        reset: std::sync::Mutex::new(None),
    };
    EngineService::new(&ctx, &dispatcher)
        .reconcile(&run)
        .unwrap();
    let body = dispatcher.reset.lock().unwrap().take().unwrap().finished();
    assert_eq!(body["reset"]["state"], "ok", "{body}");
}

#[test]
#[ignore = "subprocess lock owner, invoked only by its parent test"]
fn dispatch_lock_owner() {
    use std::io::Read;
    let path = PathBuf::from(std::env::var_os("SH791_LOCK_PATH").unwrap());
    let file = std::fs::File::create(&path).unwrap();
    file.lock_exclusive().unwrap();
    std::fs::write(path.with_extension("ready"), "locked").unwrap();
    let mut byte = [0];
    std::io::stdin().read_exact(&mut byte).unwrap();
    panic!("parent must kill this owner without publishing a lane result");
}

#[test]
fn dispatcher_process_death_unblocks_http_reset() {
    let fixture = ServiceFixture::new();
    let story = active_story(&fixture);
    let run = run(&fixture);
    seed_dispatch(&fixture, &run, 0, &story);
    let path = lock_path(&fixture, &run);
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    storyhook_test_support::TestEnv::shared().apply(&mut command);
    command
        .args(["--exact", "dispatch::dispatch_lock_owner", "--ignored"])
        .env("SH791_LOCK_PATH", &path)
        .stdin(std::process::Stdio::piped());
    let mut child = storyhook_test_support::ChildGuard::spawn_with_output(&mut command).unwrap();
    let mut patience = storyhook_test_support::load_grace::Patience::new(Duration::from_secs(10));
    while !path.with_extension("ready").exists() {
        assert!(!patience.expired(), "lock owner did not start; {patience}");
        std::thread::sleep(Duration::from_millis(10));
    }
    let reset = HttpReset::start(&fixture, &story);
    reset.assert_waiting();
    child.kill_and_reap();
    let body = reset.finished();
    assert_eq!(body["reset"]["state"], "ok", "{body}");
}

#[test]
fn a_released_target_lane_does_not_wait_for_its_runs_other_dispatches() {
    let fixture = ServiceFixture::new();
    let story = active_story(&fixture);
    let run = run(&fixture);
    seed_dispatch(&fixture, &run, 0, &story);
    let lock = std::fs::File::create(lock_path(&fixture, &run)).unwrap();
    lock.lock_exclusive().unwrap();
    let reset = HttpReset::start(&fixture, &story);
    reset.assert_waiting();
    fixture
        .store()
        .write(|tx| {
            let lanes = tx.engine_lanes(&run)?;
            let mut idle = lanes.iter().find(|l| l.lane_index == 1).unwrap().clone();
            idle.lane_index = 0;
            tx.put_engine_lane(&idle)
        })
        .unwrap();
    let body = reset.finished();
    assert_eq!(body["reset"]["state"], "ok", "{body}");
}
