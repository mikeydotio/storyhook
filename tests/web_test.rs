use assert_cmd::Command;
use predicates::prelude::*;

fn story(dir: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("story").unwrap();
    cmd.current_dir(dir);
    cmd
}

/// Registers `dir` (a fresh, already-`story init`-ed project directory) in a
/// brand-new temp registry file, returning the registry's own `TempDir`
/// guard (bind this to a local for the rest of the test, exactly the way
/// `dir` itself is held, so the file isn't deleted before the server reads
/// it), the registry's path (pass to `start_server`), and the id
/// `Registry::register` minted for `dir` (use to build `/api/repos/<id>/...`
/// URLs).
fn register_repo(dir: &std::path::Path) -> (tempfile::TempDir, std::path::PathBuf, String) {
    let registry_dir = scratch_dir();
    let registry_path = registry_dir.path().join("registry.toml");
    let repo = storyhook::registry::with_lock_at(&registry_path, |r| r.register(dir, None))
        .expect("registering the test repo must succeed");
    (registry_dir, registry_path, repo.id)
}

// --- CLI parsing tests ---

#[test]
fn web_no_subcommand_shows_usage() {
    let dir = scratch_dir();
    story(dir.path())
        .arg("web")
        .assert()
        .failure()
        .stdout(predicate::str::contains("usage:"));
}

#[test]
fn web_invalid_subcommand_shows_usage() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["web", "restart"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("usage:"));
}

#[test]
fn web_start_invalid_port_non_numeric() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["web", "start", "--port", "abc"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("invalid port"));
}

#[test]
fn web_start_invalid_port_zero() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["web", "start", "--port", "0"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("invalid port"));
}

#[test]
fn web_start_invalid_port_too_large() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["web", "start", "--port", "99999"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("invalid port"));
}

#[test]
fn web_start_port_missing_value() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["web", "start", "--port"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("--port requires a value"));
}

// --- Status tests ---
//
// `web status`/`web stop` are now registry-scoped (one global daemon, not
// one per repo), so they read `~/.storyhook/web.pid` — isolate `$HOME` to a
// temp dir so these never touch the developer's real dashboard state.

#[test]
fn web_status_not_running() {
    let dir = scratch_dir();
    let home = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Web UI is not running"));
}

#[test]
fn web_stop_when_not_running() {
    let dir = scratch_dir();
    let home = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "stop"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Web UI is not running"));
}

// --- open / address tests ---
//
// `web open` and `web address` read the same `~/.storyhook/web.pid` as
// `status`/`stop`, so `$HOME` is isolated to keep them off the developer's
// real dashboard. Unlike `status`, they must *fail* (non-zero) when nothing is
// running, and the error must carry a help-like summary of the web commands.
// The success path drives the browser/clipboard through the `$BROWSER` /
// `$STORYHOOK_CLIPBOARD_CMD` seams so no real browser opens and the real
// clipboard is never clobbered.

#[test]
fn web_open_not_running_fails_with_summary() {
    let dir = scratch_dir();
    let home = scratch_dir();
    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "open"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("not running"))
        .stdout(predicate::str::contains("story web start"));
}

#[test]
fn web_address_not_running_fails_with_summary() {
    let dir = scratch_dir();
    let home = scratch_dir();
    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "address"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("not running"))
        .stdout(predicate::str::contains("story web start"));
}

#[test]
fn web_open_and_address_succeed_when_running() {
    let home = scratch_dir();
    let dir = scratch_dir(); // deliberately NOT a storyhook project
    let port = reserve_port();
    let _daemon = DaemonGuard {
        home: home.path().to_path_buf(),
        cwd: dir.path().to_path_buf(),
    };

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "start", "--port", &port.to_string()])
        .assert()
        .success();
    wait_for_server(port);

    // `web open` targets loopback; browser launch is stubbed via $BROWSER=true.
    story(dir.path())
        .env("HOME", home.path())
        .env("BROWSER", "true")
        .args(["web", "open"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "http://127.0.0.1:{port}/"
        )));

    // `web address` copies to the clipboard, stubbed via $STORYHOOK_CLIPBOARD_CMD=cat.
    // Host is left unasserted because the CI/dev host may or may not run Tailscale.
    story(dir.path())
        .env("HOME", home.path())
        .env("STORYHOOK_CLIPBOARD_CMD", "cat")
        .args(["web", "address"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(":{port}/")))
        .stdout(predicate::str::contains("clipboard"));

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "stop"])
        .assert()
        .success();
}

/// Regression coverage for the #35 follow-up: `story web
/// start`/`status`/`address` now advertise this machine's MagicDNS FQDN
/// (not just its raw tailnet IP) whenever Tailscale reports one, since the
/// FQDN is trusted for mutations too — unlike the bare short label (see
/// `web::TailnetIdentity::advertise_host`). Skips gracefully wherever
/// MagicDNS isn't available, the same as every other tailnet-gated test in
/// this file (see `test_env_tailnet_fqdn`).
#[test]
fn web_start_status_address_advertise_magic_dns_fqdn_when_available() {
    let Some(fqdn) = test_env_tailnet_fqdn() else {
        eprintln!("skipping: no tailscale MagicDNS name available in this environment");
        return;
    };

    let home = scratch_dir();
    let dir = scratch_dir();
    let port = reserve_port();
    let _daemon = DaemonGuard {
        home: home.path().to_path_buf(),
        cwd: dir.path().to_path_buf(),
    };

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "start", "--port", &port.to_string()])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("http://{fqdn}:{port}")));
    wait_for_server(port);

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("http://{fqdn}:{port}")));

    story(dir.path())
        .env("HOME", home.path())
        .env("STORYHOOK_CLIPBOARD_CMD", "cat")
        .args(["web", "address"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!("http://{fqdn}:{port}/")));

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "stop"])
        .assert()
        .success();
}

/// `story web start` now launches the single *global* dashboard daemon — it
/// no longer requires being run from inside a storyhook project (repos are
/// added afterwards via `story web register`). `$HOME` is isolated so the
/// real background process this test spawns (and its pid/lock/log files)
/// never touches the developer's actual `~/.storyhook/`; `web stop` at the
/// end cleans it up so the test doesn't leak a bound port.
#[test]
fn web_start_succeeds_outside_a_project() {
    let home = scratch_dir();
    let dir = scratch_dir(); // deliberately NOT a storyhook project
    let port = reserve_port();
    let _daemon = DaemonGuard {
        home: home.path().to_path_buf(),
        cwd: dir.path().to_path_buf(),
    };

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "start", "--port", &port.to_string()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Web UI started"));

    wait_for_server(port);

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "stop"])
        .assert()
        .success();
}

// --- Server integration tests ---

#[test]
fn web_serve_and_query_root() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, _repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

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
}

/// The redesigned dashboard is still a single self-contained embedded file
/// (no build step, no CDN) with a Board view, a List view, a detail drawer,
/// and a create-story modal. These markers guard against a future edit
/// accidentally dropping one of those surfaces.
#[test]
fn web_serve_root_html_has_board_list_drawer_markers() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, _repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = ureq::get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();

    // Single embedded file: exactly one <style> and one <script>, no
    // external assets (CSP is script-src/style-src 'unsafe-inline' only).
    assert_eq!(body.matches("<style>").count(), 1);
    assert_eq!(body.matches("<script>").count(), 1);
    assert!(!body.contains("<link"), "no external stylesheet links");
    assert!(!body.contains("cdn."), "no CDN references");
    // Regression guard: web_dashboard.html once had a stray raw NUL byte
    // (a `.join("\0")` where a space belonged), which made the file look
    // binary to naive tooling and shipped a NUL into the served page.
    assert!(
        !body.contains('\0'),
        "served dashboard HTML must never contain a raw NUL byte"
    );

    // Board + List + view toggle
    assert!(body.contains(r#"id="board-view""#));
    assert!(body.contains(r#"id="list-view""#));
    assert!(body.contains(r#"id="view-toggle""#));
    // Detail drawer
    assert!(body.contains(r#"id="drawer""#));
    assert!(body.contains(r#"id="drawer-body""#));
    // Create-story modal, including the fields added for #36 (description,
    // priority, and the label combobox's mount point)
    assert!(body.contains(r#"id="create-modal""#));
    assert!(body.contains(r#"id="create-title""#));
    assert!(body.contains(r#"id="create-description""#));
    assert!(body.contains(r#"id="create-priority""#));
    assert!(body.contains(r#"id="create-labels-field""#));
    // Multi-repo screens (#20): repo selector, home dashboard, settings
    assert!(body.contains(r#"id="repo-select""#));
    assert!(body.contains(r#"id="home-view""#));
    assert!(body.contains(r#"id="settings-view""#));
    assert!(body.contains(r#"id="home-btn""#));
    assert!(body.contains(r#"id="settings-btn""#));
    // Mutation API call sites carry the CSRF guard header
    assert!(body.contains("X-Storyhook"));
    // Statuses editor (SH-41): its own styles, the settings-table entry
    // point, and the four calls that reach the states API.
    assert!(body.contains(".status-row"));
    assert!(body.contains(".status-add"));
    assert!(body.contains("goStatuses"));
    assert!(body.contains("function statusMutation"));
    for call in ["/states", "move_stories_to", "super_state"] {
        assert!(
            body.contains(call),
            "statuses editor should reference {call}"
        );
    }
}

#[test]
fn web_serve_api_data_empty_project() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = ureq::get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Build feature"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Fix bug"])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = ureq::get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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
fn web_serve_api_data_excludes_deleted_stories() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Build feature"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Fix bug"])
        .assert()
        .success();
    story(dir.path())
        .args(["delete", "SH-2", "duplicate"])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = ureq::get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();

    let stories = json["stories"].as_array().unwrap();
    let ids: Vec<&str> = stories
        .iter()
        .map(|s| s["story"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["SH-1"],
        "deleted story SH-2 leaked into /api/data"
    );
}

#[test]
fn web_serve_404_unknown_route() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, _repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

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
    let dir = scratch_dir();
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
    let dir = scratch_dir();
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
    let dir = scratch_dir();
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
    let dir = scratch_dir();
    story(dir.path())
        .args(["help", "web"])
        .assert()
        .success()
        .stdout(predicate::str::contains("story web start"))
        .stdout(predicate::str::contains("--port"))
        .stdout(predicate::str::contains("story web stop"))
        .stdout(predicate::str::contains("story web open"))
        .stdout(predicate::str::contains("story web address"));
}

// --- Non-GET method returns 405 ---

#[test]
fn web_serve_post_returns_405() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = ureq::post(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Fix <script>alert('xss')</script> & \"quotes\""])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = ureq::get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Support emoji: and CJK: \u{4e16}\u{754c}"])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = ureq::get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, _repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

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
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = ureq::get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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
    let dir = scratch_dir();
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

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = ureq::get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    // Append a state whose slug sorts alphabetically first ("archived" < "done"
    // < "in-progress" < "todo"), so an alphabetical (e.g. BTreeMap-derived)
    // ordering bug would put it first instead of last.
    story(dir.path())
        .args(["state", "add", "archived", "--super", "CLOSED"])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = ureq::get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = ureq::get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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
fn web_meta_includes_sorted_unique_labels() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "A", "--labels", "web,bug"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "B", "--labels", "web,cli"])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = ureq::get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();

    let labels: Vec<&str> = json["meta"]["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert_eq!(labels, vec!["bug", "cli", "web"]);
}

#[test]
fn web_serve_api_data_meta_includes_members() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["member", "add", "Alice"])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = ureq::get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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

// --- GET /api/repos/{id}/story/{sid} ---

#[test]
fn web_serve_get_story_by_id() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Fetch me"])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = ureq::get(&format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"
    ))
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
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = ureq::get(&format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-999"
    ))
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
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    // Disable ureq's default "non-2xx is an Err" behavior so we can inspect
    // the 404 response's headers and body directly.
    let resp = ureq::get(format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-999"
    ))
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
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = ureq::get(format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-999"
    ))
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

// --- Mutation API: helpers ---

/// Sends a guarded POST with a JSON body — every real mutation request must
/// set both of these for `mutation_guard_ok` to pass.
fn post_json(url: &str, body: &str) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    ureq::post(url)
        .header("X-Storyhook", "1")
        .content_type("application/json")
        .send(body)
}

fn patch_json(url: &str, body: &str) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    ureq::patch(url)
        .header("X-Storyhook", "1")
        .content_type("application/json")
        .send(body)
}

fn delete_json(url: &str, body: &str) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    ureq::delete(url)
        .header("X-Storyhook", "1")
        .force_send_body()
        .content_type("application/json")
        .send(body)
}

/// Same as `post_json` but without the guard header, for guard-rejection tests.
fn post_json_unguarded(
    url: &str,
    body: &str,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    ureq::post(url).content_type("application/json").send(body)
}

fn status_of(err: ureq::Error) -> u16 {
    match err {
        ureq::Error::StatusCode(code) => code,
        other => panic!("expected status code error, got: {other}"),
    }
}

fn story_field(json: &serde_json::Value, field: &str) -> serde_json::Value {
    json["story"]["story"][field].clone()
}

// --- Mutation API: create ---

#[test]
fn web_create_story_returns_201_and_story() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story"),
        r#"{"title":"New via web","type":"bug"}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 201);
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(story_field(&json, "id"), "SH-1");
    assert_eq!(story_field(&json, "title"), "New via web");

    // Shows up in /api/repos/{id}/data.
    let data = ureq::get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let data_json: serde_json::Value =
        serde_json::from_str(&data.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(data_json["summary"]["total_open"], 1);
}

#[test]
fn web_create_story_missing_title_is_400() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story"),
        r#"{}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 400);
}

#[test]
fn web_create_story_with_description_labels_priority() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story"),
        r#"{"title":"Rich story","description":"Full details here","priority":"high","labels":["bug","web"]}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 201);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(story_field(&json, "description"), "Full details here");
    assert_eq!(story_field(&json, "priority"), "high");
    assert_eq!(
        story_field(&json, "labels"),
        serde_json::json!(["bug", "web"])
    );
}

#[test]
fn web_create_story_invalid_priority_is_422() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story"),
        r#"{"title":"Bad priority","priority":"urgent"}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 422);

    // No orphaned story should have been created.
    let data = ureq::get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let data_json: serde_json::Value =
        serde_json::from_str(&data.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(data_json["summary"]["total_open"], 0);
}

#[test]
fn web_create_story_without_guard_header_is_403() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = post_json_unguarded(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story"),
        r#"{"title":"Should not be created"}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 403);

    let data = ureq::get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let data_json: serde_json::Value =
        serde_json::from_str(&data.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(data_json["summary"]["total_open"], 0);
}

// --- Mutation API: move (board drag-and-drop) ---

#[test]
fn web_move_story_changes_state() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Movable"])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move"),
        r#"{"state":"in-progress"}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(story_field(&json, "state"), "in-progress");
}

#[test]
fn web_move_story_to_closed_state_archives() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Will be archived"])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move"),
        r#"{"state":"done"}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 200);

    let data = ureq::get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let data_json: serde_json::Value =
        serde_json::from_str(&data.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(data_json["summary"]["total_open"], 0);
    assert_eq!(data_json["summary"]["total_closed"], 1);
    // Archived stories still appear in stories[] so the board's Closed
    // column can render the card instead of it vanishing.
    let stories = data_json["stories"].as_array().unwrap();
    assert_eq!(stories.len(), 1);
    assert_eq!(stories[0]["story"]["state"], "done");
}

#[test]
fn web_move_story_invalid_state_is_422() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move"),
        r#"{"state":"nonexistent"}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 422);
}

#[test]
fn web_move_unknown_story_is_404() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-999/move"),
        r#"{"state":"in-progress"}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 404);
}

// --- Mutation API: comment, priority, assign, labels, block/unblock, reopen ---

#[test]
fn web_comment_story_appends_comment() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/comment"),
        r#"{"text":"hello from web"}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    let comments = json["story"]["story"]["comments"].as_array().unwrap();
    assert!(comments.iter().any(|c| c["text"] == "hello from web"));
}

#[test]
fn web_priority_story_sets_priority() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/priority"),
        r#"{"priority":"critical"}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(story_field(&json, "priority"), "critical");
}

#[test]
fn web_assign_story_to_valid_member_succeeds() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();
    story(dir.path())
        .args(["member", "add", "Alice"])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/assign"),
        r#"{"member":"alice"}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(story_field(&json, "assignee"), "alice");
}

#[test]
fn web_assign_story_to_missing_member_is_404() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    // storage::find_member returns AppError::NotFound (not Validation) for
    // an unknown member id, matching `story assign <id> <unknown-member>`
    // on the CLI (exit code 3, not 2).
    let err = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/assign"),
        r#"{"member":"nobody"}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 404);
}

#[test]
fn web_labels_add_and_remove() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/labels"),
        r#"{"add":["backend","urgent"]}"#,
    )
    .unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    let labels: Vec<&str> = json["story"]["story"]["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert!(labels.contains(&"backend"));
    assert!(labels.contains(&"urgent"));

    let resp2 = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/labels"),
        r#"{"remove":["urgent"]}"#,
    )
    .unwrap();
    let json2: serde_json::Value =
        serde_json::from_str(&resp2.into_body().read_to_string().unwrap()).unwrap();
    let labels2: Vec<&str> = json2["story"]["story"]["labels"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert!(labels2.contains(&"backend"));
    assert!(!labels2.contains(&"urgent"));
}

#[test]
fn web_labels_empty_body_is_400() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/labels"),
        r#"{}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 400);
}

#[test]
fn web_block_and_unblock_story() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/block"),
        r#"{"reason":"waiting on design"}"#,
    )
    .unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(story_field(&json, "awaiting"), "waiting on design");

    let resp2 = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/unblock"),
        "",
    )
    .unwrap();
    let json2: serde_json::Value =
        serde_json::from_str(&resp2.into_body().read_to_string().unwrap()).unwrap();
    assert!(story_field(&json2, "awaiting").is_null());
}

#[test]
fn web_reopen_archived_story() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();
    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/reopen"),
        "",
    )
    .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(story_field(&json, "superstate"), "OPEN");
}

/// Regression test for #23: `Invocation::Reopen` gained a `force` field (#18)
/// that the web route didn't plumb through at all, so a soft-deleted story
/// could never be undeleted via the API — only the CLI's `--force` reached
/// it. Without `force` (an empty JSON body), reopening a deleted story must
/// fail the same guarded-undelete check the CLI enforces, surfaced as a 422
/// (`AppError::Validation`), and leave the story untouched.
#[test]
fn web_reopen_deleted_story_without_force_is_422() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();
    story(dir.path())
        .args(["delete", "SH-1", "created in error"])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/reopen"),
        "",
    )
    .unwrap_err();
    assert_eq!(status_of(err), 422);

    let show = ureq::get(format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"
    ))
    .call()
    .unwrap();
    let show_json: serde_json::Value =
        serde_json::from_str(&show.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(story_field(&show_json, "superstate"), "CLOSED");
    assert_eq!(story_field(&show_json, "deleted"), true);
}

/// Companion to the test above: `{"force": true}` in the body mirrors the
/// CLI's `story reopen <id> --force` and successfully undeletes.
#[test]
fn web_reopen_deleted_story_with_force_undeletes() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();
    story(dir.path())
        .args(["delete", "SH-1", "created in error"])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/reopen"),
        r#"{"force":true}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(story_field(&json, "superstate"), "OPEN");
    assert!(story_field(&json, "deleted").is_null());
}

#[test]
fn web_reopen_malformed_json_is_400() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();
    story(dir.path())
        .args(["move", "SH-1", "done"])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/reopen"),
        "not json",
    )
    .unwrap_err();
    assert_eq!(status_of(err), 400);
}

/// Regression test for a defect where `route_reopen_story` constructed
/// `Invocation::Reopen` without the `force` field `story reopen --force`
/// added on the CLI side, which failed to compile at all. The fix passes
/// `force: false`, so this route must keep behaving like an un-forced CLI
/// `story reopen`: reopening a *soft-deleted* story is rejected with a clear
/// error rather than silently undeleting it (see `app.rs`'s `Invocation::
/// Reopen` handler) — and, since the server has no TTY to prompt at, this
/// must fail cleanly rather than hang waiting on stdin confirmation.
#[test]
fn web_reopen_soft_deleted_story_is_rejected_without_force() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let del = delete_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"),
        r#"{"reason":"duplicate"}"#,
    )
    .unwrap();
    assert_eq!(del.status(), 200);

    let err = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/reopen"),
        "",
    )
    .unwrap_err();
    let status = match err {
        ureq::Error::StatusCode(code) => code,
        other => panic!("expected status code error, got: {other}"),
    };
    assert_eq!(
        status, 422,
        "soft-deleted reopen must be rejected, not silently undeleted or hung"
    );
}

// --- Mutation API: PATCH multi-field ---

#[test]
fn web_patch_story_updates_multiple_fields() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = patch_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"),
        r#"{"title":"Retitled","priority":"high"}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(story_field(&json, "title"), "Retitled");
    assert_eq!(story_field(&json, "priority"), "high");
}

#[test]
fn web_patch_story_no_fields_is_400() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = patch_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"),
        r#"{}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 400);
}

#[test]
fn web_patch_story_sets_description() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = patch_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"),
        r#"{"description":"Added via drawer"}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(story_field(&json, "description"), "Added via drawer");
}

#[test]
fn web_patch_story_description_without_guard_header_is_403() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = ureq::patch(format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"
    ))
    .content_type("application/json")
    .send(r#"{"description":"Should not land"}"#)
    .unwrap_err();
    assert_eq!(status_of(err), 403);

    let resp = ureq::get(format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"
    ))
    .call()
    .unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(story_field(&json, "description"), serde_json::Value::Null);
}

// --- Mutation API: relate / unrelate ---

#[test]
fn web_relate_and_unrelate_stories() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "A"]).assert().success();
    story(dir.path()).args(["new", "B"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/relate"),
        r#"{"a":"SH-1","relation":"blocks","b":"SH-2"}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    let relationships = json["story"]["story"]["relationships"].as_array().unwrap();
    assert!(
        relationships
            .iter()
            .any(|r| r["relation"] == "blocks" && r["other_id"] == "SH-2")
    );

    let resp2 = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/unrelate"),
        r#"{"a":"SH-1","relation":"blocks","b":"SH-2"}"#,
    )
    .unwrap();
    assert_eq!(resp2.status(), 200);
    let json2: serde_json::Value =
        serde_json::from_str(&resp2.into_body().read_to_string().unwrap()).unwrap();
    let relationships2 = json2["story"]["story"]["relationships"].as_array().unwrap();
    assert!(
        !relationships2
            .iter()
            .any(|r| r["relation"] == "blocks" && r["other_id"] == "SH-2")
    );
}

// --- Mutation API: delete ---

#[test]
fn web_delete_story_soft_deletes_it() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = delete_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"),
        r#"{"reason":"duplicate"}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(json["result"], "ok");
    assert!(json["message"].as_str().unwrap().contains("deleted"));

    // `story delete` is a soft delete (see cli_grammar.rs::delete_soft_deletes):
    // the story is archived with a "[deleted] <reason>" comment rather than
    // erased, so it remains fetchable for audit purposes.
    let show = ureq::get(format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"
    ))
    .call()
    .unwrap();
    let show_json: serde_json::Value =
        serde_json::from_str(&show.into_body().read_to_string().unwrap()).unwrap();
    let comments = show_json["story"]["story"]["comments"].as_array().unwrap();
    assert!(comments.iter().any(|c| {
        c["text"]
            .as_str()
            .is_some_and(|t| t.contains("[deleted] duplicate"))
    }));
}

#[test]
fn web_delete_story_without_reason_is_400() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = delete_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"),
        r#"{}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 400);
}

// --- Mutation API: malformed body ---

#[test]
fn web_move_story_malformed_json_is_400() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move"),
        "not json",
    )
    .unwrap_err();
    assert_eq!(status_of(err), 400);
}

// --- Mutation guard: CSRF header + Host allowlist ---

#[test]
fn web_mutation_without_guard_header_is_403() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = post_json_unguarded(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move"),
        r#"{"state":"in-progress"}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 403);

    // The story must not have moved.
    let data = ureq::get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let data_json: serde_json::Value =
        serde_json::from_str(&data.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(data_json["stories"][0]["story"]["state"], "todo");
}

#[test]
fn web_mutation_with_spoofed_host_is_403() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = ureq::post(format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move"
    ))
    .header("X-Storyhook", "1")
    .header("Host", "evil.example")
    .content_type("application/json")
    .send(r#"{"state":"in-progress"}"#)
    .unwrap_err();
    assert_eq!(status_of(err), 403);
}

#[test]
fn web_mutation_wrong_content_type_is_415() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = ureq::post(format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move"
    ))
    .header("X-Storyhook", "1")
    .content_type("text/plain")
    .send(r#"{"state":"in-progress"}"#)
    .unwrap_err();
    assert_eq!(status_of(err), 415);
}

#[test]
fn web_put_on_story_path_is_405() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = ureq::put(format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"
    ))
    .header("X-Storyhook", "1")
    .content_type("application/json")
    .send(r#"{"title":"x"}"#)
    .unwrap_err();
    assert_eq!(status_of(err), 405);
}

// --- Mutation guard: security headers on writes ---

#[test]
fn web_mutation_success_has_security_headers() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/priority"),
        r#"{"priority":"low"}"#,
    )
    .unwrap();
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
fn web_mutation_guard_reject_has_security_headers() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = ureq::post(format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/priority"
    ))
    .content_type("application/json")
    .config()
    .http_status_as_error(false)
    .build()
    .send(r#"{"priority":"low"}"#)
    .unwrap();
    assert_eq!(resp.status(), 403);
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

// --- State configuration API: /api/repos/<id>/states ---

/// Boots a server over a fresh project and returns (dir guard, port, repo id).
/// Every state test needs the same three lines otherwise.
fn serve_project() -> (tempfile::TempDir, tempfile::TempDir, u16, String) {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    let (registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);
    (dir, registry_dir, port, repo_id)
}

fn get_states(port: u16, repo_id: &str) -> serde_json::Value {
    let resp = ureq::get(format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/states"
    ))
    .call()
    .unwrap();
    serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap()
}

fn json_body(resp: ureq::http::Response<ureq::Body>) -> serde_json::Value {
    serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap()
}

fn slugs(json: &serde_json::Value) -> Vec<String> {
    json["states"]
        .as_array()
        .unwrap()
        .iter()
        .map(|state| state["slug"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn web_states_list_reports_config_and_counts_in_board_order() {
    let (dir, _registry_dir, port, repo_id) = serve_project();
    story(dir.path())
        .args(["new", "Open one"])
        .assert()
        .success();
    story(dir.path())
        .args(["new", "Done one"])
        .assert()
        .success();
    story(dir.path())
        .args(["move", "SH-2", "done"])
        .assert()
        .success();

    let json = get_states(port, &repo_id);
    assert_eq!(slugs(&json), vec!["todo", "in-progress", "done"]);

    let todo = &json["states"][0];
    assert_eq!(todo["super_state"], "OPEN");
    assert_eq!(todo["open_count"], 1);
    assert_eq!(todo["archived_count"], 0);
    assert!(todo["role"].is_null());
    assert!(todo["description"].is_null());

    assert_eq!(json["states"][1]["role"], "active");
    assert_eq!(json["states"][2]["archived_count"], 1);
}

#[test]
fn web_states_create_adds_a_state_and_returns_the_new_list() {
    let (_dir, _registry_dir, port, repo_id) = serve_project();

    let resp = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states"),
        r#"{"slug":"review","super_state":"OPEN","description":"Waiting on a reviewer"}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 201);

    let json = json_body(resp);
    assert_eq!(slugs(&json), vec!["todo", "in-progress", "done", "review"]);
    assert_eq!(json["states"][3]["description"], "Waiting on a reviewer");
    assert_eq!(slugs(&get_states(port, &repo_id)), slugs(&json));
}

#[test]
fn web_states_create_rejects_an_invalid_slug() {
    let (_dir, _registry_dir, port, repo_id) = serve_project();

    let error = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states"),
        r#"{"slug":"In Review","super_state":"OPEN"}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(error), 422);
}

#[test]
fn web_states_create_requires_slug_and_superstate() {
    let (_dir, _registry_dir, port, repo_id) = serve_project();
    let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states");

    for body in [r#"{"super_state":"OPEN"}"#, r#"{"slug":"review"}"#] {
        let error = post_json(&url, body).unwrap_err();
        assert_eq!(status_of(error), 400, "body: {body}");
    }
}

#[test]
fn web_states_patch_sets_and_clears_optional_fields() {
    let (_dir, _registry_dir, port, repo_id) = serve_project();
    let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states/todo");

    let json = json_body(patch_json(&url, r#"{"description":"Not started yet"}"#).unwrap());
    assert_eq!(json["states"][0]["description"], "Not started yet");

    // null clears, absent leaves alone — the whole reason the field is
    // three-valued.
    let json = json_body(patch_json(&url, r#"{"role":null,"description":null}"#).unwrap());
    assert!(json["states"][0]["description"].is_null());
}

#[test]
fn web_states_patch_leaves_unmentioned_fields_alone() {
    let (_dir, _registry_dir, port, repo_id) = serve_project();
    let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states/in-progress");

    patch_json(&url, r#"{"description":"Being worked on"}"#).unwrap();
    let json = json_body(patch_json(&url, r#"{"super_state":"OPEN"}"#).unwrap());
    let in_progress = &json["states"][1];
    assert_eq!(in_progress["description"], "Being worked on");
    assert_eq!(in_progress["role"], "active");
}

#[test]
fn web_states_patch_requires_a_destination_for_occupied_states() {
    let (dir, _registry_dir, port, repo_id) = serve_project();
    story(dir.path())
        .args(["new", "A story"])
        .assert()
        .success();
    let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states/todo");

    let error = patch_json(&url, r#"{"super_state":"CLOSED"}"#).unwrap_err();
    assert_eq!(status_of(error), 422);

    // And with a destination it goes through, moving the story.
    let json = json_body(
        patch_json(
            &url,
            r#"{"super_state":"CLOSED","move_stories_to":"in-progress"}"#,
        )
        .unwrap(),
    );
    assert_eq!(json["states"][0]["super_state"], "CLOSED");
    assert_eq!(json["states"][0]["open_count"], 0);
    assert_eq!(json["states"][1]["open_count"], 1);
    assert!(
        json["message"]
            .as_str()
            .unwrap()
            .contains("moved 1 story to in-progress"),
        "got: {}",
        json["message"]
    );
}

#[test]
fn web_states_patch_reorders_the_collection() {
    let (_dir, _registry_dir, port, repo_id) = serve_project();

    let json = json_body(
        patch_json(
            &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states"),
            r#"{"order":["done","todo","in-progress"]}"#,
        )
        .unwrap(),
    );
    assert_eq!(slugs(&json), vec!["done", "todo", "in-progress"]);
    assert_eq!(slugs(&get_states(port, &repo_id)), slugs(&json));
}

#[test]
fn web_states_reorder_rejects_a_partial_order() {
    let (_dir, _registry_dir, port, repo_id) = serve_project();

    let error = patch_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states"),
        r#"{"order":["done","todo"]}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(error), 422);

    let error = patch_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states"),
        r#"{"order":[]}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(error), 400);
}

/// `reorder` is a legal state slug, so the collection PATCH must not be
/// reachable as a sub-path that would shadow a state with that name.
#[test]
fn web_states_a_state_named_reorder_is_still_addressable() {
    let (_dir, _registry_dir, port, repo_id) = serve_project();
    post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states"),
        r#"{"slug":"reorder","super_state":"OPEN"}"#,
    )
    .unwrap();

    let json = json_body(
        patch_json(
            &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states/reorder"),
            r#"{"description":"an unfortunate name"}"#,
        )
        .unwrap(),
    );
    assert_eq!(json["states"][3]["description"], "an unfortunate name");
}

#[test]
fn web_states_delete_removes_and_migrates() {
    let (dir, _registry_dir, port, repo_id) = serve_project();
    story(dir.path())
        .args(["new", "A story"])
        .assert()
        .success();
    let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states/todo");

    // Occupied and no destination named: refused, nothing changed.
    let error = delete_json(&url, "{}").unwrap_err();
    assert_eq!(status_of(error), 422);
    assert_eq!(slugs(&get_states(port, &repo_id)).len(), 3);

    let json = json_body(delete_json(&url, r#"{"move_stories_to":"in-progress"}"#).unwrap());
    assert_eq!(slugs(&json), vec!["in-progress", "done"]);
    assert_eq!(json["states"][0]["open_count"], 1);
}

#[test]
fn web_states_delete_unknown_state_is_404() {
    let (_dir, _registry_dir, port, repo_id) = serve_project();
    let error = delete_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states/nope"),
        "{}",
    )
    .unwrap_err();
    assert_eq!(status_of(error), 404);
}

/// Every state mutation is a write, so it must carry the same CSRF header
/// the story routes require.
#[test]
fn web_states_mutations_require_the_guard_header() {
    let (_dir, _registry_dir, port, repo_id) = serve_project();

    let error = post_json_unguarded(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states"),
        r#"{"slug":"review","super_state":"OPEN"}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(error), 403);

    let error = ureq::patch(format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/states/todo"
    ))
    .content_type("application/json")
    .send(r#"{"description":"x"}"#)
    .unwrap_err();
    assert_eq!(status_of(error), 403);

    let error = ureq::delete(format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/states/in-progress"
    ))
    .force_send_body()
    .content_type("application/json")
    .send("{}")
    .unwrap_err();
    assert_eq!(status_of(error), 403);
}

#[test]
fn web_states_rejects_disallowed_methods() {
    let (_dir, _registry_dir, port, repo_id) = serve_project();

    let error = ureq::put(format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/states"
    ))
    .header("X-Storyhook", "1")
    .content_type("application/json")
    .send("{}")
    .unwrap_err();
    assert_eq!(status_of(error), 405);
}

/// The dashboard's board reads its columns from `/data`'s `meta.states`, so
/// the two views of the state set must agree — including the fields the
/// editor writes.
#[test]
fn web_data_meta_states_carry_role_and_description() {
    let (_dir, _registry_dir, port, repo_id) = serve_project();
    patch_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states/in-progress"),
        r#"{"description":"Being worked on"}"#,
    )
    .unwrap();

    let resp = ureq::get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let json = json_body(resp);
    let states = json["meta"]["states"].as_array().unwrap();
    assert_eq!(states[1]["slug"], "in-progress");
    assert_eq!(states[1]["role"], "active");
    assert_eq!(states[1]["description"], "Being worked on");
    assert!(states[0]["role"].is_null());
}

// --- Registry API: GET/POST /api/repos, DELETE /api/repos/<id> ---

#[test]
fn web_serve_repos_list_reports_available_repo_with_summary() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = ureq::get(format!("http://127.0.0.1:{port}/api/repos"))
        .call()
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let repos = json.as_array().unwrap();
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["id"], repo_id);
    assert_eq!(repos[0]["available"], true);
    assert_eq!(repos[0]["summary"]["total_open"], 1);
}

/// A repo that was registered but whose `.storyhook/` later vanished (moved,
/// deleted) must be reported as `available: false` with an `error`, never
/// take down the whole `/api/repos` response for every other repo.
#[test]
fn web_serve_repos_list_reports_unavailable_repo_without_failing() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    std::fs::remove_dir_all(dir.path().join(".storyhook")).unwrap();

    let port = serve(&registry_path);

    let resp = ureq::get(format!("http://127.0.0.1:{port}/api/repos"))
        .call()
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "one broken repo must not fail the whole list"
    );
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let repos = json.as_array().unwrap();
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["id"], repo_id);
    assert_eq!(repos[0]["available"], false);
    assert!(repos[0]["error"].is_string());
}

#[test]
fn web_serve_unknown_repo_id_is_404() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, _repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = ureq::get(format!(
        "http://127.0.0.1:{port}/api/repos/nonexistent-id/data"
    ))
    .call()
    .unwrap_err();
    assert_eq!(status_of(err), 404);
}

#[test]
fn web_register_repo_via_api() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let registry_dir = scratch_dir();
    let registry_path = registry_dir.path().join("registry.toml");
    let port = serve(&registry_path);

    let body = serde_json::json!({"path": dir.path().to_string_lossy()}).to_string();
    let resp = post_json(&format!("http://127.0.0.1:{port}/api/repos"), &body).unwrap();
    assert_eq!(resp.status(), 201);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert!(json["id"].is_string());

    let list = ureq::get(format!("http://127.0.0.1:{port}/api/repos"))
        .call()
        .unwrap();
    let list_json: serde_json::Value =
        serde_json::from_str(&list.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(list_json.as_array().unwrap().len(), 1);
}

#[test]
fn web_register_repo_requires_guard_header() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let registry_dir = scratch_dir();
    let registry_path = registry_dir.path().join("registry.toml");
    let port = serve(&registry_path);

    let body = serde_json::json!({"path": dir.path().to_string_lossy()}).to_string();
    let err =
        post_json_unguarded(&format!("http://127.0.0.1:{port}/api/repos"), &body).unwrap_err();
    assert_eq!(status_of(err), 403);
}

#[test]
fn web_deregister_repo_via_api() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = delete_json(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}"), "").unwrap();
    assert_eq!(resp.status(), 200);

    let list = ureq::get(format!("http://127.0.0.1:{port}/api/repos"))
        .call()
        .unwrap();
    let list_json: serde_json::Value =
        serde_json::from_str(&list.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(list_json.as_array().unwrap().len(), 0);

    // Deregistering must never touch the repo's own files.
    assert!(dir.path().join(".storyhook/project.toml").exists());
}

#[test]
fn web_deregister_repo_requires_guard_header() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let err = ureq::delete(format!("http://127.0.0.1:{port}/api/repos/{repo_id}"))
        .call()
        .unwrap_err();
    assert_eq!(status_of(err), 403);
}

// --- CLI: story web register|deregister|list ---
//
// All isolate $HOME to a temp dir so they never touch the developer's real
// ~/.storyhook/registry.toml.

#[test]
fn web_register_dot_registers_cwd() {
    let home = scratch_dir();
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    let canonical = dir.path().canonicalize().unwrap();

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "register", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("Registered"));

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            canonical.to_string_lossy().to_string(),
        ));
}

#[test]
fn web_register_explicit_path() {
    let home = scratch_dir();
    let project = scratch_dir();
    story(project.path()).arg("init").assert().success();
    let canonical = project.path().canonicalize().unwrap();
    let elsewhere = scratch_dir();

    story(elsewhere.path())
        .env("HOME", home.path())
        .args(["web", "register", &project.path().to_string_lossy()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Registered"));

    story(elsewhere.path())
        .env("HOME", home.path())
        .args(["web", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            canonical.to_string_lossy().to_string(),
        ));
}

#[test]
fn web_register_with_name() {
    let home = scratch_dir();
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "register", ".", "--name", "My Project"])
        .assert()
        .success();

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("My Project"));
}

#[test]
fn web_register_non_project_fails() {
    let home = scratch_dir();
    let not_a_project = scratch_dir();

    story(not_a_project.path())
        .env("HOME", home.path())
        .args(["web", "register", "."])
        .assert()
        .failure()
        .stdout(predicate::str::contains("not initialized"));
}

#[test]
fn web_deregister_by_id_cli() {
    let home = scratch_dir();
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    // Register directly through the registry API for a deterministic id,
    // at the same path `story web register` would use once HOME points at
    // `home` — sidesteps parsing `web list`'s human-readable text.
    let registry_path = home.path().join(".storyhook").join("registry.toml");
    let repo = storyhook::registry::with_lock_at(&registry_path, |r| r.register(dir.path(), None))
        .unwrap();

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "deregister", &repo.id])
        .assert()
        .success()
        .stdout(predicate::str::contains("Deregistered"));

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No repos registered"));
}

#[test]
fn web_deregister_unknown_target_fails() {
    let home = scratch_dir();
    let dir = scratch_dir();

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "deregister", "nope"])
        .assert()
        .failure();
}

#[test]
fn web_list_shows_registered_repos() {
    let home = scratch_dir();
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let registry_path = home.path().join(".storyhook").join("registry.toml");
    let repo = storyhook::registry::with_lock_at(&registry_path, |r| r.register(dir.path(), None))
        .unwrap();

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains(repo.id))
        .stdout(predicate::str::contains(repo.name));
}

#[test]
fn web_list_empty_registry_message() {
    let home = scratch_dir();
    let dir = scratch_dir();

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("No repos registered"));
}

// --- CLI DEFAULT_WEB_PORT constant ---

#[test]
fn default_web_port_constant_is_3456() {
    assert_eq!(storyhook::cli::DEFAULT_WEB_PORT, 3456);
}

// --- build_report_data with blocked stories ---

#[test]
fn build_report_data_with_blocked_story() {
    let dir = scratch_dir();
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
    let dir = scratch_dir();
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
    let dir = scratch_dir();
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
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Concurrent test"])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    // Fire 10 concurrent requests
    let handles: Vec<_> = (0..10)
        .map(|_| {
            let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data");
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
    let dir = scratch_dir();
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
fn web_parse_register_defaults_path_to_dot() {
    let inv =
        storyhook::cli::parse_invocation(&["web".to_string(), "register".to_string()]).unwrap();
    match inv {
        storyhook::cli::Invocation::Web {
            action: storyhook::cli::WebAction::Register { path, name },
        } => {
            assert_eq!(path, std::path::PathBuf::from("."));
            assert_eq!(name, None);
        }
        other => panic!("expected Web::Register, got {:?}", other),
    }
}

#[test]
fn web_parse_register_with_explicit_path_and_name() {
    let inv = storyhook::cli::parse_invocation(&[
        "web".to_string(),
        "register".to_string(),
        "/some/path".to_string(),
        "--name".to_string(),
        "My Repo".to_string(),
    ])
    .unwrap();
    match inv {
        storyhook::cli::Invocation::Web {
            action: storyhook::cli::WebAction::Register { path, name },
        } => {
            assert_eq!(path, std::path::PathBuf::from("/some/path"));
            assert_eq!(name, Some("My Repo".to_string()));
        }
        other => panic!("expected Web::Register, got {:?}", other),
    }
}

#[test]
fn web_parse_deregister_requires_target() {
    let result = storyhook::cli::parse_invocation(&["web".to_string(), "deregister".to_string()]);
    assert!(result.is_err());
}

#[test]
fn web_parse_list() {
    let inv = storyhook::cli::parse_invocation(&["web".to_string(), "list".to_string()]).unwrap();
    assert!(matches!(
        inv,
        storyhook::cli::Invocation::Web {
            action: storyhook::cli::WebAction::List
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
    ])
    .unwrap();
    match inv {
        storyhook::cli::Invocation::Web {
            action: storyhook::cli::WebAction::Serve { port },
        } => {
            assert_eq!(port, 4000);
        }
        other => panic!("expected Web::Serve, got {:?}", other),
    }
}

/// `--serve` no longer accepts `--root` (the server is registry-backed, not
/// bound to a single repo) — a stray `--root` must be rejected like any
/// other unknown flag, not silently accepted or ignored.
#[test]
fn web_parse_serve_unknown_flag_errors() {
    let result = storyhook::cli::parse_invocation(&[
        "web".to_string(),
        "--serve".to_string(),
        "--port".to_string(),
        "4000".to_string(),
        "--root".to_string(),
        "/tmp/test".to_string(),
    ]);
    assert!(result.is_err());
}

#[test]
fn web_start_extra_unknown_flag_errors() {
    let dir = scratch_dir();
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
    let dir = scratch_dir();
    story(dir.path())
        .args(["web", "start", "--port", "65536"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("invalid port"));
}

#[test]
fn web_start_port_negative_is_invalid() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["web", "start", "--port", "-1"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("invalid port"));
}

// --- is_ready and is_blocked correctness ---

#[test]
fn web_serve_api_data_ready_and_blocked_flags_correct() {
    let dir = scratch_dir();
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

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let resp = ureq::get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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
    let dir = scratch_dir();
    let result = storyhook::app::build_report_data(dir.path());
    assert!(
        result.is_err(),
        "build_report_data on non-project dir should error"
    );
}

// --- Tailnet dual-bind (skips gracefully where `tailscale` isn't available) ---

/// Mirrors `web::tailscale_ip()`'s own detection so the test can decide
/// whether to skip without depending on a private function.
fn test_env_tailscale_ip() -> Option<String> {
    let output = std::process::Command::new("tailscale")
        .args(["ip", "-4"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ip.is_empty() { None } else { Some(ip) }
}

#[test]
fn web_serve_binds_tailnet_ip_when_available() {
    let Some(tailnet_ip) = test_env_tailscale_ip() else {
        eprintln!("skipping: no tailscale IP available in this environment");
        return;
    };

    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Reachable via tailnet"])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);
    // The tailnet listener binds promptly (same as loopback) but doesn't
    // start *serving* until the watcher's own setup finishes — poll for a
    // clear failure message if it never comes up at all; the `ureq::get`
    // call below blocks for the real response regardless.
    wait_for_addr(&format!("{tailnet_ip}:{port}"));

    // Loopback still works.
    let loopback = ureq::get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    assert_eq!(loopback.status(), 200);

    // The tailnet interface is also bound and serves the same data.
    let tailnet_url = format!("http://{tailnet_ip}:{port}/api/repos/{repo_id}/data");
    let resp = ureq::get(&tailnet_url).call().unwrap_or_else(|e| {
        panic!("expected the dashboard to be reachable via its own tailnet IP {tailnet_url}: {e}")
    });
    assert_eq!(resp.status(), 200);
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["summary"]["total_open"], 1);
}

#[test]
fn web_serve_tailnet_ip_is_auto_trusted_for_mutations() {
    let Some(tailnet_ip) = test_env_tailscale_ip() else {
        eprintln!("skipping: no tailscale IP available in this environment");
        return;
    };

    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Move me from the tailnet"])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);
    // The tailnet listener binds promptly (same as loopback), but doesn't
    // start *serving* until the watcher's own setup finishes — poll for a
    // clear failure message if it never comes up at all; the `ureq::post`
    // call below blocks for the real response regardless, so nothing here
    // needs to guess how long that setup actually takes.
    wait_for_addr(&format!("{tailnet_ip}:{port}"));

    // No STORYHOOK_WEB_TRUSTED_HOSTS is set — the server itself decided to
    // bind this interface, so mutations through it must be trusted by
    // default, the same way loopback is.
    let url = format!("http://{tailnet_ip}:{port}/api/repos/{repo_id}/story/SH-1/move");
    let resp = ureq::post(&url)
        .header("X-Storyhook", "1")
        .content_type("application/json")
        .send(r#"{"state":"in-progress"}"#)
        .unwrap_or_else(|e| panic!("expected the tailnet interface to be auto-trusted: {e}"));
    assert_eq!(resp.status(), 200);
}

/// This machine's MagicDNS FQDN (trailing dot stripped, lowercased), if the
/// `tailscale` CLI is present and reports one — independently derived from
/// `tailscale status --json` (mirroring `web::parse_tailnet_identity`'s own
/// extraction) so these tests don't depend on private lib internals. `None`
/// (and the caller skips) wherever `tailscale` isn't installed, isn't logged
/// in, or MagicDNS is disabled on this tailnet.
fn test_env_tailnet_fqdn() -> Option<String> {
    let output = std::process::Command::new("tailscale")
        .args(["status", "--json"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let dns_name = json.get("Self")?.get("DNSName")?.as_str()?;
    let fqdn = dns_name.trim_end_matches('.').to_ascii_lowercase();
    if fqdn.is_empty() { None } else { Some(fqdn) }
}

#[test]
fn web_serve_trusts_magic_dns_fqdn_for_mutations() {
    // Regression test for #35: a tailnet peer reaching the dashboard by its
    // MagicDNS name (rather than the raw tailnet IP) got 403 Forbidden on
    // every mutation, because only the IP literal ever landed in
    // `trusted_hosts`. This connects over loopback but sends the `Host` a
    // tailnet browser would actually send when it opens the MagicDNS URL —
    // the same technique `web_mutation_with_spoofed_host_is_403` uses in the
    // opposite direction — so it needs no real cross-machine network access,
    // only that the tailnet listener actually bound (which is what
    // populates `trusted_hosts` with the FQDN in the first place; hence the
    // skip-if-unavailable gate and the `wait_for_addr` below).
    let Some(fqdn) = test_env_tailnet_fqdn() else {
        eprintln!("skipping: no tailscale MagicDNS name available in this environment");
        return;
    };
    let Some(tailnet_ip) = test_env_tailscale_ip() else {
        eprintln!("skipping: no tailscale IP available in this environment");
        return;
    };

    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path())
        .args(["new", "Move me via MagicDNS"])
        .assert()
        .success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);
    wait_for_addr(&format!("{tailnet_ip}:{port}"));

    let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move");
    let resp = ureq::post(&url)
        .header("X-Storyhook", "1")
        .header("Host", format!("{fqdn}:{port}"))
        .content_type("application/json")
        .send(r#"{"state":"in-progress"}"#)
        .unwrap_or_else(|e| {
            panic!("expected the MagicDNS FQDN {fqdn} to be trusted for mutations: {e}")
        });
    assert_eq!(resp.status(), 200);
}

#[test]
fn web_serve_rejects_bare_magic_dns_short_label_for_mutations() {
    // The FQDN is trusted (see the test above), but its bare first label
    // must not be: unlike the FQDN, a single-label host can resolve through
    // a DNS search domain that isn't the tailnet's, so trusting it would
    // reopen a DNS-rebinding path. Locks in that deliberate boundary.
    let Some(fqdn) = test_env_tailnet_fqdn() else {
        eprintln!("skipping: no tailscale MagicDNS name available in this environment");
        return;
    };
    let Some(tailnet_ip) = test_env_tailscale_ip() else {
        eprintln!("skipping: no tailscale IP available in this environment");
        return;
    };
    let short_label = fqdn.split('.').next().unwrap();

    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);
    wait_for_addr(&format!("{tailnet_ip}:{port}"));

    let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move");
    let err = ureq::post(&url)
        .header("X-Storyhook", "1")
        .header("Host", format!("{short_label}:{port}"))
        .content_type("application/json")
        .send(r#"{"state":"in-progress"}"#)
        .unwrap_err();
    assert_eq!(status_of(err), 403);
}

#[test]
fn web_serve_rejects_foreign_ts_net_host_for_mutations() {
    // Proves the allowlist trusts THIS machine's FQDN specifically, not any
    // `ts.net` name under the same tailnet — another host's MagicDNS name
    // must still be rejected.
    let Some(fqdn) = test_env_tailnet_fqdn() else {
        eprintln!("skipping: no tailscale MagicDNS name available in this environment");
        return;
    };
    let Some(tailnet_ip) = test_env_tailscale_ip() else {
        eprintln!("skipping: no tailscale IP available in this environment");
        return;
    };
    let suffix = fqdn.split_once('.').map_or(fqdn.as_str(), |(_, rest)| rest);
    let foreign_host = format!("definitely-not-this-host.{suffix}");

    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    story(dir.path()).args(["new", "Story"]).assert().success();

    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);
    wait_for_addr(&format!("{tailnet_ip}:{port}"));

    let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move");
    let err = ureq::post(&url)
        .header("X-Storyhook", "1")
        .header("Host", format!("{foreign_host}:{port}"))
        .content_type("application/json")
        .send(r#"{"state":"in-progress"}"#)
        .unwrap_err();
    assert_eq!(status_of(err), 403);
}

#[test]
fn web_serve_never_binds_wildcard_address() {
    // Regression guard: inspect the OS's actual LISTEN sockets for this port
    // (via `lsof`) and assert every one of them is 127.0.0.1 or a real,
    // specific tailnet IP — never 0.0.0.0, ::, or `*` (a wildcard bind would
    // make the dashboard reachable from any interface, including a public
    // one). Skips gracefully if `lsof` isn't available.
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();

    let (_registry_dir, registry_path, _repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);
    std::thread::sleep(Duration::from_millis(200));

    let Ok(output) = std::process::Command::new("lsof")
        .args(["-iTCP", "-sTCP:LISTEN", "-P", "-n"])
        .output()
    else {
        eprintln!("skipping: lsof not available in this environment");
        return;
    };
    if !output.status.success() {
        eprintln!("skipping: lsof failed in this environment");
        return;
    }

    let listing = String::from_utf8_lossy(&output.stdout);
    let bound_addrs: Vec<&str> = listing
        .lines()
        .filter(|line| line.contains(&format!(":{port} ")))
        .collect();

    assert!(
        !bound_addrs.is_empty(),
        "expected at least one LISTEN socket on port {port}, got none from lsof:\n{listing}"
    );
    for line in &bound_addrs {
        assert!(
            !line.contains(&format!("*:{port}"))
                && !line.contains(&format!("0.0.0.0:{port}"))
                && !line.contains(&format!(":::{port}")),
            "found a wildcard bind on port {port}, this must never happen: {line}"
        );
    }
}

// --- Server-Sent Events (`GET /api/events`, live/near-live updates) ---
//
// These use a raw `TcpStream` (via `connect_sse`/`read_sse_until` in
// `// --- Utilities ---` below) rather than `ureq`, since the response
// never completes — it's a long-lived `text/event-stream` connection — and
// `ureq` isn't built for reading an indefinitely-open body. The tests don't
// bother decoding `Transfer-Encoding: chunked` framing: they just search
// the raw accumulated bytes for the expected SSE substrings, which is safe
// because the small hex chunk-size prefixes `write_sse_frame` inserts never
// collide with those substrings.
//
// Every test in this group starts its own `notify`-backed filesystem
// watcher (via `start_server`/`spawn_change_watcher`). On the FSEvents
// backend (macOS), `notify::Watcher::watch`/`unwatch` tear down and rebuild
// the *entire* underlying event stream on every call (confirmed in `notify`
// 7.0's `watch_inner`: it unconditionally calls `self.stop()` before
// reconfiguring) — several of these restarting at once under `cargo test`'s
// default thread parallelism was observed to intermittently delay or drop
// events past even a generous wait budget, made worse by every `start_server`
// call in this file leaving its background threads running for the rest of
// the test binary's process (see `web_serve_and_query_root`'s comment).
// `sse_test_lock` below serializes just this group of tests against each
// other — not the rest of the suite, which doesn't touch the filesystem
// watcher at all — so they stay reliable under `make test`'s default
// parallel `cargo test` run without slowing everything else down.
static SSE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Acquires [`SSE_TEST_LOCK`] for the calling test's scope, recovering from
/// poisoning (a prior `sse_*` test panicking while holding it) rather than
/// cascading that failure into every test after it.
fn sse_test_lock() -> std::sync::MutexGuard<'static, ()> {
    SSE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Connecting and immediately mutating the registered repo delivers a
/// `repo-changed` event carrying that repo's id — the core "live" path.
#[test]
fn sse_delivers_repo_changed_on_story_mutation() {
    let _sse_guard = sse_test_lock();
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let mut sse = connect_sse(port);
    story(dir.path())
        .args(["new", "Live update smoke test"])
        .assert()
        .success();

    let received = read_sse_until(&mut sse, "event: repo-changed", Duration::from_secs(8));
    assert!(
        received.contains("event: repo-changed"),
        "expected a repo-changed event, got: {received}"
    );
    assert!(
        received.contains(&format!("\"repo_id\":\"{repo_id}\"")),
        "expected the changed repo's id `{repo_id}` in the event payload, got: {received}"
    );
}

/// Editing the project's *configuration* is a live update too: the board's
/// columns are the state set, so a second open dashboard must be told to
/// refetch rather than drawing stale columns until its slow safety poll
/// comes round. Configuration lives beside the SQLite archive and so isn't
/// watched (see `rescan_watched_repos`) — the server publishes these itself.
#[test]
fn sse_delivers_repo_changed_on_state_configuration_change() {
    let _sse_guard = sse_test_lock();
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let mut sse = connect_sse(port);
    post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states"),
        r#"{"slug":"review","super_state":"OPEN"}"#,
    )
    .unwrap();

    let received = read_sse_until(&mut sse, "event: repo-changed", Duration::from_secs(8));
    assert!(
        received.contains("event: repo-changed"),
        "expected a repo-changed event after a state was added, got: {received}"
    );
    assert!(
        received.contains(&format!("\"repo_id\":\"{repo_id}\"")),
        "expected the changed repo's id `{repo_id}` in the event payload, got: {received}"
    );
}

/// Several mutations fired back-to-back collapse into fewer published
/// events than mutations performed, proving the watcher's per-repo 200ms
/// debounce window is actually coalescing rather than publishing once per
/// underlying filesystem event.
#[test]
fn sse_coalesces_rapid_mutations_within_debounce_window() {
    let _sse_guard = sse_test_lock();
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    let (_registry_dir, registry_path, _repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    story(dir.path())
        .args(["new", "Debounce target"])
        .assert()
        .success();

    let mut sse = connect_sse(port);
    const MUTATIONS: usize = 6;
    // Mutate via `app::run` directly (in-process) rather than spawning
    // `story` subprocesses: subprocess spawn latency is too variable under
    // the CPU load this whole suite generates to reliably land all of them
    // inside the 200ms debounce window, which would make this assertion
    // flaky for reasons that have nothing to do with the debounce logic
    // under test. A tight in-process loop does.
    for _ in 0..MUTATIONS {
        storyhook::app::run(
            dir.path(),
            storyhook::cli::CliOptions {
                json: true,
                quiet: true,
                no_hooks: false,
                invocation: storyhook::cli::Invocation::Comment {
                    id: "SH-1".to_string(),
                    text: "rapid update".to_string(),
                },
            },
        )
        .expect("comment mutation must succeed");
    }

    // Give the debounce window (200ms) and one more publish cycle to settle,
    // then count how many `repo-changed` events actually arrived. Adaptive
    // rather than a fixed sleep, since this whole suite can run with other
    // test binaries hammering the CPU in parallel under `cargo test`'s
    // default concurrency.
    let received =
        read_sse_until_quiet(&mut sse, Duration::from_millis(500), Duration::from_secs(8));
    let occurrences = received.matches("event: repo-changed").count();
    assert!(
        occurrences >= 1,
        "expected at least one repo-changed event, got none: {received}"
    );
    assert!(
        occurrences < MUTATIONS,
        "expected debouncing to coalesce {MUTATIONS} rapid mutations into fewer than \
         {MUTATIONS} events, got {occurrences}: {received}"
    );
}

/// Dropping an SSE connection must not wedge the broadcaster or the server:
/// a second connection, opened after the first is gone, still receives
/// live events, and the registry/data endpoints keep responding normally.
#[test]
fn sse_disconnect_does_not_break_server_for_other_clients() {
    let _sse_guard = sse_test_lock();
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    {
        let _first = connect_sse(port); // subscribes, then drops at end of this block
    }
    // Give the dropped connection's writer thread a moment to notice the
    // closed socket and unsubscribe.
    std::thread::sleep(Duration::from_millis(200));

    // The server must still serve ordinary requests fine.
    let resp = ureq::get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    assert_eq!(resp.status(), 200);

    // And a fresh SSE subscriber must still receive live events.
    let mut second = connect_sse(port);
    story(dir.path())
        .args(["new", "After disconnect"])
        .assert()
        .success();
    let received = read_sse_until(&mut second, "event: repo-changed", Duration::from_secs(8));
    assert!(
        received.contains("event: repo-changed"),
        "expected the second connection to still receive live events, got: {received}"
    );
}

/// Registering a repo *after* the server (and an SSE client) has already
/// started is itself detected (`repos-changed`), and the newly-registered
/// repo is watched from then on (its own `repo-changed` on mutation) —
/// without restarting the dashboard.
#[test]
fn sse_detects_runtime_repo_registration_and_watches_it() {
    let _sse_guard = sse_test_lock();
    let dir_a = scratch_dir();
    story(dir_a.path()).arg("init").assert().success();
    let (_registry_dir, registry_path, _repo_a_id) = register_repo(dir_a.path());
    let port = serve(&registry_path);

    let mut sse = connect_sse(port);

    let dir_b = scratch_dir();
    story(dir_b.path()).arg("init").assert().success();
    let repo_b = storyhook::registry::with_lock_at(&registry_path, |r| {
        r.register(dir_b.path(), Some("repo-b"))
    })
    .expect("registering repo B at runtime must succeed");

    let after_register = read_sse_until(&mut sse, "event: repos-changed", Duration::from_secs(8));
    assert!(
        after_register.contains("event: repos-changed"),
        "expected repos-changed after a runtime registration, got: {after_register}"
    );

    // `ReposChanged` is only published by `run_change_watcher`'s control
    // loop *after* `rescan_watched_repos` returns (see its comment), so
    // seeing the event here is itself proof repo B's directory is already
    // being watched — no settle delay needed before mutating it.
    story(dir_b.path())
        .args(["new", "In the newly-registered repo"])
        .assert()
        .success();
    let after_mutation = read_sse_until(&mut sse, "event: repo-changed", Duration::from_secs(8));
    assert!(
        after_mutation.contains(&format!("\"repo_id\":\"{}\"", repo_b.id)),
        "expected a repo-changed event for the newly-registered repo `{}`, got: {after_mutation}",
        repo_b.id
    );
}

/// A registered repo whose directory vanishes from disk (moved, deleted)
/// *after* it was already being watched must not prevent the watcher from
/// continuing to serve every other registered repo. This deliberately
/// disrupts an already-established watch (the realistic "a user deleted a
/// registered repo's folder" scenario) rather than racing the watcher's own
/// startup scan with the deletion — the FSEvents backend rebuilds its whole
/// stream on every `watch`/`unwatch` call (see `run_change_watcher`'s
/// deadlock-avoidance comment), so which of two repos gets watched first
/// during that scan is unspecified (`HashMap` iteration order), and
/// deleting one concurrently with it would make this test's own setup
/// racy rather than proving anything about `rescan_watched_repos`.
#[test]
fn sse_one_unreachable_repo_does_not_break_stream_for_others() {
    let _sse_guard = sse_test_lock();
    let dir_broken = scratch_dir();
    story(dir_broken.path()).arg("init").assert().success();
    let (_registry_dir, registry_path, broken_id) = register_repo(dir_broken.path());

    let dir_healthy = scratch_dir();
    story(dir_healthy.path()).arg("init").assert().success();
    let healthy = storyhook::registry::with_lock_at(&registry_path, |r| {
        r.register(dir_healthy.path(), Some("healthy"))
    })
    .expect("registering the healthy repo must succeed");

    let port = serve(&registry_path);

    let mut sse = connect_sse(port);

    // Baseline: confirm the about-to-break repo is actually being watched
    // before disrupting anything.
    story(dir_broken.path())
        .args(["new", "before break"])
        .assert()
        .success();
    let baseline = read_sse_until(&mut sse, "event: repo-changed", Duration::from_secs(8));
    assert!(
        baseline.contains(&format!("\"repo_id\":\"{broken_id}\"")),
        "expected the soon-to-be-broken repo's watch to be live first, got: {baseline}"
    );

    // Now its directory vanishes out from under that already-established
    // watch.
    std::fs::remove_dir_all(dir_broken.path()).unwrap();

    story(dir_healthy.path())
        .args(["new", "Still alive"])
        .assert()
        .success();

    let received = read_sse_until(&mut sse, "event: repo-changed", Duration::from_secs(8));
    assert!(
        received.contains(&format!("\"repo_id\":\"{}\"", healthy.id)),
        "expected the healthy repo's change to still arrive despite the broken repo, got: {received}"
    );
}

/// A heartbeat `: ping` comment arrives even with no story changes at all,
/// so a client can tell "connected and idle" apart from "silently dead".
/// Runs the server as the real daemon subprocess (`web start`) rather than
/// in-thread, so `STORYHOOK_SSE_HEARTBEAT_MS` — process-wide env state —
/// is scoped to that child process instead of leaking into this test
/// binary's own environment, where it could affect other tests running
/// concurrently in the same `cargo test` process.
#[test]
fn sse_heartbeat_ping_arrives_without_any_story_changes() {
    let _sse_guard = sse_test_lock();
    let home = scratch_dir();
    let dir = scratch_dir();
    let port = reserve_port();
    let _daemon = DaemonGuard {
        home: home.path().to_path_buf(),
        cwd: dir.path().to_path_buf(),
    };

    story(dir.path())
        .env("HOME", home.path())
        .env("STORYHOOK_SSE_HEARTBEAT_MS", "300")
        .args(["web", "start", "--port", &port.to_string()])
        .assert()
        .success();
    wait_for_server(port);

    let mut sse = connect_sse(port);
    let received = read_sse_until(&mut sse, ": ping", Duration::from_secs(8));
    assert!(
        received.contains(": ping"),
        "expected a heartbeat ping, got: {received}"
    );

    story(dir.path())
        .env("HOME", home.path())
        .args(["web", "stop"])
        .assert()
        .success();
}

/// Holding an SSE connection open must not stall the accept loop: an
/// ordinary request made while the connection is live still returns
/// promptly, proving the `GET /api/events` handoff to its own thread (see
/// `accept_loop`) actually frees the loop rather than blocking it.
#[test]
fn sse_connection_does_not_block_other_requests() {
    let _sse_guard = sse_test_lock();
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    let (_registry_dir, registry_path, repo_id) = register_repo(dir.path());
    let port = serve(&registry_path);

    let _sse = connect_sse(port); // held open for the rest of the test

    let start = Instant::now();
    let resp = ureq::get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    assert_eq!(resp.status(), 200);
    // A blocked accept loop would hang for as long as the SSE connection
    // stays open (i.e. indefinitely, since this test never closes it) —
    // this threshold only needs to rule that out, not assert sub-second
    // latency, so it's generous enough to tolerate CPU contention from the
    // rest of this suite running in parallel.
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "a normal request took {:?} while an SSE connection was open — the accept loop \
         may be blocked on it",
        start.elapsed()
    );
}

// --- Utilities ---

use std::time::{Duration, Instant};

/// The root every fixture directory in this suite is created under.
///
/// Deliberately *not* `$TMPDIR`. On macOS `$TMPDIR` (`/var/folders/…/T/`) is
/// Spotlight-indexed, and this suite creates hundreds of tiny files per run
/// (temp git repos, storyhook project trees, registries); `mds_stores`
/// backlogs behind them and starves the fixture-heavy web tests until they
/// fail as unexplained 404s — a diagnosis that points nowhere near the
/// filesystem (SH-53). `/private/tmp` (the real path behind `/tmp`) is never
/// indexed. On every other platform the OS temp dir carries no such hazard.
fn scratch_root() -> std::path::PathBuf {
    let unindexed = std::path::Path::new("/private/tmp");
    if cfg!(target_os = "macos") && unindexed.is_dir() {
        unindexed.to_path_buf()
    } else {
        std::env::temp_dir()
    }
}

/// Best-effort: mark `$TMPDIR` itself Spotlight-exempt, for the sake of the
/// suites that still build fixtures there. The marker is untracked machine
/// state that an OS upgrade wipes (SH-53), so the harness creates it rather
/// than assuming a human did — a failure to create it is advisory, and
/// `scratch_dirs_live_outside_the_spotlight_indexed_tmpdir` is what makes it
/// loud.
fn ensure_tmpdir_is_spotlight_exempt() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if cfg!(target_os = "macos") {
            let marker = std::env::temp_dir().join(".metadata_never_index");
            if !marker.exists() {
                let _ = std::fs::File::create(&marker);
            }
        }
    });
}

/// Creates a fixture directory under [`scratch_root`]. Every temp directory
/// this suite creates goes through here — never `tempfile::tempdir()`, which
/// lands in the indexed `$TMPDIR`.
fn scratch_dir() -> tempfile::TempDir {
    ensure_tmpdir_is_spotlight_exempt();
    tempfile::Builder::new()
        .prefix("storyhook-web-test-")
        .tempdir_in(scratch_root())
        .expect("creating a scratch directory")
}

#[test]
fn scratch_dirs_live_outside_the_spotlight_indexed_tmpdir() {
    let dir = scratch_dir();
    let indexed = std::env::temp_dir();
    assert!(
        !dir.path().starts_with(&indexed),
        "fixtures must not be created under {} — macOS Spotlight indexes it, and this \
         suite's file churn backlogs mds_stores until the web tests fail as unexplained \
         404s (SH-53); got {}",
        indexed.display(),
        dir.path().display()
    );

    if cfg!(target_os = "macos") {
        let marker = indexed.join(".metadata_never_index");
        assert!(
            marker.exists(),
            "the harness must create {} so the suites that still use $TMPDIR are \
             Spotlight-exempt too (SH-53)",
            marker.display()
        );
    }
}

/// Picks a port for the handful of tests that must know one *before* the
/// thing that binds it exists — `story web start --port N` binds in a child
/// process, and reports success as soon as it has spawned that child, so a
/// port it loses is never reported to anyone.
///
/// Two properties, both learned from SH-51:
///
/// - **Outside the kernel's ephemeral range.** Every in-process server here
///   binds port 0, so anything drawn from the ephemeral range can be handed
///   to one of those in the window between this reservation being released
///   and the child binding it — after which the test talks to whichever won.
/// - **Not a fixed sequence.** The old counter started every run at 19000 and
///   marched upward, so any run collided with the survivors of the previous
///   one. The band here is entered at a random offset, and each candidate is
///   bind-tested before being handed out.
fn reserve_port() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};

    // Above the registered-port range, below the ephemeral range macOS and
    // Linux draw from (49152+ / 32768+ — the band avoids both).
    const BAND: std::ops::Range<u16> = 19000..29000;

    static NEXT: std::sync::LazyLock<AtomicU16> = std::sync::LazyLock::new(|| {
        let entropy = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
            ^ std::process::id();
        let span = (BAND.end - BAND.start) as u32;
        AtomicU16::new(BAND.start + (entropy % span) as u16)
    });

    for _ in 0..64 {
        let candidate = NEXT.fetch_add(1, Ordering::Relaxed);
        let candidate = if BAND.contains(&candidate) {
            candidate
        } else {
            NEXT.store(BAND.start, Ordering::Relaxed);
            BAND.start
        };
        // Binding and immediately releasing proves nothing else holds it —
        // including a daemon leaked by an earlier run, the exact hazard the
        // fixed counter walked straight into.
        if std::net::TcpListener::bind(("127.0.0.1", candidate)).is_ok() {
            return candidate;
        }
    }
    panic!("no free port in {BAND:?} — is an earlier run's daemon still holding them?");
}

/// Starts a dashboard server for `registry_path` on an OS-assigned port and
/// returns that port once the server is serving. The sanctioned way for this
/// suite to get a server: no test picks its own port, and no test waits on
/// anything weaker than the server's own readiness signal.
fn serve(registry_path: &std::path::Path) -> u16 {
    try_serve_on(registry_path, 0).unwrap_or_else(|e| panic!("starting a test server: {e}"))
}

/// [`serve`], but on a caller-chosen `port` and returning the server's own
/// start-up failure instead of panicking.
///
/// The failure must never be swallowed. A server that loses the bind leaves
/// whatever *else* holds that port answering the test's requests, and a
/// stranger's registry answers `404` to everything the test asks about — the
/// mass-failure mode of SH-51. Readiness comes from the server's `ready`
/// callback rather than a `connect()` probe for the same reason: only the
/// server can attest that the address is one it actually bound, and that it
/// has finished loading state (see `web::start_server_with_ready`).
fn try_serve_on(registry_path: &std::path::Path, port: u16) -> Result<u16, String> {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel::<Result<u16, String>>();
    let ready_tx = tx.clone();
    let path = registry_path.to_path_buf();
    std::thread::spawn(move || {
        let outcome = storyhook::web::start_server_with_ready(&path, port, move |addr| {
            let _ = ready_tx.send(Ok(addr.port()));
        });
        if let Err(e) = outcome {
            let _ = tx.send(Err(e.to_string()));
        }
    });

    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(bound)) => {
            wait_for_server(bound);
            Ok(bound)
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err(format!(
            "a server for {} neither became ready nor reported a failure within 10s",
            registry_path.display()
        )),
    }
}

/// Stops the `story web start` daemon a test launched, even if the test
/// panics first. Test-spawned servers that outlive the test are exactly what
/// poisoned later runs in SH-51; the in-process ones die with the test
/// binary, but a daemon is a detached child process and only an explicit
/// stop reaps it.
struct DaemonGuard {
    home: std::path::PathBuf,
    cwd: std::path::PathBuf,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = Command::cargo_bin("story")
            .expect("locating the story binary")
            .current_dir(&self.cwd)
            .env("HOME", &self.home)
            .args(["web", "stop"])
            .output();
    }
}

/// A foreign listener on the port the harness is about to use stands in for
/// the orphaned `web_test` server from an earlier run that caused SH-51:
/// having lost the bind, the harness must fail loudly rather than hand the
/// test a port answered by somebody else's registry (which surfaced as a wall
/// of inexplicable 404s).
#[test]
fn serving_an_occupied_port_fails_loudly_instead_of_trusting_the_squatter() {
    let dir = scratch_dir();
    story(dir.path()).arg("init").assert().success();
    let (_registry_dir, registry_path, _repo_id) = register_repo(dir.path());

    let squatter = std::net::TcpListener::bind("127.0.0.1:0").expect("binding the squatter");
    let squatted = squatter.local_addr().unwrap().port();

    let outcome = try_serve_on(&registry_path, squatted);
    let error = match outcome {
        Err(error) => error,
        Ok(port) => panic!(
            "the harness reported a ready server on port {port}, which is held by a foreign \
             listener — every request the test makes would go to that stranger (SH-51)"
        ),
    };
    assert!(
        error.to_lowercase().contains("in use"),
        "the failure must name the real cause (the port is taken), got: {error}"
    );
}

/// Two servers started in the same run must never be handed the same port,
/// and each must answer only for the registry it was started with — the
/// property that the fixed 19000-based counter could not guarantee across
/// concurrent runs.
#[test]
fn concurrent_servers_get_distinct_ports_and_serve_only_their_own_registry() {
    let dir_a = scratch_dir();
    story(dir_a.path()).arg("init").assert().success();
    let (_registry_dir_a, registry_a, id_a) = register_repo(dir_a.path());

    let dir_b = scratch_dir();
    story(dir_b.path()).arg("init").assert().success();
    let (_registry_dir_b, registry_b, id_b) = register_repo(dir_b.path());

    assert_ne!(id_a, id_b, "the two fixtures must be distinguishable");

    let port_a = serve(&registry_a);
    let port_b = serve(&registry_b);
    assert_ne!(port_a, port_b, "two servers must not share a port");

    let own = ureq::get(format!("http://127.0.0.1:{port_a}/api/repos/{id_a}/data"))
        .call()
        .expect("a server must serve the registry it was started with");
    assert_eq!(own.status(), 200);

    let cross = ureq::get(format!("http://127.0.0.1:{port_b}/api/repos/{id_a}/data")).call();
    assert_eq!(
        status_of(cross.expect_err("a server must not answer for another server's repo")),
        404
    );
}

fn wait_for_server(port: u16) {
    wait_for_addr(&format!("127.0.0.1:{port}"));
}

/// Like [`wait_for_server`], but against an arbitrary `host:port` — for the
/// tailnet listener, which `start_server` no longer starts *serving* until
/// its filesystem watcher's one-time setup finishes (see `web.rs`'s
/// `watcher_ready_rx` handshake), so a fixed sleep after `wait_for_server`
/// (loopback-only) isn't a reliable proxy for "the tailnet listener is
/// accepting requests too" under load.
fn wait_for_addr(addr: &str) {
    let start = Instant::now();
    loop {
        if std::net::TcpStream::connect(addr).is_ok() {
            return;
        }
        if start.elapsed() > Duration::from_secs(5) {
            panic!("{addr} did not start accepting connections within 5 seconds");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Opens a raw `GET /api/events` connection and reads past the response
/// head (status line + headers, through the blank line that terminates
/// them), leaving the returned `BufReader` positioned at the start of the
/// SSE body. A short per-read socket timeout (rather than one long one)
/// lets `read_sse_until`/`read_sse_until_quiet` poll their own wall-clock
/// deadline instead of blocking on a single slow `read`.
fn connect_sse(port: u16) -> std::io::BufReader<std::net::TcpStream> {
    use std::io::{BufRead, BufReader, Write};

    let mut stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))
        .expect("connecting to /api/events");
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    write!(
        stream,
        "GET /api/events HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: keep-alive\r\n\r\n"
    )
    .expect("writing the SSE request line");

    let mut reader = BufReader::new(stream);
    // Bounded, because a bound-but-not-serving listener accepts the
    // connection and then says nothing at all: without a deadline this loop
    // retried forever and took the whole suite down with it (it holds the SSE
    // lock while it waits), turning a diagnosable failure into a hang.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            Instant::now() < deadline,
            "127.0.0.1:{port} accepted the /api/events connection but never sent a response \
             head — the listener is bound but not serving"
        );
        let mut line = String::new();
        // A per-read timeout surfaces as an `Err`, not an `Ok(0)`, so retry
        // on timeout rather than treating it as a closed connection.
        match reader.read_line(&mut line) {
            Ok(0) => panic!("connection closed while reading the SSE response head"),
            Ok(_) if line == "\r\n" => break,
            Ok(_) => {}
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => panic!("reading the SSE response head: {e}"),
        }
    }
    reader
}

/// Reads from an SSE connection (opened by [`connect_sse`]) until `needle`
/// appears in the accumulated bytes, or `timeout` elapses — whichever
/// happens first — returning everything read so far (lossily decoded, since
/// the raw bytes include `Transfer-Encoding: chunked` framing this suite
/// doesn't bother stripping).
fn read_sse_until(
    reader: &mut std::io::BufReader<std::net::TcpStream>,
    needle: &str,
    timeout: Duration,
) -> String {
    use std::io::Read;

    let start = Instant::now();
    let mut acc = Vec::new();
    let mut buf = [0u8; 4096];
    while start.elapsed() < timeout {
        match reader.read(&mut buf) {
            Ok(0) => break, // connection closed
            Ok(n) => {
                acc.extend_from_slice(&buf[..n]);
                if String::from_utf8_lossy(&acc).contains(needle) {
                    break;
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => panic!("reading the SSE stream: {e}"),
        }
    }
    String::from_utf8_lossy(&acc).into_owned()
}

/// Reads from an SSE connection until `quiet_for` elapses with no new bytes
/// arriving, or `overall_timeout` elapses — whichever comes first. Used
/// where the assertion is about *how many* events arrived (e.g. debounce
/// coalescing) rather than whether one particular event did, so there's no
/// single needle to watch for. Adaptive rather than a fixed sleep: under
/// system load, scheduling the watcher/heartbeat/writer threads involved
/// can be slow, so this waits up to `overall_timeout` for the *first* byte,
/// then only `quiet_for` past whatever activity actually happens — fast in
/// the common case, tolerant in a contended one.
fn read_sse_until_quiet(
    reader: &mut std::io::BufReader<std::net::TcpStream>,
    quiet_for: Duration,
    overall_timeout: Duration,
) -> String {
    use std::io::Read;

    let start = Instant::now();
    let mut last_activity: Option<Instant> = None;
    let mut acc = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        if start.elapsed() > overall_timeout {
            break;
        }
        if last_activity.is_some_and(|t| t.elapsed() > quiet_for) {
            break;
        }
        match reader.read(&mut buf) {
            Ok(0) => break, // connection closed
            Ok(n) => {
                acc.extend_from_slice(&buf[..n]);
                last_activity = Some(Instant::now());
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => panic!("reading the SSE stream: {e}"),
        }
    }
    String::from_utf8_lossy(&acc).into_owned()
}
