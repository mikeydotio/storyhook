//! `POST /token` — the named-token cookie exchange — over a real socket
//! (SH-255, hardened by SH-319).
//!
//! `src/api/tokens.rs`'s own unit tests own `handle_exchange`'s truth table
//! by calling `intercept_exchange` directly; `src/api/admission.rs`'s own
//! unit tests own `same_origin_read`'s layered predicate by calling
//! `admission`/`named_token_ok` directly. Neither goes over a socket, so
//! neither can catch a route missing from `src/daemon/serve.rs`'s `worker`,
//! a header a real `TcpStream` round trip mangles, or — the gap this file
//! closes — that the cookie an exchange sets over the wire actually survives
//! a fresh connection ("new tab") and a second server bound to the same
//! store ("daemon restart"), the two triggers SH-319 was filed against that
//! no earlier test exercised together with a real exchange.
//!
//! See `tests/handoff_endpoint.rs`'s module doc for why requests are written
//! by hand onto a `TcpStream` rather than through a client library.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use storyhook::api::rpc::TOKEN_HEADER;
use storyhook::api::tokens::{EXCHANGE_PATH, TOKENS_PATH};
use storyhook::store::SqliteStore;
use storyhook_test_support::{TestEnv, TestServer, serve};

/// One raw HTTP response, split into the parts these tests assert on. See
/// `tests/handoff_endpoint.rs`'s identical type for why headers are kept.
struct RawResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl RawResponse {
    fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(field, _)| *field == name)
            .map(|(_, value)| value.as_str())
    }
}

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

/// A daemon serving an isolated store — no project needed, since none of
/// these routes touch one.
fn served() -> (TestEnv, Arc<SqliteStore>, TestServer) {
    let env = TestEnv::isolated();
    let store = Arc::new(env.open_store());
    let environment = env.environment();
    let server = serve(Arc::clone(&store), &environment);
    (env, store, server)
}

/// Mints a named token over `/api/v1/tokens`, the way `story token new`
/// does, and returns the raw secret.
fn mint_named_token(port: u16, master_token: &str, name: &str) -> String {
    let minted = request(
        port,
        "POST",
        &format!("{TOKENS_PATH}?name={name}"),
        &[("Host", "127.0.0.1"), (TOKEN_HEADER, master_token)],
    );
    assert_eq!(minted.status, 200, "minting failed: {}", minted.body);
    let parsed: serde_json::Value =
        serde_json::from_str(&minted.body).expect("the mint reply is JSON");
    parsed["token"]
        .as_str()
        .expect("the mint reply names the raw secret")
        .to_string()
}

/// Exchanges `token` for a cookie the way `submitTokenModal()` does, and
/// returns the raw response so a caller can inspect `Set-Cookie` or a
/// refusal.
fn exchange(port: u16, token: &str) -> RawResponse {
    request(
        port,
        "POST",
        EXCHANGE_PATH,
        &[
            ("Host", "127.0.0.1"),
            ("X-Storyhook", "1"),
            (TOKEN_HEADER, token),
        ],
    )
}

/// The `name=value` pair out of a `Set-Cookie` header, ready to replay in a
/// `Cookie` header on a later request.
fn cookie_pair(set_cookie: &str) -> &str {
    set_cookie
        .split(';')
        .next()
        .expect("a Set-Cookie header always has at least a name=value pair")
}

/// Opens a raw `GET /api/events` connection with `headers` and returns just
/// the HTTP status line — `/api/events` never closes its own connection, so
/// [`request`]'s `read_to_end` would hang forever on it. See
/// `tests/web_test.rs`'s identical `sse_status_line` for the same reasoning.
fn sse_status_line(port: u16, headers: &[(&str, &str)]) -> String {
    use std::io::BufRead;

    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connecting to /api/events");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("setting a read timeout");
    let mut head = "GET /api/events HTTP/1.1\r\n".to_string();
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");
    stream
        .write_all(head.as_bytes())
        .expect("writing the SSE request");
    let mut line = String::new();
    std::io::BufReader::new(stream)
        .read_line(&mut line)
        .expect("reading the SSE status line");
    line
}

#[test]
fn exchanging_a_named_token_sets_a_cookie_good_for_its_remaining_life() {
    let (_env, _store, server) = served();
    let secret = mint_named_token(server.port(), &server.token, "laptop");

    let exchanged = exchange(server.port(), &secret);
    assert_eq!(exchanged.status, 204, "exchange failed: {}", exchanged.body);
    assert!(exchanged.body.is_empty());

    let cookie = exchanged
        .header("set-cookie")
        .unwrap_or_else(|| panic!("no Set-Cookie on: {:?}", exchanged.headers));
    // DEFAULT_TTL is 30 days; freshly minted, so the remaining life is that
    // whole span, give or take the wall-clock skew between mint and
    // exchange in this test process.
    let max_age: i64 = cookie
        .split(';')
        .find_map(|part| part.trim().strip_prefix("Max-Age="))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("no parseable Max-Age in {cookie:?}"));
    let thirty_days_secs = 30 * 24 * 60 * 60;
    assert!(
        (thirty_days_secs - 60..=thirty_days_secs).contains(&max_age),
        "Max-Age {max_age} should be within a minute of a fresh 30-day token's remaining life"
    );
}

#[test]
fn the_retried_read_succeeds_via_x_storyhook_with_no_sec_fetch_site_at_all() {
    // The exact shape of SH-319's reproduced loop on a non-potentially-
    // trustworthy origin (any plain-http tailnet/LAN host): a real browser
    // there never sends Sec-Fetch-Site, but the dashboard's XHR reads all
    // send X-Storyhook. This is what makes the retry succeed instead of
    // looping back to the modal forever.
    let (_env, _store, server) = served();
    let secret = mint_named_token(server.port(), &server.token, "laptop");

    let exchanged = exchange(server.port(), &secret);
    let cookie = cookie_pair(exchanged.header("set-cookie").expect("Set-Cookie"));

    let read = request(
        server.port(),
        "GET",
        "/api/repos",
        &[
            ("Host", "127.0.0.1"),
            ("X-Storyhook", "1"),
            ("Cookie", cookie),
        ],
    );
    assert_eq!(read.status, 200, "{}", read.body);
}

#[test]
fn a_read_with_neither_x_storyhook_nor_sec_fetch_site_nor_referer_is_refused() {
    // Red before SH-319's fix landed on a non-loopback origin: this exact
    // header shape is what a real tailnet browser sends on the retried
    // request, and it must still be refused when none of the three proofs
    // is present at all -- fail closed, not fail open.
    let (_env, _store, server) = served();
    let secret = mint_named_token(server.port(), &server.token, "laptop");
    let exchanged = exchange(server.port(), &secret);
    let cookie = cookie_pair(exchanged.header("set-cookie").expect("Set-Cookie"));

    let read = request(
        server.port(),
        "GET",
        "/api/repos",
        &[("Host", "127.0.0.1"), ("Cookie", cookie)],
    );
    assert_eq!(read.status, 401, "{}", read.body);
}

#[test]
fn a_matching_referer_admits_the_events_stream_when_sec_fetch_site_never_arrives() {
    // EventSource cannot set X-Storyhook, so this is the one caller that
    // still needs the Referer fallback -- and it is real on the wire, not
    // just in the unit-tested predicate.
    let (_env, _store, server) = served();
    let secret = mint_named_token(server.port(), &server.token, "laptop");
    let exchanged = exchange(server.port(), &secret);
    let cookie = cookie_pair(exchanged.header("set-cookie").expect("Set-Cookie"));

    let status_line = sse_status_line(
        server.port(),
        &[
            ("Host", &format!("127.0.0.1:{}", server.port())),
            ("Cookie", cookie),
            ("Referer", &format!("http://127.0.0.1:{}/", server.port())),
        ],
    );
    assert!(
        status_line.contains("200"),
        "a matching Referer must admit the events stream, got: {status_line:?}"
    );
}

#[test]
fn a_foreign_referer_never_admits_a_read() {
    let (_env, _store, server) = served();
    let secret = mint_named_token(server.port(), &server.token, "laptop");
    let exchanged = exchange(server.port(), &secret);
    let cookie = cookie_pair(exchanged.header("set-cookie").expect("Set-Cookie"));

    let status_line = sse_status_line(
        server.port(),
        &[
            ("Host", &format!("127.0.0.1:{}", server.port())),
            ("Cookie", cookie),
            ("Referer", "http://evil.example/"),
        ],
    );
    assert!(
        status_line.contains("401"),
        "a foreign Referer must never admit the events stream, got: {status_line:?}"
    );
}

#[test]
fn a_fresh_connection_carrying_only_the_cookie_is_admitted() {
    // The "new tab" trigger: a second, wholly independent TCP connection
    // that never exchanged anything itself -- only the cookie a prior
    // connection's exchange produced.
    let (_env, _store, server) = served();
    let secret = mint_named_token(server.port(), &server.token, "laptop");
    let exchanged = exchange(server.port(), &secret);
    let cookie = cookie_pair(exchanged.header("set-cookie").expect("Set-Cookie"));

    let read = request(
        server.port(),
        "GET",
        "/api/repos",
        &[
            ("Host", "127.0.0.1"),
            ("X-Storyhook", "1"),
            ("Cookie", cookie),
        ],
    );
    assert_eq!(read.status, 200, "{}", read.body);
}

#[test]
fn the_cookie_survives_a_daemon_restart() {
    // Named tokens persist in tokens.json under the store's own state
    // directory (TokenRegistry::load), independent of any one daemon
    // process -- a second server bound to a fresh port over the same store
    // and environment is this harness's equivalent of "the daemon
    // restarted," the same substitution `tests/change_feed_subscriber.rs`
    // uses for the identical reason.
    let env = TestEnv::isolated();
    let store = Arc::new(env.open_store());
    let environment = env.environment();
    let first = serve(Arc::clone(&store), &environment);
    let secret = mint_named_token(first.port(), &first.token, "laptop");
    let exchanged = exchange(first.port(), &secret);
    let cookie = cookie_pair(exchanged.header("set-cookie").expect("Set-Cookie"));

    let second = serve(Arc::clone(&store), &environment);
    assert_ne!(
        first.port(),
        second.port(),
        "the restart must land on a different port, the way a real one does"
    );

    let read = request(
        second.port(),
        "GET",
        "/api/repos",
        &[
            ("Host", "127.0.0.1"),
            ("X-Storyhook", "1"),
            ("Cookie", cookie),
        ],
    );
    assert_eq!(
        read.status, 200,
        "a named token's cookie must survive a daemon restart: {}",
        read.body
    );
}

#[test]
fn exchanging_the_master_token_is_refused_distinguishably_and_sets_no_cookie() {
    let (_env, _store, server) = served();
    let refused = exchange(server.port(), &server.token);
    assert_eq!(
        refused.status, 422,
        "the master token must be refused distinguishably from an unknown value: {}",
        refused.body
    );
    assert!(
        refused.header("set-cookie").is_none(),
        "a refused exchange must never set a cookie"
    );
    assert!(
        refused.body.contains("story token new"),
        "the refusal must name what to paste instead: {}",
        refused.body
    );
}

#[test]
fn exchanging_an_unknown_value_still_gets_the_ordinary_401() {
    let (_env, _store, server) = served();
    let refused = exchange(server.port(), "not-a-real-token-at-all");
    assert_eq!(refused.status, 401, "{}", refused.body);
    assert!(refused.header("set-cookie").is_none());
}

#[test]
fn exchange_needs_the_csrf_guard_over_the_wire() {
    let (_env, _store, server) = served();
    let secret = mint_named_token(server.port(), &server.token, "laptop");

    let no_marker = request(
        server.port(),
        "POST",
        EXCHANGE_PATH,
        &[("Host", "127.0.0.1"), (TOKEN_HEADER, &secret)],
    );
    assert_eq!(no_marker.status, 403);
    assert!(no_marker.header("set-cookie").is_none());

    let rebound_host = request(
        server.port(),
        "POST",
        EXCHANGE_PATH,
        &[
            ("Host", "evil.example"),
            ("X-Storyhook", "1"),
            (TOKEN_HEADER, &secret),
        ],
    );
    assert_eq!(rebound_host.status, 403);
    assert!(rebound_host.header("set-cookie").is_none());
}
