//! The reset HTTP contract exercises the production daemon and real store.
use std::sync::Arc;
use std::time::{Duration, Instant};
use storyhook::service::{NewStoryInput, StoryService};
use storyhook::store::{ReadOps, SqliteStore, Store, StoryNo, WriteOps};
use storyhook_test_support::{ServiceFixture, serve};

#[test]
fn reset_requires_auth_and_confirmation_then_polls_a_scoped_durable_receipt() {
    let fixture = ServiceFixture::new();
    fixture
        .store()
        .write(|tx| tx.set_checkout_path(fixture.project(), None))
        .unwrap();
    let ctx = fixture.ctx();
    let story = StoryService::new(&ctx)
        .create(&NewStoryInput {
            title: "HTTP reset".into(),
            state: Some("in-progress".into()),
            ..Default::default()
        })
        .unwrap();
    let store = Arc::new(SqliteStore::open(fixture.env().store_path()).unwrap());
    let server = serve(store, fixture.env());
    let url = format!(
        "http://127.0.0.1:{}/api/repos/fixture/story/{}/reset",
        server.port(),
        story.id
    );
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into();
    let no_auth = agent
        .post(&url)
        .header("X-Storyhook", "1")
        .send_json(serde_json::json!({"confirmation":story.id}))
        .unwrap();
    assert_eq!(no_auth.status(), 401);
    let auth = server.token.clone();
    let no_guard = agent
        .post(&url)
        .header("X-Storyhook-Token", &auth)
        .send_json(serde_json::json!({"confirmation":story.id}))
        .unwrap();
    assert_eq!(no_guard.status(), 403);
    let wrong = agent
        .post(&url)
        .header("X-Storyhook-Token", &auth)
        .header("X-Storyhook", "1")
        .send_json(serde_json::json!({"confirmation":"wrong"}))
        .unwrap();
    assert_eq!(wrong.status(), 422);
    assert!(
        fixture
            .store()
            .read(|tx| tx.story_reset(fixture.project(), StoryNo::new(1)))
            .unwrap()
            .is_none()
    );
    let mut response = agent
        .post(&url)
        .header("X-Storyhook-Token", &auth)
        .header("X-Storyhook", "1")
        .send_json(serde_json::json!({"confirmation":story.id}))
        .unwrap();
    assert_eq!(response.status(), 202);
    let body: serde_json::Value = response.body_mut().read_json().unwrap();
    let handle = body["reset"]["handle"].as_str().unwrap();
    let poll_url = format!("{url}/{handle}");
    assert_eq!(agent.get(&poll_url).call().unwrap().status(), 401);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut response = agent
            .get(&poll_url)
            .header("X-Storyhook-Token", &auth)
            .call()
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: serde_json::Value = response.body_mut().read_json().unwrap();
        if body["reset"]["state"] == "ok" {
            break;
        }
        assert_ne!(body["reset"]["state"], "error", "{body}");
        assert!(Instant::now() < deadline, "reset never finished: {body}");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fixture
            .store()
            .read(|tx| tx.story(fixture.project(), StoryNo::new(1)))
            .unwrap()
            .unwrap()
            .state,
        "todo"
    );
    assert_eq!(
        agent
            .get(poll_url.replace(&story.id, "SH-999"))
            .header("X-Storyhook-Token", &auth)
            .call()
            .unwrap()
            .status(),
        404
    );
}
