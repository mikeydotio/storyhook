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
    let resp = ureq::get(&format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("Content-Type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
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
    let ct = resp
        .headers()
        .get("Content-Type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
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

// --- Non-GET method returns 405 ---

#[test]
fn web_serve_post_returns_405() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    let root = dir.path().to_path_buf();
    let port = pick_port();
    std::thread::spawn(move || {
        storyhook::web::start_server(&root, port).ok();
    });
    wait_for_server(port);

    let err = ureq::post(&format!("http://127.0.0.1:{port}/api/data"))
        .send(&[] as &[u8])
        .unwrap_err();
    let status = match err {
        ureq::Error::StatusCode(code) => code,
        other => panic!("expected status code error, got: {other}"),
    };
    assert_eq!(status, 405);
}

// --- Special characters in story titles are JSON-escaped ---

#[test]
fn web_serve_api_data_special_chars_in_title() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Fix <script>alert('xss')</script> & \"quotes\""])
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
    let stories = json["stories"].as_array().unwrap();
    assert_eq!(stories.len(), 1);
    let title = stories[0]["story"]["title"].as_str().unwrap();
    assert!(title.contains("<script>"));
    assert!(title.contains("\"quotes\""));
}

// --- Unicode in story titles ---

#[test]
fn web_serve_api_data_unicode_title() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Support emoji: and CJK: \u{4e16}\u{754c}"])
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
    let stories = json["stories"].as_array().unwrap();
    assert_eq!(stories.len(), 1);
    let title = stories[0]["story"]["title"].as_str().unwrap();
    assert!(title.contains("\u{4e16}\u{754c}"));
}

// --- Cache-Control headers ---

#[test]
fn web_serve_root_has_no_cache_header() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    let root = dir.path().to_path_buf();
    let port = pick_port();
    std::thread::spawn(move || {
        storyhook::web::start_server(&root, port).ok();
    });
    wait_for_server(port);

    let resp = ureq::get(&format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let cc = resp
        .headers()
        .get("Cache-Control")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(cc, "no-cache");
}

#[test]
fn web_serve_api_data_has_no_cache_header() {
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
    let cc = resp
        .headers()
        .get("Cache-Control")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(cc, "no-cache");
}

// --- API JSON structure matches dashboard expectations ---

#[test]
fn web_serve_api_json_structure_matches_dashboard() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Story one"])
        .assert()
        .success();
    // Add a label so we can verify the labels field
    story(dir.path())
        .args(["set", "SH-1", "--labels", "backend"])
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

    // Top-level keys
    assert!(json["summary"].is_object(), "missing summary object");
    assert!(json["stories"].is_array(), "missing stories array");
    assert!(json["ready_ids"].is_array(), "missing ready_ids array");
    assert!(json["blocked_ids"].is_array(), "missing blocked_ids array");

    // Summary fields the dashboard reads
    let s = &json["summary"];
    assert!(s["total_open"].is_number(), "missing summary.total_open");
    assert!(
        s["total_closed"].is_number(),
        "missing summary.total_closed"
    );
    assert!(
        s["blocked_count"].is_number(),
        "missing summary.blocked_count"
    );
    assert!(s["ready_count"].is_number(), "missing summary.ready_count");
    assert!(s["by_state"].is_array(), "missing summary.by_state");
    assert!(s["by_priority"].is_array(), "missing summary.by_priority");
    assert!(s["by_type"].is_array(), "missing summary.by_type");

    // by_state is array of [string, number] pairs
    for pair in s["by_state"].as_array().unwrap() {
        let arr = pair.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert!(arr[0].is_string());
        assert!(arr[1].is_number());
    }

    // Story object fields the dashboard reads (v.story.id, etc.)
    let st = &json["stories"][0];
    assert!(st["story"]["id"].is_string(), "missing story.id");
    assert!(st["story"]["title"].is_string(), "missing story.title");
    assert!(st["story"]["state"].is_string(), "missing story.state");
    assert!(!st["story"]["priority"].is_null(), "missing story.priority");
    assert!(
        st["story"]["updated_at"].is_string(),
        "missing story.updated_at"
    );
    // labels should be present (non-empty for this story)
    assert!(st["story"]["labels"].is_array(), "missing story.labels");
    assert!(
        !st["story"]["labels"].as_array().unwrap().is_empty(),
        "labels should contain 'backend'"
    );

    // is_ready and is_blocked per story
    assert!(st.get("is_ready").is_some(), "missing is_ready on story");
    assert!(
        st.get("is_blocked").is_some(),
        "missing is_blocked on story"
    );
}

// --- /api/data meta object ---

#[test]
fn web_serve_api_data_meta_states_are_ordered() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    // Append a state whose slug sorts alphabetically first ("archived" < "done"
    // < "in-progress" < "todo"), so an alphabetical (e.g. BTreeMap-derived)
    // ordering bug would put it first instead of last.
    story(dir.path())
        .args(["state", "add", "archived", "--super", "CLOSED"])
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

    let states: Vec<&str> = json["meta"]["states"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["slug"].as_str().unwrap())
        .collect();
    // Must match .storyhook/states.toml insertion order, not alphabetical.
    assert_eq!(
        states,
        vec!["todo", "in-progress", "done", "archived"],
        "states must be in states.toml order, not alphabetical"
    );

    let done = json["meta"]["states"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["slug"] == "done")
        .unwrap();
    assert_eq!(done["super_state"], "CLOSED");
    let todo = json["meta"]["states"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["slug"] == "todo")
        .unwrap();
    assert_eq!(todo["super_state"], "OPEN");
}

#[test]
fn web_serve_api_data_meta_has_types_priorities_relations_members() {
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
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();

    let types: Vec<&str> = json["meta"]["types"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["slug"].as_str().unwrap())
        .collect();
    assert!(types.contains(&"bug"));
    assert!(types.contains(&"epic"));

    let priorities: Vec<&str> = json["meta"]["priorities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert_eq!(
        priorities,
        vec!["critical", "high", "medium", "low", "none"]
    );

    let relations: Vec<&str> = json["meta"]["relations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    assert!(relations.contains(&"blocks"));
    assert!(relations.contains(&"parent-of"));

    // Fresh project has no members yet.
    assert_eq!(json["meta"]["members"].as_array().unwrap().len(), 0);
}

#[test]
fn web_serve_api_data_meta_includes_members() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["member", "add", "Alice"])
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

    let member_ids: Vec<&str> = json["meta"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(member_ids.contains(&"alice"));
}

// --- GET /api/story/{id} ---

#[test]
fn web_serve_get_story_by_id() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Fetch me"])
        .assert()
        .success();

    let root = dir.path().to_path_buf();
    let port = pick_port();
    std::thread::spawn(move || {
        storyhook::web::start_server(&root, port).ok();
    });
    wait_for_server(port);

    let resp = ureq::get(&format!("http://127.0.0.1:{port}/api/story/SH-1"))
        .call()
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ct = resp
        .headers()
        .get("Content-Type")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.contains("application/json"));
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["result"], "ok");
    // StoryView nests the snapshot under its own "story" key, so the id/title
    // live at story.story.*, matching the shape /api/data's stories[] use.
    assert_eq!(json["story"]["story"]["id"], "SH-1");
    assert_eq!(json["story"]["story"]["title"], "Fetch me");
}

#[test]
fn web_serve_get_story_by_id_unknown_returns_404() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    let root = dir.path().to_path_buf();
    let port = pick_port();
    std::thread::spawn(move || {
        storyhook::web::start_server(&root, port).ok();
    });
    wait_for_server(port);

    let err = ureq::get(&format!("http://127.0.0.1:{port}/api/story/SH-999"))
        .call()
        .unwrap_err();
    let status = match err {
        ureq::Error::StatusCode(code) => code,
        other => panic!("expected status code error, got: {other}"),
    };
    assert_eq!(status, 404);
}

// --- API error responses use the standard envelope + security headers ---

#[test]
fn web_serve_error_reply_has_security_headers() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    let root = dir.path().to_path_buf();
    let port = pick_port();
    std::thread::spawn(move || {
        storyhook::web::start_server(&root, port).ok();
    });
    wait_for_server(port);

    // Disable ureq's default "non-2xx is an Err" behavior so we can inspect
    // the 404 response's headers and body directly.
    let resp = ureq::get(format!("http://127.0.0.1:{port}/api/story/SH-999"))
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .unwrap();
    assert_eq!(resp.status(), 404);
    for header in [
        "X-Content-Type-Options",
        "X-Frame-Options",
        "Content-Security-Policy",
    ] {
        assert!(
            resp.headers().get(header).is_some(),
            "missing security header: {header}"
        );
    }
}

#[test]
fn web_serve_error_reply_uses_standard_envelope() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();

    let root = dir.path().to_path_buf();
    let port = pick_port();
    std::thread::spawn(move || {
        storyhook::web::start_server(&root, port).ok();
    });
    wait_for_server(port);

    let resp = ureq::get(format!("http://127.0.0.1:{port}/api/story/SH-999"))
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .unwrap();
    assert_eq!(resp.status(), 404);
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["result"], "error");
    assert!(json["error"].is_string());
    assert!(json["exit_code"].is_number());
}

// --- build_report_data with blocked stories ---

#[test]
fn build_report_data_with_blocked_story() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Blocking story"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Blocked story"])
        .assert()
        .success();
    // SH-2 depends on SH-1
    story(dir.path())
        .args(["link", "SH-1", "blocks", "SH-2"])
        .assert()
        .success();

    let data = storyhook::app::build_report_data(dir.path()).unwrap();
    assert_eq!(data.summary.total_open, 2);
    // SH-1 should be ready, SH-2 should be blocked
    assert!(
        data.ready_ids.contains(&"SH-1".to_string()),
        "SH-1 should be ready"
    );
    assert!(
        data.blocked_ids.contains(&"SH-2".to_string()),
        "SH-2 should be blocked"
    );
    assert_eq!(data.summary.blocked_count, 1);
    assert_eq!(data.summary.ready_count, 1);
}

// --- build_report_data with priority counts ---

#[test]
fn build_report_data_counts_priorities() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "High priority"])
        .assert()
        .success();
    story(dir.path())
        .args(["set", "SH-1", "--priority", "high"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "No priority"])
        .assert()
        .success();

    let data = storyhook::app::build_report_data(dir.path()).unwrap();
    // Only non-none priorities are counted in by_priority
    let high_count: usize = data
        .summary
        .by_priority
        .iter()
        .filter(|(name, _)| name == "high")
        .map(|(_, count)| *count)
        .sum();
    assert_eq!(high_count, 1);
}

// --- build_report_data with type counts ---

#[test]
fn build_report_data_counts_types() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Epic story", "--type", "epic"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Untyped story"])
        .assert()
        .success();

    let data = storyhook::app::build_report_data(dir.path()).unwrap();
    let type_names: Vec<&str> = data
        .summary
        .by_type
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    assert!(
        type_names.contains(&"epic"),
        "should have 'epic' type count"
    );
    assert!(
        type_names.contains(&"Default"),
        "should have 'Default' type count"
    );
}

// --- Concurrent requests don't hang ---

#[test]
fn web_serve_handles_concurrent_requests() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Concurrent test"])
        .assert()
        .success();

    let root = dir.path().to_path_buf();
    let port = pick_port();
    std::thread::spawn(move || {
        storyhook::web::start_server(&root, port).ok();
    });
    wait_for_server(port);

    // Fire 10 concurrent requests
    let handles: Vec<_> = (0..10)
        .map(|_| {
            let url = format!("http://127.0.0.1:{port}/api/data");
            std::thread::spawn(move || {
                let resp = ureq::get(&url).call().unwrap();
                assert_eq!(resp.status(), 200);
                let body = resp.into_body().read_to_string().unwrap();
                let json: serde_json::Value = serde_json::from_str(&body).unwrap();
                assert_eq!(json["summary"]["total_open"], 1);
            })
        })
        .collect();

    for h in handles {
        h.join().expect("concurrent request thread panicked");
    }
}

// --- HELP_TEXT includes web commands ---

#[test]
fn help_text_includes_web_commands() {
    let help = storyhook::cli::HELP_TEXT;
    assert!(
        help.contains("story web start"),
        "HELP_TEXT should mention 'story web start'"
    );
    assert!(
        help.contains("story web stop"),
        "HELP_TEXT should mention 'story web stop'"
    );
}

// --- Default port is 3456 ---

#[test]
fn web_start_default_port_is_3456() {
    let dir = tempdir().unwrap();
    // We test this via the CLI output: start without --port should mention 3456
    // But actually, the best test is to verify parse_web returns port 3456 by default.
    // We invoke the CLI parse directly.
    let inv = storyhook::cli::parse_invocation(&["web".to_string(), "start".to_string()]).unwrap();
    match inv {
        storyhook::cli::Invocation::Web {
            action: storyhook::cli::WebAction::Start { port },
        } => {
            assert_eq!(port, 3456, "default port should be 3456");
        }
        other => panic!("expected Web::Start, got {:?}", other),
    }
    drop(dir);
}

// --- CLI parse_web unit tests ---

#[test]
fn web_parse_start_with_custom_port() {
    let inv = storyhook::cli::parse_invocation(&[
        "web".to_string(),
        "start".to_string(),
        "--port".to_string(),
        "8080".to_string(),
    ])
    .unwrap();
    match inv {
        storyhook::cli::Invocation::Web {
            action: storyhook::cli::WebAction::Start { port },
        } => {
            assert_eq!(port, 8080);
        }
        other => panic!("expected Web::Start with port 8080, got {:?}", other),
    }
}

#[test]
fn web_parse_stop() {
    let inv = storyhook::cli::parse_invocation(&["web".to_string(), "stop".to_string()]).unwrap();
    assert!(matches!(
        inv,
        storyhook::cli::Invocation::Web {
            action: storyhook::cli::WebAction::Stop
        }
    ));
}

#[test]
fn web_parse_status() {
    let inv = storyhook::cli::parse_invocation(&["web".to_string(), "status".to_string()]).unwrap();
    assert!(matches!(
        inv,
        storyhook::cli::Invocation::Web {
            action: storyhook::cli::WebAction::Status
        }
    ));
}

#[test]
fn web_parse_serve_internal() {
    let inv = storyhook::cli::parse_invocation(&[
        "web".to_string(),
        "--serve".to_string(),
        "--port".to_string(),
        "4000".to_string(),
        "--root".to_string(),
        "/tmp/test".to_string(),
    ])
    .unwrap();
    match inv {
        storyhook::cli::Invocation::Web {
            action: storyhook::cli::WebAction::Serve { port, root },
        } => {
            assert_eq!(port, 4000);
            assert_eq!(root, std::path::PathBuf::from("/tmp/test"));
        }
        other => panic!("expected Web::Serve, got {:?}", other),
    }
}

#[test]
fn web_parse_serve_missing_root_errors() {
    let result = storyhook::cli::parse_invocation(&[
        "web".to_string(),
        "--serve".to_string(),
        "--port".to_string(),
        "4000".to_string(),
    ]);
    assert!(result.is_err());
}

#[test]
fn web_start_extra_unknown_flag_errors() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["web", "start", "--verbose"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("usage:"));
}

// --- Port boundary tests ---

#[test]
fn web_start_port_one_is_valid() {
    // Port 1 should be parseable (will likely fail to bind, but CLI should accept it)
    let inv = storyhook::cli::parse_invocation(&[
        "web".to_string(),
        "start".to_string(),
        "--port".to_string(),
        "1".to_string(),
    ])
    .unwrap();
    match inv {
        storyhook::cli::Invocation::Web {
            action: storyhook::cli::WebAction::Start { port },
        } => assert_eq!(port, 1),
        other => panic!("expected Web::Start, got {:?}", other),
    }
}

#[test]
fn web_start_port_65535_is_valid() {
    let inv = storyhook::cli::parse_invocation(&[
        "web".to_string(),
        "start".to_string(),
        "--port".to_string(),
        "65535".to_string(),
    ])
    .unwrap();
    match inv {
        storyhook::cli::Invocation::Web {
            action: storyhook::cli::WebAction::Start { port },
        } => assert_eq!(port, 65535),
        other => panic!("expected Web::Start, got {:?}", other),
    }
}

#[test]
fn web_start_port_65536_is_invalid() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["web", "start", "--port", "65536"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("invalid port"));
}

#[test]
fn web_start_port_negative_is_invalid() {
    let dir = tempdir().unwrap();
    story(dir.path())
        .args(["web", "start", "--port", "-1"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("invalid port"));
}

// --- is_ready and is_blocked correctness ---

#[test]
fn web_serve_api_data_ready_and_blocked_flags_correct() {
    let dir = tempdir().unwrap();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Ready story"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Blocked story"])
        .assert()
        .success();
    // SH-2 depends on SH-1 (which is open), so SH-2 is blocked
    story(dir.path())
        .args(["link", "SH-1", "blocks", "SH-2"])
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

    let stories = json["stories"].as_array().unwrap();
    for s in stories {
        let id = s["story"]["id"].as_str().unwrap();
        match id {
            "SH-1" => {
                assert_eq!(s["is_ready"], true, "SH-1 should be ready");
                assert_eq!(s["is_blocked"], false, "SH-1 should not be blocked");
            }
            "SH-2" => {
                assert_eq!(s["is_ready"], false, "SH-2 should not be ready");
                assert_eq!(s["is_blocked"], true, "SH-2 should be blocked");
            }
            other => panic!("unexpected story ID: {other}"),
        }
    }

    // Also verify ready_ids and blocked_ids arrays
    let ready: Vec<&str> = json["ready_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let blocked: Vec<&str> = json["blocked_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(ready.contains(&"SH-1"));
    assert!(blocked.contains(&"SH-2"));
}

// --- build_report_data on non-project directory ---

#[test]
fn build_report_data_non_project_errors() {
    let dir = tempdir().unwrap();
    let result = storyhook::app::build_report_data(dir.path());
    assert!(
        result.is_err(),
        "build_report_data on non-project dir should error"
    );
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
