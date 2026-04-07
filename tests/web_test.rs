use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

// --- CLI parsing tests ---

#[test]
fn web_no_subcommand_shows_usage() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .arg("web")
        .assert()
        .failure()
        .stdout(predicate::str::contains("usage:"));
}

#[test]
fn web_invalid_subcommand_shows_usage() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["web", "restart"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("usage:"));
}

#[test]
fn web_start_invalid_port_non_numeric() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["web", "start", "--port", "abc"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("invalid port"));
}

#[test]
fn web_start_invalid_port_zero() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["web", "start", "--port", "0"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("invalid port"));
}

#[test]
fn web_start_invalid_port_too_large() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["web", "start", "--port", "99999"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("invalid port"));
}

#[test]
fn web_start_port_missing_value() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["web", "start", "--port"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("--port requires a value"));
}

// --- Status tests ---

#[test]
fn web_status_not_running() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["web", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Web UI is not running"));
}

#[test]
fn web_stop_when_not_running() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .args(["web", "stop"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Web UI is not running"));
}

#[test]
fn web_start_requires_project() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["web", "start"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("not initialized"));
}

// --- Server integration tests ---

#[test]
fn web_serve_and_query_root() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    // Start server in foreground in a thread
    let root = dir.path().to_path_buf();
    let port = pick_port();
    let handle = std::thread::spawn(move || {
        storyhook::web::start_server(&root, port).ok();
    });

    // Wait for server to start
    wait_for_server(port);

    // GET / should return HTML
    let resp = ureq::get(&format!("http://127.0.0.1:{port}/")).call().unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp.headers().get("Content-Type").unwrap().to_str().unwrap().to_string();
    assert!(ct.contains("text/html"));
    let body = resp.into_body().read_to_string().unwrap();
    assert!(body.contains("Storyhook"));

    drop(handle); // server thread runs until process dies — that's OK for test
}

#[test]
fn web_serve_api_data_empty_project() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    let root = dir.path().to_path_buf();
    let port = pick_port();
    std::thread::spawn(move || {
        storyhook::web::start_server(&root, port).ok();
    });
    wait_for_server(port);

    let resp = ureq::get(&format!("http://127.0.0.1:{port}/api/data"))
        .call()
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp.headers().get("Content-Type").unwrap().to_str().unwrap().to_string();
    assert!(ct.contains("application/json"));

    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["summary"]["total_open"], 0);
    assert_eq!(json["stories"].as_array().unwrap().len(), 0);
    assert_eq!(json["ready_ids"].as_array().unwrap().len(), 0);
    assert_eq!(json["blocked_ids"].as_array().unwrap().len(), 0);
}

#[test]
fn web_serve_api_data_with_stories() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Build feature"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Fix bug"])
        .assert()
        .success();

    let root = dir.path().to_path_buf();
    let port = pick_port();
    std::thread::spawn(move || {
        storyhook::web::start_server(&root, port).ok();
    });
    wait_for_server(port);

    let resp = ureq::get(&format!("http://127.0.0.1:{port}/api/data"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(json["summary"]["total_open"], 2);
    let stories = json["stories"].as_array().unwrap();
    assert_eq!(stories.len(), 2);

    // Each story should have is_ready and is_blocked fields
    for s in stories {
        assert!(s.get("is_ready").is_some());
        assert!(s.get("is_blocked").is_some());
        assert!(s["story"]["id"].is_string());
    }
}

#[test]
fn web_serve_404_unknown_route() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    let root = dir.path().to_path_buf();
    let port = pick_port();
    std::thread::spawn(move || {
        storyhook::web::start_server(&root, port).ok();
    });
    wait_for_server(port);

    // ureq v3 returns non-2xx as errors
    let err = ureq::get(&format!("http://127.0.0.1:{port}/nonexistent"))
        .call()
        .unwrap_err();
    let status = match err {
        ureq::Error::StatusCode(code) => code,
        other => panic!("expected status code error, got: {other}"),
    };
    assert_eq!(status, 404);
}

// --- build_report_data tests ---

#[test]
fn build_report_data_empty_project() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    let data = storyhook::app::build_report_data(dir.path()).unwrap();
    assert_eq!(data.summary.total_open, 0);
    assert_eq!(data.summary.total_closed, 0);
    assert!(data.stories.is_empty());
    assert!(data.ready_ids.is_empty());
    assert!(data.blocked_ids.is_empty());
}

#[test]
fn build_report_data_with_mixed_states() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Open story"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Closed story"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-2", "done"])
        .assert()
        .success();

    let data = storyhook::app::build_report_data(dir.path()).unwrap();
    assert_eq!(data.summary.total_open, 1);
    assert_eq!(data.summary.total_closed, 1);
    assert_eq!(data.stories.len(), 2);
    assert!(data.ready_ids.contains(&"SH-1".to_string()));
}

#[test]
fn report_data_serializes_to_json() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "JSON test"])
        .assert()
        .success();

    let data = storyhook::app::build_report_data(dir.path()).unwrap();
    let json = serde_json::to_string(&data).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(parsed["summary"].is_object());
    assert!(parsed["stories"].is_array());
    assert!(parsed["ready_ids"].is_array());
    assert!(parsed["blocked_ids"].is_array());
}

// --- Help topic test ---

#[test]
fn help_web_topic_exists() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["help", "web"])
        .assert()
        .success()
        .stdout(predicate::str::contains("story web start"))
        .stdout(predicate::str::contains("--port"))
        .stdout(predicate::str::contains("story web stop"));
}

// --- Utilities ---

use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

// Use a fixed high-range counter to avoid port reuse across parallel tests
static PORT_COUNTER: AtomicU16 = AtomicU16::new(19000);

fn pick_port() -> u16 {
    PORT_COUNTER.fetch_add(1, Ordering::Relaxed)
}

fn wait_for_server(port: u16) {
    let start = Instant::now();
    loop {
        if std::net::TcpStream::connect(format!("127.0.0.1:{port}")).is_ok() {
            return;
        }
        if start.elapsed() > Duration::from_secs(5) {
            panic!("Server on port {port} did not start within 5 seconds");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
