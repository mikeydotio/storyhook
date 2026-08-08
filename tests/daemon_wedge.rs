//! Reproduces SH-172 and SH-177: a peer that opens a loopback connection and
//! never satisfies the `Content-Length` (or `Transfer-Encoding: chunked`) it
//! declared must not stop the daemon answering everyone else, and must not
//! be able to grow the daemon's thread and file-descriptor count without
//! bound.
//!
//! # SH-172 — off the dispatch thread
//!
//! Before SH-172, admission (the `X-Storyhook-Token` check) ran *after* the
//! body was read, on the single thread that also routed every other client's
//! command — so one peer that declared a body and never sent it wedged the
//! whole daemon solid, credential or not. The fix: `rpc::admission` runs from
//! the request head alone, before any body is read, and every request gets
//! its own thread so a stalled peer blocks only that thread.
//!
//! # SH-177 — bounding that one thread
//!
//! SH-172 left the *credentialed* case open: a peer that presents a valid
//! token, declares a body, and never finishes sending it still ties up one
//! thread and one file descriptor **forever** — the daemon's original
//! transport (`tiny_http` 0.12) gave no way to reach an accepted socket to
//! set a deadline on it, and `Server::from_listener` owned its whole accept
//! loop internally. `src/daemon/http1` (this daemon's own connection layer,
//! replacing `tiny_http`) closes this two ways: every read and write on a
//! peer socket is bound in wall-clock time by `http1::PEER_IO_TIMEOUT`, and
//! every listener shares one `http1::ConnectionSlots` cap
//! (`http1::MAX_CONNECTIONS`, overridable here via
//! `STORYHOOK_MAX_CONNECTIONS`) — so even a peer that opens connections
//! faster than they time out cannot grow the daemon past that ceiling.
//!
//! The tests below that hold a stalled peer without a valid token exercise
//! the SH-172 invariant (refused before its body is ever read, so its body
//! size never matters). [`stalled_connections_past_the_cap_are_refused_without_growing_the_daemon`]
//! exercises SH-177 directly: an *authenticated* peer that stalls mid-body,
//! which does reach the daemon's body-reading code, is still bounded.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use storyhook::daemon::lifecycle::{self, DaemonInfo};
use storyhook_test_support::{TestEnv, scratch_dir};

/// How long a probe waits for a healthy response before treating the daemon
/// as wedged. Generous relative to loopback, which answers in single-digit
/// milliseconds; tight enough that a genuine wedge fails the test in seconds
/// rather than however long the whole suite's own deadline is.
const PROBE_DEADLINE: Duration = Duration::from_secs(5);

/// Stops whatever daemon `env` is running, even if the test panics first.
struct DaemonGuard<'a>(&'a TestEnv);

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        let _ = lifecycle::stop(&self.0.environment(), lifecycle::StopMode::Force);
    }
}

/// Starts a daemon in `env` and returns what it published about itself.
fn start(env: &TestEnv) -> DaemonInfo {
    let dir = scratch_dir();
    env.story(dir.path())
        .args(["daemon", "start"])
        .assert()
        .success();
    env.daemon()
        .expect("a started daemon must publish a portfile")
}

/// Opens a loopback connection, sends a `POST /api/v1/invoke` head declaring
/// `content_length`, writes `sent` bytes of body, and returns the connection
/// still open — held by the caller for as long as the scenario needs the peer
/// to look stalled.
fn hold_stalled_invoke(port: u16, content_length: usize, sent: &[u8]) -> TcpStream {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_write_timeout(Some(PROBE_DEADLINE)).ok();
    let head = format!(
        "POST /api/v1/invoke HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {content_length}\r\n\r\n"
    );
    s.write_all(head.as_bytes()).expect("write head");
    if !sent.is_empty() {
        s.write_all(sent).expect("write partial body");
    }
    s
}

/// Like [`hold_stalled_invoke`], but with a valid bearer token — so the
/// connection clears `rpc::admission` and its body read genuinely blocks on
/// the daemon's own thread, rather than being refused before the body is
/// ever touched. This is the shape SH-177's own consequence describes: a
/// credentialed peer that stalls mid-body.
fn hold_stalled_authenticated_invoke(
    port: u16,
    token: &str,
    content_length: usize,
    sent: &[u8],
) -> TcpStream {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    s.set_write_timeout(Some(PROBE_DEADLINE)).ok();
    let head = format!(
        "POST /api/v1/invoke HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Storyhook-Token: {token}\r\nContent-Type: application/json\r\nContent-Length: {content_length}\r\n\r\n"
    );
    s.write_all(head.as_bytes()).expect("write head");
    if !sent.is_empty() {
        s.write_all(sent).expect("write partial body");
    }
    s
}

/// Times a raw, authenticated `GET /api/v1/hello`, bounded by
/// [`PROBE_DEADLINE`] so a wedged daemon fails this call rather than hanging
/// the test.
fn hello_elapsed(port: u16, token: &str) -> Result<(Duration, u16), String> {
    let t0 = Instant::now();
    let mut s = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(PROBE_DEADLINE)).ok();
    s.set_write_timeout(Some(PROBE_DEADLINE)).ok();
    write!(
        s,
        "GET /api/v1/hello HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Storyhook-Token: {token}\r\nConnection: close\r\n\r\n"
    )
    .map_err(|e| e.to_string())?;
    let mut buf = [0u8; 32];
    let n = s.read(&mut buf).map_err(|e| e.to_string())?;
    let status_line = String::from_utf8_lossy(&buf[..n]);
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Ok((t0.elapsed(), status))
}

/// The smallest reproduction: one unauthenticated peer, one declared body
/// never sent in full.
#[test]
fn one_stalled_body_over_the_buffering_threshold_does_not_stop_the_daemon() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let info = start(&env);

    let _attacker = hold_stalled_invoke(info.port, 1025, b"x");
    std::thread::sleep(Duration::from_millis(200));

    let (elapsed, status) = hello_elapsed(info.port, &info.token)
        .expect("hello must answer, not error, with one stalled >1024-byte body held");
    assert_eq!(status, 200);
    assert!(
        elapsed < Duration::from_secs(1),
        "hello took {elapsed:?} with one stalled >1024-byte body held; the daemon is wedged"
    );
}

/// The story's own shape, reproduced verbatim: two small stalled bodies plus
/// ten large ones.
#[test]
fn twelve_stalled_connections_do_not_stop_the_daemon() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let info = start(&env);

    let mut held = Vec::new();
    for _ in 0..2 {
        held.push(hold_stalled_invoke(info.port, 1000, b"0123456789"));
    }
    for _ in 0..10 {
        held.push(hold_stalled_invoke(info.port, 65000, b"x"));
    }
    std::thread::sleep(Duration::from_millis(200));

    let (elapsed, status) = hello_elapsed(info.port, &info.token)
        .expect("hello must answer, not error, with the story's own 12-connection shape held");
    assert_eq!(status, 200);
    assert!(
        elapsed < Duration::from_secs(1),
        "hello took {elapsed:?} with 12 stalled connections held; the daemon is wedged"
    );
    drop(held);
}

/// Pins the corrected mechanism against the discredited count theory: many
/// unauthenticated stalled connections, of whatever declared size, must
/// never block the daemon — there is no connection-count threshold where
/// this starts failing.
#[test]
fn a_body_under_the_buffering_threshold_never_reaches_the_daemon() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let info = start(&env);

    let mut held = Vec::new();
    for _ in 0..20 {
        held.push(hold_stalled_invoke(info.port, 1024, b"x"));
    }
    std::thread::sleep(Duration::from_millis(200));

    let (elapsed, status) = hello_elapsed(info.port, &info.token)
        .expect("hello must answer with 20 <=1024-byte stalled bodies held");
    assert_eq!(status, 200);
    assert!(
        elapsed < Duration::from_secs(1),
        "hello took {elapsed:?} with 20 buffered-class connections held"
    );
    drop(held);
}

/// The credential-free amplification named in the story's title: an
/// unauthenticated peer must be refused *before* the daemon waits on any of
/// the body it declared. Today (pre-fix) the token is not checked until after
/// the body has been read in full, so an attacker who never finishes sending
/// one gets no answer at all.
#[test]
fn an_unauthenticated_invoke_is_refused_without_its_body() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let info = start(&env);

    let mut attacker = hold_stalled_invoke(info.port, 65000, b"x");
    attacker.set_read_timeout(Some(PROBE_DEADLINE)).ok();

    let mut buf = [0u8; 32];
    let n = attacker
        .read(&mut buf)
        .expect("the daemon must answer 401 without waiting for the rest of the body");
    let status_line = String::from_utf8_lossy(&buf[..n]);
    assert!(
        status_line.starts_with("HTTP/1.1 401"),
        "expected 401, got: {status_line}"
    );
}

/// The other body shape a request can declare: `Transfer-Encoding: chunked`
/// with no chunk ever sent is a second, independent way to reach the same
/// stalled read.
#[test]
fn a_chunked_body_that_never_arrives_does_not_stop_the_daemon() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let info = start(&env);

    let mut attacker = TcpStream::connect(("127.0.0.1", info.port)).expect("connect");
    attacker.set_write_timeout(Some(PROBE_DEADLINE)).ok();
    write!(
        attacker,
        "POST /api/v1/invoke HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n"
    )
    .expect("write head");
    // No chunk ever follows — the body is declared but never sent.
    std::thread::sleep(Duration::from_millis(200));

    let (elapsed, status) = hello_elapsed(info.port, &info.token)
        .expect("hello must answer with a chunked body stalled");
    assert_eq!(status, 200);
    assert!(
        elapsed < Duration::from_secs(1),
        "hello took {elapsed:?} with a stalled chunked body held; the daemon is wedged"
    );
    drop(attacker);
}

/// SH-177's own fix, exercised end to end: an *authenticated* peer that
/// stalls mid-body reaches the daemon's blocking body read (unlike every test
/// above, which is refused before its body is touched at all) and still
/// cannot grow the daemon past its connection cap. `STORYHOOK_MAX_CONNECTIONS`
/// lowers the cap so this test fills it with a handful of sockets rather than
/// the production ceiling's worth.
///
/// Thread and file-descriptor counts are the story's own stated consequence
/// ("grow the daemon's thread count without limit"), but this test proves the
/// bound at the HTTP level instead of by counting OS resources: a connection
/// past the cap gets an explicit, immediate `503` rather than being accepted
/// and also piling up, which is only possible if the cap is in fact holding —
/// an unbounded accept path cannot produce this response, only silence or an
/// eventual accept. `http1::conn::tests::a_connection_past_the_cap_gets_no_permit`
/// pins the counting primitive itself in isolation.
#[test]
fn stalled_connections_past_the_cap_are_refused_without_growing_the_daemon() {
    const CAP: usize = 4;
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = scratch_dir();
    env.story(dir.path())
        .args(["daemon", "start"])
        .env("STORYHOOK_MAX_CONNECTIONS", CAP.to_string())
        .assert()
        .success();
    let info = env
        .daemon()
        .expect("a started daemon must publish a portfile");

    // Fill every connection slot with a peer that clears admission and then
    // stalls forever mid-body.
    let mut held = Vec::new();
    for _ in 0..CAP {
        held.push(hold_stalled_authenticated_invoke(
            info.port,
            &info.token,
            65000,
            b"x",
        ));
    }
    std::thread::sleep(Duration::from_millis(200));

    // One more connection, with every slot held: refused outright and told
    // why, not silently accepted alongside the rest. Read the response
    // head a line at a time rather than in one `read()` call: nothing
    // guarantees a multi-header response arrives in a single TCP segment,
    // and under the full suite's load it routinely does not.
    let mut extra = TcpStream::connect(("127.0.0.1", info.port)).expect("connect");
    extra.set_read_timeout(Some(PROBE_DEADLINE)).ok();
    let mut reader = BufReader::new(&mut extra);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .expect("a connection past the cap must be answered, not silently dropped");
    assert!(
        status_line.starts_with("HTTP/1.1 503"),
        "expected 503 once every connection slot is held, got: {status_line}"
    );
    let mut saw_retry_after = false;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read header line");
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if line.to_ascii_lowercase().starts_with("retry-after:") {
            saw_retry_after = true;
        }
    }
    assert!(
        saw_retry_after,
        "a refusal should carry Retry-After so a well-behaved client knows to back off"
    );
    drop(reader);
    drop(extra);

    // Releasing the peers that filled the cap frees their slots — the cap
    // recovers rather than staying wedged once they are gone.
    drop(held);
    std::thread::sleep(Duration::from_millis(200));
    let (elapsed, status) = hello_elapsed(info.port, &info.token)
        .expect("hello must answer once the cap has room again");
    assert_eq!(status, 200);
    assert!(
        elapsed < Duration::from_secs(1),
        "hello took {elapsed:?} after the cap should have recovered"
    );
}
