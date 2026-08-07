//! Reproduces SH-172: an unauthenticated peer that opens a loopback
//! connection and never satisfies the `Content-Length` it declared must not
//! stop the daemon answering everyone else.
//!
//! # The mechanism, established before any fix was written
//!
//! Confirmed against `tiny_http` 0.12.0's own source (`src/request.rs:194-213`
//! in the vendored crate) and reproduced against a real daemon before this
//! file existed: a body of at most 1024 bytes with no `Expect: 100-continue`
//! is read to completion on the connection's own `tiny_http` pool thread,
//! before the request ever reaches this daemon's own accept loop. A body of
//! more than 1024 bytes is wrapped in a lazy `EqualReader` instead, and this
//! daemon's own body-reading code — synchronous and peer-paced — blocks
//! whichever thread calls it, with no socket deadline available to bound that
//! block (see `serve::accept_loop`'s doc comment on why, and SH-177 for the
//! successor story that tracks it). The fix here is not a deadline: it is
//! moving that blocking read off the one thread every client's command is
//! queued behind, onto a thread of its own per connection.
//!
//! **It is a body-size class, not a connection count.** Twenty connections
//! held at exactly 1024 bytes never touch the daemon at all; one connection
//! held at 1025 wedges a serial dispatcher solid. The story's own probe (2
//! connections: safe; 12: wedged) reads as a threshold only because none of
//! its first two connections crossed 1024 bytes.

use std::io::{Read, Write};
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

/// The smallest reproduction: `tiny_http` buffers <=1024 bytes inline on the
/// connection's own thread, so 1025 is the smallest body that streams — and
/// streaming is what reaches this daemon's own read path.
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
/// connections in the buffered class must never reach the daemon at all, no
/// matter how many are held at once.
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

/// The other streamed body shape: chunked encoding is never eagerly buffered
/// by `tiny_http` regardless of size, so a chunked request that never sends
/// its first chunk is a second, independent way to reach the same read.
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
