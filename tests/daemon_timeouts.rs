//! Every call storyhook makes to a daemon has to come back.
//!
//! # Why this file exists
//!
//! W8's concurrency soak hung for twelve minutes inside `make test`. The
//! evidence at the time: the test binary alive, one daemon alive, and **no
//! client processes at all** — so nothing was waiting on a subprocess. What was
//! waiting was a `DaemonGuard` tearing itself down, blocked in
//! `lifecycle::request_shutdown`, which posted to the daemon with no timeout of
//! any kind.
//!
//! All three of storyhook's daemon-facing HTTP calls were in that shape:
//! `hello`, `request_shutdown`, and the invoker's `send`. The GitHub client sets
//! a thirty-second global timeout; the daemon client set none, on the reasonable-
//! sounding assumption that loopback either answers or refuses.
//!
//! It does not. A process that accepts a connection and then never writes
//! anything holds its peer forever, and every way that can happen is reachable:
//! a daemon wedged on a long SQLite operation, a daemon stuck in a probe (the
//! `tailscale status` hang W0 found), or — the case `hello`'s own docstring
//! names — *something else entirely* holding the port, because "a port is a
//! number anybody can hold".
//!
//! The consequence in production is worse than in a test. `story daemon stop`
//! against a wedged daemon never returns and never says why, so the only way out
//! is a Ctrl-C and a manual `kill`; the identity check written to protect a
//! client from a stranger on the port can itself be hung by that stranger.
//!
//! # How these tests work
//!
//! A listener that accepts and never answers, and a portfile pointing at it.
//! Every call is made on a worker thread with a channel deadline, so a
//! regression fails this file in a few seconds with a name instead of stalling
//! the suite — which is precisely the failure mode being retired.

use std::io::Read;
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use storyhook::daemon::lifecycle::{self, DaemonInfo};

/// How long a test waits for a call that is supposed to give up on its own.
///
/// Comfortably beyond the client's own deadline and far short of "forever": the
/// distinction being drawn is between a bounded wait and an unbounded one, not
/// between two bounded ones.
const PATIENCE: Duration = Duration::from_secs(45);

/// A socket that accepts connections and then says nothing, ever.
///
/// Deliberately not "a socket that refuses": a refused connection is the case
/// storyhook already handles, and it is the one that cannot hang. The dangerous
/// peer is the one that is *there*.
struct SilentPeer {
    port: u16,
    _accepting: std::thread::JoinHandle<()>,
}

impl SilentPeer {
    fn bind() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binding a silent peer");
        let port = listener.local_addr().expect("a bound address").port();
        let accepting = std::thread::spawn(move || {
            // Accept and hold. Reading the request keeps the client from
            // failing on a full send buffer instead of on the missing answer,
            // which would make this a different test.
            while let Ok((mut socket, _)) = listener.accept() {
                std::thread::spawn(move || {
                    let mut sink = [0_u8; 4096];
                    while socket.read(&mut sink).is_ok_and(|n| n > 0) {}
                    // Never writes, never closes, until the process ends.
                    std::thread::sleep(Duration::from_secs(600));
                });
            }
        });
        Self {
            port,
            _accepting: accepting,
        }
    }

    /// A portfile-shaped description of this socket, claiming to be a daemon.
    fn as_daemon(&self) -> DaemonInfo {
        DaemonInfo {
            pid: std::process::id(),
            port: self.port,
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol: 1,
            exe: std::env::current_exe().expect("this test binary"),
            exe_mtime: 0,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            token: "not-the-real-token".to_string(),
            store_path: std::path::PathBuf::from("/private/tmp/storyhook-timeouts/store.db"),
        }
    }
}

/// Runs `call` on its own thread and fails the test if it has not returned
/// within [`PATIENCE`].
///
/// The thread is deliberately *not* joined on timeout: it is blocked in a
/// syscall that nothing here can interrupt, and leaving it is the price of
/// reporting the failure at all. The process ends at the end of the run.
fn within_patience<T: Send + 'static>(what: &str, call: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        let _ = tx.send(call());
    });
    match rx.recv_timeout(PATIENCE) {
        Ok(value) => value,
        Err(_) => panic!(
            "`{what}` had not returned after {:?}. A daemon call with no timeout \
             hangs its caller forever when the peer accepts and never answers — \
             which is a wedged daemon, or anything else holding the port.",
            started.elapsed()
        ),
    }
}

#[test]
fn hello_gives_up_on_a_peer_that_accepts_and_never_answers() {
    let peer = SilentPeer::bind();
    let info = peer.as_daemon();

    let result = within_patience("lifecycle::hello", move || lifecycle::hello(&info));

    let error = result.expect_err("a peer that never answers is not a healthy daemon");
    let message = error.to_string();
    assert!(
        message.contains("hello") || message.contains("daemon"),
        "the failure must say what it was asking: {message}"
    );
}

#[test]
fn request_shutdown_gives_up_on_a_peer_that_accepts_and_never_answers() {
    let peer = SilentPeer::bind();
    let info = peer.as_daemon();

    let result = within_patience("lifecycle::request_shutdown", move || {
        lifecycle::request_shutdown(&info)
    });

    let error = result.expect_err("a peer that never answers has not shut down");
    assert!(
        error.to_string().contains("shut down"),
        "the failure must say what it was asking: {error}"
    );
}

/// A refused connection must stay fast and stay *distinguishable*. The timeout
/// added for the silent peer must not turn "nothing is listening" — which
/// storyhook relies on to mean "nothing was delivered, so sending again is a
/// first attempt" — into a slow, ambiguous failure.
#[test]
fn a_refused_connection_still_fails_immediately() {
    let port = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("binding");
        listener.local_addr().expect("an address").port()
        // dropped here, so nothing is listening on `port`
    };
    let info = DaemonInfo {
        pid: std::process::id(),
        port,
        version: env!("CARGO_PKG_VERSION").to_string(),
        protocol: 1,
        exe: std::env::current_exe().expect("this test binary"),
        exe_mtime: 0,
        started_at: "2026-01-01T00:00:00Z".to_string(),
        token: "irrelevant".to_string(),
        store_path: std::path::PathBuf::from("/private/tmp/storyhook-timeouts/store.db"),
    };

    let started = Instant::now();
    assert!(lifecycle::hello(&info).is_err());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a refused connection must fail at once, not after a timeout: took {:?}",
        started.elapsed()
    );
}
