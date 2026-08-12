//! The one-shot handoff coupon, over a real socket (SH-251).
//!
//! `src/api/handoff.rs`'s own unit tests own the registry's *worthlessness* —
//! redeem-twice, redeem-past-TTL, a spray that cannot disarm — and the gate's
//! truth table, because those are properties of types with an injected clock
//! and are provable without a network. What is here is everything that is only
//! true once the module is wired into a daemon: that the two routes exist at
//! the paths the client is written against, that the reply carries the right
//! cache and CORS headers on the wire, that a spoofed `Host` is refused by a
//! server rather than by a function, and that the token never appears in
//! anything this daemon serves.
//!
//! Requests are written by hand onto a `TcpStream` rather than through a
//! client library, because half of what is asserted here is about headers a
//! library sets for you: `Host` is exactly what a DNS-rebinding attack
//! controls, and no HTTP client worth using will let a caller lie about it.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use storyhook::api::handoff::{ARM_PATH, COUPON_HEADER, REDEEM_PATH};
use storyhook::api::rpc::TOKEN_HEADER;
use storyhook::api::session::SESSION_HEADER;
use storyhook::cli::parse_invocation;
use storyhook::invoke::dispatch_unscoped;
use storyhook::store::SqliteStore;
use storyhook_test_support::{TestEnv, TestServer, scratch_dir, serve};

/// One raw HTTP response, split into the parts these tests assert on.
struct RawResponse {
    status: u16,
    /// Every header, name lowercased, in the order received.
    headers: Vec<(String, String)>,
    body: String,
}

impl RawResponse {
    /// The first value for `name` (case-insensitive), or `None`.
    fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(field, _)| *field == name)
            .map(|(_, value)| value.as_str())
    }

    /// Every header whose name starts with `prefix` (lowercased).
    fn headers_starting_with(&self, prefix: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(field, _)| field.starts_with(prefix))
            .map(|(field, _)| field.as_str())
            .collect()
    }
}

/// Sends one request with exactly the headers given — no more — and reads the
/// whole response.
///
/// `Connection: close` is the only header added, so the read terminates
/// without needing to parse a length. Nothing here is a general-purpose
/// client: it exists so a test can send a `Host` no real client would.
fn request(port: u16, method: &str, path: &str, headers: &[(&str, &str)]) -> RawResponse {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connecting to the daemon");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("setting a read timeout");

    let mut head = format!("{method} {path} HTTP/1.1\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("Content-Length: 0\r\nConnection: close\r\n\r\n");
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

    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("unparseable status line: {status_line:?}"));
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();

    RawResponse {
        status,
        headers,
        body: body.to_string(),
    }
}

/// A daemon serving an isolated store with one project in it.
///
/// The project exists so that `GET /` has something real to serve — the
/// "nothing this daemon serves carries the token" assertion is worth more
/// against a populated store than an empty one.
fn served() -> (TestEnv, Arc<SqliteStore>, TestServer) {
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

    let server = serve(Arc::clone(&store), &environment);
    (env, store, server)
}

/// Arms a coupon the way `story web open` does, and returns it.
fn arm(port: u16, token: &str) -> String {
    let armed = request(
        port,
        "POST",
        ARM_PATH,
        &[("Host", "127.0.0.1"), (TOKEN_HEADER, token)],
    );
    assert_eq!(armed.status, 200, "arming failed: {}", armed.body);
    let parsed: serde_json::Value =
        serde_json::from_str(&armed.body).expect("the arming reply is JSON");
    parsed["coupon"]
        .as_str()
        .expect("the arming reply names a coupon")
        .to_string()
}

/// The headers a browser sends to redeem a coupon at 127.0.0.1.
fn redeem_headers(port: u16, coupon: &str) -> Vec<(&'static str, String)> {
    vec![
        ("Host", format!("127.0.0.1:{port}")),
        ("X-Storyhook", "1".to_string()),
        (COUPON_HEADER, coupon.to_string()),
    ]
}

/// Sends a redemption with `headers`, borrowed into the shape [`request`]
/// wants.
fn redeem(port: u16, headers: &[(&str, String)]) -> RawResponse {
    let borrowed: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    request(port, "POST", REDEEM_PATH, &borrowed)
}

#[test]
fn a_coupon_armed_over_the_control_surface_redeems_for_a_capability() {
    // The whole feature in one test: `story web open` arms, the browser
    // spends, and what comes back is a credential that works -- with nobody
    // having typed anything.
    //
    // What comes back changed in SH-254. It was this daemon's master token,
    // which made the browser tab able to delete every project on the machine
    // and to reach `POST /api/v1/invoke`. It is a scoped capability now, and
    // the two assertions below are the two halves of that: the token is not in
    // the reply, and what *is* in the reply authorizes a write.
    let (_env, _store, server) = served();
    let coupon = arm(server.port(), &server.token);

    let redeemed = redeem(server.port(), &redeem_headers(server.port(), &coupon));
    assert_eq!(redeemed.status, 200, "redemption failed: {}", redeemed.body);
    assert!(
        !redeemed.body.contains(&server.token),
        "redemption handed the browser the master token: {}",
        redeemed.body
    );

    let parsed: serde_json::Value =
        serde_json::from_str(&redeemed.body).expect("the redemption reply is JSON");
    let capability = parsed["session"]
        .as_str()
        .expect("redemption must answer with a session capability");

    // And the capability actually works, which is the only thing that makes
    // the exchange worth anything. A read, because this fixture has no project
    // to write to -- `tests/session_capability.rs` carries the write half.
    let read = request(
        server.port(),
        "GET",
        "/api/repos",
        &[("Host", "127.0.0.1"), (SESSION_HEADER, capability)],
    );
    assert_eq!(read.status, 200);
}

#[test]
fn the_redemption_reply_is_never_stored_and_never_cors() {
    // Two headers, for two different attackers. `no-store` is what keeps the
    // one reply in this daemon that carries a credential out of a browser's
    // disk cache -- `no-cache`, which every other dynamic reply here uses,
    // explicitly permits storing. The absence of every `Access-Control-*`
    // header is what makes the `X-Storyhook` conjunct mean anything: a custom
    // header forces a preflight, and a preflight this daemon never answers is
    // a cross-origin request the browser never sends.
    let (_env, _store, server) = served();
    let coupon = arm(server.port(), &server.token);
    let redeemed = redeem(server.port(), &redeem_headers(server.port(), &coupon));

    assert_eq!(redeemed.header("cache-control"), Some("no-store"));
    assert_eq!(
        redeemed.headers_starting_with("access-control"),
        Vec::<&str>::new(),
        "no CORS header may ever appear on the redemption route"
    );

    // The preflight itself is refused rather than answered, so there is no
    // second path to the same reply.
    let preflight = request(
        server.port(),
        "OPTIONS",
        REDEEM_PATH,
        &[
            ("Host", "127.0.0.1"),
            ("Origin", "https://evil.example"),
            ("Access-Control-Request-Method", "POST"),
            ("Access-Control-Request-Headers", "x-storyhook"),
        ],
    );
    assert_eq!(preflight.status, 403);
    assert_eq!(
        preflight.headers_starting_with("access-control"),
        Vec::<&str>::new()
    );
}

#[test]
fn a_second_redemption_of_the_same_coupon_is_refused() {
    let (_env, _store, server) = served();
    let coupon = arm(server.port(), &server.token);

    assert_eq!(
        redeem(server.port(), &redeem_headers(server.port(), &coupon)).status,
        200
    );
    let second = redeem(server.port(), &redeem_headers(server.port(), &coupon));
    assert_eq!(second.status, 403);
    assert!(
        !second.body.contains(&server.token),
        "a refused redemption must not carry the token"
    );
}

#[test]
fn a_redemption_from_a_rebound_host_is_refused() {
    // The assertion that needs a raw socket: a DNS-rebound page is
    // same-origin with http://127.0.0.1:PORT and can read whatever comes
    // back, so the `Host` conjunct is the only thing between it and the
    // master token. No HTTP client would let this test lie about `Host`.
    let (_env, _store, server) = served();
    let coupon = arm(server.port(), &server.token);

    let mut rebound = redeem_headers(server.port(), &coupon);
    rebound[0] = ("Host", "evil.example".to_string());
    let refused = redeem(server.port(), &rebound);
    assert_eq!(refused.status, 403);
    assert!(!refused.body.contains(&server.token));

    // The coupon was not spent by the attempt, so a rebound page cannot even
    // deny the real browser its handoff.
    assert_eq!(
        redeem(server.port(), &redeem_headers(server.port(), &coupon)).status,
        200
    );
}

#[test]
fn arming_needs_the_token_over_the_wire() {
    // `/api/v1/*` is loopback-and-token-gated by `rpc::admission` before
    // `handoff::intercept` is ever reached, and the module checks again on
    // its own account. Either refusal is correct; what must never happen is
    // a coupon coming back.
    let (_env, _store, server) = served();

    for headers in [
        vec![("Host", "127.0.0.1")],
        vec![("Host", "127.0.0.1"), (TOKEN_HEADER, "not-the-token")],
    ] {
        let refused = request(server.port(), "POST", ARM_PATH, &headers);
        assert!(
            refused.status == 401 || refused.status == 403,
            "an unauthenticated arm must be refused, got {}",
            refused.status
        );
        assert!(
            !refused.body.contains("coupon"),
            "a refused arm must not mint one"
        );
    }
}

#[test]
fn the_redemption_route_lives_outside_api_and_stays_there() {
    // The topology is the design, not a detail. `admission::admission` claims
    // `["api", ..]` and falls through on everything else, so redemption at
    // `/handoff` stands on its own gate rather than inheriting SH-250's
    // tokenless *read* exemption -- which is also what frees the verb, since
    // that exemption admits only `GET` and `HEAD`. Moved under `/api`, this
    // route would either become a side-effecting GET or need the very token
    // it exists to deliver.
    assert!(
        !REDEEM_PATH.starts_with("/api"),
        "redemption must not inherit the /api admission surface"
    );

    let (_env, _store, server) = served();
    let coupon = arm(server.port(), &server.token);

    // The same request under `/api` is not a redemption at all.
    let mut headers = redeem_headers(server.port(), &coupon);
    headers.push((TOKEN_HEADER, server.token.clone()));
    let borrowed: Vec<(&str, &str)> = headers
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect();
    for path in ["/api/handoff", "/api/v1/handoff/redeem"] {
        let elsewhere = request(server.port(), "POST", path, &borrowed);
        assert!(
            !elsewhere.body.contains(&server.token),
            "{path} must not answer with the token"
        );
    }

    // And the coupon survived every one of those, so redemption really does
    // happen at exactly one place.
    assert_eq!(
        redeem(server.port(), &redeem_headers(server.port(), &coupon)).status,
        200
    );
}

#[test]
fn nothing_this_daemon_serves_contains_the_token() {
    // The QUESTION's own constraint, and until now it was pinned by nothing
    // but the fact that nobody had put it there. A rebound page is
    // same-origin with the SPA shell and can read it; the whole reason the
    // coupon travels in a *fragment* is that the token must never be
    // templated into anything served.
    let (_env, _store, server) = served();
    let coupon = arm(server.port(), &server.token);

    for path in [
        "/",
        "/?project=sh",
        "/index.html",
        "/api/repos",
        REDEEM_PATH,
        ARM_PATH,
    ] {
        let served = request(server.port(), "GET", path, &[("Host", "127.0.0.1")]);
        assert!(
            !served.body.contains(&server.token),
            "GET {path} served the token in its body"
        );
        assert!(
            !served.body.contains(&coupon),
            "GET {path} served a live coupon in its body"
        );
    }
}

/// The witness's constructor set, pinned by reading the source — the
/// `invoker_seam.rs::the_legacy_write_path_is_gone` idiom.
///
/// `LocalRequest` has a private field, so nothing outside `src/api/http.rs`
/// *can* construct one; that much is a compile error and needs no test. What
/// this pins is the weaker, still load-bearing property: that the number of
/// places deriving locality stays two, and that a third does not appear
/// quietly. The council made this binding because the alternative — a
/// belt-and-braces check defended by nothing but a comment — is exactly what
/// a later reviewer deletes as redundant.
#[test]
fn locality_is_derived_only_where_it_was_decided_it_should_be() {
    use std::path::Path;

    fn sources(dir: &Path, into: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).expect("reading src/") {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                sources(&path, into);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let text = std::fs::read_to_string(&path).expect("reading a source file");
                into.push((path.to_string_lossy().into_owned(), text));
            }
        }
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    assert!(
        files.len() > 20,
        "expected the whole tree, got {}",
        files.len()
    );

    // `http.rs` defines the type and its one constructor. `admission.rs` is
    // SH-250's tokenless read exemption; `handoff.rs` is SH-251's redemption
    // gate; `session.rs` is SH-254's scoped dashboard capability, which is
    // honoured only for a caller on this machine -- the single largest thing
    // that story takes away, since the master token it replaced was spendable
    // from any tailnet peer.
    //
    // Adding a fifth means a new route has been given the authority to conclude
    // "this caller is local" -- which may well be right, and must be a decision
    // somebody took on purpose rather than a line that slipped in. Widening
    // this list is that decision, and this comment is where the reason goes.
    const ALLOWED: [&str; 4] = [
        "src/api/http.rs",
        "src/api/admission.rs",
        "src/api/handoff.rs",
        "src/api/session.rs",
    ];

    let mut breaches = Vec::new();
    for (path, text) in &files {
        let relative = path
            .rsplit_once("/src/")
            .map(|(_, rest)| format!("src/{rest}"))
            .unwrap_or_else(|| path.clone());
        if ALLOWED.contains(&relative.as_str()) {
            continue;
        }
        for (number, line) in text.lines().enumerate() {
            // A doc comment may name the type; naming is not deriving.
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            if code.contains("LocalRequest") || code.contains("local_request(") {
                breaches.push(format!("{relative}:{}: {}", number + 1, code.trim()));
            }
        }
    }
    assert!(
        breaches.is_empty(),
        "locality may only be derived in {ALLOWED:?}, but:\n{}",
        breaches.join("\n")
    );

    // The positive half: all three named files really do still name it, so
    // this test cannot pass by the whole mechanism having been deleted.
    for allowed in ALLOWED {
        let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(allowed))
            .unwrap_or_else(|e| panic!("reading {allowed}: {e}"));
        assert!(
            text.contains("local_request"),
            "{allowed} no longer derives locality — has the witness been removed?"
        );
    }
}
