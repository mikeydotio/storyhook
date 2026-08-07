//! The dashboard's HTTP surface, served from the store.
//!
//! # What moved, and what did not
//!
//! Every assertion in this file is the one it was before the daemon wave. What
//! changed is how a fixture is built: the dashboard used to be pointed at a
//! `registry.toml` naming `.storyhook/` directories, and it is pointed at a
//! store now. So `register_repo` is gone, `seed` writes through the service
//! layer instead of through `app::run`, and a test names its project by the
//! slug the store minted rather than by the id a registry file invented.
//!
//! # Why every fixture is isolated
//!
//! One store per test, not one per binary. `GET /api/repos` lists *every*
//! project the store knows, so a shared store would make "the catalog has one
//! entry" depend on which other tests had run — which is exactly the reason each
//! test used to get a registry file of its own.

use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;
use std::sync::Arc;
use storyhook::cli::parse_invocation;
use storyhook::daemon::serve::BoundAddress;
use storyhook::env::Environment;
use storyhook::invoke::{dispatch, dispatch_unscoped};
use storyhook::service::Ctx;
use storyhook::store::{ProjectId, ReadOps, SqliteStore, Store};
use storyhook_test_support::{
    ChildGuard, DaemonGuard, TestEnv, http_status_line, reserve_port, scratch_dir, serve,
    wait_for_addr, wait_for_server,
};

/// A `story` command in the shared environment, for the tests that only
/// exercise argument parsing and never reach a project or the daemon's state.
fn story(dir: &Path) -> Command {
    TestEnv::shared().story(dir)
}

/// A dashboard serving one project, and everything a test needs to talk to it.
///
/// Holds its environment: the temporary home lives exactly as long as the test,
/// and the server thread reading its store dies with the binary.
struct Served {
    env: TestEnv,
    store: Arc<SqliteStore>,
    project: ProjectId,
    dir: tempfile::TempDir,
    /// The loopback port the server reported for itself.
    port: u16,
    /// Every address the server reported binding, tailnet included.
    ///
    /// The only sanctioned way for a test to learn whether the best-effort
    /// tailnet listener exists — see the tailnet section near the end of this
    /// file for why probing `tailscale` here instead is the SH-110 defect.
    bound: BoundAddress,
    /// The project's slug — what a dashboard URL calls a repo id.
    repo_id: String,
}

impl Served {
    /// The checkout the project was initialized in.
    fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// The environment this fixture's store and daemon live in.
    fn environment(&self) -> Environment {
        self.env.environment()
    }

    /// Runs one storyhook command against the fixture's project, in process.
    ///
    /// The same seam the dashboard itself uses, so a fixture cannot drift from
    /// the thing it is setting up. Hooks are suppressed: a hook would shell out
    /// to `story`, and a fixture is not the place to exercise that.
    fn seed(&self, args: &[&str]) {
        let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        let invocation = parse_invocation(&owned)
            .unwrap_or_else(|e| panic!("`story {}` did not parse: {e}", args.join(" ")));
        let ctx =
            Ctx::new(&*self.store, self.project, self.dir(), self.environment()).no_hooks(true);
        dispatch(&ctx, invocation)
            .unwrap_or_else(|e| panic!("`story {}` failed in the fixture: {e}", args.join(" ")));
    }

    /// The project's report data, read straight from the store.
    ///
    /// Replaces `app::build_report_data`, which took a repository path because
    /// the data was in the repository. `QueryService::report_data` is the
    /// function that produces it now, and it is the one the dashboard's `/data`
    /// route calls, so these tests still describe the same value the route
    /// serves.
    fn report_data(&self) -> storyhook::output::ReportData {
        let now = self.environment().now();
        Store::read(&*self.store, |tx| {
            Ok(storyhook::service::QueryService::new(tx, self.project, &now).report_data())
        })
        .expect("reading the store")
        .expect("building the report data")
    }

    /// Adds a second project to the same store, returning its checkout and the
    /// slug a dashboard URL names it by.
    ///
    /// The multi-project tests need one store with two projects in it, which is
    /// what a real machine looks like — where they used to need one registry
    /// file naming two directories.
    fn add_project(&self) -> (tempfile::TempDir, String) {
        let dir = scratch_dir();
        dispatch_unscoped(
            &*self.store,
            &self.environment(),
            dir.path(),
            "2026-01-01T00:00:00Z",
            parse_invocation(&[
                "project".to_string(),
                "new".to_string(),
                "--prefix".to_string(),
                "SH".to_string(),
                "--no-agents-md".to_string(),
            ])
            .expect("`story project new` parses"),
        )
        .expect("initializing a second project");
        let project = storyhook_test_support::project_id_at(&self.store, dir.path())
            .expect("the second project is in the store");
        let slug = Store::read(&*self.store, |tx| {
            Ok(tx.project(project)?.expect("the project row").slug)
        })
        .expect("reading the second project");
        (dir, slug)
    }

    /// Forgets every checkout of this project, leaving the project itself
    /// untouched.
    ///
    /// The state `story doctor --fix` leaves behind, and the one an imported
    /// project starts in: real stories in the store, and nowhere on this
    /// machine to act on them. Written straight to the store because there is
    /// no longer a command that produces it on purpose — deregistration was
    /// retired with `story web deregister`.
    fn deregister(&self) {
        use storyhook::store::WriteOps;
        Store::write(&*self.store, |tx| tx.set_checkout_path(self.project, None))
            .expect("forgetting the fixture's checkout");
    }
}

/// An isolated environment with one initialized project, served on an
/// OS-assigned loopback port.
fn served() -> Served {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let store = Arc::new(env.open_store());
    let environment = env.environment();

    dispatch_unscoped(
        &*store,
        &environment,
        dir.path(),
        "2026-01-01T00:00:00Z",
        parse_invocation(&[
            "project".to_string(),
            "new".to_string(),
            "--prefix".to_string(),
            "SH".to_string(),
            "--no-agents-md".to_string(),
        ])
        .expect("`story project new` parses"),
    )
    .expect("initializing the fixture project");

    let project = storyhook_test_support::project_id_at(&store, dir.path())
        .expect("the fixture project is in the store");
    let repo_id = Store::read(&*store, |tx| {
        Ok(tx.project(project)?.expect("the project row").slug)
    })
    .expect("reading the fixture project");

    let bound = serve(Arc::clone(&store), &environment);
    Served {
        env,
        store,
        project,
        dir,
        port: bound.port(),
        bound,
        repo_id,
    }
}

// --- CLI parsing tests ---

#[test]
fn web_no_subcommand_shows_usage() {
    let dir = scratch_dir();
    story(dir.path())
        .arg("web")
        .assert()
        .failure()
        .stderr(predicate::str::contains("usage:"));
}

#[test]
fn web_invalid_subcommand_shows_usage() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["web", "restart"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("usage:"));
}

#[test]
fn web_start_invalid_port_non_numeric() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["web", "start", "--port", "abc"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid port"));
}

#[test]
fn web_start_invalid_port_zero() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["web", "start", "--port", "0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid port"));
}

#[test]
fn web_start_invalid_port_too_large() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["web", "start", "--port", "99999"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid port"));
}

#[test]
fn web_start_port_missing_value() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["web", "start", "--port"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--port requires a value"));
}

// --- Status tests ---
//
// `web status`/`web stop` are now registry-scoped (one global daemon, not
// one per repo), so they read `~/.storyhook/web.pid` — isolate `$HOME` to a
// temp dir so these never touch the developer's real dashboard state.

#[test]
fn web_status_not_running() {
    let dir = scratch_dir();
    let env = TestEnv::isolated();

    env.story(dir.path())
        .args(["web", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Web UI is not running"));
}

#[test]
fn web_stop_when_not_running() {
    let dir = scratch_dir();
    let env = TestEnv::isolated();

    env.story(dir.path())
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
    let env = TestEnv::isolated();
    env.story(dir.path())
        .args(["web", "open"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not running"))
        .stderr(predicate::str::contains("story web start"));
}

#[test]
fn web_address_not_running_fails_with_summary() {
    let dir = scratch_dir();
    let env = TestEnv::isolated();
    env.story(dir.path())
        .args(["web", "address"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not running"))
        .stderr(predicate::str::contains("story web start"));
}

#[test]
fn web_open_and_address_succeed_when_running() {
    let env = TestEnv::isolated();
    let dir = scratch_dir(); // deliberately NOT a storyhook project
    let port = reserve_port();
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .args(["web", "start", "--port", &port.to_string()])
        .assert()
        .success();
    wait_for_server(port);

    // `web open` targets loopback; browser launch is stubbed via $BROWSER=true.
    env.story(dir.path())
        .env("BROWSER", "true")
        .args(["web", "open"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(
            "http://127.0.0.1:{port}/"
        )));

    // `web address` copies to the clipboard, stubbed via $STORYHOOK_CLIPBOARD_CMD=cat.
    // Host is left unasserted because the CI/dev host may or may not run Tailscale.
    env.story(dir.path())
        .env("STORYHOOK_CLIPBOARD_CMD", "cat")
        .args(["web", "address"])
        .assert()
        .success()
        .stdout(predicate::str::contains(format!(":{port}/")))
        .stdout(predicate::str::contains("clipboard"));

    env.story(dir.path())
        .args(["web", "stop"])
        .assert()
        .success();
}

/// The CLI advertises the host the *daemon* bound, never one this process
/// probed for. Direction A of SH-110, mechanized.
///
/// **Never skips.** On a machine with no tailnet the expected host is simply
/// `127.0.0.1`, and the assertion still means something: all three commands
/// must agree with the portfile.
///
/// The `tailscale` shim is what makes it a regression test rather than a
/// restatement. The daemon starts with the real environment, so on a
/// tailnet-equipped machine it binds and publishes its MagicDNS name — and then
/// the three client commands run with a `tailscale` that *fails*, which is what
/// a probe overrunning its three-second deadline under load looks like from the
/// client's side. Before the fix those clients probed, got nothing, and printed
/// `127.0.0.1` for a daemon reachable at its FQDN. Now they read what it
/// published and the shim cannot affect them.
///
/// Reading the portfile while a daemon runs is deliberate and safe: unlike the
/// store, it is written once, not held open, and `TestEnv::daemon` exists for
/// exactly this.
#[test]
fn web_start_status_address_advertise_the_host_the_daemon_bound() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let port = reserve_port();
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .args(["web", "start", "--port", &port.to_string()])
        .assert()
        .success();
    wait_for_server(port);

    let info = env
        .daemon()
        .expect("the daemon published a portfile after binding");
    let expected = format!("http://{}:{}", info.advertised_host(), port);

    // A `tailscale` that fails, ahead of everything else: a client that still
    // probes gets nothing and falls back to loopback.
    let broken = tempfile::Builder::new()
        .prefix("storyhook-tailscale-broken-")
        .tempdir_in("/private/tmp")
        .expect("a scratch directory");
    let shim = broken.path().join("tailscale");
    std::fs::write(&shim, "#!/bin/sh\nexit 1\n").expect("writing the shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
            .expect("making the shim executable");
    }
    let mut entries = vec![broken.path().to_path_buf()];
    entries.extend(std::env::split_paths(&env.path_with_binary()));
    let path = std::env::join_paths(entries).expect("joining PATH");

    for args in [["web", "status"], ["web", "address"]] {
        let printed = env
            .story(dir.path())
            .env("PATH", &path)
            .env("STORYHOOK_CLIPBOARD_CMD", "cat")
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("running `story {}`: {e}", args.join(" ")));
        let stdout = String::from_utf8_lossy(&printed.stdout).into_owned();
        assert!(
            stdout.contains(&expected),
            "`story {}` must advertise {expected} — the address the daemon published — \
             not a host derived from its own probe (SH-110); got: {stdout}",
            args.join(" ")
        );
    }

    env.story(dir.path())
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
    let env = TestEnv::isolated();
    let dir = scratch_dir(); // deliberately NOT a storyhook project
    let port = reserve_port();
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .args(["web", "start", "--port", &port.to_string()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Web UI started"));

    wait_for_server(port);

    env.story(dir.path())
        .args(["web", "stop"])
        .assert()
        .success();
}

// --- Server integration tests ---

#[test]
fn web_serve_and_query_root() {
    let fixture = served();

    let port = fixture.port;

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
    let fixture = served();

    let port = fixture.port;

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
    // Dispatch's token modal (SH-50) -- the Dispatch button itself is built
    // by JS only for an open story in a project with a checkout, so it
    // never appears in this static markup; the modal it opens does.
    assert!(body.contains(r#"id="token-modal""#));
    assert!(body.contains(r#"id="token-input""#));
    assert!(body.contains(r#"id="token-submit""#));
    // Multi-repo screens (#20): the header's project selector (SH-42), home
    // dashboard, settings
    assert!(body.contains(r#"id="projsel-btn""#));
    assert!(body.contains(r#"id="projsel-menu""#));
    assert!(
        !body.contains(r#"id="repo-select""#),
        "the native <select> was replaced by the popover"
    );
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

    // SH-157: the type badge — its lookup function, its CSS class, the
    // accessible name an emoji-only span needs, and the drawer's mount
    // point for it.
    assert!(body.contains("function typeGlyph"));
    assert!(body.contains("function buildTypeBadge"));
    assert!(body.contains(".type-badge"));
    assert!(body.contains(r#"aria-label"#));
    assert!(body.contains(r#""Type: " + slug"#));
    assert!(body.contains(r#"id="drawer-type-badge""#));
    // The vocabulary fix: every picker labels "no type" the same way
    // `story list --type none` already does, not "–"/"Untyped"/"Default
    // type" for one idea in three places.
    assert!(body.contains("function typeLabel"));
    assert!(body.contains(r#"return "none";"#));
}

#[test]
fn web_serve_api_data_empty_project() {
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Build feature"]);
    fixture.seed(&["new", "Fix bug"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Build feature"]);
    fixture.seed(&["new", "Fix bug"]);
    fixture.seed(&["delete", "SH-2", "duplicate"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();

    let port = fixture.port;

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
    let fixture = served();

    let data = fixture.report_data();
    assert_eq!(data.summary.total_open, 0);
    assert_eq!(data.summary.total_closed, 0);
    assert!(data.stories.is_empty());
    assert!(data.ready_ids.is_empty());
    assert!(data.blocked_ids.is_empty());
}

#[test]
fn build_report_data_with_mixed_states() {
    let fixture = served();
    fixture.seed(&["new", "Open story"]);
    fixture.seed(&["new", "Closed story"]);
    fixture.seed(&["move", "SH-2", "done"]);

    let data = fixture.report_data();
    assert_eq!(data.summary.total_open, 1);
    assert_eq!(data.summary.total_closed, 1);
    assert_eq!(data.stories.len(), 2);
    assert!(data.ready_ids.contains(&"SH-1".to_string()));
}

#[test]
fn report_data_serializes_to_json() {
    let fixture = served();
    fixture.seed(&["new", "JSON test"]);

    let data = fixture.report_data();
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
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Fix <script>alert('xss')</script> & \"quotes\""]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Support emoji: and CJK: \u{4e16}\u{754c}"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();

    let port = fixture.port;

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
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story one"]);
    // Add a label so we can verify the labels field
    fixture.seed(&["set", "SH-1", "--labels", "backend"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    // Append a state whose slug sorts alphabetically first ("archived" < "done"
    // < "in-progress" < "todo"), so an alphabetical (e.g. BTreeMap-derived)
    // ordering bug would put it first instead of last.
    fixture.seed(&["state", "add", "archived", "--super", "CLOSED"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
        vec!["todo", "in-progress", "blocked", "done", "archived"],
        "states must be in configured order, not alphabetical"
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
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    // SH-157: `task` was a default type; it no longer is.
    assert!(!types.contains(&"task"));

    // SH-157: each default type carries the emoji the dashboard's badge
    // reads — this is `meta_json`'s own field, not `TypeDef`'s `Serialize`
    // impl, so a handler that forgets to plumb it through would not be
    // caught by the domain-level round-trip tests alone.
    let bug = json["meta"]["types"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["slug"] == "bug")
        .expect("the default bug type");
    assert_eq!(bug["emoji"], "🐞");

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
    let fixture = served();
    fixture.seed(&["new", "A", "--labels", "web,bug"]);
    fixture.seed(&["new", "B", "--labels", "web,cli"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["member", "add", "Alice"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Fetch me"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let err = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story"),
        r#"{}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 400);
}

#[test]
fn web_create_story_with_description_labels_priority() {
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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

/// SH-164: a REST client's JSON array is not guaranteed to have split a
/// comma-bearing value already — this is the same shape SH-145 was filed
/// through, one layer removed.
#[test]
fn web_create_story_splits_a_comma_bearing_label() {
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story"),
        r#"{"title":"SH-145 repro","labels":["web,sse"]}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 201);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(
        story_field(&json, "labels"),
        serde_json::json!(["sse", "web"])
    );
}

#[test]
fn web_create_story_invalid_priority_is_422() {
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Movable"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Will be archived"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let err = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move"),
        r#"{"state":"nonexistent"}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 422);
}

#[test]
fn web_move_unknown_story_is_404() {
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);
    fixture.seed(&["member", "add", "Alice"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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

/// SH-164: `add` is a raw JSON array here too, and `set_labels` has to
/// normalize it rather than trust it was pre-split.
#[test]
fn web_labels_add_splits_a_comma_bearing_value() {
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/labels"),
        r#"{"add":["web,sse"]}"#,
    )
    .unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(
        story_field(&json, "labels"),
        serde_json::json!(["sse", "web"])
    );
}

#[test]
fn web_labels_empty_body_is_400() {
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let err = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/labels"),
        r#"{}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 400);
}

#[test]
fn web_block_and_unblock_story() {
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);
    fixture.seed(&["move", "SH-1", "done"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);
    fixture.seed(&["delete", "SH-1", "created in error"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);
    fixture.seed(&["delete", "SH-1", "created in error"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);
    fixture.seed(&["move", "SH-1", "done"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let err = patch_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"),
        r#"{}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 400);
}

#[test]
fn web_patch_story_sets_description() {
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "A"]);
    fixture.seed(&["new", "B"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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

/// Boots a server over a fresh project. Every state test needs the same lines
/// otherwise.
fn serve_project() -> Served {
    served()
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
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    fixture.seed(&["new", "Open one"]);
    fixture.seed(&["new", "Done one"]);
    fixture.seed(&["move", "SH-2", "done"]);

    let json = get_states(port, repo_id);
    assert_eq!(slugs(&json), vec!["todo", "in-progress", "blocked", "done"]);

    let todo = &json["states"][0];
    assert_eq!(todo["super_state"], "OPEN");
    assert_eq!(todo["open_count"], 1);
    assert_eq!(todo["archived_count"], 0);
    assert!(todo["role"].is_null());
    assert!(todo["description"].is_null());

    assert_eq!(json["states"][1]["role"], "active");
    assert_eq!(json["states"][3]["archived_count"], 1);
}

#[test]
fn web_states_create_adds_a_state_and_returns_the_new_list() {
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states"),
        r#"{"slug":"review","super_state":"OPEN","description":"Waiting on a reviewer"}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 201);

    let json = json_body(resp);
    assert_eq!(
        slugs(&json),
        vec!["todo", "in-progress", "blocked", "done", "review"]
    );
    assert_eq!(json["states"][4]["description"], "Waiting on a reviewer");
    assert_eq!(slugs(&get_states(port, repo_id)), slugs(&json));
}

#[test]
fn web_states_create_rejects_an_invalid_slug() {
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let error = post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states"),
        r#"{"slug":"In Review","super_state":"OPEN"}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(error), 422);
}

#[test]
fn web_states_create_requires_slug_and_superstate() {
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states");

    for body in [r#"{"super_state":"OPEN"}"#, r#"{"slug":"review"}"#] {
        let error = post_json(&url, body).unwrap_err();
        assert_eq!(status_of(error), 400, "body: {body}");
    }
}

#[test]
fn web_states_patch_sets_and_clears_optional_fields() {
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
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
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states/in-progress");

    patch_json(&url, r#"{"description":"Being worked on"}"#).unwrap();
    let json = json_body(patch_json(&url, r#"{"super_state":"OPEN"}"#).unwrap());
    let in_progress = &json["states"][1];
    assert_eq!(in_progress["description"], "Being worked on");
    assert_eq!(in_progress["role"], "active");
}

#[test]
fn web_states_patch_requires_a_destination_for_occupied_states() {
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    // A state beyond the required floor: `todo` cannot be reclassified at all
    // now (SH-125), so the destination rule needs a state that can be.
    fixture.seed(&["state", "add", "in-review", "--super", "OPEN"]);
    fixture.seed(&["new", "A story"]);
    fixture.seed(&["move", "SH-1", "in-review"]);
    let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states/in-review");

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
    let reclassified = json["states"]
        .as_array()
        .unwrap()
        .iter()
        .find(|state| state["slug"] == "in-review")
        .expect("the state is still listed");
    assert_eq!(reclassified["super_state"], "CLOSED");
    assert_eq!(reclassified["open_count"], 0);
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
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let json = json_body(
        patch_json(
            &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states"),
            r#"{"order":["done","todo","blocked","in-progress"]}"#,
        )
        .unwrap(),
    );
    assert_eq!(slugs(&json), vec!["done", "todo", "blocked", "in-progress"]);
    assert_eq!(slugs(&get_states(port, repo_id)), slugs(&json));
}

#[test]
fn web_states_reorder_rejects_a_partial_order() {
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
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
    assert_eq!(json["states"][4]["description"], "an unfortunate name");
}

#[test]
fn web_states_delete_removes_and_migrates() {
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    fixture.seed(&["state", "add", "in-review", "--super", "OPEN"]);
    fixture.seed(&["new", "A story"]);
    fixture.seed(&["move", "SH-1", "in-review"]);
    let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states/in-review");

    // Occupied and no destination named: refused, nothing changed.
    let error = delete_json(&url, "{}").unwrap_err();
    assert_eq!(status_of(error), 422);
    assert_eq!(slugs(&get_states(port, repo_id)).len(), 5);

    let json = json_body(delete_json(&url, r#"{"move_stories_to":"in-progress"}"#).unwrap());
    assert_eq!(slugs(&json), vec!["todo", "in-progress", "blocked", "done"]);
    assert_eq!(json["states"][1]["open_count"], 1);
}

#[test]
fn web_states_delete_unknown_state_is_404() {
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
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
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
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
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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

/// SH-42's header selector shows `PREFIX · name` for the current project, so
/// `/api/repos` has to carry the prefix — it was already reading the record
/// that has it, just not putting it on the wire.
#[test]
fn web_serve_repos_list_reports_each_projects_prefix() {
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = ureq::get(format!("http://127.0.0.1:{port}/api/repos"))
        .call()
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let repos = json.as_array().unwrap();
    assert_eq!(repos.len(), 1);
    assert_eq!(repos[0]["id"], repo_id);
    assert_eq!(repos[0]["prefix"], "SH");
}

/// **A behaviour change, deliberate.** A registered path whose `.storyhook/`
/// had since been deleted was the common broken repo, and the list had to keep
/// serving every other one around it — hence `available: false` with an
/// `error`.
///
/// A project in the store has no directory to lose. Its data is in the database
/// the daemon already has open, so deleting the checkout leaves the project
/// perfectly readable, and `available: false` is unreachable through this
/// surface — the same way `story doctor`'s dangling-relation finding became
/// unreachable when the schema started refusing the shape.
///
/// The flag and the arm that would set it stay: the cost is one match arm, and
/// the day a project genuinely cannot be read is not the day to discover that
/// one bad row fails the whole list. This pins what a caller sees today.
#[test]
fn a_project_survives_the_deletion_of_its_checkout() {
    let fixture = served();
    fixture.seed(&["new", "Still here"]);
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    std::fs::remove_dir_all(fixture.dir()).unwrap();

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
    assert_eq!(
        repos[0]["available"], true,
        "the stories are in the store, not in the directory that just vanished"
    );
    assert_eq!(repos[0]["summary"]["total_open"], 1);
}

#[test]
fn web_serve_unknown_repo_id_is_404() {
    let fixture = served();

    let port = fixture.port;

    let err = ureq::get(format!(
        "http://127.0.0.1:{port}/api/repos/nonexistent-id/data"
    ))
    .call()
    .unwrap_err();
    assert_eq!(status_of(err), 404);
}

#[test]
fn web_init_creates_a_project_at_a_path() {
    let fixture = served();
    let port = fixture.port;
    let fresh = fixture.dir().join("another");
    std::fs::create_dir_all(&fresh).unwrap();

    let body = serde_json::json!({
        "path": fresh.to_string_lossy(),
        "name": "Another",
        "prefix": "AN",
    })
    .to_string();
    let resp = post_json(&format!("http://127.0.0.1:{port}/api/repos"), &body).unwrap();
    assert_eq!(resp.status(), 201);

    // The same operation the CLI performs: a pointer file in the repository,
    // and a row the dashboard can see.
    assert!(fresh.join(".storyhook.toml").exists());
    let list = ureq::get(format!("http://127.0.0.1:{port}/api/repos"))
        .call()
        .unwrap();
    let repos: serde_json::Value =
        serde_json::from_str(&list.into_body().read_to_string().unwrap()).unwrap();
    let names: Vec<&str> = repos
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert!(names.contains(&"Another"), "{names:?}");
}

/// The browser is the one caller that can never be asked, so `prefix` is
/// required of it and a missing one is a 400 naming the field.
///
/// This is the test that goes red if a default ever creeps back in — an
/// `unwrap_or(DEFAULT_PREFIX)` on the route, or a `prefix: None` reaching the
/// service. Every CLI fixture in the suite passes `--prefix` explicitly, so the
/// whole Rust corpus is blind to that regression by construction; only this and
/// its CLI sibling in `tests/project_new.rs` would notice.
#[test]
fn web_init_without_a_prefix_is_a_400_naming_the_field() {
    let fixture = served();
    let port = fixture.port;
    let fresh = fixture.dir().join("prefixless");
    std::fs::create_dir_all(&fresh).unwrap();

    let body = serde_json::json!({"path": fresh.to_string_lossy()}).to_string();
    let err = post_json(&format!("http://127.0.0.1:{port}/api/repos"), &body).unwrap_err();

    assert_eq!(status_of(err), 400, "a missing prefix is a usage error");
    assert!(
        !fresh.join(".storyhook.toml").exists(),
        "and nothing is created on the way to saying so"
    );
}

/// A prefix the shared validator refuses is refused here too, rather than
/// stored.
///
/// The dashboard derives its suggestion in six lines of JavaScript that no test
/// in this repository can reach — there is no JS test infrastructure and the
/// assets are embedded in the binary. This is the mitigation: whatever the
/// browser sends goes through `domain::prefix::validate`, so a wrong derivation
/// is a testable 400 rather than a project minting ids nobody can parse back.
#[test]
fn web_init_with_an_invalid_prefix_is_refused_rather_than_stored() {
    let fixture = served();
    let port = fixture.port;
    let fresh = fixture.dir().join("badprefix");
    std::fs::create_dir_all(&fresh).unwrap();

    let body =
        serde_json::json!({"path": fresh.to_string_lossy(), "prefix": "hello world"}).to_string();
    let err = post_json(&format!("http://127.0.0.1:{port}/api/repos"), &body).unwrap_err();

    // 422 rather than 400, and the difference is the point: a *missing* prefix
    // is a malformed request (`AppError::Usage`), an *unusable* one is a
    // well-formed request carrying a value the domain refuses
    // (`AppError::Validation`). Both exit 2 on the CLI; HTTP can tell them
    // apart, and the dashboard shows the server's message either way.
    assert_eq!(status_of(err), 422);
    assert!(!fresh.join(".storyhook.toml").exists());
}

#[test]
fn web_init_requires_the_guard_header() {
    let fixture = served();
    let port = fixture.port;

    let body = serde_json::json!({"path": fixture.dir().to_string_lossy()}).to_string();
    let err =
        post_json_unguarded(&format!("http://127.0.0.1:{port}/api/repos"), &body).unwrap_err();
    assert_eq!(status_of(err), 403);
}

/// An unconfirmed delete answers with the plan and destroys nothing.
///
/// The same two-step the terminal runs, and deliberately the same value: the
/// browser draws its warning from the `DeletePlan` the CLI prompts from, so the
/// two front-ends cannot grow two different ideas of what delete does.
#[test]
fn web_delete_without_confirmation_returns_the_plan_and_deletes_nothing() {
    let fixture = served();
    fixture.seed(&["new", "Precious"]);
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let err = delete_json(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}"), "").unwrap_err();
    let ureq::Error::StatusCode(code) = err else {
        panic!("expected a status error");
    };
    assert_eq!(code, 409, "confirmation required");

    // Still there, stories and all.
    let data = ureq::get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap()
        .into_body()
        .read_to_string()
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&data).unwrap();
    assert_eq!(json["summary"]["total_open"], 1);
    assert!(fixture.dir().join(".storyhook.toml").exists());
}

#[test]
fn web_delete_with_the_slug_typed_back_destroys_the_project() {
    let fixture = served();
    fixture.seed(&["new", "Doomed"]);
    let (port, repo_id) = (fixture.port, fixture.repo_id.to_string());

    let body = serde_json::json!({ "confirm": repo_id }).to_string();
    let resp = delete_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}"),
        &body,
    )
    .unwrap();
    assert_eq!(resp.status(), 200);

    let list = ureq::get(format!("http://127.0.0.1:{port}/api/repos"))
        .call()
        .unwrap()
        .into_body()
        .read_to_string()
        .unwrap();
    let repos: serde_json::Value = serde_json::from_str(&list).unwrap();
    assert_eq!(
        repos.as_array().unwrap().len(),
        0,
        "the project is gone, not merely unlisted"
    );
    assert!(
        fixture.dir().join(".storyhook.toml").exists(),
        "and the browser, like the CLI, writes nothing into the repository"
    );
}

#[test]
fn web_delete_with_the_wrong_slug_is_refused() {
    let fixture = served();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let body = serde_json::json!({ "confirm": "not-the-slug" }).to_string();
    let err = delete_json(
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}"),
        &body,
    )
    .unwrap_err();
    let ureq::Error::StatusCode(code) = err else {
        panic!("expected a status error");
    };
    assert_eq!(code, 409, "a mistyped confirmation is not a confirmation");
    assert!(fixture.dir().join(".storyhook.toml").exists());
}

#[test]
fn web_deregister_repo_requires_guard_header() {
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let err = ureq::delete(format!("http://127.0.0.1:{port}/api/repos/{repo_id}"))
        .call()
        .unwrap_err();
    assert_eq!(status_of(err), 403);
}

// --- CLI DEFAULT_WEB_PORT constant ---

#[test]
fn default_web_port_constant_is_3456() {
    assert_eq!(storyhook::cli::DEFAULT_WEB_PORT, 3456);
}

// --- build_report_data with blocked stories ---

#[test]
fn build_report_data_with_blocked_story() {
    let fixture = served();
    fixture.seed(&["new", "Blocking story"]);
    fixture.seed(&["new", "Blocked story"]);
    // SH-2 depends on SH-1
    fixture.seed(&["link", "SH-1", "blocks", "SH-2"]);

    let data = fixture.report_data();
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
    let fixture = served();
    fixture.seed(&["new", "High priority"]);
    fixture.seed(&["set", "SH-1", "--priority", "high"]);
    fixture.seed(&["new", "No priority"]);

    let data = fixture.report_data();
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
    let fixture = served();
    fixture.seed(&["new", "Epic story", "--type", "epic"]);
    fixture.seed(&["new", "Untyped story"]);

    let data = fixture.report_data();
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
    let fixture = served();
    fixture.seed(&["new", "Concurrent test"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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

/// The catalog verbs are gone; `story project` owns them. They must be
/// *refused* rather than quietly parsed into something else.
#[test]
fn web_parse_rejects_the_retired_catalog_verbs() {
    for verb in ["register", "deregister", "list"] {
        let result = storyhook::cli::parse_invocation(&["web".to_string(), verb.to_string()]);
        assert!(
            result.is_err(),
            "`story web {verb}` must not parse: {result:?}"
        );
    }
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
    // Since SH-62 the flag gate answers before `parse_web`, so the rejection
    // names the token rather than printing a bare `usage:` line. Still a
    // failure, still about the flag — and `--verbose` is now reported as the
    // undeclared flag it is.
    let dir = scratch_dir();
    story(dir.path())
        .args(["web", "start", "--verbose"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown flag `--verbose`"));
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
        .stderr(predicate::str::contains("invalid port"));
}

#[test]
fn web_start_port_negative_is_invalid() {
    let dir = scratch_dir();
    story(dir.path())
        .args(["web", "start", "--port", "-1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("invalid port"));
}

// --- is_ready and is_blocked correctness ---

#[test]
fn web_serve_api_data_ready_and_blocked_flags_correct() {
    let fixture = served();
    fixture.seed(&["new", "Ready story"]);
    fixture.seed(&["new", "Blocked story"]);
    // SH-2 depends on SH-1 (which is open), so SH-2 is blocked
    fixture.seed(&["link", "SH-1", "blocks", "SH-2"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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

// --- A directory that is not a project ---

/// The store answers "no such project" rather than the dashboard discovering it
/// by failing to read a directory.
///
/// The question this used to ask — does `build_report_data` error outside a
/// project — is not answerable any more: report data is a function of a
/// `ProjectId`, and there is no way to name one that does not exist. The
/// property it was protecting, that the dashboard refuses to invent a project
/// for an unknown id, is asked of the route instead.
#[test]
fn an_unknown_project_is_refused_rather_than_invented() {
    let fixture = served();
    let port = fixture.port;

    let err = ureq::get(format!(
        "http://127.0.0.1:{port}/api/repos/not-a-project/data"
    ))
    .call()
    .unwrap_err();
    assert_eq!(status_of(err), 404);
}

// --- Tailnet dual-bind ---
//
// These five ask about a real tailnet interface, so they need a machine that
// has one and a server that managed to bind it. What they must never do is
// decide that for themselves: a `tailscale` probe in this process answers "does
// this machine have a tailnet?", while the only question that matters here is
// "did the server I just started bind one?". Those came apart under load and
// that is SH-110. Every one of them now takes the answer from
// `fixture.bound.tailnet`, which is the server's own report.
//
// They still skip on a machine without a tailnet, and a skip is invisible. The
// guard against the whole family silently going to zero is not here: it is
// `tests/tailnet_advertise.rs`, whose tests bring their own `tailscale` and
// never skip, plus the unit tests in `daemon::lifecycle`. A sentinel that
// failed whenever *this* machine has a tailnet the daemon did not bind was
// considered and rejected — it would fire during exactly the load episode the
// story documents, relocating the flake rather than removing it.

/// Says why a tailnet test did not run. Loud, and uniform, so a run's output
/// shows the whole family skipping together rather than one line lost in it.
fn skip_no_tailnet_listener() {
    eprintln!(
        "skipping: the test server bound no tailnet interface (no tailnet on this \
         machine, or its probe missed the 3s deadline)"
    );
}

#[test]
fn web_serve_binds_tailnet_ip_when_available() {
    let fixture = served();
    let Some(bind) = fixture.bound.tailnet.clone() else {
        return skip_no_tailnet_listener();
    };
    fixture.seed(&["new", "Reachable via tailnet"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    // Bound is not accepting: `ready` fires before the accept loops are
    // spawned, so this wait is not made redundant by the server's report. What
    // the report removes is the possibility of waiting on an address the server
    // never bound at all — which is what timed out at five seconds in SH-110.
    wait_for_addr(&format!("{}:{port}", bind.ip()));

    // Loopback still works.
    let loopback = ureq::get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    assert_eq!(loopback.status(), 200);

    // The tailnet interface is also bound and serves the same data.
    let tailnet_url = format!("http://{}:{port}/api/repos/{repo_id}/data", bind.ip());
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
    let fixture = served();
    let Some(bind) = fixture.bound.tailnet.clone() else {
        return skip_no_tailnet_listener();
    };
    fixture.seed(&["new", "Move me from the tailnet"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    wait_for_addr(&format!("{}:{port}", bind.ip()));

    // No STORYHOOK_WEB_TRUSTED_HOSTS is set — the server itself decided to
    // bind this interface, so mutations through it must be trusted by
    // default, the same way loopback is.
    let url = format!(
        "http://{}:{port}/api/repos/{repo_id}/story/SH-1/move",
        bind.ip()
    );
    let resp = ureq::post(&url)
        .header("X-Storyhook", "1")
        .content_type("application/json")
        .send(r#"{"state":"in-progress"}"#)
        .unwrap_or_else(|e| panic!("expected the tailnet interface to be auto-trusted: {e}"));
    assert_eq!(resp.status(), 200);
}

/// The MagicDNS FQDN this server's own bind earned, or a skip.
///
/// A test that needs the name rather than the address: a tailnet can be bound
/// without MagicDNS being enabled on it.
fn fqdn_of(fixture: &Served) -> Option<String> {
    fixture
        .bound
        .tailnet
        .as_ref()
        .and_then(|bind| bind.magic_dns().map(str::to_string))
}

/// Asserts the tailnet listener answers a *trusted* mutation, so that a
/// rejection asserted afterwards means "this host is not trusted" and not
/// "nothing here accepts anything".
///
/// Without it, a regression that emptied `trusted_hosts` would make the two
/// rejection tests below pass while proving nothing — and this change rewrites
/// where `trusted_hosts` comes from, so that is the regression it is most
/// exposed to. Their protection against it used to live in a *different* test
/// function, which any later edit could delete without touching them.
fn assert_the_listener_accepts_a_trusted_host(fixture: &Served, story: &str) {
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/{story}/move");
    let resp = ureq::post(&url)
        .header("X-Storyhook", "1")
        .content_type("application/json")
        .send(r#"{"state":"in-progress"}"#)
        .unwrap_or_else(|e| {
            panic!("the positive control failed: a trusted mutation must succeed here, or a 403 below proves nothing: {e}")
        });
    assert_eq!(resp.status(), 200, "the positive control must return 200");
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
    // only that the tailnet listener actually bound (which is what populates
    // `trusted_hosts` with the FQDN in the first place).
    let fixture = served();
    let Some(bind) = fixture.bound.tailnet.clone() else {
        return skip_no_tailnet_listener();
    };
    let Some(fqdn) = fqdn_of(&fixture) else {
        eprintln!("skipping: the bound tailnet has no MagicDNS name");
        return;
    };
    fixture.seed(&["new", "Move me via MagicDNS"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    wait_for_addr(&format!("{}:{port}", bind.ip()));

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
    let fixture = served();
    let Some(bind) = fixture.bound.tailnet.clone() else {
        return skip_no_tailnet_listener();
    };
    let Some(fqdn) = fqdn_of(&fixture) else {
        eprintln!("skipping: the bound tailnet has no MagicDNS name");
        return;
    };
    let short_label = fqdn.split('.').next().unwrap().to_string();

    fixture.seed(&["new", "Story"]);
    fixture.seed(&["new", "Control"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    wait_for_addr(&format!("{}:{port}", bind.ip()));
    assert_the_listener_accepts_a_trusted_host(&fixture, "SH-2");

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
    let fixture = served();
    let Some(bind) = fixture.bound.tailnet.clone() else {
        return skip_no_tailnet_listener();
    };
    let Some(fqdn) = fqdn_of(&fixture) else {
        eprintln!("skipping: the bound tailnet has no MagicDNS name");
        return;
    };
    let suffix = fqdn
        .split_once('.')
        .map_or(fqdn.clone(), |(_, rest)| rest.to_string());
    let foreign_host = format!("definitely-not-this-host.{suffix}");

    fixture.seed(&["new", "Story"]);
    fixture.seed(&["new", "Control"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    wait_for_addr(&format!("{}:{port}", bind.ip()));
    assert_the_listener_accepts_a_trusted_host(&fixture, "SH-2");

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
    let fixture = served();

    let port = fixture.port;
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
    let fixture = served();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let mut sse = connect_sse(port);
    fixture.seed(&["new", "Live update smoke test"]);

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
    let fixture = served();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    let fixture = served();
    let port = fixture.port;

    fixture.seed(&["new", "Debounce target"]);

    let mut sse = connect_sse(port);
    const MUTATIONS: usize = 6;
    // Mutate in-process rather than by spawning `story` subprocesses:
    // subprocess spawn latency is too variable under the CPU load this whole
    // suite generates to reliably land all of them inside the coalescing
    // window, which would make this assertion flaky for reasons that have
    // nothing to do with the logic under test. A tight in-process loop does.
    for _ in 0..MUTATIONS {
        fixture.seed(&["comment", "SH-1", "rapid update"]);
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
    let fixture = served();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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
    fixture.seed(&["new", "After disconnect"]);
    let received = read_sse_until(&mut second, "event: repo-changed", Duration::from_secs(8));
    assert!(
        received.contains("event: repo-changed"),
        "expected the second connection to still receive live events, got: {received}"
    );
}

/// Registering a repo *after* the server (and an SSE client) has already
/// started is itself detected (`repos-changed`), and the newly-registered
/// repo is served from then on (its own `repo-changed` on mutation) —
/// without restarting the dashboard.
#[test]
fn sse_detects_runtime_repo_registration_and_serves_it() {
    let _sse_guard = sse_test_lock();
    let fixture = served();
    let port = fixture.port;

    let (dir_b, slug_b) = fixture.add_project();
    fixture.deregister();

    let mut sse = connect_sse(port);

    let body =
        serde_json::json!({"path": dir_b.path().to_string_lossy(), "prefix": "RB"}).to_string();
    post_json(&format!("http://127.0.0.1:{port}/api/repos"), &body)
        .expect("registering repo B at runtime must succeed");

    let after_register = read_sse_until(&mut sse, "event: repos-changed", Duration::from_secs(8));
    assert!(
        after_register.contains("event: repos-changed"),
        "expected repos-changed after a runtime registration, got: {after_register}"
    );

    // The catalog change is published at the request boundary, after the write
    // has committed — so seeing the event is itself proof repo B is resolvable,
    // and no settle delay is needed before mutating it.
    let mutate = serde_json::json!({"title": "In the newly-registered repo"}).to_string();
    post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{slug_b}/story"),
        &mutate,
    )
    .expect("creating a story in repo B must succeed");
    let after_mutation = read_sse_until(
        &mut sse,
        &format!("\"repo_id\":\"{slug_b}\""),
        Duration::from_secs(8),
    );
    assert!(
        after_mutation.contains(&format!("\"repo_id\":\"{slug_b}\"")),
        "expected a repo-changed event for the newly-registered repo `{slug_b}`, \
         got: {after_mutation}"
    );
}

/// A project whose checkout vanishes from disk (moved, deleted) must not stop
/// the feed from serving every other project.
///
/// This used to be a statement about a filesystem watcher, which held one watch
/// per repository and could lose one. There are no per-project resources any
/// more — one store, one connection pool, one bus — so the property is
/// structural rather than defended. It is asserted anyway, because "structurally
/// true" is a claim about today's code and this is the test that notices when
/// somebody reintroduces per-project state.
#[test]
fn sse_one_unreachable_repo_does_not_break_stream_for_others() {
    let _sse_guard = sse_test_lock();
    let fixture = served();
    let port = fixture.port;
    let broken_id = fixture.repo_id.clone();
    let (dir_healthy, healthy_id) = fixture.add_project();

    let mut sse = connect_sse(port);

    // Baseline: confirm the about-to-break project's changes are actually
    // reaching this client before disrupting anything.
    fixture.seed(&["new", "before break"]);
    // Waits for *this* project's event rather than for any `repo-changed`: the
    // second project's creation is itself a change, so the stream legitimately
    // carries events for both and the order between them is not a contract.
    let baseline = read_sse_until(
        &mut sse,
        &format!("\"repo_id\":\"{broken_id}\""),
        Duration::from_secs(8),
    );
    assert!(
        baseline.contains(&format!("\"repo_id\":\"{broken_id}\"")),
        "expected the soon-to-be-broken repo's changes to be live first, got: {baseline}"
    );

    // Now its directory vanishes.
    std::fs::remove_dir_all(fixture.dir()).unwrap();

    let mutate = serde_json::json!({"title": "Still alive"}).to_string();
    post_json(
        &format!("http://127.0.0.1:{port}/api/repos/{healthy_id}/story"),
        &mutate,
    )
    .expect("the healthy project must still take writes");
    drop(dir_healthy);

    let received = read_sse_until(
        &mut sse,
        &format!("\"repo_id\":\"{healthy_id}\""),
        Duration::from_secs(8),
    );
    assert!(
        received.contains(&format!("\"repo_id\":\"{healthy_id}\"")),
        "expected the healthy repo's change to still arrive despite the broken repo, got: {received}"
    );
}

/// **Proof the filesystem watcher is gone, not merely unused.**
///
/// The feed used to be driven by a `notify` watcher over each repository's
/// story directory, which is why writing a file there produced an event. It is
/// driven by the store now, so a file appearing in the checkout — a build
/// artifact, an editor swap file, a `git checkout` rewriting half the tree — is
/// none of the dashboard's business.
///
/// Asserting the *absence* of an event is only meaningful if something proves
/// the stream is alive, so this touches files first and then makes a real change
/// afterwards: the story event must arrive, and nothing must precede it.
#[test]
fn no_filesystem_watcher_remains() {
    let _sse_guard = sse_test_lock();
    let fixture = served();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let mut sse = connect_sse(port);

    // Every shape the old watcher reacted to: a file in the repository root, a
    // file in the legacy story directory, and a file in the legacy config
    // directory.
    let legacy = fixture.dir().join(".storyhook/open/stories");
    std::fs::create_dir_all(&legacy).expect("creating a legacy-looking directory");
    std::fs::write(fixture.dir().join("BUILD.log"), "noise").expect("writing a repo file");
    std::fs::write(legacy.join("SH-1.jsonl"), "{}\n").expect("writing a story-looking file");
    std::fs::write(fixture.dir().join(".storyhook/states.toml"), "[[state]]\n")
        .expect("writing a config-looking file");

    // Now something that genuinely changed the store. Its event is the proof
    // that the stream was live the whole time the files above were being
    // written and ignored.
    fixture.seed(&["new", "A real change"]);
    let received = read_sse_until(
        &mut sse,
        &format!("\"repo_id\":\"{repo_id}\""),
        Duration::from_secs(8),
    );

    assert_eq!(
        received.matches("event: repo-changed").count(),
        1,
        "exactly one change event — the store write — must have reached the client; \
         a second one means a filesystem watcher is still running: {received}"
    );
}

/// A heartbeat `ping` event arrives even with no story changes at all, so a
/// client can tell "connected and idle" apart from "silently dead". It is a
/// real named event, not a bare SSE comment, precisely so a browser can act
/// on it (`sseWatchdog` in `web_dashboard.html`, SH-145) — this pins the
/// wire format that depends on.
/// Runs the server as the real daemon subprocess (`web start`) rather than
/// in-thread, so `STORYHOOK_SSE_HEARTBEAT_MS` — process-wide env state —
/// is scoped to that child process instead of leaking into this test
/// binary's own environment, where it could affect other tests running
/// concurrently in the same `cargo test` process.
#[test]
fn sse_heartbeat_ping_arrives_without_any_story_changes() {
    let _sse_guard = sse_test_lock();
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let port = reserve_port();
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .env("STORYHOOK_SSE_HEARTBEAT_MS", "300")
        .args(["web", "start", "--port", &port.to_string()])
        .assert()
        .success();
    wait_for_server(port);

    let mut sse = connect_sse(port);
    let received = read_sse_until(&mut sse, "event: ping", Duration::from_secs(8));
    assert!(
        received.contains("event: ping"),
        "expected a heartbeat ping, got: {received}"
    );

    env.story(dir.path())
        .args(["web", "stop"])
        .assert()
        .success();
}

/// SH-145: a story created through the CLI's `/api/v1/invoke` transport
/// must still reach an open dashboard tab live.
///
/// Every other `sse_*` test in this file mutates through `Served::seed`
/// (an in-process `dispatch`, bypassing the daemon's HTTP surface entirely)
/// or through the dashboard's own REST routes (`rest::route`, published at
/// the request boundary by `dispatch()` in `daemon/serve.rs`). Neither is
/// what a real `story` command does: since SH-114 every CLI write goes
/// through `rpc::route`'s `POST /api/v1/invoke`, which — unlike
/// `rest::route` — publishes nothing at the request boundary and leaves the
/// change feed to notice the write only via `poll_change_token`'s 250ms
/// safety-net poll. This test is the one that actually exercises that path,
/// running the real daemon subprocess and a real `story new` alongside it.
#[test]
fn sse_delivers_repo_changed_for_a_cli_write_through_the_daemon() {
    let _sse_guard = sse_test_lock();
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let port = reserve_port();
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .args(["web", "start", "--port", &port.to_string()])
        .assert()
        .success();
    wait_for_server(port);

    env.story(dir.path())
        .args(["project", "new", "--prefix", "SH", "--no-agents-md"])
        .assert()
        .success();

    let mut sse = connect_sse(port);

    env.story(dir.path())
        .args(["new", "Created through the CLI, not the dashboard"])
        .assert()
        .success();

    let received = read_sse_until(&mut sse, "event: repo-changed", Duration::from_secs(8));
    assert!(
        received.contains("event: repo-changed"),
        "a story created via `story new` (the CLI's `/api/v1/invoke` transport) must \
         reach an open dashboard tab live, got: {received}"
    );

    env.story(dir.path())
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
    let fixture = served();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

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

/// `tailscale status --json` is shelled out to during server start-up, and
/// the CLI talks to `tailscaled`, which wedges: probes stuck for minutes were
/// observed on this machine, orphaned by servers that had already exited.
/// Because the loopback listener is bound before that probe runs, a wedged
/// probe leaves the dashboard accepting connections and answering nothing at
/// all — indistinguishable from a healthy server until a request hangs. The
/// probe must be bounded, and the dashboard must serve loopback regardless.
#[test]
fn a_wedged_tailscale_cli_cannot_stop_the_dashboard_from_serving() {
    let env = TestEnv::isolated();
    let fake_bin = scratch_dir();
    let fake_tailscale = fake_bin.path().join("tailscale");
    std::fs::write(&fake_tailscale, "#!/bin/sh\nsleep 120\n").expect("writing the fake CLI");
    std::fs::set_permissions(
        &fake_tailscale,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
    )
    .expect("making the fake CLI executable");

    let port = reserve_port();
    let path = format!(
        "{}:{}",
        fake_bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_story"));
    env.apply(&mut command);
    let child = command
        .args(["web", "--serve", "--port", &port.to_string()])
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawning the dashboard");
    let _guard = ChildGuard::new(child);

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut last = None;
    while Instant::now() < deadline {
        last = http_status_line(port, Duration::from_millis(500));
        if last.as_deref().is_some_and(|line| line.contains("200")) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!(
        "the dashboard never served a request while `tailscale` hung; last response line: {last:?}"
    );
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

// --- Which checkout the dashboard acts in ---
//
// `a_project_with_a_worktree_resolves_to_its_main_checkout` used to live here.
// Its subject was `preferred_checkout`, which chose between the several rows
// `project_paths` held for one project — a worktree sorting before the main
// tree won the toss, and the dashboard then ran that branch's hooks. There is
// nothing left to choose between: a project records **one** checkout, and a
// second `story project new` in a worktree cannot displace it, because
// `adopt_checkout` fills a gap and never replaces (SH-119).

// --- A project with no checkout on this machine ---
//
// Reachable by deleting a checkout and running `story doctor --fix`, or by
// importing a project this machine has never had a copy of. Its stories are in
// the database the daemon already has open, so the honest answer is to serve
// them — read-only, because every write needs a working directory to fire the
// project's hooks and run its git operations in.

#[test]
fn a_project_with_no_checkout_is_listed_rather_than_hidden() {
    let fixture = served();
    fixture.seed(&["new", "Still readable"]);
    fixture.deregister();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    let body = ureq::get(format!("http://127.0.0.1:{port}/api/repos"))
        .call()
        .unwrap()
        .into_body()
        .read_to_string()
        .unwrap();
    let repos: serde_json::Value = serde_json::from_str(&body).unwrap();
    let repos = repos.as_array().unwrap();

    assert_eq!(repos.len(), 1, "the project is still a project: {repos:?}");
    assert_eq!(repos[0]["id"], repo_id);
    assert!(repos[0]["path"].is_null(), "there is no path to report");
    assert_eq!(repos[0]["read_only"], true);
    assert_eq!(
        repos[0]["available"], false,
        "there is nowhere to act, and the dashboard has to say so"
    );
    assert!(
        repos[0]["reason"].as_str().is_some_and(|r| !r.is_empty()),
        "and why: {:?}",
        repos[0]["reason"]
    );
    assert_eq!(
        repos[0]["summary"]["total_open"], 1,
        "its stories are still counted — they are in the store"
    );
}

#[test]
fn a_project_with_no_checkout_still_serves_its_board() {
    let fixture = served();
    fixture.seed(&["new", "Still readable"]);
    fixture.deregister();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    let resp = ureq::get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "reading needs no working directory — this used to be a 404"
    );
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["summary"]["total_open"], 1);
}

#[test]
fn a_project_with_no_checkout_refuses_writes_and_says_why() {
    let fixture = served();
    fixture.deregister();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    let err = ureq::post(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story"))
        .header("X-Storyhook-Dashboard", "1")
        .send_json(serde_json::json!({ "title": "Nope" }))
        .expect_err("a write with nowhere to run must be refused");

    let body = match err {
        ureq::Error::StatusCode(code) => {
            assert!((400..500).contains(&code), "expected a 4xx, got {code}");
            String::new()
        }
        other => panic!("expected a status error, got {other:?}"),
    };
    let _ = body;

    // Nothing was created.
    let data = ureq::get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap()
        .into_body()
        .read_to_string()
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&data).unwrap();
    assert_eq!(json["summary"]["total_open"], 0);
}
