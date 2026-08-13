//! A client for the daemon's change feed (`GET /api/events`).
//!
//! Every other in-process consumer of a daemon speaks through
//! [`crate::invoke::HttpInvoker`], which is built on `ureq`. This is a raw
//! [`TcpStream`] instead, and that is deliberate rather than an oversight:
//! `ureq`'s only body deadline (`timeout_recv_body`) bounds the *whole*
//! response body, which is the wrong shape for a connection meant to stay
//! open all day — it would kill a healthy stream on a timer. What a
//! heartbeat-driven watchdog needs is a deadline on each individual read, and
//! `TcpStream::set_read_timeout` is exactly that. The wire is entirely ours in
//! both directions — loopback, plain HTTP/1.1, one fixed request, framing
//! written by [`crate::api::http::write_sse_frame`] — so the cost of rolling
//! it by hand is a small chunked-transfer parser, not a general HTTP client.
//!
//! [`Subscriber`] is the whole surface: connect, read the next [`Change`], and
//! reconnect transparently when the connection drops. A lost connection is
//! never reported as an error — it is reported as one [`Change::Resync`] once
//! the replacement is up, because a gap in the stream is a gap in what the
//! caller knows, and the honest instruction for that is "refetch", the same
//! instruction [`ChangeBus`](crate::daemon::bus::ChangeBus) gives its own
//! overflowing subscribers.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::thread;
use std::time::{Duration, Instant};

use crate::daemon::bus::Change;
use crate::daemon::lifecycle::{self, DaemonInfo};
use crate::env::Environment;
use crate::error::AppError;

/// How many missed heartbeats make a quiet connection dead rather than merely
/// idle. Two, not one: the interval boundary has its own scheduling jitter,
/// and a connection must not be torn down for being late to its own first
/// heartbeat.
const WATCHDOG_MULTIPLE: u32 = 2;

/// The socket's own read-timeout granularity. Independent of the daemon's
/// heartbeat interval — this only bounds how promptly a call to
/// [`Subscriber::poll`] notices its own deadline or a `stop` flag the caller
/// is checking between calls, the same role `EventSource`'s other threads
/// give their own poll intervals (crossterm's 100ms, the 250ms tick).
const READ_POLL: Duration = Duration::from_millis(200);

/// How long one connection attempt — the TCP connect plus waiting for the
/// daemon's `: connected` frame — may take before it counts as failed.
///
/// Deliberately **not** derived from the heartbeat interval, unlike
/// [`WATCHDOG_MULTIPLE`]: that constant answers "how long may an established
/// connection stay quiet", which is a property of the daemon's own publish
/// cadence. This answers "how long may loopback take to say hello", which
/// is not — [`Subscriber::daemon_info`] just confirmed the daemon it named is
/// live, and five seconds is already generous for a `TcpStream::connect` and
/// a handshake on loopback. Keeping the two separate is what keeps a
/// repeatedly-failing reconnect loop's per-attempt cost bounded at a few
/// seconds rather than at `heartbeat * WATCHDOG_MULTIPLE` (40s in
/// production).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a failed reconnect attempt waits before trying again, so a
/// daemon that is simply gone does not turn into a tight loop of TCP
/// connects.
const RECONNECT_BACKOFF: Duration = Duration::from_millis(500);

/// A live (or lapsed) subscription to one daemon's change feed, reconnecting
/// on its own whenever the connection is lost.
///
/// **Never spawns a daemon.** That is deliberate, not an oversight:
/// [`lifecycle::ensure`] decides whether to spawn by asking `is_this_binary`
/// — "is the daemon in the portfile running the same executable as *me*?" —
/// which answers the right question for a CLI command about to run work
/// through it, and the wrong one for a subscriber, which is never a
/// candidate to spawn one itself. A live TUI session's next keystroke already
/// calls `HttpInvoker::invoke`, which calls `lifecycle::ensure` on its own
/// behalf; the subscriber's job is only to notice when a daemon becomes
/// live and follow it there, which is why reconnecting re-reads the portfile
/// and checks liveness rather than asking "is it me".
pub struct Subscriber {
    env: Environment,
    /// The daemon info the caller already resolved. Kept, not spent: every
    /// attempt retries `seed` until the *first* connection actually
    /// succeeds, on the premise that a daemon a caller just resolved is
    /// worth a few retries before assuming it needs replacing.
    /// [`Self::connected_once`] is the switch: once it is `true`, every
    /// later reconnect re-reads the portfile instead, because *that*
    /// connection really did work and then stopped, which is what makes "a
    /// different daemon now holds this store" the likelier explanation.
    seed: DaemonInfo,
    connected_once: bool,
    heartbeat: Duration,
    conn: Option<Connection>,
}

/// The hex-length-prefixed chunk framing this wire uses, decoded
/// incrementally so a read-timeout mid-chunk resumes on the next call instead
/// of losing whatever bytes already arrived.
enum ChunkState {
    /// Waiting for (the rest of) the size line.
    Size,
    /// Waiting for `needed` more bytes: the chunk's payload plus its trailing
    /// CRLF. `write_sse_frame` flushes once per [`Change`], so on this wire a
    /// whole chunk is always exactly one SSE frame — but TCP does not
    /// preserve write boundaries, so the bytes of *this* chunk may still
    /// arrive split across several reads.
    Body { needed: usize, buf: Vec<u8> },
}

struct Connection {
    reader: BufReader<TcpStream>,
    last_activity: Instant,
    line: String,
    state: ChunkState,
}

impl Connection {
    /// Advances the chunk parser by whatever the socket has ready right now.
    /// `Ok(Some(frame))` is one complete SSE frame's text (the same shape
    /// [`Change::to_sse`] produces, minus nothing — [`Change::from_sse`]
    /// parses it directly). `Ok(None)` means the read timed out with no
    /// frame yet complete — a normal tick, not a failure — and every byte
    /// read so far stays in `self` for the next call.
    fn advance(&mut self) -> io::Result<Option<String>> {
        loop {
            match &mut self.state {
                ChunkState::Size => match self.reader.read_line(&mut self.line) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "the change feed closed",
                        ));
                    }
                    Ok(_) if self.line.ends_with('\n') => {
                        let trimmed = self.line.trim_end().to_string();
                        self.line.clear();
                        if trimmed.is_empty() {
                            // A stray blank line between chunks: not part of
                            // this wire's framing, but harmless to skip
                            // rather than treat as a parse failure.
                            continue;
                        }
                        let size = usize::from_str_radix(&trimmed, 16).map_err(|_| {
                            io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("not a chunk size: {trimmed:?}"),
                            )
                        })?;
                        self.state = ChunkState::Body {
                            needed: size + 2, // the payload, plus its trailing CRLF
                            buf: Vec::with_capacity(size + 2),
                        };
                    }
                    // A partial line: `read_line` appends rather than
                    // overwrites, so `self.line` already holds what arrived
                    // and the next call picks up from there.
                    Ok(_) => return Ok(None),
                    Err(e) if is_timeout(&e) => return Ok(None),
                    Err(e) => return Err(e),
                },
                ChunkState::Body { needed, buf } => {
                    while buf.len() < *needed {
                        let mut tmp = vec![0u8; *needed - buf.len()];
                        match self.reader.read(&mut tmp) {
                            Ok(0) => {
                                return Err(io::Error::new(
                                    io::ErrorKind::UnexpectedEof,
                                    "the change feed closed mid-chunk",
                                ));
                            }
                            Ok(n) => buf.extend_from_slice(&tmp[..n]),
                            Err(e) if is_timeout(&e) => return Ok(None),
                            Err(e) => return Err(e),
                        }
                    }
                    let size = *needed - 2;
                    let frame = String::from_utf8_lossy(&buf[..size]).into_owned();
                    self.state = ChunkState::Size;
                    return Ok(Some(frame));
                }
            }
        }
    }
}

fn is_timeout(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

impl Subscriber {
    /// Names the daemon `daemon` already resolved, for the very first
    /// connection — so a caller that just called [`lifecycle::ensure`] to
    /// learn the port does not pay for a second one here. Nothing connects
    /// yet; [`Self::connect`] does that.
    pub fn new(env: Environment, daemon: DaemonInfo, heartbeat: Duration) -> Self {
        Self {
            env,
            seed: daemon,
            connected_once: false,
            heartbeat,
            conn: None,
        }
    }

    /// Connects (or reconnects) and blocks until the daemon's own
    /// `: connected` frame arrives.
    ///
    /// That block is load-bearing, not cosmetic: a caller that gets `Ok`
    /// back knows the daemon has already run `bus.subscribe()`, so any write
    /// this process makes after this call returns cannot land in the gap
    /// SH-140 found in the store-polling version of this mechanism — a
    /// baseline read taken before the listener actually existed.
    pub fn connect(&mut self) -> Result<(), AppError> {
        let daemon = self.daemon_info()?;
        self.conn = Some(Self::open(&daemon)?);
        self.connected_once = true;
        Ok(())
    }

    /// The daemon to (re)connect to.
    ///
    /// Deliberately not [`lifecycle::ensure`]: that function spawns a daemon
    /// if none is usable, and decides "usable" partly by `is_this_binary` —
    /// is the *calling process* the same executable as the one in the
    /// portfile. Correct for a CLI command, which is a legitimate candidate
    /// to spawn a replacement; wrong for a subscriber, which is not, and
    /// which would otherwise try to spawn a daemon from whatever process
    /// happens to own it (fatal if that process is not `story` itself). A
    /// live daemon found this way is trusted the same way `lifecycle::ensure`
    /// trusts one it did not have to spawn: it is running, and it names this
    /// store.
    fn daemon_info(&mut self) -> Result<DaemonInfo, AppError> {
        if !self.connected_once {
            return Ok(self.seed.clone());
        }
        lifecycle::read_info(&self.env)
            .filter(|info| info.serves(self.env.store_path()) && lifecycle::is_live(&self.env))
            .ok_or_else(|| {
                AppError::Storage("no live daemon is currently serving this store".to_string())
            })
    }

    fn open(daemon: &DaemonInfo) -> Result<Connection, AppError> {
        let addr = format!("127.0.0.1:{}", daemon.port);
        let stream = TcpStream::connect(&addr).map_err(|e| {
            AppError::Storage(format!(
                "connecting to the daemon's change feed at {addr}: {e}"
            ))
        })?;
        stream.set_nodelay(true).ok();
        stream.set_read_timeout(Some(READ_POLL)).ok();

        let mut writer = stream
            .try_clone()
            .map_err(|e| AppError::Storage(format!("cloning the change-feed connection: {e}")))?;
        write!(
            writer,
            "GET /api/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{}: {}\r\nConnection: keep-alive\r\n\r\n",
            daemon.port,
            crate::api::rpc::TOKEN_HEADER,
            daemon.token,
        )
        .and_then(|()| writer.flush())
        .map_err(|e| AppError::Storage(format!("sending the change-feed request: {e}")))?;

        let mut conn = Connection {
            reader: BufReader::new(stream),
            last_activity: Instant::now(),
            line: String::new(),
            state: ChunkState::Size,
        };

        read_head(&mut conn.reader, CONNECT_TIMEOUT)
            .map_err(|e| AppError::Storage(format!("reading the change-feed response: {e}")))?;

        // Block for the first frame -- `serve_sse`'s `: connected` sentinel,
        // written before anything else. `Change::from_sse` returns `None`
        // for it, which is correct: it is not a change, it is proof the
        // subscription exists.
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        loop {
            if Instant::now() >= deadline {
                return Err(AppError::Storage(
                    "the daemon accepted the change-feed connection but never confirmed it"
                        .to_string(),
                ));
            }
            match conn.advance() {
                Ok(Some(_)) => break,
                Ok(None) => continue,
                Err(e) => {
                    return Err(AppError::Storage(format!(
                        "reading the change-feed connection: {e}"
                    )));
                }
            }
        }
        conn.last_activity = Instant::now();
        Ok(conn)
    }

    /// The next change worth acting on, waiting up to `budget`.
    ///
    /// `None` means nothing to report and the connection is healthy — a
    /// normal outcome, not a failure, exactly as an unchanged
    /// `PRAGMA data_version` used to mean nothing happened. A ping, or an
    /// event name this build does not recognise, is proof of life and also
    /// answers `None`.
    ///
    /// A connection that dies — a read error, silence past the watchdog, or
    /// a [`Change::Reload`] (the daemon's own ahead-of-restart notice,
    /// followed immediately rather than waited out) — is replaced
    /// transparently. The replacement's first answer is always
    /// `Some(Change::Resync)`: the gap is a gap in what the caller knows, and
    /// the honest instruction for that is "refetch", not a caller-visible
    /// error.
    pub fn poll(&mut self, budget: Duration) -> Option<Change> {
        let deadline = Instant::now() + budget;

        if self.conn.is_none() {
            // Retried within `budget`, paced by `RECONNECT_BACKOFF`, rather
            // than one attempt per call: a caller that polls back-to-back
            // (as `EventSource`'s change thread does) would otherwise turn a
            // truly absent daemon into a tight loop of TCP connects, since
            // each failed attempt returns near-instantly.
            loop {
                if let Some(change) = self.reconnect_and_resync() {
                    return Some(change);
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return None;
                }
                thread::sleep(RECONNECT_BACKOFF.min(remaining));
            }
        }

        loop {
            if Instant::now() >= deadline {
                return None;
            }

            let stale = self.conn.as_ref().is_some_and(|conn| {
                conn.last_activity.elapsed() > self.heartbeat * WATCHDOG_MULTIPLE
            });
            if stale {
                self.conn = None;
                return self.reconnect_and_resync();
            }

            let conn = self.conn.as_mut().expect("checked above");
            match conn.advance() {
                Ok(Some(text)) => {
                    conn.last_activity = Instant::now();
                    match Change::from_sse(&text) {
                        // Proof of life; nothing for the caller to act on.
                        Some(Change::Ping) => {}
                        // The daemon is telling every client to reconnect on
                        // purpose -- sent ahead of a version-skew restart, so
                        // this follows it there immediately rather than
                        // waiting for the connection to die of its own
                        // accord, which is what `web_dashboard.html`'s
                        // `EventSource` does with the same event.
                        Some(Change::Reload) => {
                            self.conn = None;
                            return self.reconnect_and_resync();
                        }
                        Some(change) => return Some(change),
                        // An event name this build does not recognise: still
                        // proof of life, same as a ping.
                        None => {}
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    self.conn = None;
                    return self.reconnect_and_resync();
                }
            }
        }
    }

    /// One reconnect attempt. `Some(Change::Resync)` on success — the gap in
    /// the stream is a gap in what the caller knows, so the honest
    /// instruction is "refetch" — `None` if this attempt failed.
    fn reconnect_and_resync(&mut self) -> Option<Change> {
        match self.connect() {
            Ok(()) => Some(Change::Resync),
            Err(_) => None,
        }
    }
}

/// Reads and discards the HTTP response head, leaving `reader` positioned at
/// the first byte of the chunked SSE body.
fn read_head(reader: &mut BufReader<TcpStream>, budget: Duration) -> io::Result<()> {
    let deadline = Instant::now() + budget;
    loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "no response head from the daemon",
            ));
        }
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed while reading the response head",
                ));
            }
            Ok(_) if line == "\r\n" => return Ok(()),
            Ok(_) => {}
            Err(e) if is_timeout(&e) => {}
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use crate::store::{SqliteStore, Store as _};

    fn scratch() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("storyhook-subscribe-")
            .tempdir_in("/private/tmp")
            .expect("a scratch directory")
    }

    /// A [`DaemonInfo`] naming `port` and `token`, with every other field a
    /// value that will never plausibly be checked here: `/api/events`
    /// carries no version handshake, only [`Subscriber`]'s own use of `port`
    /// and `token` is exercised. `token` must be the real one `serve()`
    /// minted (SH-187: every route requires it now, `/api/events` included)
    /// rather than a placeholder, or `connect()` gets a 401.
    fn daemon_at(port: u16, token: &str) -> DaemonInfo {
        DaemonInfo {
            pid: std::process::id(),
            port,
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol: 1,
            exe: std::env::current_exe().expect("this test binary"),
            exe_mtime: 0,
            started_at: "2026-01-01T00:00:00Z".to_string(),
            token: token.to_string(),
            store_path: std::path::PathBuf::new(),
            tailnet: None,
            cookie_name: "storyhook_test".to_string(),
        }
    }

    /// Starts an in-process server and returns everything a caller needs to
    /// keep it alive for the rest of the test: the scratch directory must
    /// outlive the call (its `TempDir` is returned rather than dropped here),
    /// since the server runs on a detached thread that keeps reading and
    /// writing under it for as long as the test binary does.
    ///
    /// Not `storyhook_test_support::serve` -- that helper's signature takes
    /// this crate's own `SqliteStore`/`Environment`, and a unit test compiled
    /// as part of *this* crate's `--lib` test target is a second, distinct
    /// instance of it from the type system's point of view, so the two
    /// `SqliteStore`s do not unify. `bind_and_serve` is what that helper
    /// wraps; called directly here, everything stays the same crate instance.
    fn serve() -> (
        u16,
        String,
        Environment,
        Arc<SqliteStore>,
        tempfile::TempDir,
    ) {
        use std::sync::mpsc;

        let dir = scratch();
        let env = Environment::at(dir.path());
        let store = Arc::new(SqliteStore::open(env.store_path()).expect("opening the store"));
        store.migrate().expect("migrating");

        let (tx, rx) = mpsc::channel();
        let serving_store = Arc::clone(&store);
        let serving_env = env.clone();
        std::thread::spawn(move || {
            let _ = crate::daemon::serve::bind_and_serve(
                &*serving_store,
                &serving_env,
                0,
                move |bound, token| {
                    let _ = tx.send((bound.port(), token));
                },
            );
        });
        let (port, token) = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the test server never became ready");
        (port, token, env, store, dir)
    }

    #[test]
    fn connect_blocks_until_the_daemon_confirms_the_subscription() {
        let (port, token, _env, _store, _dir) = serve();
        let daemon = daemon_at(port, &token);
        // The subscriber's own `env` only matters for a reconnect, which
        // this test never triggers.
        let env = Environment::at("/private/tmp");
        let mut subscriber = Subscriber::new(env, daemon, Duration::from_secs(20));
        subscriber.connect().expect("connecting to the change feed");
    }

    #[test]
    fn a_write_made_after_connecting_is_reported() {
        use crate::store::{Store as _, WriteOps as _};

        let (port, token, env, store, _dir) = serve();
        let daemon = daemon_at(port, &token);
        let mut subscriber = Subscriber::new(env, daemon, Duration::from_secs(20));
        subscriber.connect().expect("connecting to the change feed");

        // Ordered strictly after `connect` returns -- the property under
        // test is that nothing here can land in a subscribe-vs-baseline gap,
        // because there is no baseline: `connect` already proved the
        // subscription exists before this line runs.
        store
            .write(|tx| {
                tx.create_project(&crate::store::NewProject {
                    uuid: "uuid-pf".to_string(),
                    slug: "pf".to_string(),
                    name: "Project".to_string(),
                    prefix: "PF".to_string(),
                    created_at: "2026-01-01T00:00:00Z".to_string(),
                })
            })
            .expect("writing a project");

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                Instant::now() < deadline,
                "no change was reported within the deadline"
            );
            if subscriber.poll(Duration::from_millis(500)).is_some() {
                return;
            }
        }
    }

    /// A listener standing in for a daemon that accepts a connection, answers
    /// the SSE handshake once, and then vanishes without a clean close --
    /// exactly the shape a dropped daemon leaves behind.
    fn one_shot_listener() -> u16 {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").expect("binding a one-shot listener");
        let port = listener.local_addr().expect("the bound address").port();
        std::thread::spawn(move || {
            let Ok((stream, _)) = listener.accept() else {
                return;
            };
            let mut reader = BufReader::new(stream.try_clone().expect("cloning the stream"));
            let mut writer = stream;
            // Drain the request head before answering, so the client's
            // write never blocks on a peer that stopped reading.
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 0) && line != "\r\n" {
                line.clear();
            }
            let _ = crate::api::http::write_sse_head(&mut writer);
            let _ = crate::api::http::write_sse_frame(&mut writer, "retry: 3000\n: connected\n\n");
            // Then the peer closes -- no more frames, no clean shutdown.
        });
        port
    }

    /// The low-level half of "a dead connection is detected": once the
    /// one-shot listener above closes, [`Connection::advance`] must surface
    /// that as an error rather than hanging or reporting `Ok(None)` forever.
    ///
    /// A [`Connection`]-level test rather than a [`Subscriber::poll`] one
    /// because that is the layer this property belongs to; `poll`'s own
    /// reconnect behaviour, restoring a live subscription after a real daemon
    /// restart, is exercised end to end in `tests/change_feed_subscriber.rs`,
    /// where `CARGO_BIN_EXE_story` provides the real daemon.
    #[test]
    fn a_closed_connection_is_reported_as_an_error_not_a_silent_timeout() {
        let port = one_shot_listener();
        let stream = TcpStream::connect(("127.0.0.1", port)).expect("connecting");
        stream
            .set_read_timeout(Some(Duration::from_millis(200)))
            .ok();
        let mut writer = stream.try_clone().expect("cloning the stream");
        write!(writer, "GET /api/events HTTP/1.1\r\nHost: x\r\n\r\n").expect("writing the request");
        writer.flush().expect("flushing the request");

        let mut conn = Connection {
            reader: BufReader::new(stream),
            last_activity: Instant::now(),
            line: String::new(),
            state: ChunkState::Size,
        };
        read_head(&mut conn.reader, Duration::from_secs(5)).expect("reading the response head");

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                Instant::now() < deadline,
                "the connected sentinel never arrived"
            );
            match conn.advance() {
                Ok(Some(_)) => break, // the `: connected` sentinel
                Ok(None) => continue,
                Err(e) => panic!("unexpected error awaiting the sentinel: {e}"),
            }
        }

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                Instant::now() < deadline,
                "a closed connection was never reported as an error"
            );
            match conn.advance() {
                Ok(Some(_)) => panic!("no further frame should arrive from a closed peer"),
                Ok(None) => continue,
                Err(_) => return, // the assertion: a closed peer surfaces as `Err`
            }
        }
    }
}
