//! What the dashboard's scoped capability can and cannot do, over a real
//! socket (SH-254).
//!
//! `src/api/session.rs`'s own unit tests own the registry — that a capability
//! does not expire, that revoking ends every one at once, that eviction takes
//! the least recently used — and the gate's truth table, because those are
//! properties of a type and are provable without a network. What is here is
//! everything that is only true once the capability is wired into a daemon:
//! that a browser holding one can actually run the board, that the three routes
//! it is refused are refused by a *server* rather than by a function, that a
//! refusal is shaped so the dashboard's own error path cannot escalate the tab
//! — and the one claim that cannot be made anywhere else.
//!
//! # The claim that has to be tested rather than written down
//!
//! **This capability can cause `sh -c` to run.** Every write it authorizes
//! fires the project's configured event hooks, and a hook is a shell command.
//! The story that filed this work made the point sharply: *"Shipping a false
//! security claim is worse than shipping a narrow one."* So
//! [`a_capability_authorized_move_fires_the_projects_shell_hook`] configures a
//! real hook, moves a story with nothing but a capability, and asserts the
//! shell ran. If somebody later narrows the scope so that is no longer true,
//! that test goes red and the spec gets corrected — which is the outcome the
//! prose alone could never force.
//!
//! Requests are written by hand onto a `TcpStream`, as
//! `tests/handoff_endpoint.rs` does and for the same reason: half of what is
//! asserted here is about headers a client library sets for you.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use storyhook::api::handoff::{ARM_PATH, COUPON_HEADER, REDEEM_PATH};
use storyhook::api::rpc::TOKEN_HEADER;
use storyhook::api::session::SESSION_HEADER;
use storyhook::cli::parse_invocation;
use storyhook::invoke::{dispatch, dispatch_unscoped};
use storyhook::service::Ctx;
use storyhook::store::{ReadOps, SqliteStore, Store};
use storyhook_test_support::{TestEnv, TestServer, project_id_at, scratch_dir, serve};

/// One raw HTTP response.
struct RawResponse {
    status: u16,
    body: String,
}

impl RawResponse {
    /// The `code` this daemon put in a refusal, if it put one there.
    ///
    /// The whole mechanism by which the dashboard tells a scope refusal from an
    /// authentication failure without parsing prose.
    fn code(&self) -> Option<String> {
        serde_json::from_str::<serde_json::Value>(&self.body)
            .ok()
            .and_then(|v| v.get("code").and_then(|c| c.as_str()).map(str::to_string))
    }
}

/// Sends one request with exactly the headers given, and an optional body.
fn request(
    port: u16,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> RawResponse {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connecting to the daemon");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("setting a read timeout");

    let mut head = format!("{method} {path} HTTP/1.1\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    stream
        .write_all(head.as_bytes())
        .expect("writing the request");
    stream.flush().expect("flushing the request");

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("reading the response");
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("a response with no header terminator: {raw:?}"));
    let status = head
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("unparseable status line in {head:?}"));

    RawResponse {
        status,
        body: body.to_string(),
    }
}

/// A daemon serving one project with one story, and the checkout it lives in.
struct Fixture {
    _env: TestEnv,
    _store: Arc<SqliteStore>,
    dir: tempfile::TempDir,
    server: TestServer,
    repo: String,
}

impl Fixture {
    fn new() -> Fixture {
        Fixture::with_hooks(None)
    }

    /// The same fixture, with `hooks` appended to the checkout's committed
    /// pointer file — where event-hook configuration lives.
    fn with_hooks(hooks: Option<&str>) -> Fixture {
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

        let project = project_id_at(&store, dir.path()).expect("the fixture project");
        let repo = Store::read(&*store, |tx| {
            Ok(tx.project(project)?.expect("the project row").slug)
        })
        .expect("reading the fixture project");

        // Seeded with hooks suppressed: a fixture is not the place to fire one.
        let ctx = Ctx::new(&*store, project, dir.path(), environment.clone()).no_hooks(true);
        dispatch(
            &ctx,
            parse_invocation(&["new".to_string(), "A story".to_string()])
                .expect("`story new` parses"),
        )
        .expect("seeding the fixture story");

        if let Some(hooks) = hooks {
            let pointer = dir.path().join(".storyhook.toml");
            let identity = std::fs::read_to_string(&pointer)
                .expect("`story project new` wrote the pointer file");
            std::fs::write(&pointer, format!("{identity}\n{hooks}"))
                .expect("appending the hook configuration");
        }

        let server = serve(Arc::clone(&store), &environment);
        Fixture {
            _env: env,
            _store: store,
            dir,
            server,
            repo,
        }
    }

    fn port(&self) -> u16 {
        self.server.port()
    }

    /// Redeems a coupon the way `story web open` and the dashboard do between
    /// them, and returns the capability.
    fn capability(&self) -> String {
        let armed = request(
            self.port(),
            "POST",
            ARM_PATH,
            &[("Host", "127.0.0.1"), (TOKEN_HEADER, &self.server.token)],
            "",
        );
        assert_eq!(armed.status, 200, "arming failed: {}", armed.body);
        let coupon = serde_json::from_str::<serde_json::Value>(&armed.body)
            .expect("the arming reply is JSON")["coupon"]
            .as_str()
            .expect("the arming reply names a coupon")
            .to_string();

        let host = format!("127.0.0.1:{}", self.port());
        let redeemed = request(
            self.port(),
            "POST",
            REDEEM_PATH,
            &[
                ("Host", &host),
                ("X-Storyhook", "1"),
                (COUPON_HEADER, &coupon),
            ],
            "",
        );
        assert_eq!(redeemed.status, 200, "redemption failed: {}", redeemed.body);
        serde_json::from_str::<serde_json::Value>(&redeemed.body)
            .expect("the redemption reply is JSON")["session"]
            .as_str()
            .expect("the redemption reply names a session")
            .to_string()
    }

    /// A request carrying `capability` and nothing else — no token anywhere,
    /// which is what makes every assertion below about the capability.
    fn as_session(&self, capability: &str, method: &str, path: &str, body: &str) -> RawResponse {
        let host = format!("127.0.0.1:{}", self.port());
        request(
            self.port(),
            method,
            path,
            &[
                ("Host", &host),
                ("X-Storyhook", "1"),
                ("Content-Type", "application/json"),
                (SESSION_HEADER, capability),
            ],
            body,
        )
    }
}

// --- What it can do ---

#[test]
fn a_capability_runs_the_board() {
    let fixture = Fixture::new();
    let capability = fixture.capability();
    let repo = &fixture.repo;

    for (method, path, body) in [
        (
            "POST",
            format!("/api/repos/{repo}/story"),
            r#"{"title":"From the browser"}"#,
        ),
        (
            "POST",
            format!("/api/repos/{repo}/story/SH-1/comment"),
            r#"{"text":"a comment"}"#,
        ),
        (
            "POST",
            format!("/api/repos/{repo}/story/SH-1/priority"),
            r#"{"priority":"high"}"#,
        ),
        (
            "POST",
            format!("/api/repos/{repo}/story/SH-1/labels"),
            r#"{"add":["web"]}"#,
        ),
        (
            "PATCH",
            format!("/api/repos/{repo}/story/SH-1"),
            r#"{"title":"Renamed from the browser"}"#,
        ),
    ] {
        let answered = fixture.as_session(&capability, method, &path, body);
        assert!(
            (200..300).contains(&answered.status),
            "{method} {path} was refused a capability that should reach it: {} {}",
            answered.status,
            answered.body
        );
    }
}

#[test]
fn a_capability_reads_without_needing_to_be_offered_at_all() {
    // SH-250's exemption already admits a loopback read, so a capability adds
    // nothing here — which is worth pinning, because it is the reason the
    // dashboard renders before it has redeemed anything.
    let fixture = Fixture::new();
    let host = format!("127.0.0.1:{}", fixture.port());
    let read = request(fixture.port(), "GET", "/api/repos", &[("Host", &host)], "");
    assert_eq!(read.status, 200);
}

// --- What it cannot do ---

#[test]
fn a_capability_cannot_create_or_destroy_a_project_or_dispatch_an_agent() {
    // The three routes this story exists to take away from a browser tab. Each
    // is refused by the *server*; the dashboard also renders its control
    // disabled, but that is an affordance and this is the boundary.
    let fixture = Fixture::new();
    let capability = fixture.capability();
    let repo = &fixture.repo;

    for (method, path, body) in [
        (
            "POST",
            "/api/repos".to_string(),
            format!(
                r#"{{"path":"{}","prefix":"XX"}}"#,
                fixture.dir.path().display()
            ),
        ),
        (
            "DELETE",
            format!("/api/repos/{repo}"),
            format!(r#"{{"confirm":"{repo}"}}"#),
        ),
        (
            "POST",
            format!("/api/repos/{repo}/story/SH-1/dispatch"),
            "{}".to_string(),
        ),
    ] {
        let answered = fixture.as_session(&capability, method, &path, &body);
        assert_eq!(
            answered.status, 403,
            "{method} {path} was not refused: {}",
            answered.body
        );
        assert_eq!(
            answered.code().as_deref(),
            Some("session_scope"),
            "{method} {path} refused without the marker the dashboard reads, so the tab \
             cannot tell this from an authentication failure"
        );
    }
}

#[test]
fn a_capability_cannot_reach_the_cli() {
    // The largest thing a browser tab holding the master token could do, and
    // the reason excluding `/api/v1/*` by topology matters: `invoke` runs every
    // verb storyhook has, project deletion and daemon shutdown included.
    let fixture = Fixture::new();
    let capability = fixture.capability();
    let answered = fixture.as_session(
        &capability,
        "POST",
        "/api/v1/invoke",
        r#"{"invocation":{"Summary":{}}}"#,
    );
    assert!(
        answered.status == 401 || answered.status == 403,
        "a capability reached the control surface: {} {}",
        answered.status,
        answered.body
    );
}

#[test]
fn a_scope_refusal_is_never_a_401() {
    // The defect the story named in as many words: the dashboard clears its
    // credential and opens the master-token modal on a 401 and on nothing else,
    // so a scope refusal answered 401 would end with the user pasting full
    // authority into the tab. Least privilege undone by its own error path.
    let fixture = Fixture::new();
    let capability = fixture.capability();
    let refused = fixture.as_session(
        &capability,
        "POST",
        &format!("/api/repos/{}/story/SH-1/dispatch", fixture.repo),
        "{}",
    );
    assert_ne!(
        refused.status, 401,
        "a scope refusal answered 401 escalates the tab to the master token"
    );
    assert_eq!(refused.status, 403);

    // And the capability survived being refused: a refusal that also revoked
    // would be the same defect wearing a different status code.
    let still_works = fixture.as_session(
        &capability,
        "POST",
        &format!("/api/repos/{}/story/SH-1/comment", fixture.repo),
        r#"{"text":"still here"}"#,
    );
    assert_eq!(still_works.status, 200, "{}", still_works.body);
}

#[test]
fn a_capability_is_refused_from_the_tailnet_listener() {
    // A capability is worth nothing off this machine, which the master token it
    // replaced was not: `mutation_guard_ok` accepts a bound tailnet `Host`, so
    // a leaked master token is a live write credential for any tailnet peer.
    //
    // Asserted against the loopback listener with a rebound `Host`, because
    // that is the same conjunct and every machine has a loopback listener.
    let fixture = Fixture::new();
    let capability = fixture.capability();
    let refused = request(
        fixture.port(),
        "POST",
        &format!("/api/repos/{}/story/SH-1/comment", fixture.repo),
        &[
            ("Host", "evil.example"),
            ("X-Storyhook", "1"),
            ("Content-Type", "application/json"),
            (SESSION_HEADER, &capability),
        ],
        r#"{"text":"from somewhere else"}"#,
    );
    assert_eq!(
        refused.status, 403,
        "a rebound Host must be refused before a capability is even looked at: {}",
        refused.body
    );
}

// --- Revocation ---

#[test]
fn revoking_ends_a_capability_and_says_so_without_escalating_the_tab() {
    let fixture = Fixture::new();
    let capability = fixture.capability();
    let repo = &fixture.repo;

    let revoked = request(
        fixture.port(),
        "DELETE",
        "/api/v1/sessions",
        &[("Host", "127.0.0.1"), (TOKEN_HEADER, &fixture.server.token)],
        "",
    );
    assert_eq!(revoked.status, 200, "{}", revoked.body);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&revoked.body).expect("JSON")["revoked"],
        1
    );

    let after = fixture.as_session(
        &capability,
        "POST",
        &format!("/api/repos/{repo}/story/SH-1/comment"),
        r#"{"text":"after revocation"}"#,
    );
    assert_eq!(after.status, 401);
    assert_eq!(
        after.code().as_deref(),
        Some("session_unknown"),
        "a revoked capability must be distinguishable from no credential, or the tab \
         prompts for the master token and undoes the revocation's whole point"
    );
}

#[test]
fn a_capability_cannot_revoke_its_siblings() {
    // Revocation is administrative, which is why it lives on the control
    // surface. A capability reaching it would let a compromised tab lock the
    // owner out of their own dashboard.
    let fixture = Fixture::new();
    let capability = fixture.capability();
    let refused = fixture.as_session(&capability, "DELETE", "/api/v1/sessions", "");
    assert_ne!(refused.status, 200, "a capability revoked sessions");

    let still_works = fixture.as_session(
        &capability,
        "POST",
        &format!("/api/repos/{}/story/SH-1/comment", fixture.repo),
        r#"{"text":"still here"}"#,
    );
    assert_eq!(still_works.status, 200, "{}", still_works.body);
}

#[test]
fn listing_sessions_never_serves_one() {
    let fixture = Fixture::new();
    let capability = fixture.capability();
    let listed = request(
        fixture.port(),
        "GET",
        "/api/v1/sessions",
        &[("Host", "127.0.0.1"), (TOKEN_HEADER, &fixture.server.token)],
        "",
    );
    assert_eq!(listed.status, 200, "{}", listed.body);
    assert!(
        !listed.body.contains(&capability),
        "the listing route served a live capability: {}",
        listed.body
    );
}

// --- The claim the spec has to make honestly ---

#[test]
fn a_capability_authorized_move_fires_the_projects_shell_hook() {
    // **The capability reaches `sh -c`.** Not a caveat added to the spec out of
    // caution — a fact, and this is what makes it one. A hook is a shell
    // command the project's owner configured; a capability cannot choose or
    // change what runs, and no route it reaches can edit a hook or repoint a
    // checkout. But on a project with hooks configured, a browser tab holding
    // nothing but this capability causes that command to run.
    //
    // If a later change narrows the scope so this stops being true, this test
    // goes red and `docs/spec/dashboard-authorization.md` gets corrected. That
    // is the point of testing a claim rather than writing it down.
    let marker = scratch_dir();
    let output = marker.path().join("hook-ran.json");
    let fixture = Fixture::with_hooks(Some(&format!(
        "[hooks.on_state_change]\ncommand = \"cat > {}\"\n",
        output.display()
    )));
    let capability = fixture.capability();

    let moved = fixture.as_session(
        &capability,
        "POST",
        &format!("/api/repos/{}/story/SH-1/move", fixture.repo),
        r#"{"state":"in-progress"}"#,
    );
    assert_eq!(moved.status, 200, "the move was refused: {}", moved.body);

    // The hook runs alongside the write rather than inside its reply, so give
    // the shell a moment to land the file before concluding it never ran.
    for _ in 0..50 {
        if output.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        output.exists(),
        "a capability-authorized move did not fire the project's hook — if this is now \
         deliberate, the spec's `sh -c` paragraph is wrong and must be corrected rather \
         than this test relaxed"
    );
    let payload: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&output).expect("reading the hook output"))
            .expect("the hook payload is JSON");
    assert_eq!(payload["event_type"], "state_change");
}
