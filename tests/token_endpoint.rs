//! Named, persistent, revocable dashboard tokens, over a real socket (SH-255).
//!
//! `src/api/tokens.rs`'s own unit tests own the registry's properties — expiry,
//! the clock's monotonic floor, revocation deleting the record outright — and
//! the wire gate's shape, by calling `intercept` directly. What is here is
//! everything that is only true once the module is wired into a daemon
//! (`src/daemon/serve.rs`'s `worker`): that `/api/v1/tokens` is actually
//! reachable over a socket, that a full mint→list→revoke round trip works end
//! to end, and that nothing this daemon serves ever carries a minted secret.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use storyhook::api::rpc::TOKEN_HEADER;
use storyhook::api::tokens::TOKENS_PATH;
use storyhook::store::SqliteStore;
use storyhook_test_support::{TestEnv, TestServer, serve};

/// One raw HTTP response, split into the parts these tests assert on.
struct RawResponse {
    status: u16,
    body: String,
}

/// Sends one request with exactly the headers given, and reads the whole
/// response. See `tests/handoff_endpoint.rs`'s identical helper for why this
/// is a raw socket rather than an HTTP client.
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
    let status = head
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("unparseable status line: {head:?}"));

    RawResponse {
        status,
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

fn master_headers(token: &str) -> Vec<(&'static str, String)> {
    vec![
        ("Host", "127.0.0.1".to_string()),
        (TOKEN_HEADER, token.to_string()),
    ]
}

fn borrowed<'a>(headers: &'a [(&'static str, String)]) -> Vec<(&'a str, &'a str)> {
    headers
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect()
}

#[test]
fn a_token_can_be_minted_listed_and_revoked_over_the_wire() {
    let (_env, _store, server) = served();
    let headers = master_headers(&server.token);

    let minted = request(
        server.port(),
        "POST",
        &format!("{TOKENS_PATH}?name=laptop"),
        &borrowed(&headers),
    );
    assert_eq!(minted.status, 200, "mint failed: {}", minted.body);
    let parsed: serde_json::Value =
        serde_json::from_str(&minted.body).expect("the mint reply is JSON");
    let secret = parsed["token"]
        .as_str()
        .expect("the mint reply names the raw secret")
        .to_string();
    assert_eq!(parsed["name"], "laptop");

    let listed = request(server.port(), "GET", TOKENS_PATH, &borrowed(&headers));
    assert_eq!(listed.status, 200);
    assert!(
        listed.body.contains("laptop"),
        "a listed token must be named: {}",
        listed.body
    );
    assert!(
        !listed.body.contains(&secret),
        "a listing must never carry the raw secret: {}",
        listed.body
    );

    let revoked = request(
        server.port(),
        "DELETE",
        &format!("{TOKENS_PATH}/laptop"),
        &borrowed(&headers),
    );
    assert_eq!(revoked.status, 200, "revoke failed: {}", revoked.body);

    let listed_again = request(server.port(), "GET", TOKENS_PATH, &borrowed(&headers));
    assert!(
        !listed_again.body.contains("laptop"),
        "a revoked token must not remain listed: {}",
        listed_again.body
    );
}

#[test]
fn minting_needs_the_master_token_over_the_wire() {
    let (_env, _store, server) = served();

    for headers in [
        vec![("Host", "127.0.0.1")],
        vec![("Host", "127.0.0.1"), (TOKEN_HEADER, "not-the-token")],
    ] {
        let refused = request(
            server.port(),
            "POST",
            &format!("{TOKENS_PATH}?name=laptop"),
            &headers,
        );
        assert!(
            refused.status == 401 || refused.status == 403,
            "an unauthenticated mint must be refused, got {}",
            refused.status
        );
        assert!(
            !refused.body.contains("\"token\":"),
            "a refused mint must not mint one: {}",
            refused.body
        );
    }
}

#[test]
fn a_duplicate_name_is_refused_over_the_wire() {
    let (_env, _store, server) = served();
    let headers = master_headers(&server.token);

    let first = request(
        server.port(),
        "POST",
        &format!("{TOKENS_PATH}?name=laptop"),
        &borrowed(&headers),
    );
    assert_eq!(first.status, 200);

    let second = request(
        server.port(),
        "POST",
        &format!("{TOKENS_PATH}?name=laptop"),
        &borrowed(&headers),
    );
    assert_eq!(second.status, 409, "a duplicate name must be refused");
}

#[test]
fn revoking_an_unknown_name_over_the_wire_is_404() {
    let (_env, _store, server) = served();
    let headers = master_headers(&server.token);

    let refused = request(
        server.port(),
        "DELETE",
        &format!("{TOKENS_PATH}/nope"),
        &borrowed(&headers),
    );
    assert_eq!(refused.status, 404);
}

#[test]
fn nothing_this_daemon_serves_contains_a_minted_secret() {
    let (_env, _store, server) = served();
    let headers = master_headers(&server.token);

    let minted = request(
        server.port(),
        "POST",
        &format!("{TOKENS_PATH}?name=laptop"),
        &borrowed(&headers),
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&minted.body).expect("the mint reply is JSON");
    let secret = parsed["token"].as_str().expect("a raw secret").to_string();

    for path in ["/", "/api/repos", TOKENS_PATH] {
        let served = request(server.port(), "GET", path, &[("Host", "127.0.0.1")]);
        assert!(
            !served.body.contains(&secret),
            "GET {path} served a minted secret in its body"
        );
    }
}
