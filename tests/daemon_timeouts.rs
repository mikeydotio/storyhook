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
            tailnet: None,
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
        tailnet: None,
    };

    let started = Instant::now();
    assert!(lifecycle::hello(&info).is_err());
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a refused connection must fail at once, not after a timeout: took {:?}",
        started.elapsed()
    );
}

/// The fourth unbounded wait, and the one that was not an HTTP call at all.
///
/// `spawn_locked` took `daemon.spawn.lock` with `fs4`'s blocking `lock_exclusive`
/// — a `flock` with no timeout — so a client that arrived behind a holder waited
/// for it with no bound of any kind. Measured before the fix: four clients
/// against a wedged daemon at 10.16s, 20.25s, 30.33s and 40.42s, strictly
/// linear. With the lock held and never released, as here, the wait was
/// unbounded outright and this test hung until `PATIENCE` (SH-143).
///
/// The lock is held by *this process*, which is the sharpest available form of
/// the fixture: there is no holder that might finish, so anything less than a
/// real bound fails.
#[test]
fn ensure_gives_up_on_a_spawn_lock_somebody_else_holds() {
    use fs4::FileExt;

    let dir = storyhook_test_support::scratch_dir();
    let env = storyhook::env::Environment::at(dir.path());
    std::fs::create_dir_all(env.daemon_state_dir()).expect("the daemon directory");

    let held = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(env.daemon_spawn_lock())
        .expect("opening the spawn lock");
    held.try_lock_exclusive()
        .expect("this test must be the one holding it");

    let started = Instant::now();
    let result = within_patience("lifecycle::ensure behind a held spawn lock", move || {
        lifecycle::ensure(&env)
    });
    let waited = started.elapsed();

    let error = result.expect_err("a lock nobody will release cannot yield a daemon");
    assert!(
        error.to_string().contains("spawn lock"),
        "the failure must name what it was waiting for rather than describing a \
         daemon that never started: {error}"
    );
    // The bound is 30s and `PATIENCE` is 45s. The assertion is that the client's
    // own deadline fired, not the harness's — which is the whole difference
    // between a bounded wait and an unbounded one.
    assert!(
        waited < Duration::from_secs(40),
        "the client's own bound must fire well inside the harness's: took {waited:?}"
    );

    let _ = FileExt::unlock(&held);
}

/// The invoker's own exchange, which is the fourth call and the one that
/// carried the user's work.
///
/// # Why this is not simply `timeout_global`
///
/// The three calls above have no legitimate slow case, so a flat deadline is
/// right for them. This one does: it carries an import, a `github-sync`, an
/// event hook the user configured. The daemon also serves **one request at a
/// time**, so a client's wall clock includes however long somebody else's
/// command takes — which means a flat deadline here would have to be as long as
/// the longest command it might queue behind, or it would abandon healthy work.
///
/// The bound is therefore on the daemon's own published record
/// (`daemon.current.json`): the clock resets whenever that record changes, and
/// fires only when it has stopped moving. `tests/../lifecycle.rs` holds the
/// decision table as pure unit tests; these three prove the wiring, and they
/// drive the bound in milliseconds because it is a parameter (SH-144).
mod exchange {
    use super::{PATIENCE, SilentPeer, within_patience};
    use std::time::{Duration, Instant};
    use storyhook::daemon::lifecycle::{CurrentRequest, ExchangeBound};
    use storyhook::env::Environment;
    use storyhook::invoke::{HttpInvoker, Transport};

    /// The bound every test here drives, far below the production 120s.
    const DRIVEN: Duration = Duration::from_millis(250);

    fn envelope() -> storyhook::api::wire::WireRequest {
        storyhook::api::wire::WireRequest::new(
            storyhook::cli::Invocation::Version,
            std::path::Path::new("/"),
        )
    }

    /// A record naming `command`, written where a client will look for it.
    fn publish(env: &Environment, command: &str, served_deadline_secs: u64) -> CurrentRequest {
        std::fs::create_dir_all(env.daemon_state_dir()).expect("the daemon directory");
        let record = CurrentRequest {
            request_id: "somebody-elses-request".to_string(),
            command: command.to_string(),
            project: None,
            pid: std::process::id(),
            started_at: "2026-01-01T00:00:00Z".to_string(),
            served_deadline_secs,
            cwd: std::path::PathBuf::from("/"),
        };
        storyhook::daemon::lifecycle::publish_inflight(env, std::slice::from_ref(&record));
        record
    }

    /// The defect itself: a daemon that accepts and never answers used to hold
    /// its client forever.
    ///
    /// The peer publishes no record at all, which is the "never appeared"
    /// branch — and post-SH-144 the only shape an unauthenticated wedge can
    /// present to a client.
    #[test]
    fn the_exchange_gives_up_on_a_peer_that_accepts_and_never_answers() {
        let dir = storyhook_test_support::scratch_dir();
        let env = Environment::at(dir.path());
        let peer = SilentPeer::bind();
        let info = peer.as_daemon();

        let started = Instant::now();
        let result = within_patience("HttpInvoker::send against a silent peer", move || {
            HttpInvoker::send(
                &env,
                &info,
                &envelope(),
                Some(ExchangeBound::After(DRIVEN)),
                Duration::from_millis(25),
                PATIENCE,
            )
        });
        let waited = started.elapsed();

        let error = result.expect_err("a peer that never answers cannot have answered");
        let Transport::Sent(message) = error else {
            panic!("a request that left this process must be classified as Sent");
        };
        assert!(
            message.contains("may or may not have run"),
            "the client must say it cannot know whether the write landed: {message}"
        );
        assert!(
            message.contains("kill"),
            "the client must name the way out, because `story daemon stop` cannot \
             kill a daemon in this state: {message}"
        );
        // The client's own bound must fire, not the harness's. That difference
        // is the whole distinction between a bounded wait and an unbounded one.
        assert!(
            waited < Duration::from_secs(40),
            "the client's own bound must fire well inside the harness's: {waited:?}"
        );
    }

    /// **The regression this design exists for**, at the integration level: a
    /// client whose daemon is visibly working through a queue keeps waiting,
    /// however many deadlines pass.
    ///
    /// Red under any wall-clock bound, green only under one measured on the
    /// record. The record is rewritten with a fresh id on a cadence, which is
    /// what a daemon finishing one command after another looks like from here.
    #[test]
    fn a_client_behind_a_daemon_that_keeps_finishing_things_keeps_waiting() {
        let dir = storyhook_test_support::scratch_dir();
        let env = Environment::at(dir.path());
        let peer = SilentPeer::bind();
        let info = peer.as_daemon();

        // The directory must exist before the churn starts: `publish_inflight`
        // is best-effort by design, so a missing directory would make every
        // write a silent no-op and this test would pass for the wrong reason —
        // the client would be giving up on a daemon that published nothing,
        // which is a different branch entirely.
        std::fs::create_dir_all(env.daemon_state_dir()).expect("the daemon directory");

        let churning = env.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopper = stop.clone();
        let churn = std::thread::spawn(move || {
            let mut n = 0_u32;
            while !stopper.load(std::sync::atomic::Ordering::Relaxed) {
                n += 1;
                let record = CurrentRequest {
                    request_id: format!("request-{n}"),
                    command: "comment".to_string(),
                    project: None,
                    pid: std::process::id(),
                    started_at: "2026-01-01T00:00:00Z".to_string(),
                    served_deadline_secs: DRIVEN.as_secs(),
                    cwd: std::path::PathBuf::from("/"),
                };
                storyhook::daemon::lifecycle::publish_inflight(
                    &churning,
                    std::slice::from_ref(&record),
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        });

        let (tx, rx) = std::sync::mpsc::channel();
        let waiting = env.clone();
        std::thread::spawn(move || {
            let _ = tx.send(HttpInvoker::send(
                &waiting,
                &info,
                &envelope(),
                Some(ExchangeBound::After(DRIVEN)),
                Duration::from_millis(25),
                PATIENCE,
            ));
        });

        // Eight driven deadlines deep. A wall-clock bound would have fired
        // thirty times over.
        let slept = DRIVEN * 8;
        assert!(
            rx.recv_timeout(slept).is_err(),
            "a client must not give up while the daemon is visibly finishing things"
        );

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        churn.join().expect("the churn thread");
        // And once the record stops moving, it does give up.
        let outcome = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("a record that stops moving must end the wait");
        assert!(outcome.is_err(), "a silent peer cannot have answered");
    }

    /// The deadline belongs to the command in the record, so a frozen
    /// `github-sync` is waited on far longer than a frozen `comment`.
    #[test]
    fn a_frozen_github_sync_is_waited_on_longer_than_anything_else() {
        let dir = storyhook_test_support::scratch_dir();
        let env = Environment::at(dir.path());
        let peer = SilentPeer::bind();
        let info = peer.as_daemon();
        publish(
            &env,
            "github-sync",
            storyhook::daemon::lifecycle::SYNC_SERVED_DEADLINE.as_secs(),
        );

        let (tx, rx) = std::sync::mpsc::channel();
        let waiting = env.clone();
        std::thread::spawn(move || {
            // No override at all: the production deadlines apply, so the record
            // naming `github-sync` buys an hour.
            let _ = tx.send(HttpInvoker::send(
                &waiting,
                &info,
                &envelope(),
                None,
                Duration::from_millis(25),
                PATIENCE,
            ));
        });

        assert!(
            rx.recv_timeout(Duration::from_secs(3)).is_err(),
            "a frozen github-sync must not be given up on at the ordinary bound"
        );
    }
}
