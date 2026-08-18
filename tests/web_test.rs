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
use storyhook::daemon::lifecycle::CONTROL_DEADLINE;
use storyhook::daemon::serve::BoundAddress;
use storyhook::domain::provenance::Provenance;
use storyhook::env::Environment;
use storyhook::invoke::{dispatch, dispatch_unscoped};
use storyhook::service::Ctx;
use storyhook::store::{ProjectId, ReadOps, SqliteStore, Store, StoryNo};
use storyhook_test_support::{
    DaemonGuard, TestEnv, path_without_tailscale, scratch_dir, serve, wait_for_addr,
    wait_for_server,
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
    /// The bearer token every `/api/**` route on this fixture's server
    /// requires (SH-187). Minted fresh per server by
    /// `storyhook::daemon::serve::bind_and_serve`, real rather than empty —
    /// an empty `expected` fails closed (`rpc::token_ok`).
    token: String,
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

    /// An HTTP client for this fixture, authenticated.
    ///
    /// The one seam every test request flows through: middleware attaches
    /// `X-Storyhook-Token` to every outgoing request, so this file's ~150
    /// call sites needed no change when the server started requiring one
    /// (SH-187) — only this method did.
    fn agent(&self) -> ureq::Agent {
        token_agent(&self.token)
    }
}

/// An agent that attaches `token` as `X-Storyhook-Token` to every request.
///
/// Separate from [`Served::agent`] so a test driving the *real daemon
/// subprocess* — which has no `Served` fixture, only a token read from the
/// portfile — reaches the same seam rather than rebuilding the middleware.
fn token_agent(token: &str) -> ureq::Agent {
    let token = token.to_string();
    let config = ureq::Agent::config_builder()
        .middleware(
            move |mut req: ureq::http::Request<ureq::SendBody>,
                  next: ureq::middleware::MiddlewareNext|
                  -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
                req.headers_mut().insert(
                    "X-Storyhook-Token",
                    ureq::http::HeaderValue::from_str(&token)
                        .expect("the fixture's own token is a valid header value"),
                );
                next.handle(req)
            },
        )
        .build();
    ureq::Agent::new_with_config(config)
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

    let server = serve(Arc::clone(&store), &environment);
    Served {
        env,
        store,
        project,
        dir,
        port: server.bound.port(),
        bound: server.bound,
        repo_id,
        token: server.token,
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
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .args(["web", "start"])
        .assert()
        .success();
    let port = started_port(&env);
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

/// A `$BROWSER` that records the argv it was handed, instead of opening one.
///
/// The only way to see what `story web open` actually sent to a browser:
/// stdout carries the bare URL by design (SH-251), so a test that reads
/// stdout alone cannot tell a handoff from its absence — which is precisely
/// the distinction the two tests below turn on.
///
/// Returns the scratch directory (which must outlive the command) and the
/// path the shim writes to.
fn recording_browser() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("storyhook-browser-shim-")
        .tempdir_in("/private/tmp")
        .expect("a scratch directory");
    let recorded = dir.path().join("opened-url");
    let shim = dir.path().join("browser");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nprintf '%s' \"$1\" > {}\n",
            recorded.to_string_lossy()
        ),
    )
    .expect("writing the shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
            .expect("making the shim executable");
    }
    (dir, recorded)
}

/// SH-251's client-side acceptance criteria, both halves of them: the browser
/// gets a one-shot coupon in the URL *fragment*, and the terminal never does.
///
/// The fragment is what keeps the coupon out of the daemon's own access
/// path — a fragment is never sent to a server — and printing the bare URL is
/// what keeps it out of scrollback, shell history and any script piping this
/// command. A test asserting only on stdout would pass with the whole feature
/// deleted, which is why the shim above exists.
#[test]
fn web_open_hands_the_browser_a_coupon_and_the_terminal_the_bare_url() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .args(["web", "start"])
        .assert()
        .success();
    let port = started_port(&env);
    wait_for_server(port);

    let (_shim_dir, recorded) = recording_browser();
    let opened = env
        .story(dir.path())
        .env("BROWSER", _shim_dir.path().join("browser"))
        .args(["web", "open"])
        .output()
        .expect("running `story web open`");
    assert!(opened.status.success());

    let url = std::fs::read_to_string(&recorded).expect("the shim recorded a URL");
    let expected_prefix = format!("http://127.0.0.1:{port}/#h=");
    assert!(
        url.starts_with(&expected_prefix),
        "the browser must be opened at a handoff fragment; got {url}"
    );
    let coupon = &url[expected_prefix.len()..];
    assert_eq!(coupon.len(), 32, "a coupon is 32 hex characters: {coupon}");
    assert!(coupon.chars().all(|c| c.is_ascii_hexdigit()));

    // And neither stream ever carried it. stdout names the dashboard, with no
    // fragment at all.
    let stdout = String::from_utf8_lossy(&opened.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&opened.stderr).into_owned();
    assert!(stdout.contains(&format!("http://127.0.0.1:{port}/")));
    assert!(!stdout.contains(coupon), "stdout carried the coupon");
    assert!(!stderr.contains(coupon), "stderr carried the coupon");
    assert!(!stdout.contains("#h="), "stdout carried a handoff fragment");

    env.story(dir.path())
        .args(["web", "stop"])
        .assert()
        .success();
}

/// A failed arming is a degraded convenience, never a failed command.
///
/// Simulated the only honest way from outside the daemon: rewrite the
/// portfile's token so the client's arming request is refused by the very
/// gate that protects it, run the command, then put the real token back. The
/// daemon must not rewrite the portfile itself in that window, or its own
/// write — carrying the real token — would silently undo the corruption and
/// this test would stop testing anything.
///
/// Since SH-186 the daemon's portfile is no longer just-once-at-startup: a
/// tailnet bind that lands on `tailnet_reprobe`'s background thread rewrites
/// it too (`on_late_tailnet_bind`), and on a tailnet-equipped machine that
/// now happens for nearly every daemon shortly after start, not only for
/// SH-146's rare self-heal. `web start` below therefore runs with
/// [`path_without_tailscale`], which denies `tailscale` outright — the daemon
/// can never succeed a bind, so it can never rewrite the portfile out from
/// under this test's corruption, deterministically rather than by the timing
/// luck of finishing the corrupt/read/restore sequence before a real probe
/// resolves. `tests/daemon_lifecycle.rs`'s version-skew test needs the same
/// guarantee against the same hazard (SH-345) and shares this helper.
#[test]
fn web_open_falls_back_to_the_bare_url_when_arming_fails() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let _daemon = DaemonGuard::new(&env, dir.path());

    let (_no_tailscale, path) = path_without_tailscale(&env);
    env.story(dir.path())
        .env("PATH", &path)
        .args(["web", "start"])
        .assert()
        .success();
    let port = started_port(&env);
    wait_for_server(port);

    let portfile = env.environment().daemon_file();
    let real = std::fs::read_to_string(&portfile).expect("reading the portfile");
    let mut info: serde_json::Value = serde_json::from_str(&real).expect("the portfile is JSON");
    info["token"] = serde_json::Value::String("not-the-daemons-token".to_string());
    std::fs::write(&portfile, info.to_string()).expect("rewriting the portfile");

    let (_shim_dir, recorded) = recording_browser();
    let opened = env
        .story(dir.path())
        .env("BROWSER", _shim_dir.path().join("browser"))
        .args(["web", "open"])
        .output()
        .expect("running `story web open`");
    std::fs::write(&portfile, &real).expect("restoring the portfile");

    assert!(
        opened.status.success(),
        "a refused arming must not fail the command"
    );
    let url = std::fs::read_to_string(&recorded).expect("the shim recorded a URL");
    assert_eq!(
        url,
        format!("http://127.0.0.1:{port}/"),
        "without a coupon the browser gets exactly the URL it always got"
    );
    // Said out loud rather than swallowed, and naming what happens instead.
    let stderr = String::from_utf8_lossy(&opened.stderr).into_owned();
    assert!(
        stderr.contains("one-time handoff"),
        "the fallback must say what was lost; got: {stderr}"
    );

    env.story(dir.path())
        .args(["web", "stop"])
        .assert()
        .success();
}

/// `story web address` carries no handoff, pinned by a test rather than by
/// the absence of code.
///
/// Unanimous across every round of SH-251's council, and the reason is the
/// difference between the two commands: `web open` targets a browser on *this
/// machine*, while a copied URL is for another device — which is why it
/// advertises the tailnet host at all. `mutation_guard_ok` accepts a bound
/// tailnet `Host`, so a credential pasted into a URL bar over there would be
/// a live *write* credential for any tailnet peer. `web address` is a
/// location; `story daemon token` is a credential.
#[test]
fn web_address_copies_a_location_and_never_a_credential() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .args(["web", "start"])
        .assert()
        .success();
    let port = started_port(&env);
    wait_for_server(port);

    let copied = env
        .story(dir.path())
        .env("STORYHOOK_CLIPBOARD_CMD", "cat")
        .args(["web", "address"])
        .output()
        .expect("running `story web address`");
    assert!(copied.status.success());
    let stdout = String::from_utf8_lossy(&copied.stdout).into_owned();

    assert!(stdout.contains(&format!(":{port}/")));
    assert!(
        !stdout.contains('#'),
        "a copied URL must carry no fragment at all: {stdout}"
    );
    let token = env.daemon().expect("the daemon published a portfile").token;
    assert!(
        !stdout.contains(&token),
        "a copied URL must never carry the daemon's token"
    );

    env.story(dir.path())
        .args(["web", "stop"])
        .assert()
        .success();
}

/// How long [`web_start_status_address_advertise_the_host_the_daemon_bound`]
/// waits for the daemon's first tailnet probe to settle before treating
/// loopback as the final answer.
///
/// Since SH-186, `web start` no longer waits for the probe at all — its
/// portfile can read `tailnet: None` for a moment after a fresh spawn even on
/// a tailnet-equipped machine, simply because the background probe hasn't
/// answered yet (`serve::tailnet_reprobe`'s first attempt fires immediately,
/// but "immediately" is still asynchronous). This test polls rather than
/// reading the portfile once, the same shape `tests/tailnet_rebind.rs` already
/// proved out for its own self-heal assertion. Generous relative to the
/// production case it is bounding — a healthy `tailscale` typically answers
/// in well under a second — because this test must mean something on a
/// machine with no tailnet too: the poll notices nothing ever arrives and
/// proceeds with loopback as `expected`, rather than skip.
const TAILNET_SETTLE_DEADLINE: Duration = Duration::from_secs(5);

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
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .args(["web", "start"])
        .assert()
        .success();

    let settle_deadline = Instant::now() + TAILNET_SETTLE_DEADLINE;
    let info = loop {
        let info = env
            .daemon()
            .expect("the daemon published a portfile after binding");
        if info.tailnet.is_some() || Instant::now() >= settle_deadline {
            break info;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    wait_for_server(info.port);
    let expected = format!("http://{}:{}", info.advertised_host(), info.port);

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

/// The fallback half of [`web_start_status_address_advertise_the_host_the_
/// daemon_bound`]'s settle poll: a daemon with genuinely no tailnet to bind
/// must settle on loopback and stay there, not merely time out once. Every
/// `web status`/`web address` call after the settle deadline reports the
/// same loopback URL every time — never an error, never a stale claim of a
/// tailnet it does not have.
#[test]
fn web_start_settles_on_loopback_when_there_is_never_a_tailnet() {
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let _daemon = DaemonGuard::new(&env, dir.path());

    let shim = tempfile::Builder::new()
        .prefix("storyhook-tailscale-absent-")
        .tempdir_in("/private/tmp")
        .expect("a scratch directory");
    let fake = shim.path().join("tailscale");
    std::fs::write(&fake, "#!/bin/sh\nexit 1\n").expect("writing the shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))
            .expect("making the shim executable");
    }
    let mut entries = vec![shim.path().to_path_buf()];
    entries.extend(std::env::split_paths(&env.path_with_binary()));
    let path = std::env::join_paths(entries).expect("joining PATH");

    let started = Instant::now();
    env.story(dir.path())
        .env("PATH", &path)
        .args(["web", "start"])
        .assert()
        .success();
    assert!(
        started.elapsed() < TAILNET_SETTLE_DEADLINE,
        "`web start` must return promptly even though this machine's `tailscale` \
         never answers — SH-186's whole point is that nothing on this path waits \
         on the probe"
    );

    // Give the background probe every chance it will ever get, then confirm
    // it never produced a tailnet bind — the honest terminal state for a
    // machine with none.
    std::thread::sleep(TAILNET_SETTLE_DEADLINE);
    let info = env
        .daemon()
        .expect("the daemon published a portfile after binding");
    assert!(
        info.tailnet.is_none(),
        "a `tailscale` that only ever fails must never produce a bind: {:?}",
        info.tailnet
    );
    let expected = format!("http://127.0.0.1:{}", info.port);

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
            "`story {}` must settle on loopback and stay there rather than error or \
             invent a tailnet host; got: {stdout}",
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
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .args(["web", "start"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Web UI started"));

    wait_for_server(started_port(&env));

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
    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/"))
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

/// SH-197's deep link (`?project=<slug>&story=<id>`) is resolved client-side
/// by the dashboard's own JS, not by the server -- but that only works if
/// the server still serves the dashboard for a `/` request that carries a
/// query string. `request_path` (`src/api/http.rs`) strips the query before
/// routing, so this should already hold; pinned here so a future change to
/// that stripping can't silently break every pasted link.
#[test]
fn web_serve_root_html_with_query_string_still_serves_the_dashboard() {
    let fixture = served();

    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!(
            "http://127.0.0.1:{port}/?project=some-project&story=SH-1"
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

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
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

    // SH-217: the dashboard builds DOM, never HTML text. With
    // `script-src 'unsafe-inline'` (src/api/http.rs::CSP) a single
    // string-to-markup sink turns any story description or comment body
    // into script the moment the markdown renderer exists to build one
    // from untrusted text. `esc()`, the file's one-time innerHTML reader
    // with zero call sites, was removed so this assertion is true rather
    // than aspirational.
    for sink in [
        "innerHTML =",
        "innerHTML=",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "new Function(",
        "eval(",
    ] {
        assert!(
            !body.contains(sink),
            "the dashboard must never build markup from a string: found `{sink}`"
        );
    }

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
    // The daemon token modal (SH-50, generalized dashboard-wide by SH-187)
    // -- the Dispatch (and, since SH-208, Dispatch Auto) buttons themselves
    // are built by JS only for an open story in a project with a checkout,
    // so they never appear as HTML `id="..."` attributes in this static
    // markup; the modal they (and every other authenticated call) can open
    // does. Their ids are still pinned here, as the JS *source* text the
    // single embedded <script> carries -- the same idiom already used below
    // for typeGlyph/buildTypeBadge, JS-only constructs with no static tag.
    assert!(body.contains(r#"id="token-modal""#));
    assert!(body.contains(r#"id="token-input""#));
    assert!(body.contains(r#"id="token-submit""#));
    assert!(body.contains(r#"id: "dispatch-btn""#));
    assert!(body.contains(r#"id: "dispatch-auto-btn""#));
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
    // SH-255: the token modal exchanges a pasted value for a cookie rather
    // than attaching it by hand, and the SSE stream rides that cookie
    // automatically -- no header and no ?token= query parameter to find.
    assert!(body.contains("X-Storyhook-Token"));
    assert!(body.contains(r#""/api/events""#));
    assert!(!body.contains(r#""/api/events?token=""#));
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

    // SH-197: a story can be opened straight from ?project=&story= on load,
    // and the address bar is kept in sync as the URL is the only mechanism
    // "Copy URL" has for the caller to actually get a shareable link.
    assert!(body.contains("function readDeepLink"));
    assert!(body.contains("function syncUrl"));
    assert!(body.contains("function storyPermalink"));
    assert!(body.contains("history.replaceState"));
    assert!(body.contains(r#""?project=""#));

    // SH-197: one delete-confirmation modal, shared by the drawer footer's
    // Delete button and the context menu's Delete item, replacing the
    // drawer's own inline typed-reason form.
    assert!(body.contains(r#"id="delete-modal""#));
    assert!(body.contains(r#"id="delete-reason""#));
    assert!(body.contains(r#"id="delete-modal-submit""#));
    assert!(body.contains(r#"id="delete-modal-error""#));
    assert!(
        !body.contains("renderDeleteConfirm"),
        "the inline typed-reason footer form was replaced by the shared delete modal"
    );

    // SH-197: board cards and list rows carry a roving tabindex (WAI-ARIA
    // grid pattern) so Tab can reach a story at all -- the prerequisite for
    // Shift+F10/the Menu key ever raising the context menu from a keyboard.
    assert!(body.contains("function syncRoving"));
    assert!(body.contains("function bindRovingKeys"));
    assert!(body.contains("function bindRovingFocus"));
    assert!(body.contains(".card:focus-visible"));

    // SH-197: the story context menu -- Copy ID/URL/Description for now,
    // Dispatch/Set Status/Delete added by later commits onto the same
    // storyMenuModel.
    assert!(body.contains(".ctxmenu"));
    assert!(body.contains("function openStoryMenu"));
    assert!(body.contains("function storyMenuModel"));
    assert!(body.contains("function copyText"));
    assert!(body.contains(r#"execCommand("copy")"#));
    assert!(body.contains("\"Copy Description\""));

    // SH-197: the context menu's Dispatch group -- gated identically to the
    // drawer footer's own Dispatch buttons (dashboard-dispatch.md's As-built
    // section names the shared expression).
    assert!(body.contains("\"Dispatch\""));
    assert!(body.contains("\"Dispatch Auto\""));
    assert!(body.contains("dispatchHidden"));

    // SH-197: the context menu's Set Status submenu.
    assert!(body.contains("\"Set Status\""));
    assert!(body.contains("function setStoryState"));
    assert!(body.contains("function statusMenuItems"));
    assert!(body.contains(".ctxmenu-sub"));

    // SH-310: the context menu's Set Priority submenu -- the daemon's own
    // priority vocabulary as a radio group with the story's current value
    // checked, reaching the same POST /priority the drawer's own select
    // does. Hidden (not disabled) on a closed story, for the same reason and
    // the same expression as the Dispatch group.
    assert!(body.contains("\"Set Priority\""));
    assert!(body.contains("function priorityMenuItems"));
    assert!(body.contains("function setStoryPriority"));
    assert!(body.contains(r#"ariaLabel: "Set priority""#));
    // The rule the daemon does not enforce on its own: `set_priority`
    // appends an event and fires a PriorityChange hook even when the value
    // is unchanged, so re-picking the current one must not leave this page.
    assert!(body.contains("story.story.priority === priority"));

    // SH-358: the create modal renders the envelope's `warnings` — before
    // this, `api()` resolved the parsed envelope (warnings included) and
    // nothing in this file read the field at all.
    assert!(body.contains("function toastEnvelopeWarnings"));
    assert!(body.contains("payload.warnings"));
    assert!(body.contains("toastEnvelopeWarnings(payload)"));

    // SH-197: the context menu's Delete item, reaching the same shared
    // modal (commit 3) the drawer footer's own Delete button opens.
    assert!(body.contains("\"Delete\", danger: true"));

    // SH-310: a drawer field edit (and, via the same function, a context-
    // menu action) reports a client timeout the honest way -- through
    // `describeMutationFailure` (SH-312's answer for the create modal),
    // not through the flat, sometimes-false `toastError`.
    assert!(body.contains("toast(describeMutationFailure(err)"));
    assert!(
        !body.contains("api(method, path, body).then(handleMutationSuccess).catch(toastError)"),
        "runFieldMutation must not report an unproven mutation outcome as a definite failure"
    );

    // SH-305: every board column gets its own sort menu (replacing SH-128's
    // single board-wide pair of buttons), opened from a `.column-sort-btn`
    // in the column header and built on the same `.ctxmenu` machinery the
    // story context menu above uses.
    assert!(body.contains("function openColumnSortMenu"));
    assert!(body.contains("function columnSortMenuModel"));
    assert!(body.contains("function columnSortFor"));
    assert!(body.contains("function columnSortOptionsFor"));
    assert!(body.contains(".column-sort-btn"));
    for label in [
        "Added ↑",
        "Added ↓",
        "Modified ↑",
        "Modified ↓",
        "Priority ↑",
        "Priority ↓",
        // SH-407: an OPEN column additionally offers "Next" (the order
        // `story next` would hand this queue out in) and a CLOSED column
        // "Completed" (`closed_at`) -- see `columnSortOptionsFor`.
        "Next ↑",
        "Next ↓",
        "Completed ↑",
        "Completed ↓",
    ] {
        assert!(
            body.contains(label),
            "the column sort menu must offer `{label}`"
        );
    }
    assert!(body.contains("function nextRank"));
    // The global filter-panel sort control SH-128 built is gone outright --
    // SH-305 replaced it, rather than adding the per-column menu alongside
    // it.
    assert!(
        !body.contains(r#"id="board-sort""#),
        "the board-wide sort control was replaced by a per-column menu"
    );
    assert!(!body.contains(r#"id="boardsort-priority""#));
    assert!(!body.contains(r#"id="boardsort-order""#));

    // SH-203: the status-light component and its consumers -- storyLight()
    // (the dot alone), storyRef() (light + the existing .rel-id id
    // button), and linkifyStoryIds() (splitting free text around bare
    // mentions), adopted by the drawer's relationships/referenced-by
    // sections, comment bodies, and the blocked banner's awaiting reason.
    assert!(body.contains("function storyLight"));
    assert!(body.contains("function storyRef"));
    assert!(body.contains("function linkifyStoryIds"));
    assert!(body.contains(".story-ref"));
    assert!(body.contains(".story-light"));
    assert!(body.contains(r#"role: "img""#));
    assert!(body.contains(r#""Status: " + slug"#));
    // stateColor() re-colours by meaning, not board position (SH-203): the
    // four REQUIRED_STATES anchors, checked in this order so a renamed or
    // reordered catalog still reads right.
    assert!(body.contains("function stateColor"));
    assert!(body.contains(r#"slug === "blocked""#));
    assert!(body.contains(r#"def.super_state === "CLOSED""#));
    assert!(body.contains(r#"def.role === "active""#));
    assert!(body.contains(r#"slug === "todo""#));

    // SH-203 consumer 2: the card blockers list and its cleared-blocker
    // dwell -- openBlockers() mirrors the server's is_ready rule client-
    // side; the ledger functions keep a cleared entry visible, lit green,
    // for a beat after openBlockers() itself would already drop it.
    assert!(body.contains("function openBlockers"));
    assert!(body.contains(".card-blockers"));
    assert!(body.contains("function recordClearedBlockers"));
    assert!(body.contains("function dwellingBlockerIds"));
    assert!(body.contains("var clearedBlockers"));
    assert!(body.contains(".blocker-cleared"));
    assert!(body.contains("BLOCKER_CLEARED_DWELL_MS"));

    // SH-309: blockedFlag() is the one place that decides what the blocked
    // badge says, so a hand-written sentence can't assert a cause it never
    // tested (see `every_blocked_badge_sentence_comes_from_the_one_deriver`
    // for the fence this only names the existence of).
    assert!(body.contains("function blockedFlag"));
    assert!(body.contains("blockedFlag(st, !!blocked[st.id])"));

    // SH-277: the list view's own `.state-pill` -- unlike every other
    // renderer in the file (the board's column placement, the drag-drop
    // no-op guard, storyLight()), it read the literal `st.state` until
    // this landed. buildStatePill() reads display_state || state instead,
    // same as the rest, and names both states in a title whenever they
    // disagree; sortValue()'s own "state" case is kept in step with it so
    // sorting the column sorts by the word it shows.
    assert!(body.contains("function buildStatePill"));
    assert!(body.contains("var slug = v.display_state || st.state;"));
    assert!(body.contains(r#"class: "state-pill""#));
    assert!(body.contains("recorded state is "));
    assert!(body.contains(r#"case "state": return v.display_state || st.state;"#));

    // SH-217: the markdown renderer -- builds DOM nodes directly (never an
    // HTML string, see the sink-pin assertions above), and its link
    // scheme allowlist by name so a future edit that widens it is a
    // visible diff rather than a silent one.
    assert!(body.contains("function renderMarkdown"));
    assert!(body.contains("function appendBlocks"));
    assert!(body.contains("function appendInline"));
    assert!(body.contains("function appendList"));
    assert!(body.contains("function safeHref"));
    assert!(body.contains("MARKDOWN_LINK_SCHEMES"));
    assert!(body.contains(r#"rel: "noopener noreferrer""#));
    assert!(body.contains(r#"target: "_blank""#));
    assert!(body.contains(".description-view"));
    assert!(body.contains(".description-section"));
}

/// The full opening tag containing the byte at `at`, e.g. `<div class="x" id="y">`.
///
/// Walks out from the match to the enclosing `<` and `>` rather than
/// re-finding the element, so a caller can locate a tag by any attribute it
/// carries and then read the rest of them.
fn enclosing_tag(body: &str, at: usize) -> &str {
    let open = body[..at]
        .rfind('<')
        .expect("an attribute sits inside a tag");
    let close = open + body[open..].find('>').expect("the tag closes");
    &body[open..=close]
}

/// The value of `tag`'s `name` attribute, if it carries one.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!(" {name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = start + tag[start..].find('"')?;
    Some(&tag[start..end])
}

/// Every backdrop-based overlay is wired into the focus trap (SH-299).
///
/// `.backdrop` is `position: fixed; inset: 0`, which makes an overlay modal
/// for the mouse and for nothing else: before SH-299 nothing marked the
/// background `inert`, so the board, the topbar and the settings table behind
/// an open drawer kept their place in the tab order and stayed activatable
/// with Enter. Half a modal is worse than none, because the half that works
/// is the half a sighted mouse user tests with.
///
/// Derived over the served markup rather than from a list kept here, the same
/// style `tests/dead_public_surface.rs` and `tests/store_isolation.rs` use for
/// the same reason: an eighth overlay is exactly the thing most likely to be
/// half-wired, and a hand-maintained list goes stale precisely then. Each
/// backdrop must name the surface it dims, that surface must exist, must be
/// focusable as a fallback target (`activateOverlay()` focuses the container
/// when a surface has no obvious first control), and must be handed to both
/// halves of the machinery. Behaviour is proved in a real browser by
/// `e2e/specs/overlay-modality.spec.ts`; this fences the wiring, which that
/// suite can only check for the overlays it can reach.
#[test]
fn every_backdrop_overlay_is_wired_into_the_focus_trap() {
    let fixture = served();

    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();

    let backdrops: Vec<&str> = body
        .match_indices(r#"class="backdrop""#)
        .map(|(at, _)| enclosing_tag(&body, at))
        .collect();
    assert!(
        backdrops.len() >= 7,
        "expected the dashboard's backdrop overlays, found {}: {backdrops:?}",
        backdrops.len()
    );

    for backdrop in &backdrops {
        let surface = attribute(backdrop, "data-overlay").unwrap_or_else(|| {
            panic!(
                "this backdrop names no surface in `data-overlay`, so nothing goes inert while \
                 it is up and it is modal for the mouse only (SH-299): {backdrop}"
            )
        });

        let id_at = body
            .find(&format!(r#"id="{surface}""#))
            .unwrap_or_else(|| panic!("`{backdrop}` dims `{surface}`, which is not in the markup"));
        let tag = enclosing_tag(&body, id_at);
        assert_eq!(
            attribute(tag, "tabindex"),
            Some("-1"),
            "`{surface}` needs `tabindex=\"-1\"` so `activateOverlay()` can put focus on the \
             surface itself when it has no first control to focus: {tag}"
        );

        for half in ["activateOverlay", "releaseOverlay"] {
            assert!(
                body.contains(&format!("{half}(\"{surface}\"")),
                "nothing calls {half}(\"{surface}\"), so opening or closing that overlay leaves \
                 the background in whatever modality the previous one left behind (SH-299)"
            );
        }
    }

    // The app shell is what every one of those backdrops dims. The marker
    // lives on the element rather than in a list inside the script, so
    // `applyOverlayModality()` needs no names of its own.
    let app_at = body
        .find(r#"id="app""#)
        .expect("the app shell carries an id");
    assert_eq!(
        attribute(enclosing_tag(&body, app_at), "data-modal"),
        Some("covered"),
        "the app shell must be marked `data-modal=\"covered\"` or no overlay covers anything"
    );
}

/// A card's presentational body is transparent to pointer events, so a click
/// anywhere on it always lands on `.card` itself (SH-397).
///
/// `.card`'s own node survives an ordinary board re-render
/// (`reconcileColumnCards` reuses it, keyed by `data-id`), but
/// `populateCard()` clears and rebuilds every *child* on every render,
/// unconditionally. The click listener is bound once on `.card`, so a
/// re-render landing between a real mousedown and mouseup can destroy the
/// child the pointer is actually over -- and per the UI Events
/// click-dispatch algorithm, a `mousedown` target disconnected before
/// `mouseup` means no `click` fires anywhere at all, not even at an
/// ancestor. `.card * { pointer-events: none }` makes this structurally
/// impossible rather than timing-dependent: whatever the pointer is over
/// inside a card, the hit test resolves to `.card`, which nothing destroys
/// on an ordinary render. `e2e/specs/drawer-open-race.spec.ts` proves the
/// behaviour in a real browser, deterministically, by forcing the exact
/// race through the daemon's own `/data` reply; this fences the CSS rule
/// that closes it so a future edit to this block cannot narrow or drop it
/// silently.
///
/// The two descendants punched back through with `pointer-events: auto`
/// need genuine per-element interactivity for what THEY do, not what the
/// card does: `.card-actions-btn` (opens this card's own menu) and `.rel-id`
/// (a reference to a DIFFERENT story, reachable from a blocked-by badge or a
/// cleared-blocker chip -- `storyRef()`'s own `stopPropagation()` depends on
/// the click reaching it). Both remain individually exposed to the identical
/// hazard this rule closes for the card body -- named in SH-397's own
/// closing comment, not fenced here, since neither can take
/// `pointer-events: none` and closing it needs a different shape of fix.
#[test]
fn every_presentational_descendant_of_a_card_is_transparent_to_pointer_events() {
    let html = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/web_dashboard.html"),
    )
    .expect("reading src/web_dashboard.html");

    const OPEN: &str = "<style>";
    const CLOSE: &str = "</style>";
    let opens = html.matches(OPEN).count();
    let closes = html.matches(CLOSE).count();
    assert_eq!(
        (opens, closes),
        (1, 1),
        "expected exactly one <style> block in src/web_dashboard.html, found \
         {opens} open tag(s) and {closes} close tag(s) -- this scan reads only \
         the first, and a second block would leave rules unseen"
    );
    let start = html.find(OPEN).unwrap() + OPEN.len();
    let end = html.find(CLOSE).unwrap();
    let style = &html[start..end];

    assert!(
        style.contains(".card * { pointer-events: none; }")
            || style.contains(".card *{pointer-events:none;}")
            || style.contains(".card * {\n  pointer-events: none;\n}"),
        "expected a rule making every descendant of `.card` transparent to \
         pointer events (SH-397) -- without it, a click can land on a child \
         `populateCard()` is free to destroy between mousedown and mouseup, \
         and WebKit's (and Chromium's) own click-dispatch algorithm then \
         fires no `click` event at all. Exact text expected: \
         `.card * {{ pointer-events: none; }}` (whitespace-flexible)."
    );

    for selector in [".card-actions-btn", ".rel-id"] {
        assert!(
            style.contains(&format!(".card .{}", &selector[1..])),
            "expected `.card {selector}` to be punched back through with \
             `pointer-events: auto` -- without it, the blanket `.card * {{ \
             pointer-events: none }}` rule above makes {selector} unclickable \
             wherever it appears inside a card (SH-397)"
        );
    }
}

/// Every backdrop is shown and hidden through the shared pair, never by hand
/// (SH-302).
///
/// A close fades its backdrop out and hides it on a timer, so the fade has
/// something to fade. That timer belongs to the close that scheduled it, and
/// a reopen inside its window undoes that close -- so the write it is still
/// going to perform lands on a surface that is now open, leaving
/// `<div class="backdrop open" hidden>`: invisible, unclickable, and still
/// the thing every click on the page hits.
///
/// Seven overlays stated that lifecycle by hand and five of them never
/// cancelled anything. The two that tried were the interesting ones: the
/// drafts popover re-read `.open` when the timer fired, which is the right
/// question asked of a signal that arrives a frame late (SH-284, and SH-302
/// is the window that survived it), and the drawer read a variable set
/// synchronously, which worked and was still a second way of saying what
/// `showBackdrop()` now says once.
///
/// So this asserts the *routing* rather than the repair: no site may write a
/// backdrop's `hidden` or touch its `classList` itself. Behaviour is proved
/// in a real browser by `e2e/specs/overlay-reopen-race.spec.ts`, which can
/// reach three of the seven; an eighth overlay added later, or a sixth
/// hand-rolled hide, fails here instead of intermittently in someone else's
/// spec on a loaded machine.
#[test]
fn every_backdrop_is_shown_and_hidden_through_the_helpers() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();

    let ids: Vec<&str> = body
        .match_indices(r#"class="backdrop""#)
        .map(|(at, _)| enclosing_tag(&body, at))
        .map(|tag| {
            attribute(tag, "id").unwrap_or_else(|| {
                panic!(
                    "this backdrop carries no id, so nothing can name it to \
                     `showBackdrop`/`hideBackdrop`: {tag}"
                )
            })
        })
        .collect();
    assert!(
        ids.len() >= 7,
        "expected the dashboard's backdrop overlays, found {}: {ids:?}",
        ids.len()
    );

    for id in &ids {
        for half in ["showBackdrop", "hideBackdrop"] {
            assert!(
                body.contains(&format!("{half}(\"{id}\"")),
                "nothing calls {half}(\"{id}\"). A backdrop opened or closed outside the shared \
                 pair keeps no cancellable timer, so reopening its overlay inside the fade leaves \
                 `class=\"backdrop open\" hidden` -- covering the page with something the user \
                 cannot see or click (SH-302)"
            );
        }

        // The pair reaches the element through its own parameter, so these
        // literals can only come from a site doing it by hand. Registering a
        // listener on `$("<id>")` is untouched: this forbids the two writes
        // that make up the lifecycle, not every mention of the element.
        for forbidden in [
            format!("$(\"{id}\").hidden"),
            format!("$(\"{id}\").classList"),
        ] {
            assert!(
                !body.contains(&forbidden),
                "`{forbidden}` writes a backdrop's own visibility outside `showBackdrop`/\
                 `hideBackdrop`, which is where the cancellation lives (SH-302)"
            );
        }
    }
}

/// The Drafts popover names its project, and does it in a box that can hold
/// an unbounded name (SH-292).
///
/// Two halves, both cheap and both load-bearing, and neither observable from
/// the browser suite without a project whose name is long enough to prove it:
///
/// 1. **The slot exists, inside the popover, hidden by default.** Hidden
///    because `currentProjectLabel()` answers `null` off the board screen and
///    an empty subject line would otherwise reserve its own padding above the
///    list.
/// 2. **It elides.** `.modal` is `min(30rem, 92vw)` and a project name has no
///    length bound anywhere in `src/domain/` — so the one-line ellipsis recipe
///    `.projsel-label` and `.drafts-row-title` already use is the whole reason
///    this line can be given user input at all. Without `nowrap` a long name
///    silently wraps to two or three lines and reads as a title;
///    `e2e/specs/responsive.mobile.spec.ts` proves the rendered consequence at
///    four widths, and this fails in seconds if the recipe is unpicked.
///
/// The header is asserted to stay the control's own name. Folding the project
/// into it is the shape SH-292's council rejected: `.modal-header` is
/// `1rem/700` with no truncation and is shared by six modals, so one surface's
/// data would impose a truncation rule on five that never asked for one.
#[test]
fn the_drafts_popover_names_its_project_in_a_box_that_can_hold_one() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();

    let modal_at = body
        .find(r#"<div class="modal" id="drafts-modal""#)
        .expect("the dashboard has a drafts popover");
    let modal_end = modal_at
        + body[modal_at..]
            .find("</div>\n\n")
            .expect("the drafts popover's markup ends");
    let modal = &body[modal_at..modal_end];

    assert!(
        modal.contains(r#"<div class="modal-header">Drafts</div>"#),
        "the Drafts popover's header must stay the control's own name. Folding the project \
         into it puts unbounded user input in a `1rem/700` box with no truncation, shared by \
         six modals (SH-292)"
    );
    let subject_at = modal.find(r#"id="drafts-subject""#).unwrap_or_else(|| {
        panic!(
            "the Drafts popover names no project. Its own copy says \"this project's drafts\" \
             and \"No drafts in this project.\" while `.backdrop` blurs the topbar and SH-299's \
             `inert` removes it from the accessibility tree, so nothing inside the overlay \
             resolves either sentence (SH-292): {modal}"
        )
    });
    let subject = enclosing_tag(modal, subject_at);
    assert_eq!(
        attribute(subject, "class"),
        Some("drafts-subject"),
        "the subject line's class is deliberately named for this surface rather than for the \
         modal family: a `.modal-subject` reads as a house pattern the next contributor is \
         invited to spread to `#create-modal` and `#archive-modal`, neither of which claims a \
         project scope it cannot resolve (SH-292)"
    );
    assert!(
        subject.contains(" hidden"),
        "the subject line starts hidden: `currentProjectLabel()` answers null off the board \
         screen, and an empty line would still reserve its own padding above the list -- {subject}"
    );

    let css = stylesheet(&body);
    let rule = declarations(css, ".drafts-subject");
    for (property, value) in [
        ("overflow", "hidden"),
        ("text-overflow", "ellipsis"),
        ("white-space", "nowrap"),
    ] {
        assert!(
            rule.contains(&format!("{property}: {value}")),
            "`.drafts-subject` must carry `{property}: {value}` -- a project name has no length \
             bound, and without the full one-line ellipsis recipe a long one wraps into a title \
             or clips mid-glyph inside a `min(30rem, 92vw)` box (SH-292). Found: {rule}"
        );
    }
}

/// A catalog refresh repaints the Drafts popover's project name, not just the
/// topbar's (SH-292).
///
/// `fetchReposOnce()`'s success path is the only thing that reassigns
/// `state.repos`, and the subject line is derived from it. It repainted the
/// project selector, the home cards and the settings table and nothing
/// belonging to this popover — so a project renamed by another client left the
/// popover naming the old one, and left it there: the board's `/data` is a
/// separate request, `renderAll()` runs only on a parsed 200, and
/// `markDataSettled()` fires at most once per project. A stale name is worse
/// than no name, and this is the dwell state the naming exists for.
///
/// `updateDraftsButton()` is the right call and not a fourth `render*`: it
/// owns this surface (SH-284) and early-returns unless the popover is open.
/// Behaviour is proved in a real browser by `board-readiness.spec.ts`'s
/// renamed-elsewhere test; this is the cheap layer that fails without one.
#[test]
fn a_catalog_refresh_repaints_the_drafts_popover_not_only_the_topbar() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();

    let guard = "if (state.reposFetchOk) {";
    let at = body
        .find(guard)
        .expect("`fetchReposOnce()` repaints behind a `state.reposFetchOk` guard");
    let block_end = at + body[at..].find("\n      }").expect("that block closes");
    let block = &body[at..block_end];

    assert!(
        block.contains("updateDraftsButton()"),
        "nothing repaints the Drafts popover when the catalog changes, so a project renamed by \
         another client leaves the popover naming the old one indefinitely -- the board's own \
         `/data` cannot correct it, and this is exactly the dwell state the name was added for \
         (SH-292). Found: {block}"
    );
}

/// The dashboard's `<script>` block, so a text-literal assertion below
/// cannot accidentally match the same words inside the markup that precedes
/// it -- the topbar's own bootstrap placeholders (`#projsel-label`,
/// `#subtitle`) read "Loading…" in the HTML itself, replaced the instant the
/// script runs, and are not part of the readiness logic this scopes to.
fn script(body: &str) -> &str {
    let start = body
        .find("<script>")
        .expect("dashboard has a <script> block")
        + "<script>".len();
    let end = body[start..]
        .find("</script>")
        .expect("the <script> block closes");
    &body[start..start + end]
}

/// Every "Loading…" and "Couldn't load…" sentence in the dashboard's script
/// comes from `readinessNote()`, never a hand-written literal (SH-301,
/// SH-291).
///
/// A readiness sentence written by hand cannot tell "not yet" from "not at
/// all" -- that distinction is the entire point of `readinessNote()`'s
/// `failed` parameter, and it is only kept honest for every fetch at once by
/// there being exactly one place that spells the words. `renderHome()`'s own
/// permanent "No projects yet." (SH-301's defect) and `renderStatuses()`'s
/// own permanent "Loading…" (SH-291's, one fetch over) were both a literal
/// string sitting where a settled check belonged; this fails the same way
/// either one would return.
///
/// Comment lines (trimmed to start with `*` or `//`) are exempt -- several
/// doc comments *name* "Loading…" while explaining the function that used to
/// hard-code it, and are not a second source of the sentence.
#[test]
fn every_loading_line_comes_from_the_one_generator() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let script = script(&body);

    let fn_start = script
        .find("function readinessNote(subject, failed) {")
        .expect("readinessNote(subject, failed) must exist with this exact signature");
    let close = "\n  }\n";
    let fn_end = fn_start
        + script[fn_start..]
            .find(close)
            .expect("readinessNote's closing brace")
        + close.len();

    for needle in ["Loading", "Couldn't load"] {
        for (at, _) in script.match_indices(needle) {
            let line_start = script[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let line = script[line_start..].lines().next().unwrap_or("");
            if line.trim_start().starts_with('*') || line.trim_start().starts_with("//") {
                continue;
            }
            assert!(
                at >= fn_start && at < fn_end,
                "a bare {needle:?} literal outside readinessNote() at script byte {at}: {line:?} \
                 -- every readiness sentence must be generated, not hand-written, so \"not yet\" \
                 and \"not at all\" cannot drift apart on one fetch while staying fixed on another"
            );
        }
    }
}

/// Every blocked-badge sentence in the dashboard's script comes from
/// `blockedFlag()`, never a hand-written literal (SH-309).
///
/// The badge used to test only `st.awaiting`, so a story blocked by an open
/// `blocked-by` relationship or an `obviated-by` edge got the same
/// "(no reason)" label as one genuinely parked with no reason at all --
/// `blockedFlag()` exists to test every cause `is_ready` (`src/domain.rs`)
/// tests, in one place, so a future branch cannot claim "(no reason)" (or
/// even the bare "● blocked" fallback) without having tested for a cause
/// first. This pins that "one place" the same way
/// `every_loading_line_comes_from_the_one_generator` pins `readinessNote()`:
/// find the function's own bounds, then insist every occurrence of the
/// literals it owns falls inside them.
///
/// Comment lines (trimmed to start with `*` or `//`) are exempt -- this
/// function's own doc comment, and `openBlockers()`'s, both *name* these
/// sentences while explaining the badge, and are not a second source of
/// them.
#[test]
fn every_blocked_badge_sentence_comes_from_the_one_deriver() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let script = script(&body);

    let fn_start = script
        .find("function blockedFlag(st, isBlocked) {")
        .expect("blockedFlag(st, isBlocked) must exist with this exact signature");
    let close = "\n  }\n";
    let fn_end = fn_start
        + script[fn_start..]
            .find(close)
            .expect("blockedFlag's closing brace")
        + close.len();

    for needle in ["(no reason)", "● blocked"] {
        for (at, _) in script.match_indices(needle) {
            let line_start = script[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
            let line = script[line_start..].lines().next().unwrap_or("");
            if line.trim_start().starts_with('*') || line.trim_start().starts_with("//") {
                continue;
            }
            assert!(
                at >= fn_start && at < fn_end,
                "a bare {needle:?} literal outside blockedFlag() at script byte {at}: {line:?} \
                 -- every blocked-badge sentence must be derived from the cause list, not \
                 hand-written, so a rendered label cannot assert a cause it never tested"
            );
        }
    }

    // The badge's own blocker/obviator refs must be the shared component,
    // not a bespoke element -- keeps a blocking story's chip looking and
    // behaving exactly like every other story reference (SH-203).
    // `refList()` is blockedFlag()'s own private helper for this and sits
    // directly above it, so the scope runs from there through
    // blockedFlag()'s own close rather than blockedFlag()'s bounds alone.
    let refs_start = script
        .find("function refList(ids) {")
        .expect("refList(ids), blockedFlag()'s own ref-rendering helper, must exist");
    assert!(
        refs_start < fn_start,
        "refList() must be defined ahead of blockedFlag(), the one place that calls it"
    );
    assert!(
        script[refs_start..fn_end].contains("storyRef("),
        "blockedFlag()/refList() must build blocker/obviator references with storyRef(), \
         the shared status-light component, not a bespoke element"
    );
}

/// The dashboard's `<style>` block, so a selector assertion below cannot
/// accidentally match the same text in the markup that follows it --
/// `.search-input` names both a CSS rule and a `class="search-input"`
/// attribute.
fn stylesheet(body: &str) -> &str {
    let start = body.find("<style>").expect("dashboard has a <style> block") + "<style>".len();
    let end = body[start..]
        .find("</style>")
        .expect("the <style> block closes");
    &body[start..start + end]
}

/// The full opening tag of the first element in `html` bearing `id="target_id"`
/// -- from its own `<` back to its own `>`. Scoped to just that tag rather than
/// a substring search over the whole document, so a check against it cannot
/// accidentally match an HTML comment mentioning the same attribute in prose
/// (this file's markup comments quote `role="status"` and `aria-atomic="false"`
/// verbatim while explaining them) or a sibling element's own attributes.
fn opening_tag_for_id(html: &str, target_id: &str) -> String {
    let needle = format!("id=\"{target_id}\"");
    let at = html
        .find(&needle)
        .unwrap_or_else(|| panic!("no element with id=\"{target_id}\" found in the served body"));
    let tag_start = html[..at]
        .rfind('<')
        .unwrap_or_else(|| panic!("id=\"{target_id}\" does not sit inside a tag"));
    let tag_end = html[at..]
        .find('>')
        .unwrap_or_else(|| panic!("the tag carrying id=\"{target_id}\" never closes"))
        + at;
    html[tag_start..=tag_end].to_string()
}

/// Every `/* … */` span replaced by a single space, so a comment can neither
/// join two selectors nor hide inside a declaration block. Unterminated
/// comment: everything from the opener on is dropped, which is what a browser
/// does with one too.
fn strip_css_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(open) = rest.find("/*") {
        out.push_str(&rest[..open]);
        out.push(' ');
        match rest[open + 2..].find("*/") {
            Some(close) => rest = &rest[open + 2 + close + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// The declaration text of the rule that styles `selector` -- what's between
/// its opening brace and the next `}`. Panics naming the selector when no
/// such rule exists, so a rule that was renamed rather than deleted fails by
/// name instead of as a silent `false`.
///
/// A rule written for `selector` **alone** is the rule for it. Failing that,
/// every rule listing it as a *member* of a selector list answers, their
/// declarations concatenated: two selectors that must carry identical
/// declarations belong in one grouped rule rather than two copies free to
/// drift, and the contract each is asserted against here is a property of the
/// selector rather than of how the stylesheet chose to group it. The
/// alone-first order matters -- `.projsel-item` carries `touch-action` in a
/// list of ten and its own sizing in a rule of its own, and it is the second
/// that answers "how big is this control". Members are compared exactly after
/// trimming, so `.toast-dismiss` never matches `.toast-dismiss:hover`.
fn declarations(css: &str, selector: &str) -> String {
    // Comments come out first, and they are not cosmetic to this parser: a
    // rule's selector list is read as "the text since the previous rule
    // closed", and this stylesheet documents nearly every rule with a comment
    // sitting in exactly that gap. One containing a comma (`/* Stacked
    // arrows, the only way to reorder without a pointer ... */`, above
    // `.status-reorder button`) would otherwise split into members that match
    // nothing.
    let css = &strip_css_comments(css);
    let mut exact = None;
    let mut grouped = String::new();
    let mut cursor = 0;

    while let Some(offset) = css[cursor..].find('{') {
        let brace = cursor + offset;
        // The selector list is whatever follows the previous rule's close (or
        // the start of a block) -- `}`, `{` and `;` all terminate one, the
        // last so an at-rule's prelude cannot be read as a selector.
        let list_start = css[..brace]
            .rfind(['}', '{', ';'])
            .map_or(0, |index| index + 1);
        let list = css[list_start..brace].trim();
        let end = css[brace + 1..].find('}').expect("every CSS rule closes") + brace + 1;
        let body = &css[brace + 1..end];

        if list == selector {
            exact.get_or_insert(body);
        } else if list.split(',').any(|member| member.trim() == selector) {
            grouped.push_str(body);
        }
        cursor = brace + 1;
    }

    if let Some(body) = exact {
        return body.to_string();
    }
    assert!(
        !grouped.is_empty(),
        "no `{selector}` rule in the dashboard's stylesheet"
    );
    grouped
}

/// SH-256: on a coarse pointer, no text-entry control may compute under 16
/// CSS pixels -- the size below which iOS Safari zooms the viewport to the
/// field being focused, and does not zoom back out when it blurs.
///
/// The behavior itself is measured in a real coarse-pointer browser by
/// `e2e/specs/zoom.mobile.spec.ts`, across every surface. This is the cheap
/// layer: it fails in seconds, without a browser, if the mechanism is
/// deleted or unpicked -- a rule reverted to a literal, or the
/// coarse-pointer override dropped or detuned.
#[test]
fn web_serve_root_html_keeps_text_controls_above_the_ios_zoom_threshold() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let css = stylesheet(&body);

    // The viewport stays scalable: this is an exact match on the whole tag,
    // so it already rules out `user-scalable=no` / `maximum-scale` sneaking
    // in as an easier-looking fix. iOS has ignored both since iOS 10, and
    // they take pinch-zoom from the readers who need it most -- the fix
    // belongs in the font sizes, not the viewport.
    assert!(
        body.contains(r#"<meta name="viewport" content="width=device-width, initial-scale=1">"#)
    );

    // Every text-entry rule reads a token; none carries its own literal
    // size, which is what makes the coarse-pointer override below total.
    for selector in [
        ".search-input",
        ".settings-form input",
        ".status-row select, .status-row input[type=text]",
        ".status-add input[type=text], .status-add select",
        ".drawer-title",
        ".field select, .field input[type=text], .field textarea",
        ".inline-add input, .inline-add select",
        ".comment-add textarea",
        ".description-field",
        ".confirm-delete input",
        ".modal-body input[type=text], .modal-body select",
        ".modal-body textarea",
    ] {
        assert!(
            declarations(css, selector).contains("font-size: var(--control-font-"),
            "`{selector}` must size itself from a --control-font-* token, so \
             the coarse-pointer override reaches it"
        );
    }

    // The override itself, and its exact values. Literal pixels: WebKit's
    // threshold is an absolute CSS pixel count, nothing in this sheet sets
    // `html { font-size }`, so `1rem` here would follow the reader's
    // browser default and could still land under 16.
    let block_start = css
        .find("@media (pointer: coarse) {")
        .expect("the coarse-pointer block is what raises the tokens");
    let block = &css[block_start..];
    let block = &block[..block.find("\n}").expect("the coarse-pointer block closes")];
    for decl in [
        "--control-font-xs: 16px;",
        "--control-font-sm: 16px;",
        "--control-font-md: 16px;",
        "--control-font-lg: 18px;",
    ] {
        assert!(
            block.contains(decl),
            "the coarse-pointer block must set {decl}"
        );
    }
    // No fifth token can sneak in here at a `rem` value and slip past the
    // four literal checks above.
    assert_eq!(
        block.matches("--control-font").count(),
        4,
        "the coarse-pointer block should raise exactly the four control-font tokens"
    );

    // Double-tap-to-zoom, on the tap targets only -- never on `body`, where
    // double-tapping to zoom the board's own text is a gesture the reader
    // is entitled to.
    let touch_action_selector = "button, select, .card, .repo-card, tbody tr, .ctxmenu-item, \
         .projsel-item, .fdd-option, .filter-toggle, .pref-toggle";
    assert!(
        declarations(css, touch_action_selector).contains("touch-action: manipulation"),
        "the dashboard's tap targets should carry touch-action: manipulation"
    );
    assert_eq!(
        css.matches("touch-action").count(),
        1,
        "exactly one touch-action rule, on the tap targets -- never on the page as a whole"
    );
}

/// SH-235: `100vh`/`90vh`/`60vh` are the *largest* possible iOS Safari
/// viewport -- with the URL bar showing, the true visible area is shorter,
/// so a bare `vh` value overflows the screen (the app shell) or mis-sizes a
/// modal/popover so its own footer sits behind the browser chrome. Every
/// affected rule must carry the `vh` value first (the fallback for a
/// browser that predates `dvh` -- CSS drops a declaration with an
/// unsupported unit entirely, so without the `vh` line first that browser
/// gets no height at all) and the matching `dvh` value second, so the later
/// declaration wins in every browser that understands it.
///
/// Headless Blink has no dynamic toolbar, so `dvh` and `vh` compute
/// identically there -- this is the layer that can guard the *mechanism*
/// (the fallback pair itself) without a real device or a toolbar to hide.
#[test]
fn web_serve_root_html_sizes_the_shell_to_the_dynamic_viewport() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let css = stylesheet(&body);

    for (selector, prop, vh_value) in [
        (".app", "height", "100"),
        (".modal", "max-height", "90"),
        (".drafts-list", "max-height", "60"),
    ] {
        let decl = declarations(css, selector);
        let vh_decl = format!("{prop}: {vh_value}vh;");
        let dvh_decl = format!("{prop}: {vh_value}dvh;");
        assert!(
            decl.contains(&vh_decl),
            "`{selector}` must keep its `{vh_decl}` fallback for browsers that predate `dvh`"
        );
        assert!(
            decl.contains(&dvh_decl),
            "`{selector}` must also set `{dvh_decl}`, so the dynamic (toolbar-aware) \
             viewport wins over the fallback in every browser that supports it"
        );
        // The `dvh` declaration must come after the `vh` one -- CSS applies
        // the last matching declaration, so a swapped order would silently
        // put the fallback back in charge in every browser.
        assert!(
            decl.find(&vh_decl).unwrap() < decl.find(&dvh_decl).unwrap(),
            "`{selector}`'s `{dvh_decl}` must be declared after `{vh_decl}`, not before"
        );
    }

    // iOS's automatic post-rotation text-inflation heuristic, disabled --
    // not the same thing as pinch-zoom or the OS's own accessibility text
    // size, both of which stay untouched (see the rule's own comment).
    let html_decl = declarations(css, "html");
    assert!(
        html_decl.contains("-webkit-text-size-adjust: 100%;"),
        "`html` must set -webkit-text-size-adjust: 100% for Safari's still-prefixed property"
    );
    assert!(
        html_decl.contains("text-size-adjust: 100%;"),
        "`html` must set the unprefixed text-size-adjust: 100% too"
    );
}

/// SH-235: a notice surface is `position: fixed; right: 1rem` -- a bare
/// `max-width` (22rem / 26rem) leaves no room for the matching 1rem the box
/// needs on its *left* too, so on a narrow enough viewport it runs off the
/// left edge. The box must cap itself at the viewport minus both margins.
///
/// SH-323 moved WHERE that cap lives without changing what it promises.
/// `.toast-stack` and `.dispatch-history` are no longer positioned at all:
/// both are children of `.notice-dock`, which is the single element that
/// touches the viewport edge, so it is the one that owes the arithmetic. The
/// stacks inside it are bounded by `100%` of whatever the dock resolved to,
/// which cannot exceed the viewport by construction -- one element doing this
/// sum rather than two is the same consolidation SH-323 applied to the height
/// bound, and for the same reason: two boxes independently deciding how much
/// room they may take is two numbers that can disagree.
///
/// The `26rem` ceiling is the dock's because the dispatch history is the wider
/// of the two surfaces; the toast stack keeps its own narrower `22rem` inside
/// it, which is the difference SH-235 chose deliberately and this test still
/// pins below.
///
/// This is the cheap, browser-free layer: it pins the source text of the
/// `min()`/`calc()` expression so the mechanism can't be quietly reverted
/// to a literal. Whether the browser actually *resolves* that expression
/// to the intended pixel value is what `responsive.mobile.spec.ts`'s own
/// test for this proves -- a text match here cannot catch a rounding or
/// nesting mistake inside the expression.
#[test]
fn web_serve_root_html_clamps_overlay_widths_to_the_viewport() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let css = stylesheet(&body);

    let dock = declarations(css, ".notice-dock");
    assert!(
        dock.contains("max-width: min(26rem, calc(100vw - 2rem));"),
        "`.notice-dock` must set `max-width: min(26rem, calc(100vw - 2rem));` -- \
         it is the element at the viewport edge now, so it owes the margin \
         arithmetic both notice surfaces used to do separately"
    );

    // Inside the dock, each stack is bounded by the dock rather than by the
    // viewport. `.toast-stack` keeps a narrower ceiling of its own; the history
    // panel takes the dock's full width, which is why only one of them names a
    // rem value here.
    let toast = declarations(css, ".toast-stack");
    assert!(
        toast.contains("max-width: min(22rem, 100%);"),
        "`.toast-stack` must stay narrower than the dock (22rem) while never \
         exceeding it (100%)"
    );
    let history = declarations(css, ".dispatch-history");
    assert!(
        history.contains("max-width: 100%;"),
        "`.dispatch-history` must be bounded by the dock it sits in"
    );

    // Neither stack may re-acquire a viewport-relative width: two elements
    // computing this independently is what SH-323 consolidated.
    for selector in [".toast-stack", ".dispatch-history"] {
        let decl = declarations(css, selector);
        assert!(
            !decl.contains("100vw"),
            "`{selector}` must not measure itself against the viewport -- \
             `.notice-dock` is the element that touches it"
        );
    }
}

/// SH-235, WCAG 2.2 SC 2.5.8 (Target Size, Minimum): every tap target in
/// the stylesheet reads `--tap-min` -- 24px everywhere (the floor SC 2.5.8
/// sets for mouse input too), 44px under a coarse pointer (Apple's and
/// Android's own guidance for a comfortable fingertip target). One token,
/// many readers -- the same shape as `--control-font-*` (SH-256) and for
/// the same reason: a literal value on any one of these selectors would be
/// a silent, undetectable regression the moment `--tap-min` itself changed.
///
/// `responsive.mobile.spec.ts`'s own sweep is the layer that proves the
/// browser actually *resolves* every one of these to 44px or more under a
/// real coarse pointer, across every surface that renders them -- this
/// test only pins the source text.
///
/// `select` is the one exception to "reads `min-height` from --tap-min"
/// below (SH-377): WebKit ignores `min-height` entirely on a
/// default-appearance `<select>`, so every select instead reads an
/// EXPLICIT `height` from the same token -- see that assertion's own
/// comment for why `min-height` alone cannot be trusted here.
#[test]
fn web_serve_root_html_meets_wcag_tap_target_size() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let css = stylesheet(&body);

    assert!(
        css.contains("--tap-min: 24px;"),
        ":root must set --tap-min: 24px, WCAG 2.2 SC 2.5.8's own floor"
    );
    let block_start = css
        .find("@media (pointer: coarse) {")
        .expect("the coarse-pointer block is what raises --tap-min");
    let coarse_block = &css[block_start..];
    let coarse_block = &coarse_block[..coarse_block
        .find("\n}")
        .expect("the coarse-pointer block closes")];
    assert!(
        coarse_block.contains("--tap-min: 44px;"),
        "the coarse-pointer block must raise --tap-min to 44px"
    );

    // Selectors whose only fix was a min-height floor -- their width was
    // already compliant (a full-width row, or a text label wide enough on
    // its own).
    for selector in [
        ".btn",
        ".projsel-btn",
        ".projsel-item",
        ".back-link",
        ".fdd-option",
        ".filter-toggle",
        ".filter-clear",
        ".column-archive-btn",
        ".section-toggle",
        ".field select, .field input[type=text], .field textarea",
        ".modal-body input[type=text], .modal-body select",
        ".ctxmenu-item",
        ".view-toggle button",
        ".fdd-btn",
        ".status-row select, .status-row input[type=text]",
        ".status-add input[type=text], .status-add select",
    ] {
        assert!(
            declarations(css, selector).contains("min-height: var(--tap-min)"),
            "`{selector}` must read min-height from --tap-min"
        );
    }

    // Selectors whose fix needed both axes -- each is a glyph-only icon
    // button (a chip's "x", a relation's delete button, a dismiss "x")
    // narrower than --tap-min on its own.
    //
    // `.rel-row button` was replaced by `.rel-id`/`.rel-remove` (SH-203):
    // storyRef()'s id button now also renders outside any `.rel-row`
    // (`.referenced-by-text`, `.comment-text`, `.card-blockers`), so its
    // tap-target floor had to move off a `.rel-row`-scoped selector too.
    for selector in [
        ".status-reorder button",
        ".label-chip button",
        ".rel-id",
        ".rel-remove",
        // SH-305: the per-column board sort button is glyph-only (a bare
        // "⇅"), narrower than --tap-min on its own the same way the
        // dismiss/remove buttons above are.
        ".column-sort-btn",
        ".dispatch-history-dismiss",
        // SH-304 gave the durable error toast a dismiss button of its own.
        // It shares one grouped rule with the history row's, which is why
        // `declarations` resolves a selector that is a *member* of a list --
        // the alternative was a second copy of the same six declarations,
        // free to drift on one surface and not the other.
        ".toast-dismiss",
    ] {
        let decl = declarations(css, selector);
        assert!(
            decl.contains("min-width: var(--tap-min)"),
            "`{selector}` must read min-width from --tap-min"
        );
        assert!(
            decl.contains("min-height: var(--tap-min)"),
            "`{selector}` must read min-height from --tap-min"
        );
    }

    // .field textarea and .description-field each had a taller rest height
    // than --tap-min's own 24px desktop floor before this token existed
    // (36px, 40px) -- max() keeps whichever floor is taller, rather than a
    // bare var(--tap-min) silently shrinking either on a fine pointer.
    for (selector, original) in [
        (".field textarea", "2.25rem"),
        (".description-field", "2.5rem"),
    ] {
        let expected = format!("min-height: max({original}, var(--tap-min))");
        assert!(
            declarations(css, selector).contains(&expected),
            "`{selector}` must set `{expected}`"
        );
    }

    // SH-377: WebKit ignores `min-height` entirely on a default-appearance
    // <select> -- every select rendered ~20-23px regardless of --tap-min's
    // own value, confirmed against this repo's own webkit-2336 build.
    // `height` is the one sizing property that engine does honour, so
    // `select` gets its floor from an EXPLICIT height instead of another
    // min-height rule a WebKit reader would silently drop. `2.5rem` is
    // `.description-field`'s own rest-height constant, chosen because it
    // clears every select's tallest measured natural render (37px, this
    // sheet's own `.modal-body select` on desktop Chromium) with margin --
    // `max()` still means this can only raise a select's height, never
    // compress it below its own unstyled size, the same guarantee the loop
    // above gives `.field textarea`/`.description-field`. A bare `select`
    // selector rather than a repeat of the seven grouped lists above so a
    // select added anywhere in this file is covered with no further edit.
    let select_rule = declarations(css, "select");
    assert!(
        select_rule.contains("height: max(2.5rem, var(--tap-min))"),
        "`select` must set `height: max(2.5rem, var(--tap-min))` -- min-height alone is \
         silently inert on WebKit's menulist <select> (SH-377)"
    );
    assert!(
        !select_rule.contains("min-height"),
        "`select`'s own rule must not (re)introduce `min-height` -- WebKit ignores it on this \
         element, so a min-height here would silently do nothing there (SH-377)"
    );
}

/// SH-305: the column header's Archive button (CLOSED-superstate columns
/// only) moved to sit between the title and the count/sort-button group,
/// via a second `margin-left: auto` splitting the header's free space with
/// `.column-count`'s own -- pinned so the centering can't be quietly
/// reverted to Archive being packed against the title again.
#[test]
fn web_serve_root_html_centers_the_column_archive_button() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let css = stylesheet(&body);

    assert!(
        declarations(css, ".column-archive-btn").contains("margin-left: auto"),
        "`.column-archive-btn` must carry its own margin-left: auto"
    );
    assert!(
        declarations(css, ".column-count").contains("margin-left: auto"),
        "`.column-count` must keep its margin-left: auto"
    );
}

/// SH-304: a notification is the one element that appears unbidden, so its
/// entrance and exit are exactly the motion a reader who asked for less of it
/// did not ask for -- and `.toast`/`.toast.leaving` sat OUTSIDE the
/// reduced-motion block for as long as it has existed, while `.card`'s
/// equivalents sat inside it.
///
/// Two halves, and the second is the one that keeps this honest: the
/// animations must be gated, and the *dismissal* must not be. A fix that
/// silenced the fade by never dismissing at all would satisfy the first
/// assertion and leave notices piling up forever under reduced motion.
/// `e2e/specs/notification-contract.spec.ts` proves the behaviour in a real
/// browser with `emulateMedia`; this is the cheap layer that fails in seconds
/// if the rules are moved back out.
#[test]
fn web_serve_root_html_gates_notification_motion_but_never_its_dismissal() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .expect("dashboard responds");
    let body = resp.into_body().read_to_string().unwrap();
    let css = stylesheet(&body);

    for selector in [".toast", ".dispatch-history-row"] {
        assert!(
            !declarations(css, selector).contains("animation:"),
            "`{selector}`'s ungated rule must not animate -- move the animation \
             inside the prefers-reduced-motion block"
        );
    }

    let motion_block_start = css
        .find("@media (prefers-reduced-motion: no-preference) {")
        .expect("the reduced-motion block exists");
    let motion_block = &css[motion_block_start..];
    for fragment in [".toast, .dispatch-history-row", ".toast.leaving"] {
        assert!(
            motion_block.contains(fragment),
            "`{fragment}` must be gated behind prefers-reduced-motion, like every \
             other decorative animation in this file"
        );
    }

    // The fade duration is a token the script reads (`readMsToken`), not a
    // number restated on both sides: two hand-kept copies let the node be
    // removed mid-fade the first time one of them moves.
    assert!(
        css.contains("--toast-fade:"),
        "the fade duration must be a custom property, so the script can read it"
    );
    assert!(
        body.contains("animation: toast-out var(--toast-fade)"),
        "the fade animation must be driven by --toast-fade rather than a literal"
    );
    assert!(
        body.contains("readMsToken(\"--toast-fade\""),
        "the script must READ --toast-fade rather than restate its value"
    );
}

/// SH-333 gave `#toast-stack` `aria-atomic="false"` (overriding
/// `role="status"`'s implicit `true`, WAI-ARIA 1.2) plus `aria-atomic="true"`
/// on each incrementally-inserted `.toast`, so a mutation announces exactly
/// the notice that arrived rather than the whole standing pile. It could not
/// give `#dispatch-history` the same treatment at the time:
/// `renderDispatchHistory()` cleared and rebuilt the panel wholesale on every
/// render, so no live-region attribute stopped it re-announcing every row --
/// the reason that surface was demoted out of `aria-live` entirely and given
/// a side-channel `sr-only` announcer instead, logged as deliberate tech
/// debt naming SH-337 as the trigger to retire it.
///
/// SH-337 ended the rebuild, so `#dispatch-history` now carries the identical
/// shape `#toast-stack` has: `aria-live="polite" aria-atomic="false""` on the
/// region, `aria-atomic="true""` on each row, inserted one at a time. The
/// side-channel announcer is gone.
///
/// `e2e/specs/notice-announcement.spec.ts` proves the announced *text* in a
/// real browser; this is the cheap layer that fails in seconds if the
/// attributes themselves are moved back, without needing a browser at all.
#[test]
fn web_serve_root_html_gives_both_notice_surfaces_the_same_atomic_live_region_shape() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .expect("dashboard responds");
    let body = resp.into_body().read_to_string().unwrap();

    let toast_stack = opening_tag_for_id(&body, "toast-stack");
    assert!(
        toast_stack.contains(r#"role="status""#) && toast_stack.contains(r#"aria-live="polite""#),
        "#toast-stack must keep role=\"status\" aria-live=\"polite\" -- SH-333 \
         narrowed what gets announced, not whether the region is live. Found: \
         {toast_stack}"
    );
    assert!(
        toast_stack.contains(r#"aria-atomic="false""#),
        "#toast-stack must override role=\"status\"'s implicit aria-atomic=\"true\", \
         or a mutation re-announces the whole standing pile again. Found: {toast_stack}"
    );

    let dispatch_history = opening_tag_for_id(&body, "dispatch-history");
    assert!(
        dispatch_history.contains(r#"role="region""#),
        "#dispatch-history must keep its role=\"region\" landmark. Found: {dispatch_history}"
    );
    assert!(
        dispatch_history.contains(r#"aria-live="polite""#),
        "#dispatch-history must be aria-live=\"polite\" (SH-337): the panel no \
         longer rebuilds wholesale, so a live region can safely announce just \
         the row that arrived. Found: {dispatch_history}"
    );
    assert!(
        dispatch_history.contains(r#"aria-atomic="false""#),
        "#dispatch-history must override role=\"region\"'s absence of implicit \
         atomicity explicitly, matching #toast-stack's shape rather than \
         leaving it to each browser's default. Found: {dispatch_history}"
    );

    let announcer = opening_tag_for_id(&body, "notice-dock-status");
    assert!(
        announcer.contains(r#"role="status""#),
        "#notice-dock-status must be role=\"status\" -- it is the element a \
         test (and an assistive technology) reads a dismissal's announced \
         text from. Found: {announcer}"
    );
    assert!(
        !body.contains(r#"id="dispatch-history-status""#),
        "#dispatch-history-status was SH-333's stopgap side channel, fed by \
         hand from addDispatchHistoryRow() because the panel rebuilt \
         wholesale and no live-region attribute could announce just the \
         arriving row. SH-337 ended the rebuild, so #dispatch-history \
         announces its own arrivals now (aria-live=\"polite\" \
         aria-atomic=\"false\" on the region, aria-atomic=\"true\" per row) \
         and the side channel is retired rather than left standing beside a \
         fix that no longer needs it."
    );

    // Both `.toast` and `.dispatch-history-row` set their own
    // aria-atomic="true" from JS (`el()`'s props in `toast()` and
    // `buildDispatchHistoryRow()`), not the static markup, so each is pinned
    // as source text scoped to its own function rather than a tag scan.
    let script = script(&body);
    for signature in [
        "function toast(message, variant, detail, reason) {",
        "function buildDispatchHistoryRow(entry) {",
    ] {
        let fn_start = script
            .find(signature)
            .unwrap_or_else(|| panic!("{signature} must exist with this exact signature"));
        let close = "\n  }\n";
        let fn_end = fn_start
            + script[fn_start..]
                .find(close)
                .unwrap_or_else(|| panic!("{signature}'s closing brace"))
            + close.len();
        let body_of_fn = &script[fn_start..fn_end];
        assert!(
            body_of_fn.contains(r#""aria-atomic": "true""#),
            "{signature} must give its own node aria-atomic=\"true\" -- \
             without it, the container's aria-atomic=\"false\" would narrow \
             every mutation to nothing rather than to the notice that just \
             arrived"
        );
    }
}

/// SH-203: the status light itself never carries the `unknown` colour --
/// stateColor() runs and sets an inline `background` instead -- so the
/// only place its own colour comes from CSS is the "id doesn't resolve"
/// case, `.story-light.unknown`, which must actually paint something
/// (a hollow ring) rather than default to invisible-on-white.
#[test]
fn web_serve_root_html_styles_the_status_light_and_card_blockers() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let css = stylesheet(&body);

    assert!(
        declarations(css, ".story-ref").contains("display: inline-flex"),
        ".story-ref must lay its light and id out inline, not stacked"
    );
    let unknown = declarations(css, ".story-light.unknown");
    assert!(
        unknown.contains("background: transparent"),
        ".story-light.unknown must not paint stateColor()'s inline background"
    );
    assert!(
        unknown.contains("border:"),
        ".story-light.unknown must render a ring -- an id that can't resolve \
         should read as \"unresolvable\", not silently blank"
    );
    assert!(
        declarations(css, ".card-blockers").contains("flex-wrap: wrap"),
        ".card-blockers must wrap rather than overflow a narrow card"
    );
    // SH-309: the blocked badge can now carry a whole sentence of `.rel-id`
    // refs, each pinned to `min-width: var(--tap-min)` (44px under
    // `pointer: coarse`, asserted above at the `.rel-id`/`.rel-remove`
    // sweep) -- three alone are 132px of irreducible width, and `.card`
    // clips nothing, so `.card-flags` must wrap or the badge runs off a
    // narrow card instead of merely looking tight.
    assert!(
        declarations(css, ".card-flags").contains("flex-wrap: wrap"),
        ".card-flags must wrap -- three tap-target-floored blocker refs plus the rest of the \
         badge's text overflow a 320px-viewport card, which carries no overflow clip of its own"
    );
    // The cleared-blocker pulse is reduced-motion-gated the same way every
    // other flash-* animation in this file is (see the block's own
    // comment) -- pinned by finding the rule inside that block, not just
    // anywhere in the stylesheet.
    let motion_block_start = css
        .find("@media (prefers-reduced-motion: no-preference) {")
        .expect("the reduced-motion block exists");
    let motion_block = &css[motion_block_start..];
    assert!(
        motion_block.contains(".blocker-cleared .story-light"),
        "the cleared-blocker pulse must be gated behind prefers-reduced-motion, \
         like every other card flash animation"
    );
}

/// SH-277: the list view's `.state-pill` is coloured through the same
/// `--state-color` custom property `.card`'s own `--card-accent` uses
/// (`buildStatePill()` is the one write that drives the tint, the ring,
/// and the dot's fill together) -- not the inline
/// `style: "background:" + stateColor(slug)` string every single-
/// declaration dot elsewhere in this file uses. The plain
/// `background`/`border-color` fallback must come FIRST, and the
/// `color-mix()` pair after it: that ordering is what makes the
/// declaration a real progressive-enhancement pair rather than an
/// assertion this test only pretends to make -- a browser without
/// `color-mix()` support keeps the first declaration of each property and
/// ignores the second, landing on today's uncoloured pill exactly.
#[test]
fn web_serve_root_html_colours_the_list_state_pill() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let css = stylesheet(&body);

    let pill = declarations(css, ".state-pill");
    assert!(
        pill.contains("display: inline-flex"),
        ".state-pill must lay its dot and word out inline, matching .story-ref"
    );
    assert!(
        pill.contains("--state-color: var(--border)"),
        ".state-pill must default --state-color so the rule is valid standalone"
    );
    let plain_bg = pill
        .find("background: var(--bg-sunken)")
        .expect(".state-pill must keep the plain background as a color-mix() fallback");
    let mixed_bg = pill
        .find("background: color-mix(in srgb, var(--state-color)")
        .expect(".state-pill must mix --state-color into its background");
    assert!(
        plain_bg < mixed_bg,
        "the plain background must come BEFORE the color-mix() one, or a browser \
         without color-mix() support keeps the coloured declaration's fallback \
         value instead of today's uncoloured pill"
    );
    assert!(
        pill.contains("border-color: color-mix(in srgb, var(--state-color)"),
        ".state-pill's ring must also track --state-color"
    );
    assert!(
        declarations(css, ".state-pill .dot").contains("background: var(--state-color)"),
        ".state-pill .dot must paint the same --state-color the pill itself mixes in"
    );
}

/// SH-217: three CSS rules ARE the description's read/edit mechanism --
/// the field is `display: none` by default, shown only under `.editing`,
/// while the read view flips the opposite way. A selector rename here
/// would silently break the swap in every browser at once; this pins the
/// mechanism without needing one.
#[test]
fn web_serve_root_html_styles_the_description_read_edit_swap_and_rendered_markdown() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let css = stylesheet(&body);

    assert!(
        declarations(css, ".description-section .description-field").contains("display: none"),
        "the raw textarea must be hidden outside edit mode"
    );
    assert!(
        declarations(css, ".description-section.editing .description-field")
            .contains("display: block"),
        "the raw textarea must be shown in edit mode"
    );
    assert!(
        declarations(css, ".description-section.editing .description-view")
            .contains("display: none"),
        "the rendered view must be hidden in edit mode -- the two must never both show"
    );

    // The read view matches the textarea's own pinned floor (SH-235) and
    // reads a --control-font-* token like every other text-entry control,
    // so the read<->edit swap never resizes the box or the type.
    let view = declarations(css, ".description-view");
    assert!(
        view.contains("min-height: max(2.5rem, var(--tap-min))"),
        ".description-view must match .description-field's own height floor"
    );
    assert!(
        view.contains("font-size: var(--control-font-"),
        ".description-view must size its text from a --control-font-* token"
    );
    assert!(
        declarations(css, ".description-view:focus-visible").contains("outline"),
        "a keyboard user tabbing to the read view needs a visible focus ring"
    );

    // Rendered markdown (the description's read view, and comment bodies):
    // a code block scrolls rather than wrapping or widening the drawer
    // (the same lesson as docs/spec/responsive-dashboard.md's defect D2),
    // and a table is wrapped for the same reason.
    assert!(declarations(css, ".md pre").contains("overflow-x: auto"));
    assert!(declarations(css, ".md-table-wrap").contains("overflow-x: auto"));
    assert!(declarations(css, ".md code").contains("var(--mono)"));
    assert!(declarations(css, ".md a").contains("var(--accent)"));

    // .comment-text no longer forces every line break visible with
    // pre-wrap -- the markdown renderer's block structure supplies
    // paragraph spacing now; overflow-wrap survives unrelated to that.
    let comment_text = declarations(css, ".comment-text");
    assert!(
        !comment_text.contains("pre-wrap"),
        "SH-217: .comment-text's content is block-structured now, so \
         pre-wrap would double every blank line the renderer already \
         turned into real paragraph spacing"
    );
    assert!(comment_text.contains("overflow-wrap: anywhere"));
}

/// SH-235: the filter bar's dropdowns, checkboxes and sort buttons collapse
/// behind a "Filters" disclosure, at every viewport size -- the filter bar
/// alone measured 145px tall at a 390px width, on top of the topbar's own
/// 108px. `#filter-summary` (the toggle, `#filter-count`, `#filter-clear`)
/// stays outside `#filter-panel`'s `hidden` so a reader always sees whether
/// a filter is active and can clear it without opening anything.
///
/// The interaction itself (default collapsed, opens on click, ARIA/chevron
/// sync, survives a reload, the active-class heuristic) is
/// `filter-bar-disclosure.spec.ts`'s job, under the desktop project -- this
/// is not a mobile-only behavior, so it isn't gated behind
/// `mobile-chromium`. This is the cheap, browser-free layer: the markup
/// shape and the `closeAllPopovers` scoping fix the disclosure needed.
#[test]
fn web_serve_root_html_has_a_collapsible_filter_panel() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();

    assert!(body.contains(r#"id="filter-summary""#));
    assert!(body.contains(r#"id="filter-toggle-btn""#));
    assert!(body.contains(r#"aria-controls="filter-panel""#));
    // Collapsed by default in the served markup itself -- not just via a
    // JS-applied class, so a reader whose JS is still loading (or fails)
    // never sees the panel flash open before script runs.
    assert!(body.contains(r#"id="filter-panel" hidden>"#));
    // #filter-count and #filter-clear moved into the always-visible summary
    // row -- pinned by each still appearing before filter-panel's own
    // opening tag, i.e. outside it, not merely present somewhere in the file.
    let panel_start = body
        .find(r#"id="filter-panel""#)
        .expect("the filter panel exists");
    assert!(body[..panel_start].contains(r#"id="filter-count""#));
    assert!(body[..panel_start].contains(r#"id="filter-clear""#));

    // The generic aria-expanded reset in closeAllPopovers() must exclude
    // this disclosure (and the drawer's own SH-169 section toggles) -- see
    // that function's own comment for why an unscoped reset is a real,
    // silent ARIA-state bug for a persistent disclosure, not merely a
    // popover it's meant to dismiss.
    assert!(
        body.contains(r#"[aria-expanded="true"]:not(.section-toggle):not(.filter-toggle-btn)"#)
    );
}

/// SH-235 (D9): HTML5 native drag-and-drop (`card.draggable` + `dragstart`)
/// never fires on touch, so before this the only touch path to a card's
/// actions was an undocumented long-press. `.card-actions-btn` (board) and
/// `.row-actions-btn` (list) open the exact same menu right-click does
/// (`openStoryMenu`) -- coarse-pointer visible only, so desktop's rendering
/// is untouched.
///
/// `responsive.mobile.spec.ts`'s own tests are the layer that proves the
/// menu items actually match right-click's and that the coarse-pointer
/// sizing resolves correctly in a real browser; this is the cheap,
/// browser-free layer pinning the source text of the mechanism.
#[test]
fn web_serve_root_html_has_coarse_pointer_actions_buttons() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let css = stylesheet(&body);

    // The list table's static thead grows a trailing, visually-hidden-label
    // actions column -- present in the served markup itself, unlike the
    // board/list bodies which are built client-side.
    assert!(body.contains(r#"<th class="col-actions"><span class="sr-only">Actions</span></th>"#));

    // Both buttons reuse openStoryMenu (the same menu right-click opens),
    // not a second implementation that could drift out of step with it.
    assert!(body.contains("class: \"card-actions-btn\""));
    assert!(body.contains("openStoryMenu(e, v, cardActionsBtn)"));
    assert!(body.contains("class: \"row-actions-btn\""));
    assert!(body.contains("openStoryMenu(e, v, rowActionsBtn)"));

    // The card's button is deliberately not a Tab stop (role="button" on
    // .card makes any nested interactive element ARIA-presentational
    // regardless of markup -- Shift+F10/the Menu key stays the keyboard
    // path); the row's carries no tabIndex override, since a <tr> is not
    // role="button" and the button is a normal part of the a11y tree there.
    assert!(body.contains("type: \"button\", class: \"card-actions-btn\", tabIndex: -1,"));
    assert!(!body.contains("type: \"button\", class: \"row-actions-btn\", tabIndex"));

    // Hidden on a fine pointer (right-click already reaches this menu
    // there); coarse-pointer-only, both axes -- an icon-only button is
    // narrower than --tap-min once nothing else sets its width. Each
    // button gets its own dedicated `@media (pointer: coarse)` block right
    // beside its base rule (a third such block in the sheet, after the
    // `:root` tokens' and `.card-actions-btn`'s own) -- checked as one
    // literal snippet per button, the same way this file already pins
    // `--tap-min`'s and `--control-font-*`'s own coarse-pointer values.
    for (selector, coarse_block) in [
        (
            ".card-actions-btn",
            "@media (pointer: coarse) {\n  .card-actions-btn {\n    display: inline-flex; align-items: center; justify-content: center;\n    min-width: var(--tap-min); min-height: var(--tap-min);\n  }\n}",
        ),
        (
            ".row-actions-btn",
            "@media (pointer: coarse) {\n  .col-actions { display: table-cell; }\n  .row-actions-btn {\n    display: inline-flex; align-items: center; justify-content: center;\n    min-width: var(--tap-min); min-height: var(--tap-min);\n  }\n}",
        ),
    ] {
        assert!(
            declarations(css, selector).contains("display: none"),
            "`{selector}` must default to display: none on a fine pointer"
        );
        assert!(
            css.contains(coarse_block),
            "`{selector}` must be revealed and sized to --tap-min inside its own coarse-pointer block"
        );
    }

    // The list table's own overflow-x scroll must not let the browser's
    // mobile viewport-fit heuristic treat the table's un-clamped intrinsic
    // width as the page's real content width -- contain: layout is what
    // isolates the wrap's internal layout from that outer measurement.
    assert!(declarations(css, ".list-table-wrap").contains("contain: layout"));
}

/// SH-235 (D8): `.column`'s base rule (`flex: 0 0 18rem; max-width: 18rem`)
/// is nearly the entire screen on the narrowest supported phones -- 288px
/// of a 320px viewport leaves only a 32px (10%) sliver of the next column,
/// not enough to read as "there's more this way". The `<=768px` layout
/// block (not `pointer: coarse` -- this is a screen-width question, the
/// same distinction the coarse-pointer block's own comment draws the other
/// way) narrows the ceiling to `min(18rem, 85vw)`, which only bites below
/// ~339px (18rem / 0.85) -- `responsive.mobile.spec.ts`'s own pair of
/// tests is what proves the browser actually resolves that expression
/// correctly at 320px and leaves 375px+ alone.
#[test]
fn web_serve_root_html_lets_the_next_board_column_peek_on_narrow_phones() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let css = stylesheet(&body);

    assert!(
        declarations(css, ".column").contains("flex: 0 0 18rem")
            && declarations(css, ".column").contains("max-width: 18rem"),
        ".column's base rule must keep its full 18rem ceiling outside the narrow-phone override"
    );

    let block_start = css
        .find("@media (max-width: 768px) {")
        .expect("the <=768px layout block is what narrows .column on the smallest phones");
    let block = &css[block_start..];
    let block = &block[..block.find("\n}").expect("the <=768px block closes")];
    assert!(
        block.contains(".column { flex-basis: min(18rem, 85vw); max-width: min(18rem, 85vw); }"),
        "the <=768px block must narrow .column to min(18rem, 85vw)"
    );
}

/// SH-303: the `<=768px` block used to remove `.projsel-btn`'s only width
/// ceiling outright (`max-width: none`) -- exactly the widths that can least
/// afford it, since `.projsel-label`'s `text-overflow: ellipsis` (elsewhere
/// in this stylesheet) has nothing to elide against once the button is
/// unconstrained. A long project name then carried `.brand`, `.projsel` and
/// the whole document past the viewport's own width (measured:
/// `document.documentElement.scrollWidth` 799px against a 320px
/// `clientWidth`, with a 100-character name).
///
/// The fix keeps the button's own 14rem ceiling under the narrow-width block
/// too -- council-decided (verdict on SH-303) over a
/// wider mobile-only figure, because `.projsel-btn` shares topbar row 1 with
/// `.view-toggle`/`.topbar-right` under the same `<=768px` block, and that
/// row's own chrome-budget test (`responsive.mobile.spec.ts`) has too little
/// headroom to safely absorb a wider button with no test that would catch a
/// regression there -- plus a `calc(100vw - 2.5rem)` backstop, `2.5rem` being
/// `.topbar`'s own horizontal padding, for viewports narrower than anything
/// this file's own sweep tests. This is the cheap, browser-free layer: it
/// pins the source text so the mechanism can't be quietly reverted to
/// `none`. Whether the browser actually *resolves* it correctly is
/// `responsive.mobile.spec.ts`'s own job.
#[test]
fn web_serve_root_html_caps_the_project_selector_width_on_narrow_phones() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let css = stylesheet(&body);

    assert!(
        declarations(css, ".projsel-btn").contains("max-width: 14rem"),
        ".projsel-btn's base rule must keep its 14rem ceiling outside the narrow-phone override"
    );

    let block_start = css
        .find("@media (max-width: 768px) {")
        .expect("the <=768px layout block is what used to remove .projsel-btn's ceiling");
    let block = &css[block_start..];
    let block = &block[..block.find("\n}").expect("the <=768px block closes")];
    assert!(
        !block.contains(".projsel-btn { max-width: none; }"),
        "the <=768px block must not remove .projsel-btn's width ceiling -- that is SH-303"
    );
    assert!(
        block.contains(".projsel-btn { max-width: min(14rem, calc(100vw - 2.5rem)); }"),
        "the <=768px block must cap .projsel-btn at min(14rem, calc(100vw - 2.5rem))"
    );
}

#[test]
fn web_serve_api_data_empty_project() {
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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

    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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

/// SH-336: `head_global_seq` must reach the wire on `/data`, or the board's
/// same-second recency tiebreak has nothing to read. Two stories written
/// back to back get two distinct, increasing values — proving the field is
/// both present and actually derived from write order, not a stub.
#[test]
fn web_serve_api_data_carries_head_global_seq() {
    let fixture = served();
    fixture.seed(&["new", "First"]);
    fixture.seed(&["new", "Second"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();

    let stories = json["stories"].as_array().unwrap();
    assert_eq!(stories.len(), 2);
    let seqs: Vec<i64> = stories
        .iter()
        .map(|s| {
            s["head_global_seq"]
                .as_i64()
                .expect("head_global_seq must be present and numeric")
        })
        .collect();
    assert_ne!(
        seqs[0], seqs[1],
        "two distinct writes must carry two distinct write positions"
    );
}

/// The JS half of the same wire contract: `web_dashboard.html`'s
/// `compareWriteOrder` must read the key `StoryView::head_global_seq`
/// actually serializes to. Derived from a live round trip rather than a
/// hand-typed literal on both sides, so a Rust-side rename that left the JS
/// reading a key nobody emits fails this test instead of silently
/// resurrecting SH-336.
#[test]
fn web_dashboard_js_reads_the_wire_key_head_global_seq_actually_serializes_to() {
    let fixture = served();
    fixture.seed(&["new", "Only story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let story = &json["stories"].as_array().unwrap()[0];
    let key = story
        .as_object()
        .unwrap()
        .keys()
        .find(|k| k.as_str() == "head_global_seq")
        .expect("the wire payload must carry a `head_global_seq` key");

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let html = resp.into_body().read_to_string().unwrap();
    assert!(
        html.contains(&format!("a.{key}")),
        "compareWriteOrder must read `a.{key}` — the exact key the wire emits"
    );
}

/// SH-407: `next_ids` must reach the wire on `/data`, in the exact order
/// `story next` would hand this queue out in, or the board's "Next" column
/// sort has nothing to read.
#[test]
fn web_serve_api_data_carries_next_ids() {
    let fixture = served();
    fixture.seed(&["new", "First"]);
    fixture.seed(&["new", "Second"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();

    let next_ids: Vec<&str> = json["next_ids"]
        .as_array()
        .expect("next_ids must be present and an array")
        .iter()
        .map(|v| v.as_str().expect("each id is a string"))
        .collect();
    assert_eq!(
        next_ids,
        ["SH-1", "SH-2"],
        "both stories tie on priority, so the order must fall back to ready_order's story-number tiebreak"
    );
}

/// SH-407, mirroring `web_dashboard_js_reads_the_wire_key_head_global_seq_actually_serializes_to`
/// immediately above: `next_ids` is a top-level field, not per-story, so the
/// dashboard's `nextRank` must read `state.data.next_ids` literally rather
/// than a hand-typed guess on both sides.
#[test]
fn web_dashboard_js_reads_the_wire_key_next_ids_actually_serializes_to() {
    let fixture = served();
    fixture.seed(&["new", "Only story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    let key = json
        .as_object()
        .unwrap()
        .keys()
        .find(|k| k.as_str() == "next_ids")
        .expect("the wire payload must carry a top-level `next_ids` key");

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let html = resp.into_body().read_to_string().unwrap();
    assert!(
        html.contains(&format!("data.{key}")),
        "nextRank must read `state.data.{key}` — the exact key the wire emits"
    );
}

/// SH-197's context menu "Copy Description" reads straight off the summary
/// record `/data` already returns -- no separate detail fetch -- so this
/// pins that `description` really is there rather than something only the
/// single-story `GET .../story/<id>` (`openDrawer`'s own follow-up call)
/// carries.
#[test]
fn web_serve_api_data_carries_story_descriptions() {
    let fixture = served();
    fixture.seed(&["new", "Build feature", "--description", "Ship the thing"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();

    let stories = json["stories"].as_array().unwrap();
    assert_eq!(stories.len(), 1);
    assert_eq!(stories[0]["story"]["description"], "Ship the thing");
}

#[test]
fn web_serve_api_data_excludes_deleted_stories() {
    let fixture = served();
    fixture.seed(&["new", "Build feature"]);
    fixture.seed(&["new", "Fix bug"]);
    fixture.seed(&["delete", "SH-2", "duplicate"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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

/// SH-175's council verdict: the board renders from `stories`, which must
/// never carry a draft, while the Drafts popover and its count badge read
/// `drafts` — a separate array in the same `/api/data` payload, not a flag
/// on each story a render call site could forget to check.
#[test]
fn web_serve_api_data_routes_drafts_into_their_own_array() {
    let fixture = served();
    fixture.seed(&["new", "Live story"]);
    fixture.seed(&["new", "A sketch", "--draft"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();

    let story_ids: Vec<&str> = json["stories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["story"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        story_ids,
        vec!["SH-1"],
        "draft SH-2 leaked into the board's own `stories` array"
    );

    let draft_ids: Vec<&str> = json["drafts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["story"]["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        draft_ids,
        vec!["SH-2"],
        "the draft must still be reachable, just not in `stories`"
    );
}

#[test]
fn web_serve_404_unknown_route() {
    let fixture = served();

    let port = fixture.port;

    // ureq v3 returns non-2xx as errors
    let err = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/nonexistent"))
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

    // Authenticated and guarded, so this actually reaches routing rather
    // than being refused before it: an unauthenticated POST here is 403
    // (SH-187's admission gate runs first, the same reasoning
    // rpc::admission documents for the identical ordering) whether or not
    // the method would have been allowed.
    let err = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"),
        "",
    )
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

    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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

    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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

    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/"))
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

    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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

    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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

    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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

    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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
fn web_serve_api_data_meta_defaults_are_first_configured_not_alphabetical() {
    let fixture = served();
    // Append a state and a type that would sort first alphabetically, so a
    // defaulting rule keyed on sorted (e.g. BTreeMap-derived) order would
    // pick them instead of the project's actual first-configured state/type
    // (SH-44) — the same trap `web_serve_api_data_meta_states_are_ordered`
    // above guards for `meta.states` itself.
    fixture.seed(&["state", "add", "aaa-earlier", "--super", "OPEN"]);
    fixture.seed(&["type", "add", "aaa-earlier"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();

    assert_eq!(
        json["meta"]["defaults"]["state"], "todo",
        "default state must be the first *configured* OPEN state, not alphabetical"
    );
    assert_eq!(
        json["meta"]["defaults"]["type"], "normal",
        "default type must be the first *configured* type, not alphabetical"
    );
}

#[test]
fn web_meta_includes_sorted_unique_labels() {
    let fixture = served();
    fixture.seed(&["new", "A", "--labels", "web,bug"]);
    fixture.seed(&["new", "B", "--labels", "web,cli"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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

    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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

    let resp = fixture
        .agent()
        .get(&format!(
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

    let err = fixture
        .agent()
        .get(&format!(
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
    let resp = fixture
        .agent()
        .get(format!(
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

    let resp = fixture
        .agent()
        .get(format!(
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
fn post_json(
    fixture: &Served,
    url: &str,
    body: &str,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    fixture
        .agent()
        .post(url)
        .header("X-Storyhook", "1")
        .content_type("application/json")
        .send(body)
}

fn patch_json(
    fixture: &Served,
    url: &str,
    body: &str,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    fixture
        .agent()
        .patch(url)
        .header("X-Storyhook", "1")
        .content_type("application/json")
        .send(body)
}

fn delete_json(
    fixture: &Served,
    url: &str,
    body: &str,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    fixture
        .agent()
        .delete(url)
        .header("X-Storyhook", "1")
        .force_send_body()
        .content_type("application/json")
        .send(body)
}

/// Same as `post_json` but without the guard header, for guard-rejection tests.
fn post_json_unguarded(
    fixture: &Served,
    url: &str,
    body: &str,
) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    fixture
        .agent()
        .post(url)
        .content_type("application/json")
        .send(body)
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
        &fixture,
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
    let data = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let data_json: serde_json::Value =
        serde_json::from_str(&data.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(data_json["summary"]["total_open"], 1);
}

/// The dashboard's create route (`Invocation::New`) reaches the same
/// unassessed-priority warning `story new` does (SH-354/SH-359/SH-358): the
/// envelope's `warnings` field, not a new one, is what makes the browser able
/// to read it without any server-side dashboard-specific code.
#[test]
fn web_create_story_with_no_priority_carries_the_unassessed_warning() {
    let fixture = served();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story"),
        r#"{"title":"No priority named"}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 201);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    let warnings = json["warnings"]
        .as_array()
        .expect("the envelope must carry `warnings` when the story is unassessed");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().is_some_and(|w| w.contains("priority not set"))),
        "{json}"
    );
}

/// The load-bearing pairing with the test above: a stated priority raises no
/// warning at all, so the dashboard never has to reason about `warnings`
/// being present-but-empty versus genuinely absent.
#[test]
fn web_create_story_with_a_stated_priority_carries_no_warning() {
    let fixture = served();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story"),
        r#"{"title":"Assessed already","priority":"high"}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 201);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert!(
        json.get("warnings").is_none(),
        "an assessed story must carry no `warnings` key: {json}"
    );
}

#[test]
fn web_create_story_missing_title_is_400() {
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let err = post_json(
        &fixture,
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

    let resp = post_json(&fixture,
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
        &fixture,
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
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story"),
        r#"{"title":"Bad priority","priority":"urgent"}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 422);

    // No orphaned story should have been created.
    let data = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let data_json: serde_json::Value =
        serde_json::from_str(&data.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(data_json["summary"]["total_open"], 0);
}

/// SH-312: before this, `route_create_story`'s `Ctx` carried no provenance at
/// all, so a web-created story's `StoryCreated` event read back as
/// [`Provenance::unrecorded`] — indistinguishable from a pre-SH-246 row or a
/// test fixture, which is what made diagnosing SH-310/SH-311 (two identical
/// stories filed 24 seconds apart from the dashboard) require a raw store
/// dump instead of a `story log`.
#[test]
fn web_create_story_is_attributable_to_the_web_door() {
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story"),
        r#"{"title":"Attributable via web"}"#,
    )
    .unwrap();

    let events = Store::read(&*fixture.store, |tx| {
        tx.events_for(fixture.project, StoryNo::new(1))
    })
    .unwrap();
    let created = events
        .iter()
        .find(|e| e.kind == "StoryCreated")
        .expect("a StoryCreated event");
    assert!(
        !created.provenance.is_unrecorded(),
        "a web-door write must not fold to Provenance::unrecorded"
    );
    assert_eq!(created.provenance, Provenance::command("web:new"));
}

/// SH-312's council verdict (on that story, and reasoned through in
/// `docs/rca/duplicate-story-from-the-dashboard.md`):
/// truth-telling plus a client-side in-flight guard, not a server-side
/// idempotency key. This pins that decision as *current, intentional*
/// behavior at the layer this test can see — two independent REST creates
/// with identical bodies are two distinct stories, exactly as `story new`
/// typed twice at a terminal would be. If a future incident or a
/// non-interactive REST consumer trips either flip trigger recorded in the
/// council decision, this test is the one that must change alongside the fix.
#[test]
fn web_create_story_twice_with_identical_bodies_files_two_stories() {
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story");
    let body =
        r#"{"title":"Add a priority chooser to the story card right click menu","labels":["web"]}"#;

    let first = post_json(&fixture, &url, body).unwrap();
    let second = post_json(&fixture, &url, body).unwrap();
    assert_eq!(first.status(), 201);
    assert_eq!(second.status(), 201);

    let first_json: serde_json::Value =
        serde_json::from_str(&first.into_body().read_to_string().unwrap()).unwrap();
    let second_json: serde_json::Value =
        serde_json::from_str(&second.into_body().read_to_string().unwrap()).unwrap();
    assert_ne!(
        story_field(&first_json, "id"),
        story_field(&second_json, "id")
    );
    assert_eq!(
        story_field(&first_json, "title"),
        story_field(&second_json, "title")
    );

    let data = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let data_json: serde_json::Value =
        serde_json::from_str(&data.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(data_json["summary"]["total_open"], 2);
}

#[test]
fn web_create_story_without_guard_header_is_403() {
    let fixture = served();

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let err = post_json_unguarded(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story"),
        r#"{"title":"Should not be created"}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 403);

    let data = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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
        &fixture,
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
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move"),
        r#"{"state":"done"}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 200);

    let data = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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
        &fixture,
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
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-999/move"),
        r#"{"state":"in-progress"}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 404);
}

#[test]
fn web_move_story_with_reason_sets_awaiting_atomically() {
    // The dashboard's Blocked-column drop prompt (SH-205) — a skippable
    // reason threaded through the same /move call, not a second request.
    let fixture = served();
    fixture.seed(&["new", "Movable"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move"),
        r#"{"state":"blocked","reason":"waiting on SH-9"}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(story_field(&json, "state"), "blocked");
    assert_eq!(story_field(&json, "awaiting"), "waiting on SH-9");
}

#[test]
fn web_move_story_without_reason_leaves_awaiting_null() {
    // Every drop that isn't into Blocked (or that skips the prompt) must see
    // no behavior change from before `reason` existed on this endpoint.
    let fixture = served();
    fixture.seed(&["new", "Movable"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move"),
        r#"{"state":"blocked"}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(story_field(&json, "state"), "blocked");
    assert!(json["story"]["story"]["awaiting"].is_null(), "{json}");
}

#[test]
fn web_move_story_reason_combined_with_closed_state_is_422() {
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let err = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move"),
        r#"{"state":"done","reason":"why though"}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 422);
}

// --- Mutation API: comment, priority, assign, labels, block/unblock, reopen ---

#[test]
fn web_comment_story_appends_comment() {
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = post_json(
        &fixture,
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
        &fixture,
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
        &fixture,
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
        &fixture,
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
        &fixture,
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
        &fixture,
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
        &fixture,
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
        &fixture,
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
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/block"),
        r#"{"reason":"waiting on design"}"#,
    )
    .unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(story_field(&json, "awaiting"), "waiting on design");

    let resp2 = post_json(
        &fixture,
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
        &fixture,
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
/// leave the story untouched.
///
/// **Updated for SH-154.** The guarded-undelete check used to be a hard
/// refusal from inside the service layer (422, `AppError::Validation`) —
/// which was itself a defect: a service running inside the daemon has no
/// terminal to prompt at, so the check always refused regardless of who was
/// asking or how. It now answers `Response::ConfirmationRequired` (409), the
/// same two-step `delete`/`purge`/`set-prefix` already give the dashboard, so
/// a browser client can draw its own confirmation modal instead of just
/// failing.
#[test]
fn web_reopen_deleted_story_without_force_is_409_confirmation_required() {
    let fixture = served();
    fixture.seed(&["new", "Story"]);
    fixture.seed(&["delete", "SH-1", "created in error"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let err = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/reopen"),
        "",
    )
    .unwrap_err();
    assert_eq!(status_of(err), 409);

    let show = fixture
        .agent()
        .get(format!(
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
        &fixture,
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
        &fixture,
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
/// `story reopen`: reopening a *soft-deleted* story requires confirmation
/// rather than silently undeleting it (see `invoke.rs`'s `Invocation::
/// Reopen` handler) — and, since the server has no TTY to prompt at, this
/// must answer cleanly rather than hang waiting on stdin confirmation.
///
/// **Updated for SH-154**: "a clear error" was itself the defect that story
/// fixed — see `web_reopen_deleted_story_without_force_is_409_confirmation_required`
/// just above. What must still be true, and is all this test checks now, is
/// that nothing gets silently undeleted and nothing hangs.
#[test]
fn web_reopen_soft_deleted_story_requires_confirmation_without_force() {
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let del = delete_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"),
        r#"{"reason":"duplicate"}"#,
    )
    .unwrap();
    assert_eq!(del.status(), 200);

    let err = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/reopen"),
        "",
    )
    .unwrap_err();
    let status = match err {
        ureq::Error::StatusCode(code) => code,
        other => panic!("expected status code error, got: {other}"),
    };
    assert_eq!(
        status, 409,
        "soft-deleted reopen must ask for confirmation, not silently undelete or hang"
    );

    let show = fixture
        .agent()
        .get(format!(
            "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"
        ))
        .call()
        .unwrap();
    let show_json: serde_json::Value =
        serde_json::from_str(&show.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(story_field(&show_json, "deleted"), true, "not undeleted");
}

// --- Mutation API: PATCH multi-field ---

#[test]
fn web_patch_story_updates_multiple_fields() {
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = patch_json(
        &fixture,
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
        &fixture,
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
        &fixture,
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

    let err = fixture
        .agent()
        .patch(format!(
            "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"
        ))
        .content_type("application/json")
        .send(r#"{"description":"Should not land"}"#)
        .unwrap_err();
    assert_eq!(status_of(err), 403);

    let resp = fixture
        .agent()
        .get(format!(
            "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"
        ))
        .call()
        .unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(story_field(&json, "description"), serde_json::Value::Null);
}

// --- Mutation API: draft / publish (SH-175) ---

#[test]
fn web_create_story_with_draft_true_creates_a_draft() {
    let fixture = served();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story"),
        r#"{"title":"A sketch","draft":true}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 201);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(story_field(&json, "draft"), true);
}

#[test]
fn web_create_story_without_draft_is_live() {
    let fixture = served();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story"),
        r#"{"title":"Not a sketch"}"#,
    )
    .unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    // `draft` carries `skip_serializing_if = "is_false"`, matching `deleted`
    // — a live story's wire form omits the key rather than sending `false`.
    assert_eq!(story_field(&json, "draft"), serde_json::Value::Null);
}

#[test]
fn web_publish_makes_a_draft_live() {
    let fixture = served();
    fixture.seed(&["new", "A sketch", "--draft"]);
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/publish"),
        "{}",
    )
    .unwrap();
    assert_eq!(resp.status(), 200);
    let json: serde_json::Value =
        serde_json::from_str(&resp.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(story_field(&json, "draft"), serde_json::Value::Null);
}

#[test]
fn web_publish_without_the_csrf_header_is_refused() {
    let fixture = served();
    fixture.seed(&["new", "A sketch", "--draft"]);
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let err = post_json_unguarded(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/publish"),
        "{}",
    )
    .unwrap_err();
    assert_eq!(status_of(err), 403);
}

// --- Mutation API: relate / unrelate ---

#[test]
fn web_relate_and_unrelate_stories() {
    let fixture = served();
    fixture.seed(&["new", "A"]);
    fixture.seed(&["new", "B"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = post_json(
        &fixture,
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
        &fixture,
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

// --- Mutation API: link-pr / unlink-pr (SH-49) ---

#[test]
fn web_link_pr_and_unlink_pr() {
    let fixture = served();
    fixture.seed(&["new", "Linked to a PR"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/link-pr"),
        r#"{"url":"https://github.com/acme/widgets/pull/7","close_on_merge":true}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 200);

    let resp2 = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/unlink-pr"),
        r#"{"url":"https://github.com/acme/widgets/pull/7"}"#,
    )
    .unwrap();
    assert_eq!(resp2.status(), 200);
}

#[test]
fn web_link_pr_defaults_close_on_merge_to_true_when_absent() {
    let fixture = served();
    fixture.seed(&["new", "Default close_on_merge"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/link-pr"),
        r#"{"url":"https://github.com/acme/widgets/pull/7"}"#,
    )
    .unwrap();
    assert_eq!(resp.status(), 200);
}

#[test]
fn web_link_pr_without_a_url_is_400() {
    let fixture = served();
    fixture.seed(&["new", "No URL"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let err = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/link-pr"),
        r#"{}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 400);
}

#[test]
fn web_link_pr_without_guard_header_is_403() {
    let fixture = served();
    fixture.seed(&["new", "Guarded"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let err = post_json_unguarded(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/link-pr"),
        r#"{"url":"https://github.com/acme/widgets/pull/7"}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 403);

    // Nothing was linked — a rejected mutation must not have partially landed.
    let show = fixture
        .agent()
        .get(format!(
            "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"
        ))
        .call()
        .unwrap();
    let show_json: serde_json::Value =
        serde_json::from_str(&show.into_body().read_to_string().unwrap()).unwrap();
    // `link-pr`'s success carries no field on the story view itself besides
    // its ordinary shape, so the guard's effect is proven by never having
    // reached the service at all rather than by a visible field — confirmed
    // by the 403 itself, which `guarded` returns before `route_link_pr_story`
    // is ever called.
    assert_eq!(story_field(&show_json, "id"), "SH-1");
}

#[test]
fn web_unlink_pr_without_guard_header_is_403() {
    let fixture = served();
    fixture.seed(&["new", "Guarded unlink"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/link-pr"),
        r#"{"url":"https://github.com/acme/widgets/pull/7"}"#,
    )
    .unwrap();

    let err = post_json_unguarded(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/unlink-pr"),
        r#"{"url":"https://github.com/acme/widgets/pull/7"}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 403);
}

// --- Mutation API: delete ---

#[test]
fn web_delete_story_soft_deletes_it() {
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = delete_json(
        &fixture,
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
    let show = fixture
        .agent()
        .get(format!(
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
        &fixture,
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
        &fixture,
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
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move"),
        r#"{"state":"in-progress"}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(err), 403);

    // The story must not have moved.
    let data = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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

    let err = fixture
        .agent()
        .post(format!(
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

    let err = fixture
        .agent()
        .post(format!(
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

    let err = fixture
        .agent()
        .put(format!(
            "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1"
        ))
        .header("X-Storyhook", "1")
        .content_type("application/json")
        .send(r#"{"title":"x"}"#)
        .unwrap_err();
    assert_eq!(status_of(err), 405);
}

// --- Mutation guard: bearer token (SH-187; loopback reads exempted by
// SH-250, and that exemption retired by SH-255) ---
//
// Every request in the rest of this file already carries the token, via
// `Served::agent`'s middleware -- these are the ones that deliberately
// don't, to prove the requirement is real. Each pairs a positive control
// (the fixture's own agent, which does carry the token) with the rejection,
// the same house rule the tailnet section below documents at
// `assert_the_listener_accepts_a_trusted_host`: a 401 proves nothing on its
// own if nothing here has proven a real request can still succeed.
//
// SH-250 exempted loopback *reads* from the token; SH-255 deleted that
// exemption, so every read needs a token on every listener now, same as
// every mutation always has. `web_mutation_without_a_token_is_401` and its
// event-hook sibling below were already asserting that for mutations and are
// unchanged; the read tests above them are what actually moved.

/// SH-255: a read needs a token on every listener now, loopback included --
/// SH-250's exemption for exactly this case is gone.
#[test]
fn web_loopback_read_needs_a_token() {
    let fixture = served();
    fixture.seed(&["new", "Story"]);
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let err = ureq::get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap_err();
    assert_eq!(
        status_of(err),
        401,
        "a tokenless loopback read must be refused"
    );

    // Positive control: the same read, credentialed, is served -- and it
    // really is the project's data, not an empty shell that happens to
    // return 200.
    let served_response = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .expect("the positive control failed: a credentialed read must succeed");
    assert_eq!(served_response.status(), 200);
    let body: serde_json::Value =
        serde_json::from_str(&served_response.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(body["stories"][0]["story"]["title"], "Story");
}

/// A rebound `Host` needs no separate rule now that every read needs a
/// token regardless of `Host` -- but the request must still be *served* once
/// credentialed, which is what the positive control below actually proves;
/// before SH-255 this was the assertion that a rebound origin could not
/// forge its way past the exemption's `Host` conjunct specifically.
#[test]
fn web_loopback_read_from_a_rebound_host_is_401() {
    let fixture = served();
    fixture.seed(&["new", "Story"]);
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let err = ureq::get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .header("Host", "evil.example")
        .call()
        .unwrap_err();
    assert_eq!(status_of(err), 401);

    // Positive control: credentialed, the same rebound read still works --
    // `Host` was never part of a read's own gate beyond the exemption that no
    // longer exists.
    let ok = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .header("Host", "evil.example")
        .call()
        .expect("the positive control failed: a credentialed read must still succeed");
    assert_eq!(ok.status(), 200);
}

/// `GET .../dispatch/{handle}` is a read, and it still needs a token like
/// every other one -- named separately from the rest because that route
/// spawns processes, so `dispatch::intercept`'s own gate has to agree with
/// `admission`'s.
#[test]
fn web_loopback_dispatch_poll_still_needs_a_token() {
    let fixture = served();
    fixture.seed(&["new", "Story"]);
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let err = ureq::get(format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/dispatch/nonexistent-handle"
    ))
    .call()
    .unwrap_err();
    assert_eq!(status_of(err), 401);
}

#[test]
fn web_mutation_without_a_token_is_401() {
    let fixture = served();
    fixture.seed(&["new", "Story"]);
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    // The CSRF guard headers are present and correct -- only the token is
    // missing -- so a 401 here proves the token is checked in its own
    // right, not merely as a side effect of the guard failing.
    let err = ureq::post(format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move"
    ))
    .header("X-Storyhook", "1")
    .content_type("application/json")
    .send(r#"{"state":"in-progress"}"#)
    .unwrap_err();
    assert_eq!(status_of(err), 401);

    // And the story must not have moved.
    let data = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    let data_json: serde_json::Value =
        serde_json::from_str(&data.into_body().read_to_string().unwrap()).unwrap();
    assert_eq!(data_json["stories"][0]["story"]["state"], "todo");
}

// --- Mutation guard: the SH-188 chain -- event hooks are behind it too ---
//
// SH-187's token gate closed this as a side effect, not as its own design
// goal: `web_mutation_without_a_token_is_401` above proves a tokenless move
// is refused and the *story* doesn't change, but no test anywhere combines
// an HTTP mutation, a configured event hook, and the token gate -- so the
// specific reachability chain SH-50's authorization review named as finding
// F2 (a browser-reachable mutation already reaches `sh -c` through event
// hooks, `event_hooks.rs:384-391` <- `service/mod.rs:288-302`) had never
// been pinned by an assertion. This is that assertion: a tokenless move must
// not fire the project's own configured hook, and a credentialed one must.

#[test]
fn web_mutation_without_a_token_cannot_reach_the_projects_event_hook() {
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    // The pointer file's `[hooks]` table is where event hooks live post-flip
    // (`event_hooks::load_hooks_config`); appended, not overwritten, because
    // replacing the whole file would destroy the identity `project new` just
    // wrote and make the checkout unresolvable.
    let sentinel = fixture.dir().join("hook_fired");
    let pointer = fixture.dir().join(".storyhook.toml");
    let identity = std::fs::read_to_string(&pointer)
        .expect("`story project new` must have written the pointer file this appends to");
    std::fs::write(
        &pointer,
        format!(
            "{identity}\n[hooks.on_state_change]\ncommand = \"touch {}\"\n",
            sentinel.display()
        ),
    )
    .expect("appending the project's event-hook configuration");

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    // The CSRF guard header is present and correct -- only the token is
    // missing -- so this proves the token is what stands between the request
    // and the hook, not merely that the guard also happened to fail.
    let err = ureq::post(format!(
        "http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move"
    ))
    .header("X-Storyhook", "1")
    .content_type("application/json")
    .send(r#"{"state":"in-progress"}"#)
    .unwrap_err();
    assert_eq!(status_of(err), 401);
    assert!(
        !sentinel.exists(),
        "a tokenless mutation must not reach the project's event hook"
    );

    // The same request, credentialed, both succeeds and fires the hook --
    // the positive control that proves the negative case above means what it
    // claims: the hook is genuinely reachable, and the token is what was
    // standing between it and the request, not a misconfigured hook that
    // would never have fired either way.
    let resp = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story/SH-1/move"),
        r#"{"state":"in-progress"}"#,
    )
    .expect("a credentialed, guarded move must succeed");
    assert_eq!(resp.status(), 200);
    assert!(
        sentinel.exists(),
        "a credentialed move must fire the project's configured event hook"
    );
}

#[test]
fn web_root_is_served_without_a_token() {
    let fixture = served();

    let resp = ureq::get(format!("http://127.0.0.1:{}/", fixture.port))
        .call()
        .expect("the SPA shell must load with no token, so it can bootstrap the token prompt");
    assert_eq!(resp.status(), 200);
}

// --- Mutation guard: security headers on writes ---

#[test]
fn web_mutation_success_has_security_headers() {
    let fixture = served();
    fixture.seed(&["new", "Story"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let resp = post_json(
        &fixture,
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

    let resp = fixture
        .agent()
        .post(format!(
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

fn get_states(fixture: &Served, port: u16, repo_id: &str) -> serde_json::Value {
    let resp = fixture
        .agent()
        .get(format!(
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

    let json = get_states(&fixture, port, repo_id);
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
        &fixture,
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
    assert_eq!(slugs(&get_states(&fixture, port, repo_id)), slugs(&json));
}

#[test]
fn web_states_create_rejects_an_invalid_slug() {
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let error = post_json(
        &fixture,
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
        let error = post_json(&fixture, &url, body).unwrap_err();
        assert_eq!(status_of(error), 400, "body: {body}");
    }
}

#[test]
fn web_states_patch_sets_and_clears_optional_fields() {
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states/todo");

    let json =
        json_body(patch_json(&fixture, &url, r#"{"description":"Not started yet"}"#).unwrap());
    assert_eq!(json["states"][0]["description"], "Not started yet");

    // null clears, absent leaves alone — the whole reason the field is
    // three-valued.
    let json =
        json_body(patch_json(&fixture, &url, r#"{"role":null,"description":null}"#).unwrap());
    assert!(json["states"][0]["description"].is_null());
}

#[test]
fn web_states_patch_leaves_unmentioned_fields_alone() {
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states/in-progress");

    patch_json(&fixture, &url, r#"{"description":"Being worked on"}"#).unwrap();
    let json = json_body(patch_json(&fixture, &url, r#"{"super_state":"OPEN"}"#).unwrap());
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

    let error = patch_json(&fixture, &url, r#"{"super_state":"CLOSED"}"#).unwrap_err();
    assert_eq!(status_of(error), 422);

    // And with a destination it goes through, moving the story.
    let json = json_body(
        patch_json(
            &fixture,
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
            &fixture,
            &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states"),
            r#"{"order":["done","todo","blocked","in-progress"]}"#,
        )
        .unwrap(),
    );
    assert_eq!(slugs(&json), vec!["done", "todo", "blocked", "in-progress"]);
    assert_eq!(slugs(&get_states(&fixture, port, repo_id)), slugs(&json));
}

#[test]
fn web_states_reorder_rejects_a_partial_order() {
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let error = patch_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states"),
        r#"{"order":["done","todo"]}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(error), 422);

    let error = patch_json(
        &fixture,
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
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states"),
        r#"{"slug":"reorder","super_state":"OPEN"}"#,
    )
    .unwrap();

    let json = json_body(
        patch_json(
            &fixture,
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
    let error = delete_json(&fixture, &url, "{}").unwrap_err();
    assert_eq!(status_of(error), 422);
    assert_eq!(slugs(&get_states(&fixture, port, repo_id)).len(), 5);

    let json =
        json_body(delete_json(&fixture, &url, r#"{"move_stories_to":"in-progress"}"#).unwrap());
    assert_eq!(slugs(&json), vec!["todo", "in-progress", "blocked", "done"]);
    assert_eq!(json["states"][1]["open_count"], 1);
}

#[test]
fn web_states_delete_unknown_state_is_404() {
    let fixture = serve_project();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    let error = delete_json(
        &fixture,
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
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states"),
        r#"{"slug":"review","super_state":"OPEN"}"#,
    )
    .unwrap_err();
    assert_eq!(status_of(error), 403);

    let error = fixture
        .agent()
        .patch(format!(
            "http://127.0.0.1:{port}/api/repos/{repo_id}/states/todo"
        ))
        .content_type("application/json")
        .send(r#"{"description":"x"}"#)
        .unwrap_err();
    assert_eq!(status_of(error), 403);

    let error = fixture
        .agent()
        .delete(format!(
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

    let error = fixture
        .agent()
        .put(format!(
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
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}/states/in-progress"),
        r#"{"description":"Being worked on"}"#,
    )
    .unwrap();

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos"))
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

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos"))
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

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos"))
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

    let err = fixture
        .agent()
        .get(format!(
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
    let resp = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos"),
        &body,
    )
    .unwrap();
    assert_eq!(resp.status(), 201);

    // The same operation the CLI performs: a pointer file in the repository,
    // and a row the dashboard can see.
    assert!(fresh.join(".storyhook.toml").exists());
    let list = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos"))
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
    let err = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos"),
        &body,
    )
    .unwrap_err();

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
    let err = post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos"),
        &body,
    )
    .unwrap_err();

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
    let err = post_json_unguarded(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos"),
        &body,
    )
    .unwrap_err();
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

    let err = delete_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}"),
        "",
    )
    .unwrap_err();
    let ureq::Error::StatusCode(code) = err else {
        panic!("expected a status error");
    };
    assert_eq!(code, 409, "confirmation required");

    // Still there, stories and all.
    let data = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos/{repo_id}"),
        &body,
    )
    .unwrap();
    assert_eq!(resp.status(), 200);

    let list = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos"))
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
        &fixture,
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

    let err = fixture
        .agent()
        .delete(format!("http://127.0.0.1:{port}/api/repos/{repo_id}"))
        .call()
        .unwrap_err();
    assert_eq!(status_of(err), 403);
}

// --- The dashboard's default port ---

/// One constant for the port a daemon prefers when nothing names one, and it
/// lives with the environment that resolves it.
///
/// `cli::DEFAULT_WEB_PORT` was a second copy of this number, and a copy is what
/// let `story web start` disagree with `story daemon start` about the same port
/// (SH-249). `env::DEFAULT_DAEMON_PORT` is the survivor because it is the one
/// `default_daemon_port` actually consults — the parser now names no port at all.
#[test]
fn the_dashboards_default_port_is_3456_and_there_is_one_of_it() {
    assert_eq!(storyhook::env::DEFAULT_DAEMON_PORT, 3456);
    assert_eq!(
        storyhook::env::default_daemon_port(true),
        3456,
        "the default store's daemon prefers the port a bookmarked dashboard URL uses"
    );
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
    let agent = fixture.agent();

    // Fire 10 concurrent requests
    let handles: Vec<_> = (0..10)
        .map(|_| {
            let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data");
            let agent = agent.clone();
            std::thread::spawn(move || {
                let resp = agent.get(&url).call().unwrap();
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

// --- An omitted --port is a question for the environment ---

/// An omitted `--port` parses to `None` — "wherever the environment says" — and
/// not to a port this parser chose.
///
/// It used to parse to `3456`, which `handle_start` then handed to
/// `commands::start` as an explicit request, overriding both
/// `$STORYHOOK_DAEMON_ADDR` and the store's own resolved preference (SH-249).
/// The behaviour that override defeated is fenced by
/// `tests/store_isolation.rs::every_spelling_that_starts_a_daemon_honours_the_
/// preferred_port`; this pins the parse itself, which is where the wrong value
/// entered.
#[test]
fn web_start_without_a_port_defers_to_the_environment() {
    for spelling in [["web", "start"], ["web", "--serve"]] {
        let argv: Vec<String> = spelling.iter().map(|s| (*s).to_string()).collect();
        let port = match storyhook::cli::parse_invocation(&argv).unwrap() {
            storyhook::cli::Invocation::Web {
                action: storyhook::cli::WebAction::Start { port },
            }
            | storyhook::cli::Invocation::Web {
                action: storyhook::cli::WebAction::Serve { port },
            } => port,
            other => panic!("expected a Web start/serve action, got {other:?}"),
        };
        assert_eq!(
            port,
            None,
            "`story {}` must leave the port to the environment; a default chosen here \
             outranks $STORYHOOK_DAEMON_ADDR and the store's own preference",
            spelling.join(" ")
        );
    }
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
            assert_eq!(port, Some(8080));
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
            assert_eq!(port, Some(4000));
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
        } => assert_eq!(port, Some(1)),
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
        } => assert_eq!(port, Some(65535)),
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

    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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

    let err = fixture
        .agent()
        .get(format!(
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
    let loopback = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    assert_eq!(loopback.status(), 200);

    // The tailnet interface is also bound and serves the same data.
    let tailnet_url = format!("http://{}:{port}/api/repos/{repo_id}/data", bind.ip());
    let resp = fixture
        .agent()
        .get(&tailnet_url)
        .call()
        .unwrap_or_else(|e| {
            panic!(
                "expected the dashboard to be reachable via its own tailnet IP {tailnet_url}: {e}"
            )
        });
    assert_eq!(resp.status(), 200);
    let body = resp.into_body().read_to_string().unwrap();
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["summary"]["total_open"], 1);
}

/// The tailnet listener needs the token too (SH-187) -- being an allowed
/// `Host` never was, and still is not, a substitute for a credential.
#[test]
fn web_serve_tailnet_read_without_a_token_is_401() {
    let fixture = served();
    let Some(bind) = fixture.bound.tailnet.clone() else {
        return skip_no_tailnet_listener();
    };
    fixture.seed(&["new", "Reachable via tailnet"]);

    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());
    wait_for_addr(&format!("{}:{port}", bind.ip()));
    let tailnet_url = format!("http://{}:{port}/api/repos/{repo_id}/data", bind.ip());

    // Positive control: the same URL, with the token, succeeds.
    let ok = fixture
        .agent()
        .get(&tailnet_url)
        .call()
        .unwrap_or_else(|e| panic!("the positive control failed: {e}"));
    assert_eq!(ok.status(), 200, "the positive control must return 200");

    let err = ureq::get(&tailnet_url).call().unwrap_err();
    assert_eq!(status_of(err), 401);
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
    let resp = fixture
        .agent()
        .post(&url)
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
    let resp = fixture.agent().post(&url)
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
    let resp = fixture
        .agent()
        .post(&url)
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
    let err = fixture
        .agent()
        .post(&url)
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
    let err = fixture
        .agent()
        .post(&url)
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

/// Opens a raw `GET /api/events` connection with `request_line` as the exact
/// request text, and returns just the HTTP status line -- enough to prove
/// admission, without the rest of `connect_sse`'s work of reading past the
/// whole response head for a test that goes on to consume the stream.
fn sse_status_line(port: u16, request_line: &str) -> String {
    use std::io::{BufRead, BufReader, Write};

    let mut stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))
        .expect("connecting to /api/events");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    write!(stream, "{request_line}").expect("writing the SSE request line");
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .expect("reading the SSE status line");
    line
}

/// `/api/events` is a read, and SH-255 retired the exemption that used to
/// admit a loopback read with no token at all -- so a tokenless connection
/// is refused now, the same as any other read. `EventSource` cannot set
/// headers, which is exactly why a same-origin `EventSource` authenticates
/// through the named-token cookie instead (`web_dashboard.html`'s
/// `connectEvents`, and `tests/token_endpoint.rs`/`handoff_endpoint.rs` for
/// the cookie's own wire coverage); this file's positive control below uses
/// a header instead, since a raw socket has no cookie jar to carry one.
#[test]
fn sse_needs_a_token() {
    let _sse_guard = sse_test_lock();
    let fixture = served();

    let refused = sse_status_line(
        fixture.port,
        "GET /api/events HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    );
    assert!(
        refused.contains("401"),
        "a tokenless SSE read must be refused, got: {refused:?}"
    );

    let served_line = sse_status_line(
        fixture.port,
        &format!(
            "GET /api/events HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Storyhook-Token: {}\r\n\
             Connection: close\r\n\r\n",
            fixture.token
        ),
    );
    assert!(
        served_line.contains("200"),
        "a credentialed SSE read must be served, got: {served_line:?}"
    );
}

/// A rebound `Host` needs no separate rule now that every SSE connection
/// needs a token regardless of `Host` -- kept to prove a credentialed
/// request is still served rather than refused for an unrelated reason. A
/// rebound page is same-origin with `http://127.0.0.1:PORT` and so *could*
/// read the stream it opens, which is why this daemon has never trusted
/// `Host` alone to decide anything here.
#[test]
fn sse_from_a_rebound_host_is_still_401() {
    let _sse_guard = sse_test_lock();
    let fixture = served();

    let refused = sse_status_line(
        fixture.port,
        "GET /api/events HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n",
    );
    assert!(
        refused.contains("401"),
        "a tokenless SSE read must be refused regardless of Host, got: {refused:?}"
    );

    // The same rebound request, credentialed, is admitted -- so the 401
    // above is the missing credential, not a rejection of this `Host`.
    let ok = sse_status_line(
        fixture.port,
        &format!(
            "GET /api/events HTTP/1.1\r\nHost: evil.example\r\nX-Storyhook-Token: {}\r\n\
             Connection: close\r\n\r\n",
            fixture.token
        ),
    );
    assert!(
        ok.contains("200"),
        "the positive control must return 200, got: {ok:?}"
    );
}

/// Connecting and immediately mutating the registered repo delivers a
/// `repo-changed` event carrying that repo's id — the core "live" path.
#[test]
fn sse_delivers_repo_changed_on_story_mutation() {
    let _sse_guard = sse_test_lock();
    let fixture = served();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let mut sse = connect_sse(port, &fixture.token);
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

    let mut sse = connect_sse(port, &fixture.token);
    post_json(
        &fixture,
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

/// Several out-of-band mutations fired back-to-back collapse into fewer
/// published events than mutations performed.
///
/// The name and doc this test carried until SH-216 credited the collapse to a
/// "per-repo 200ms debounce window" in the bus. It was never that: `seed`
/// dispatches **in process**, so these writes never cross a request boundary
/// and the only publisher that can see them is `poll_change_token`, whose
/// `ChangeWatcher::notice` diffs the store on a 250ms tick and emits one
/// `Change::Project` however many writes accumulated since the last baseline.
/// The collapse is the watcher's poll granularity — which is also why the test
/// survived SH-216 deleting the bus's coalescing outright, and why two ticks
/// (250ms apart) were always further apart than that window anyway.
///
/// Kept, because what it describes is real and worth pinning: a burst of writes
/// this daemon did not serve — a `story tui` session, a second machine — costs
/// a browser one refetch, not one per write.
#[test]
fn sse_collapses_a_burst_of_out_of_band_writes_into_fewer_events() {
    let _sse_guard = sse_test_lock();
    let fixture = served();
    let port = fixture.port;

    fixture.seed(&["new", "Debounce target"]);

    let mut sse = connect_sse(port, &fixture.token);
    const MUTATIONS: usize = 6;
    // In-process on purpose, and load-bearing twice over: it is what makes
    // these writes *out of band* (no request boundary sees them, so only the
    // poller can report them), and a tight loop is what lands them all inside
    // one poll tick. Spawning `story` subprocesses would do neither reliably
    // under the CPU load this suite generates.
    for _ in 0..MUTATIONS {
        fixture.seed(&["comment", "SH-1", "rapid update"]);
    }

    // Wait for the poll tick (250ms) to publish at all — bounded by the 8s
    // cap, because that tick can be descheduled for far longer than a debounce
    // window under the CPU load this suite generates — and only then let 500ms
    // of quiet decide that the burst is over and the count is final.
    let received = read_sse_until_quiet_after(
        &mut sse,
        "event: repo-changed",
        Duration::from_millis(500),
        Duration::from_secs(8),
    );
    let occurrences = received.matches("event: repo-changed").count();
    assert!(
        occurrences >= 1,
        "expected at least one repo-changed event, got none: {received}"
    );
    assert!(
        occurrences < MUTATIONS,
        "expected the safety-net poll to collapse {MUTATIONS} out-of-band writes into fewer \
         than {MUTATIONS} events, got {occurrences}: {received}"
    );
}

/// **SH-216, over the wire.** Two mutations to the *same* project, from two
/// genuinely different causes, landing well inside the 200ms window the bus
/// used to coalesce over. Both must reach a live client.
///
/// The bus used to drop the second, because it keyed coalescing on a change's
/// *value* and kept the *first* publish of a value within the window. That is
/// sound only while the first notice is still undelivered — and here it is not:
/// the client has been told once and has refetched off that notice, so the
/// second write commits with nothing left to announce it, and the dashboard
/// shows state that write already superseded.
///
/// Mutations go over HTTP rather than through `Fixture::seed`, which matters:
/// `seed` dispatches in process, so its writes never cross the request boundary
/// and are only ever picked up — already collapsed into one publish — by
/// `poll_change_token`'s diff. Only a real request produces the precise,
/// per-mutation `Changed` publish this test is about. Two loopback POSTs also
/// land milliseconds apart, where two `story` subprocesses would not reliably
/// land inside 200ms at all.
///
/// **`STORYHOOK_CHANGE_POLL_MS` is set beyond this test's own timeout, and that
/// is what gives the assertion its teeth** — the same technique
/// `sse_delivers_a_cli_write_with_the_safety_net_poll_disabled` uses, and for a
/// sharper reason here. With the safety net free to tick, this test passes
/// against the *defect*: the poller re-publishes the same project ~250ms later,
/// outside the old 200ms window, so a second `repo-changed` arrives even when
/// the second mutation's own publish was dropped. Verified by restoring the old
/// coalescing rule and watching it stay green. Disabling the poll leaves the two
/// request-boundary publishes as the only ones that can occur, so a suppressed
/// second publish is a failure and not a delay. Hence the real daemon
/// subprocess rather than the in-thread fixture: the variable is process-wide,
/// and a child process is where it can be set without reaching other tests.
#[test]
fn sse_delivers_a_second_change_to_the_same_repo_immediately_after_the_first() {
    let _sse_guard = sse_test_lock();
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .env("STORYHOOK_CHANGE_POLL_MS", "600000")
        .args(["web", "start"])
        .assert()
        .success();
    let info = started(&env);
    let port = info.port;
    wait_for_server(port);

    env.story(dir.path())
        .args(["project", "new", "--prefix", "SH", "--no-agents-md"])
        .assert()
        .success();

    let agent = token_agent(&info.token);
    let repos: serde_json::Value = serde_json::from_str(
        &agent
            .get(format!("http://127.0.0.1:{port}/api/repos"))
            .call()
            .expect("listing repos")
            .into_body()
            .read_to_string()
            .expect("a repo list body"),
    )
    .expect("the repo list is JSON");
    let repo_id = repos[0]["id"]
        .as_str()
        .expect("the project's id")
        .to_string();

    let mut sse = connect_sse(port, &info.token);

    // Two distinct causes: two separate story creations, each its own request,
    // each committing before its own publish. Creations rather than
    // transitions, so no workflow rule can make the second request fail for a
    // reason that has nothing to do with what is under test.
    let url = format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story");
    for title in ["First cause", "Second cause"] {
        let resp = agent
            .post(&url)
            .header("X-Storyhook", "1")
            .content_type("application/json")
            .send(format!(r#"{{"title":"{title}"}}"#))
            .unwrap_or_else(|e| panic!("creating {title:?} must succeed: {e}"));
        assert_eq!(resp.status(), 201, "creating {title:?} must return 201");
    }

    let received = read_sse_until_quiet_after(
        &mut sse,
        "event: repo-changed",
        Duration::from_millis(500),
        Duration::from_secs(8),
    );
    let occurrences = received.matches("event: repo-changed").count();
    assert_eq!(
        occurrences, 2,
        "both mutations must be announced, and with the safety-net poll disabled these two \
         request boundaries are the only publishers that can fire; {occurrences} event(s) \
         means a write committed with nothing left to tell the client about it: {received}"
    );

    env.story(dir.path())
        .args(["web", "stop"])
        .assert()
        .success();
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
        let _first = connect_sse(port, &fixture.token); // subscribes, then drops at end of this block
    }
    // Give the dropped connection's writer thread a moment to notice the
    // closed socket and unsubscribe.
    std::thread::sleep(Duration::from_millis(200));

    // The server must still serve ordinary requests fine.
    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    assert_eq!(resp.status(), 200);

    // And a fresh SSE subscriber must still receive live events.
    let mut second = connect_sse(port, &fixture.token);
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

    let mut sse = connect_sse(port, &fixture.token);

    let body =
        serde_json::json!({"path": dir_b.path().to_string_lossy(), "prefix": "RB"}).to_string();
    post_json(
        &fixture,
        &format!("http://127.0.0.1:{port}/api/repos"),
        &body,
    )
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
        &fixture,
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

    let mut sse = connect_sse(port, &fixture.token);

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
        &fixture,
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

    let mut sse = connect_sse(port, &fixture.token);

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
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .env("STORYHOOK_SSE_HEARTBEAT_MS", "300")
        .args(["web", "start"])
        .assert()
        .success();
    let info = started(&env);
    wait_for_server(info.port);

    let mut sse = connect_sse(info.port, &info.token);
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

/// A directory holding a `tailscale` that sleeps briefly before answering
/// with a real, bindable identity — long enough that a test can be sure an
/// SSE connection made without any artificial delay is already open by the
/// time the probe succeeds, short enough not to slow the suite down.
fn slow_bindable_tailscale_shim(ip: std::net::IpAddr, fqdn: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("storyhook-tailscale-slow-bindable-")
        .tempdir_in("/private/tmp")
        .expect("a scratch directory for the shim");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"status\" ]; then\n\
         \x20 sleep 0.5\n\
         \x20 printf '%s' '{{\"Self\":{{\"DNSName\":\"{fqdn}.\",\"TailscaleIPs\":[\"{ip}\"]}}}}'\n\
         \x20 exit 0\n\
         fi\n\
         exit 1\n",
    );
    let path = dir.path().join("tailscale");
    std::fs::write(&path, script).expect("writing the shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("making the shim executable");
    }
    dir
}

/// A real, bindable, non-loopback IP on this machine — the same
/// routing-lookup trick `tailnet_rebind.rs` uses. No live Tailscale required.
fn a_bindable_non_loopback_ip() -> std::net::IpAddr {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").expect("binding an ephemeral UDP socket");
    socket
        .connect("8.8.8.8:80")
        .expect("a route to pick a source address from, even with no real connectivity behind it");
    socket.local_addr().expect("the socket's own address").ip()
}

/// Regression test for the lock-scoping defect SH-186 exposed: `worker`'s
/// accept-loop closure used to hold `trusted_hosts`'s read lock for the
/// *entire* duration of the request it was handling, not merely for the
/// admission checks that actually need it. That was latent as long as
/// `tailnet_reprobe`'s write (the lock's only writer) almost never ran on a
/// machine whose tailnet was already bound at startup — SH-186 makes that
/// write happen for nearly every daemon, on a background thread, shortly
/// after startup. An `EventSource` held open (a browser tab, or this test's
/// raw SSE connection) is exactly the "entire duration" `worker` never used
/// to bound: with the old scoping, its read guard blocked the reprobe's
/// write indefinitely, which in turn blocked every *other* reader — `story
/// daemon stop`'s own admission check included — until the SSE connection
/// closed. Observed as `story daemon stop` timing out against a daemon
/// serving an open dashboard tab.
///
/// Red before the fix: `story daemon stop`, issued while the SSE connection
/// below is still open and the tailnet bind has just landed, hung until
/// `CONTROL_DEADLINE` (5s) and failed. Green after: the read lock is cloned
/// out and released immediately, so an open SSE connection can no longer
/// block anything.
#[test]
fn a_late_tailnet_bind_does_not_block_shutdown_behind_an_open_sse_connection() {
    let _sse_guard = sse_test_lock();
    let env = TestEnv::isolated();
    let dir = scratch_dir();

    // The fixture-ordering trap SH-146's test found: force a fresh spawn so
    // the shimmed PATH below is actually observed.
    env.stop_daemon();

    let ip = a_bindable_non_loopback_ip();
    let shim = slow_bindable_tailscale_shim(ip, "sh186-lock-scope.tail00000.ts.net");
    let mut entries = vec![shim.path().to_path_buf()];
    entries.extend(std::env::split_paths(&env.path_with_binary()));
    let path = std::env::join_paths(entries).expect("joining PATH");

    env.story(dir.path())
        .env("PATH", &path)
        .args(["web", "start"])
        .assert()
        .success();
    let info = started(&env);
    wait_for_server(info.port);

    // Connecting blocks until the response head is fully received, which
    // only happens once the server has admitted the request and entered
    // `serve_sse` — the exact point at which the old, over-broad read guard
    // would already be held for the rest of this connection's life. The
    // shim's 0.5s delay means this always finishes well before the probe
    // does, so the ordering below is not a race to get right.
    let sse = connect_sse(info.port, &info.token);

    // Confirm the premise: the bind must land while the SSE connection
    // above is still open, or this test proves nothing about the ordering
    // it exists to pin.
    let bind_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if env.daemon().is_some_and(|i| i.tailnet.is_some()) {
            break;
        }
        assert!(
            Instant::now() < bind_deadline,
            "the daemon never bound the shimmed tailnet identity within 5s"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // The connection is still open here — `sse` has not been read from or
    // dropped since `connect_sse` returned. A read-requiring request must
    // still complete promptly.
    let started_at = Instant::now();
    env.story(dir.path())
        .args(["web", "stop"])
        .assert()
        .success();
    let elapsed = started_at.elapsed();
    // A blocked reader would hang for as long as the SSE connection stays
    // open above (i.e. indefinitely — this test never closes it before this
    // assertion), so the property being ruled out has no finite worst case
    // to derive a ceiling from directly. `2 * CONTROL_DEADLINE` (SH-394)
    // covers a full `story web stop` subprocess — spawn, connect, the
    // shutdown request's own admission check — against the same "generous
    // for CPU contention" doctrine `sse_connection_does_not_block_other_
    // requests` states for its own, cheaper (no subprocess) HTTP-level
    // sibling of this assertion.
    assert!(
        elapsed < CONTROL_DEADLINE * 2,
        "`story daemon stop` took {elapsed:?} with an SSE connection open — an open, \
         long-lived request must never hold the trusted-hosts lock across its whole \
         lifetime, or every other reader (including this shutdown request's own \
         admission check) queues behind it"
    );

    // Held open until here on purpose — dropping it any earlier would let
    // the connection close before the assertion above, which is exactly the
    // ordering this test exists to rule out.
    drop(sse);
}

/// SH-145: a story created through the CLI's `/api/v1/invoke` transport
/// must still reach an open dashboard tab live.
///
/// Every other `sse_*` test in this file mutates through `Served::seed`
/// (an in-process `dispatch`, bypassing the daemon's HTTP surface entirely)
/// or through the dashboard's own REST routes (`rest::route`, published at
/// the request boundary by `route_job_inner` in `daemon/serve.rs`). Neither
/// is what a real `story` command does: since SH-114 every CLI write goes
/// through `rpc::route`'s `POST /api/v1/invoke`. This test is the one that
/// actually exercises that path, running the real daemon subprocess and a
/// real `story new` alongside it — but it does not distinguish *which*
/// publisher carried the write (the request boundary or the safety-net
/// poll); `sse_delivers_a_cli_write_with_the_safety_net_poll_disabled`
/// below (SH-202) is the one that isolates the boundary.
#[test]
fn sse_delivers_repo_changed_for_a_cli_write_through_the_daemon() {
    let _sse_guard = sse_test_lock();
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .args(["web", "start"])
        .assert()
        .success();
    let info = started(&env);
    wait_for_server(info.port);

    env.story(dir.path())
        .args(["project", "new", "--prefix", "SH", "--no-agents-md"])
        .assert()
        .success();

    let mut sse = connect_sse(info.port, &info.token);

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

/// SH-202: the request boundary itself — not the safety-net poll — must
/// carry a CLI write to an open dashboard tab.
///
/// The test above (SH-145) proves the promise end to end but not which
/// publisher keeps it: the 250ms `poll_change_token` safety net was, before
/// this story, the *only* thing that ever noticed an `/api/v1/invoke` write,
/// so a regression that silently dropped the request-boundary publish would
/// pass it too. This test closes that gap by setting
/// `STORYHOOK_CHANGE_POLL_MS` far longer than the test's own timeout, so the
/// safety net cannot tick even once during the run — if `event: repo-changed`
/// still arrives promptly, the request boundary carried it alone.
#[test]
fn sse_delivers_a_cli_write_with_the_safety_net_poll_disabled() {
    let _sse_guard = sse_test_lock();
    let env = TestEnv::isolated();
    let dir = scratch_dir();
    let _daemon = DaemonGuard::new(&env, dir.path());

    env.story(dir.path())
        .env("STORYHOOK_CHANGE_POLL_MS", "600000")
        .args(["web", "start"])
        .assert()
        .success();
    let info = started(&env);
    wait_for_server(info.port);

    env.story(dir.path())
        .args(["project", "new", "--prefix", "SH", "--no-agents-md"])
        .assert()
        .success();

    let mut sse = connect_sse(info.port, &info.token);

    env.story(dir.path())
        .args(["new", "Carried by the request boundary alone"])
        .assert()
        .success();

    let received = read_sse_until(&mut sse, "event: repo-changed", Duration::from_secs(8));
    assert!(
        received.contains("event: repo-changed"),
        "a story created via `story new` must reach an open dashboard tab even with the \
         safety-net poll unable to fire during this test, got: {received}"
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
    // A blocked accept loop would hang for as long as the SSE connection
    // stays open below (i.e. indefinitely, since this test never closes it)
    // — this threshold only needs to rule that out, not assert sub-second
    // latency, so it's generous enough to tolerate CPU contention from the
    // rest of this suite running in parallel. Named (SH-394's
    // `tests/timing_assertions.rs` fence) — this is the reference bound
    // `a_late_tailnet_bind_does_not_block_shutdown_behind_an_open_sse_
    // connection` cites for its own, heavier (subprocess-inclusive) sibling
    // of this assertion.
    const OPEN_SSE_CEILING: Duration = Duration::from_secs(5);

    let _sse_guard = sse_test_lock();
    let fixture = served();
    let (port, repo_id) = (fixture.port, fixture.repo_id.as_str());

    let _sse = connect_sse(port, &fixture.token); // held open for the rest of the test

    let start = Instant::now();
    let resp = fixture
        .agent()
        .get(&format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(
        start.elapsed() < OPEN_SSE_CEILING,
        "a normal request took {:?} while an SSE connection was open — the accept loop \
         may be blocked on it",
        start.elapsed()
    );
}

// --- Utilities ---

use std::time::{Duration, Instant};

/// What the daemon a `story web start` just returned from actually bound.
///
/// **Read back, never remembered from the request.** A requested port is only a
/// preference: `bind_preferred` falls back to a kernel-assigned one the moment
/// its first choice is taken, so a test that waited on — or asserted about — the
/// number it asked for would be describing a daemon that may not exist. Worse
/// than waiting forever, it can *succeed*: whatever else claimed that port
/// answers the connection, and a stranger's server answers 404 or 200 to
/// everything the test asks about, which is the mass-failure mode of SH-51.
/// SH-195 fixed this shape for the direct `daemon --serve` spawns; SH-237 is the
/// same fix for the `web start` ones.
///
/// No polling: `web start` blocks in `lifecycle::ensure` until the daemon is
/// healthy, and a healthy daemon has already published its portfile.
fn started(env: &TestEnv) -> storyhook::daemon::lifecycle::DaemonInfo {
    env.daemon()
        .expect("`web start` returned success, so its daemon has published a portfile")
}

/// [`started`], for the callers that only want the port.
fn started_port(env: &TestEnv) -> u16 {
    started(env).port
}

/// Opens a raw `GET /api/events` connection, carrying `token` as the
/// `X-Storyhook-Token` header (SH-187 -- every route requires one now, and
/// a raw socket, unlike a real `EventSource`, can set headers same as any
/// other request), and reads past the response head (status line + headers,
/// through the blank line that terminates them), leaving the returned
/// `BufReader` positioned at the start of the SSE body. A short per-read
/// socket timeout (rather than one long one) lets `read_sse_until`/
/// `read_sse_until_quiet` poll their own wall-clock deadline instead of
/// blocking on a single slow `read`.
fn connect_sse(port: u16, token: &str) -> std::io::BufReader<std::net::TcpStream> {
    use std::io::{BufRead, BufReader, Write};

    let mut stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}"))
        .expect("connecting to /api/events");
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    write!(
        stream,
        "GET /api/events HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Storyhook-Token: {token}\r\n\
         Connection: keep-alive\r\n\r\n"
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
    let mut acc = Vec::new();
    read_sse_into_until(reader, &mut acc, needle, Instant::now() + timeout);
    String::from_utf8_lossy(&acc).into_owned()
}

/// Reads into `acc` until `needle` appears in it or `deadline` passes,
/// reporting whether the connection is still open — which the caller needs in
/// order to tell a stream that has gone quiet from one that has gone away.
fn read_sse_into_until(
    reader: &mut std::io::BufReader<std::net::TcpStream>,
    acc: &mut Vec<u8>,
    needle: &str,
    deadline: Instant,
) -> bool {
    let mut open = true;
    while open && Instant::now() < deadline && !String::from_utf8_lossy(acc).contains(needle) {
        open = !matches!(read_sse_chunk(reader, acc), SseRead::Closed);
    }
    open
}

/// The outcome of one bounded `read` on an SSE socket. The distinction that
/// matters is [`Idle`](SseRead::Idle) versus [`Closed`](SseRead::Closed): the
/// short per-read timeout [`connect_sse`] sets makes "nothing has arrived yet"
/// an ordinary, frequent result, and a reader that mistook it for a hang-up
/// would stop at the first pause in the stream.
enum SseRead {
    /// Bytes arrived and were appended to the accumulator.
    Bytes,
    /// The per-read timeout expired with nothing to show for it.
    Idle,
    /// The server hung up.
    Closed,
}

/// Performs one bounded read, appending whatever arrived to `acc`.
fn read_sse_chunk(
    reader: &mut std::io::BufReader<std::net::TcpStream>,
    acc: &mut Vec<u8>,
) -> SseRead {
    use std::io::Read;

    let mut buf = [0u8; 4096];
    match reader.read(&mut buf) {
        Ok(0) => SseRead::Closed,
        Ok(n) => {
            acc.extend_from_slice(&buf[..n]);
            SseRead::Bytes
        }
        Err(e)
            if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut =>
        {
            SseRead::Idle
        }
        Err(e) => panic!("reading the SSE stream: {e}"),
    }
}

/// Reads from an SSE connection until `first` has appeared *and* `quiet_for`
/// has then elapsed with no new bytes arriving. Used where the assertion is
/// about *how many* events arrived (e.g. debounce coalescing) rather than
/// whether one particular event did, so a needle alone will not do.
///
/// The two bounds answer two different questions, which is why they are two
/// phases rather than one loop (SH-288):
///
/// * **Has the stream started?** Bounded by `overall_timeout`, which is
///   generous, because this is where system load shows up — a publisher's tick
///   can be descheduled for far longer than any debounce window.
/// * **Has it stopped?** Bounded by `quiet_for` since the last byte, which is
///   a debounce measure and correct for that job alone.
///
/// Conflating them is the defect this signature exists to prevent: a quiet
/// window cannot tell "no more events are coming" from "events have not
/// started yet", and the preamble every SSE stream opens with (`retry:`, a
/// `: connected` comment) is enough to start the clock on a stream that has
/// published nothing. Under load the window then closed before the first real
/// event and the caller counted none.
///
/// `overall_timeout` backstops the second phase too, so a stream that never
/// falls quiet cannot hang the suite. Returning early from either phase is not
/// an error here — the accumulated text is returned either way, and the
/// caller's own assertion reports it.
fn read_sse_until_quiet_after(
    reader: &mut std::io::BufReader<std::net::TcpStream>,
    first: &str,
    quiet_for: Duration,
    overall_timeout: Duration,
) -> String {
    let mut acc = Vec::new();
    let mut open = read_sse_into_until(reader, &mut acc, first, Instant::now() + overall_timeout);

    let quiet_backstop = Instant::now() + overall_timeout;
    let mut last_activity = Instant::now();
    while open && Instant::now() < quiet_backstop && last_activity.elapsed() <= quiet_for {
        match read_sse_chunk(reader, &mut acc) {
            SseRead::Bytes => last_activity = Instant::now(),
            SseRead::Idle => {}
            SseRead::Closed => open = false,
        }
    }

    String::from_utf8_lossy(&acc).into_owned()
}

/// Serves exactly one SSE connection on a fresh loopback port: the stream
/// preamble immediately, then `events` `repo-changed` frames back to back —
/// but only after `first_event_after`. Returns the port it bound.
///
/// A fake rather than a daemon, because the subject here is the *reader*, and
/// the shape it must survive is one the real publisher only produces under
/// heavy load: a preamble that arrives at once and a first event that does
/// not. Driving that from a real daemon would mean reproducing the load, which
/// is exactly what made SH-288 read as a product-neutral flake instead of the
/// test-side timing assumption it was.
///
/// The connection is held open well past the last frame on purpose: a server
/// that hangs up ends the read with `Ok(0)`, which would let a reader with no
/// working quiet window pass this test for the wrong reason.
fn spawn_sse_server_whose_first_event_is_late(first_event_after: Duration, events: usize) -> u16 {
    use std::io::{BufRead, BufReader, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("binding a fake SSE server");
    let port = listener
        .local_addr()
        .expect("the fake SSE server's address")
        .port();
    std::thread::spawn(move || {
        let (mut stream, _) = listener
            .accept()
            .expect("accepting the reader's connection");
        let mut head = BufReader::new(stream.try_clone().expect("cloning the accepted socket"));
        loop {
            let mut line = String::new();
            match head.read_line(&mut line) {
                Ok(0) => return,
                Ok(_) if line == "\r\n" => break,
                Ok(_) => {}
                Err(e) => panic!("reading the fake server's request head: {e}"),
            }
        }
        // Everything a real `/api/events` response opens with, and nothing an
        // assertion counts: the bytes that used to satisfy the quiet window.
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n\
             retry: 3000\n\n: connected\n\n"
        )
        .expect("writing the fake SSE preamble");
        stream.flush().expect("flushing the fake SSE preamble");

        std::thread::sleep(first_event_after);
        for i in 0..events {
            write!(
                stream,
                "event: repo-changed\ndata: {{\"repo_id\":\"R{i}\"}}\n\n"
            )
            .expect("writing a fake repo-changed frame");
        }
        stream
            .flush()
            .expect("flushing the fake repo-changed frames");
        std::thread::sleep(Duration::from_secs(5));
    });
    port
}

/// **SH-288.** A quiet window cannot tell "no more events are coming" from
/// "events have not started yet", so the reader must not start one until the
/// stream has actually said something worth counting.
///
/// Here the preamble arrives immediately and the first real event only after
/// three times the quiet window — the timing a contended machine produced,
/// where `poll_change_token`'s 250ms tick slipped past the 500ms window and
/// `sse_collapses_a_burst_of_out_of_band_writes_into_fewer_events` failed with
/// `got none: 19\nretry: 3000\n: connected`: a preamble and nothing else.
///
/// No `sse_test_lock`: this fixture is a socket, with no daemon, no store and
/// no filesystem watcher for the lock to serialize against.
#[test]
fn read_sse_until_quiet_after_waits_for_the_first_event_not_the_first_byte() {
    const QUIET: Duration = Duration::from_millis(400);
    const EVENTS: usize = 3;
    // Named rather than inline (SH-394's `tests/timing_assertions.rs` fence).
    const OVERALL_CAP: Duration = Duration::from_secs(15);
    let port = spawn_sse_server_whose_first_event_is_late(QUIET * 3, EVENTS);

    let mut sse = connect_sse(port, "fake-token");
    let start = Instant::now();
    let received = read_sse_until_quiet_after(&mut sse, "event: repo-changed", QUIET, OVERALL_CAP);
    let elapsed = start.elapsed();

    assert_eq!(
        received.matches("event: repo-changed").count(),
        EVENTS,
        "the read must outlast a quiet window that elapses before the first event, and then \
         count every event that follows it: {received}"
    );
    // The other half of the contract: the quiet window still ends the read.
    // A reader that simply ran to its cap would satisfy the count above while
    // costing every caller the full overall timeout. Just under half of
    // OVERALL_CAP (SH-394) rather than a second hand-picked number: still a
    // strong distinction from "ran to the cap", derived from the same
    // constant the read itself is bounded by.
    assert!(
        elapsed < OVERALL_CAP / 2,
        "the quiet window, not the overall cap, must end the read; it took {elapsed:?}"
    );
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
    let body = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos"))
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
    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
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
    // Guarded and authenticated -- this must actually reach `pathless_
    // refusal` (a 422 naming why) rather than be turned away earlier by the
    // CSRF guard or the token check, which a wrong header name here would
    // silently do instead (it did, once: `X-Storyhook-Dashboard` is not the
    // guard header `mutation_guard_ok` checks for, so this test used to pass
    // for the wrong reason -- a loose `(400..500)` assertion never told the
    // two apart).
    let err = fixture
        .agent()
        .post(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/story"))
        .header("X-Storyhook", "1")
        .send_json(serde_json::json!({ "title": "Nope" }))
        .expect_err("a write with nowhere to run must be refused");
    assert_eq!(
        status_of(err),
        422,
        "expected the pathless-project refusal specifically, not some other 4xx"
    );

    // Nothing was created.
    let data = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/api/repos/{repo_id}/data"))
        .call()
        .unwrap()
        .into_body()
        .read_to_string()
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&data).unwrap();
    assert_eq!(json["summary"]["total_open"], 0);
}

/// The statuses editor's delete confirmation runs on no clock at all (SH-324).
///
/// `DELETE_CONFIRM_TIMEOUT_MS = 6000` disarmed it six seconds after arming,
/// which is WCAG 2.2 SC 2.2.1's central case -- a time limit on *completing an
/// action* -- with no turn-off, no adjust and no warn-and-extend. The council
/// route was removal rather than any of the criterion's three clauses, so what
/// this pins is an absence: there is no constant to configure and no timer to
/// find.
///
/// Scoped to `onDeleteStatus`'s own body rather than grepping the whole file,
/// which would fire on every animation cleanup, every poll and every debounce
/// -- all of which are legitimate and none of which is a deadline on a user.
#[test]
fn the_status_delete_confirmation_runs_on_no_clock() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let script = script(&body);

    assert!(
        !script.contains("DELETE_CONFIRM_TIMEOUT_MS"),
        "the deleted confirmation timeout is back. The route SH-324's council took was \
         removal, not Adjust: a constant here means the limit exists again, and a limit \
         that exists has to satisfy Turn off / Adjust / Extend, which nothing in this file \
         does."
    );

    let signature = "function onDeleteStatus(status, row) {";
    let fn_start = script
        .find(signature)
        .expect("onDeleteStatus(status, row) must exist with this exact signature");
    let close = "\n  }\n";
    let fn_end = fn_start
        + script[fn_start..]
            .find(close)
            .expect("onDeleteStatus's closing brace")
        + close.len();
    let body_of_fn = &script[fn_start..fn_end];

    assert!(
        !body_of_fn.contains("setTimeout"),
        "onDeleteStatus schedules something. Whatever it is, a confirmation that a timer \
         can reach is a time limit on completing an action -- SH-324 removed the last one, \
         and a debounce added here to guard the doubled click would be a new one rather \
         than a fix (the layout guards that now; see buildDeleteConfirmPanel)."
    );
}

/// The statuses editor's open question lives in `state`, not on the DOM node
/// (SH-324, reshaped by SH-334) -- the class fence, in the style of
/// `dead_public_surface.rs` and `release_targets.rs`.
///
/// This is the half a `setTimeout` scan cannot see. The flag used to live in
/// `button.dataset.confirming`, and `renderStatuses()` clears and rebuilds every
/// row -- so the armed state died on any of at least eight rebuild paths, two of
/// which (`statusMutation()`'s callbacks) consult no busy guard and so discarded
/// it on every browser. Deleting the six-second timer while leaving the flag on
/// the node would have swapped a visible limit for an invisible 0-25s one set by
/// the safety poll, which is worse for being undeclared. SH-334 reshaped the
/// field from a bare slug (`armedDeleteSlug`) into an object (`statusPrompt`) so
/// the same substrate covers the destination/reclassify question too; this fence
/// moved with the rename and still guards the identical property.
///
/// So the fence is on the *substrate*, not on this one call site: a future
/// question reintroduced as per-node state is the same defect wearing a
/// different name, and nothing else in the suite would catch it.
///
/// Comment lines (trimmed to start with `*` or `//`) are exempt -- the state
/// field's own doc comment names `button.dataset.confirming` while explaining
/// what replaced it, and is not a second source of the pattern.
#[test]
fn status_prompt_state_lives_in_state_not_on_the_node() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let script = script(&body);

    for (at, _) in script.match_indices("dataset.confirming") {
        let line_start = script[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line = script[line_start..].lines().next().unwrap_or("");
        let trimmed = line.trim_start();
        if trimmed.starts_with('*') || trimmed.starts_with("//") {
            continue;
        }
        panic!(
            "the editor's open question is on a DOM node again, at script byte {at}: \
             {line:?} -- renderStatuses() rebuilds every row from scratch, so a rebuild \
             eats it, and the rebuild happens on a poll the user cannot see. Keep it in \
             `state` and paint from it (SH-324)."
        );
    }

    let signature = "function buildStatusRow(status, index) {";
    let fn_start = script
        .find(signature)
        .expect("buildStatusRow(status, index) must exist with this exact signature");
    let close = "\n  }\n";
    let fn_end = fn_start
        + script[fn_start..]
            .find(close)
            .expect("buildStatusRow's closing brace")
        + close.len();

    assert!(
        script[fn_start..fn_end].contains("state.statusPrompt"),
        "buildStatusRow no longer reads state.statusPrompt, so a rebuild stops \
         repainting an open question and starts discarding it again. Painting from \
         state on every render is what makes \"this dashboard sets no time limit here\" \
         true structurally, rather than true only while every renderStatuses() caller \
         remembers to consult a busy predicate -- and two callers already do not."
    );
}

/// `promptForDestination` is gone, and no clock reaches its replacement
/// (SH-334).
///
/// `promptForDestination` appended its panel straight into a live `.status-row`
/// node and held the editor's refreshes off with nothing but `select.focus()`
/// -- the exact substrate SH-324's own council rejected for the sibling
/// confirmation, and the reason a click on the page background or a Tab away
/// let the 25s safety poll discard a destination choice the user had made but
/// not applied. A future reintroduction of a function by this name, appended
/// into a row the same way, is the identical defect wearing its original name;
/// pinning its absence is cheaper than trusting nobody brings it back.
///
/// `setTimeout` is checked in both of its replacement's arm/build sites --
/// `openStatusMove` (where a debounce guarding a doubled click would be a new
/// time limit, not a fix; the row's Cancel and the panel's own Apply-only
/// shape guard that instead) and `buildDestinationPanel` (the render itself).
#[test]
fn the_destination_prompt_runs_on_no_clock() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let script = script(&body);

    assert!(
        !script.contains("function promptForDestination"),
        "promptForDestination is back. It held the editor's refreshes off with nothing but \
         select.focus() -- the exact mechanism SH-324's council rejected -- and a click on \
         the page background or a Tab away let the safety poll discard a destination choice \
         the user had made but not applied (SH-334). Paint from state.statusPrompt instead."
    );

    for signature in [
        "function openStatusMove(status, intent, superState) {",
        "function buildDestinationPanel(status, row, prompt) {",
    ] {
        let fn_start = script
            .find(signature)
            .unwrap_or_else(|| panic!("{signature} must exist with this exact signature"));
        let close = "\n  }\n";
        let fn_end = fn_start
            + script[fn_start..]
                .find(close)
                .unwrap_or_else(|| panic!("{signature}'s closing brace"))
            + close.len();
        assert!(
            !script[fn_start..fn_end].contains("setTimeout"),
            "{signature} schedules something. Whatever it is, a question a timer can reach \
             is a time limit on completing an action -- SH-334 removed the last one here, \
             and a debounce added to guard a doubled click would be a new one rather than a \
             fix (the row's Cancel and the panel's Apply-only shape guard that instead)."
        );
    }
}

/// The destination question's *answer* survives a repaint, not just the
/// question itself (SH-334).
///
/// This is the half a `statusPrompt`-only fence would miss. `buildStatusRow`
/// painting `state.statusPrompt` (pinned above) proves the question is not
/// discarded; it says nothing about whether the choice inside it is. Two
/// further paints have to read from `state` for that to hold:
/// `buildStatusRow`'s own superstate `<select>` must show a pending
/// `superState` while a reclassify question is open beneath it, or the
/// control and the sentence disagree from the first repaint onward with no
/// second render to notice; and `buildDestinationPanel`'s own `<select>` must
/// show a pending `destination`, or a repaint resets it to "nothing chosen"
/// and Apply either sends stories nobody picked or (as this dashboard does
/// instead) refuses until the user re-answers a question they already
/// answered once.
#[test]
fn the_destination_prompts_answer_is_painted_from_state_too() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let script = script(&body);

    let slice_of = |signature: &str| {
        let fn_start = script
            .find(signature)
            .unwrap_or_else(|| panic!("{signature} must exist with this exact signature"));
        let close = "\n  }\n";
        let fn_end = fn_start
            + script[fn_start..]
                .find(close)
                .unwrap_or_else(|| panic!("{signature}'s closing brace"))
            + close.len();
        script[fn_start..fn_end].to_string()
    };

    assert!(
        // The exact expression the superstate `<select>`'s `selected:` reads,
        // not just the substring "prompt.superState" -- which the earlier
        // reconciliation block (the "already applied elsewhere" clear) also
        // contains, and would let a mutation that deletes ONLY the paint site
        // pass this fence while the real regression -- the select snapping
        // back to the server's stale value mid-question -- still reproduces,
        // as confirmed against status-destination-prompt.spec.ts.
        slice_of("function buildStatusRow(status, index) {")
            .contains("prompt.intent === \"reclassify\" ? prompt.superState"),
        "buildStatusRow's superstate select no longer paints prompt.superState, so a repaint \
         mid-question snaps it back to the server's stale value while the panel beneath it \
         still describes the pending change -- the control and the sentence would disagree, \
         and there is no second render to notice."
    );
    let destination_panel = slice_of("function buildDestinationPanel(status, row, prompt) {");
    assert!(
        destination_panel.contains("!prompt.destination"),
        "buildDestinationPanel's placeholder option no longer reads prompt.destination, so \
         \"nothing chosen\" (SH-334 Q3) stops being paintable and Apply can no longer tell a \
         fresh open from an answered one."
    );
    assert!(
        // The exact comparison inside the mapped `<option>`s, not just the
        // substring "prompt.destination" -- which the placeholder option
        // above also contains, and would let a mutation that deletes ONLY
        // the mapped options' `selected:` (leaving the placeholder's own
        // check intact) pass this fence while the real regression -- every
        // repaint resetting a chosen destination to the placeholder -- still
        // reproduces, as confirmed against status-destination-prompt.spec.ts.
        destination_panel.contains("other.slug === prompt.destination"),
        "buildDestinationPanel's mapped options no longer mark the CHOSEN destination as \
         selected, so a repaint resets it to the placeholder -- surviving the QUESTION is not \
         the same fact as surviving the ANSWER, and this is the half that fence would miss."
    );
}

/// `statusEditorIsBusy()` is not taught a new question (SH-324's council,
/// applied to SH-334).
///
/// SH-324's council rejected widening this predicate to cover the confirmation
/// it was fixing, because `statusMutation()`'s two callbacks consult no busy
/// guard and cannot be made to -- repainting from the server's authoritative
/// answer is their whole job, so a widened predicate would still be bypassed
/// by exactly the paths that mattered. The same reasoning applies to SH-334's
/// destination question without needing to be re-litigated; this fence pins
/// that the repair was not reached for a second time.
///
/// It is not the only thing that would notice: mutating this predicate to
/// `|| !!state.statusPrompt` also turns
/// `status-destination-prompt.spec.ts`'s "cleared, not withdrawn" spec red --
/// `refreshStatusesIfIdle()` and `renderSettings()`'s own guard both consult
/// this predicate, so a permanently-true reading (a prompt being open is true
/// for the whole time it is open, blur or no blur -- unlike the real
/// focus-based reading) blocks every poll from ever refreshing `state.statuses`
/// at all, and a destination that vanished on the server is never noticed
/// client-side. But this fence still earns its keep: it pins the doctrine
/// directly, at the one call site the repair would touch, rather than by way
/// of a downstream symptom in one e2e spec -- and it fails in milliseconds
/// rather than the ~60s of frozen-clock time that spec spends proving it.
#[test]
fn status_editor_is_busy_is_not_taught_about_the_destination_prompt() {
    let fixture = served();
    let port = fixture.port;

    let resp = fixture
        .agent()
        .get(format!("http://127.0.0.1:{port}/"))
        .call()
        .unwrap();
    let body = resp.into_body().read_to_string().unwrap();
    let script = script(&body);

    let signature = "function statusEditorIsBusy() {";
    let fn_start = script
        .find(signature)
        .expect("statusEditorIsBusy() must exist with this exact signature");
    let close = "\n  }\n";
    let fn_end = fn_start
        + script[fn_start..]
            .find(close)
            .expect("statusEditorIsBusy's closing brace")
        + close.len();
    let body_of_fn = &script[fn_start..fn_end];

    for needle in ["statusPrompt", "status-destination"] {
        assert!(
            !body_of_fn.contains(needle),
            "statusEditorIsBusy() now mentions {needle:?}. SH-324's council rejected \
             widening this predicate to cover an open question, because statusMutation()'s \
             two callbacks consult no busy guard and cannot be made to -- repainting from \
             the server's authoritative answer is their whole job. Paint the question from \
             state.statusPrompt on every render instead (see buildStatusRow)."
        );
    }
}
