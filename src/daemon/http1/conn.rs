//! Owning the accepted socket: deadlines, the connection cap, and the
//! per-connection request loop.
//!
//! This is the module SH-177 exists to add. `tiny_http` (SH-172's transport)
//! gave no way to reach an accepted socket at all — `Server::from_listener`
//! owns the whole accept loop internally, and a `Request`'s body reader was
//! an opaque `Box<dyn Read>` with no file descriptor behind it. Everything
//! here runs on the thread that `accept()`ed the connection, so a deadline
//! set here applies to the socket the peer actually holds.
//!
//! # One thread per connection, not per request
//!
//! A kept-alive connection serves every request it carries on the same
//! thread, in a loop — cheaper than SH-172's one-thread-per-request, and it
//! is what makes [`ConnectionSlots`] a bound on the daemon's *own* thread
//! count, not just a bound on how many peers can be waited on at once.
//!
//! # Deadlines bound wall-clock time, not just one syscall
//!
//! A bare `SO_RCVTIMEO` bounds a single `read()` call, and nothing stops a
//! peer from staying just under it forever — one byte every 29 seconds holds
//! a connection open indefinitely against a naive 30-second-per-call limit.
//! [`Deadline`] instead fixes an absolute instant when it is created and
//! shrinks the socket's timeout on every call that follows, so the *sum* of
//! however many reads or writes a phase takes is bounded, not just each one
//! individually. A fresh [`Deadline`] is created for each phase — waiting for
//! a request head, reading its body, writing its response — because the
//! daemon's own processing time between phases (dispatch can legitimately
//! run for tens of seconds) must never count against a peer-I/O budget.

use std::cell::{Cell, RefCell};
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::body::{BodyState, PeerReader};
use super::message::{Header, Method, Response, write_response};
use super::parse::{Framing, Head, ParseError, ParseOutcome, parse_head};

/// The read/write deadline on every peer socket, and the total wall-clock
/// budget for reading a request head or a request body — see the module
/// docs for why one constant does all three. Also serves as the keep-alive
/// idle timeout: how long a connection may sit between requests before this
/// daemon closes it.
///
/// Chosen above `ureq`'s `max_idle_age` (15s, `ureq::config::Config`) so a
/// pooled client always discards a connection on its own before this daemon
/// would close it out from under an in-flight request —
/// `an_idle_client_gives_up_before_the_daemon_does` pins the inequality
/// against `ureq`'s own constant rather than restating it.
pub const PEER_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Bounds a dribbled request head's memory. Any real request line and header
/// block is a small fraction of this.
pub const MAX_HEAD_BYTES: usize = 16 * 1024;

/// Bounds a dribbled request head's header count, independent of its byte
/// size (many tiny headers cost `httparse` parsing effort even if they fit
/// in [`MAX_HEAD_BYTES`]).
pub const MAX_HEADERS: usize = 64;

/// The daemon-wide cap on concurrent connections — shared across every
/// listener this daemon binds (loopback and, when present, the tailnet
/// interface), so the bound is per daemon rather than per interface. This is
/// SH-177's actual fix for the story's stated consequence: without it, a
/// peer that opens connections faster than [`PEER_IO_TIMEOUT`] expires them
/// can still grow the daemon's thread count without limit.
///
/// Sized well above any legitimate concurrent use: one connection per `story`
/// CLI invocation, one per open dashboard tab's SSE stream, and up to
/// `DISPATCHERS` (8, `crate::daemon::serve`) requests actually being routed
/// at once.
///
/// Overridable via `STORYHOOK_MAX_CONNECTIONS`, the same shape
/// `serve::dispatchers` already uses for its own pool size — so a test can
/// prove the refusal path fires without opening hundreds of real sockets.
pub const MAX_CONNECTIONS: usize = 128;

fn max_connections() -> usize {
    std::env::var("STORYHOOK_MAX_CONNECTIONS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(MAX_CONNECTIONS)
}

/// How long the accept loop itself will wait to write a `503` to a
/// connection refused for being over [`ConnectionSlots`]'s ceiling, before
/// giving up on that one write and moving on. Short and fixed, unlike
/// [`PEER_IO_TIMEOUT`]: this write happens *on the accept thread*, so an
/// unbounded wait here would let the same class of peer that this cap exists
/// to stop also stall the accept loop that enforces it.
const REFUSAL_WRITE_BUDGET: Duration = Duration::from_secs(1);

/// The size and time bounds a connection is held to. `Copy` — cheap enough
/// to hand to every accepted connection by value.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub(super) peer_io: Duration,
    pub(super) max_head_bytes: usize,
    pub(super) max_headers: usize,
}

impl Limits {
    pub fn production() -> Limits {
        Limits {
            peer_io: PEER_IO_TIMEOUT,
            max_head_bytes: MAX_HEAD_BYTES,
            max_headers: MAX_HEADERS,
        }
    }

    /// A `Limits` with a caller-chosen `peer_io` budget, for a test that
    /// needs a deadline to actually fire without waiting out the production
    /// 30 seconds.
    #[cfg(test)]
    pub fn for_tests(peer_io: Duration) -> Limits {
        Limits {
            peer_io,
            max_head_bytes: MAX_HEAD_BYTES,
            max_headers: MAX_HEADERS,
        }
    }
}

/// An absolute instant a peer-I/O phase must finish by. See the module docs:
/// this is what makes the bound wall-clock rather than per-syscall.
pub(super) struct Deadline {
    at: Instant,
}

impl Deadline {
    pub(super) fn new(budget: Duration) -> Deadline {
        Deadline {
            at: Instant::now() + budget,
        }
    }

    /// Time left, or a `TimedOut` error if the deadline has already passed.
    /// Floored just above zero: an OS-level socket timeout of exactly zero
    /// means "block forever" on at least one platform's `setsockopt`, which
    /// would silently *disable* the very bound this exists to enforce, so
    /// this never asks for one.
    fn remaining(&self) -> io::Result<Duration> {
        let now = Instant::now();
        if now >= self.at {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "peer I/O deadline exceeded",
            ))
        } else {
            Ok((self.at - now).max(Duration::from_millis(1)))
        }
    }

    pub(super) fn arm_read(&self, stream: &TcpStream) -> io::Result<()> {
        stream.set_read_timeout(Some(self.remaining()?))
    }

    fn arm_write(&self, stream: &TcpStream) -> io::Result<()> {
        stream.set_write_timeout(Some(self.remaining()?))
    }
}

/// A read or write timeout, however this platform happens to report it —
/// Unix typically surfaces `WouldBlock`, Windows `TimedOut` (documented on
/// `TcpStream::set_write_timeout`).
fn is_deadline_error(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

/// The daemon-wide connection cap: one atomic counter, shared by every
/// listener's accept loop via [`Arc`].
pub struct ConnectionSlots {
    held: AtomicUsize,
    ceiling: usize,
}

impl ConnectionSlots {
    pub fn new(ceiling: usize) -> Arc<ConnectionSlots> {
        Arc::new(ConnectionSlots {
            held: AtomicUsize::new(0),
            ceiling,
        })
    }

    pub fn production() -> Arc<ConnectionSlots> {
        Self::new(max_connections())
    }

    /// Reserves one slot, or `None` if every slot is already held. `None`
    /// means the caller must not spawn a thread for this connection — the
    /// whole point of the cap.
    fn try_acquire(self: &Arc<Self>) -> Option<Permit> {
        let mut held = self.held.load(Ordering::Acquire);
        loop {
            if held >= self.ceiling {
                return None;
            }
            match self.held.compare_exchange_weak(
                held,
                held + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(Permit {
                        slots: Arc::clone(self),
                    });
                }
                Err(actual) => held = actual,
            }
        }
    }
}

/// Holds one [`ConnectionSlots`] reservation for a connection's whole
/// lifetime. Releasing on `Drop` — rather than at some explicit "I'm done"
/// call — is what keeps the slot released on every exit path out of
/// [`serve_one_connection`], panics included.
struct Permit {
    slots: Arc<ConnectionSlots>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.slots.held.fetch_sub(1, Ordering::AcqRel);
    }
}

/// What a [`Request`] tells the connection loop to do once its handler
/// returns. Shared via `Rc<Cell<_>>` rather than returned from the handler
/// directly, because the handler's signature (`Fn(Request)`) is fixed by
/// `tiny_http`'s original shape, which this module stays compatible with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The handler returned without answering — a bug, or an early return
    /// this module did not anticipate. Treated the same as `Close`: closing
    /// an unanswered connection is always safe, continuing one is not.
    Unanswered,
    /// Answered, and the connection may serve another request.
    KeepAlive,
    /// Answered (or handed off, via `into_writer`) — close after this.
    Close,
}

/// One accepted connection's request, mid-flight.
///
/// Shares its socket with the connection loop via `Rc<RefCell<TcpStream>>`
/// rather than owning it outright: [`Request::respond`] needs the socket
/// back in the loop's hands afterward so a kept-alive connection can read
/// its next request, and passing ownership back out through `respond`'s
/// fixed `io::Result<()>` return type has no room for that. Everything here
/// runs on one thread — the connection's — so the extra reference counting
/// this costs is uncontended.
pub struct Request {
    stream: Rc<RefCell<TcpStream>>,
    head: Head,
    body: BodyState,
    /// The body's byte source — see [`PeerReader`] on why this exists at
    /// all: a leftover byte read alongside the head must still reach the
    /// body decoder, not be silently dropped.
    peer: PeerReader,
    /// Created lazily, on the first byte actually read from the body —
    /// not at head-parse time, so time this daemon itself spends between
    /// parsing a head and starting to read its body never counts against a
    /// peer-I/O budget.
    body_deadline: Option<Deadline>,
    limits: Limits,
    client_wants_keep_alive: bool,
    outcome: Rc<Cell<Outcome>>,
}

impl Request {
    fn new(
        stream: Rc<RefCell<TcpStream>>,
        head: Head,
        framing: Framing,
        leftover: Vec<u8>,
        limits: Limits,
        outcome: Rc<Cell<Outcome>>,
    ) -> Request {
        let client_wants_keep_alive = wants_keep_alive(&head.headers, head.http_version);
        let peer = PeerReader::new(Rc::clone(&stream), leftover);
        Request {
            stream,
            body: BodyState::from_framing(framing),
            peer,
            head,
            body_deadline: None,
            limits,
            client_wants_keep_alive,
            outcome,
        }
    }

    pub fn method(&self) -> &Method {
        &self.head.method
    }

    pub fn url(&self) -> &str {
        &self.head.url
    }

    pub fn headers(&self) -> &[Header] {
        &self.head.headers
    }

    pub fn as_reader(&mut self) -> &mut dyn Read {
        self
    }

    /// Sends `response` and decides, from three things — whether the client
    /// asked to keep the connection alive, whether its declared body was
    /// actually read to the end, and whether the write itself succeeded —
    /// whether the connection may serve another request.
    ///
    /// An unconsumed body forces a close rather than being drained here:
    /// draining is exactly the block-forever trap SH-172 found in
    /// `tiny_http`'s `EqualReader::drop` (an unauthenticated `/api/v1/*`
    /// request is refused from its header alone, in
    /// [`crate::api::rpc::admission`], specifically so its body is never
    /// read at all — this is the code path that leaves it unconsumed).
    pub fn respond(self, response: Response) -> io::Result<()> {
        let keep_alive = self.client_wants_keep_alive && self.body.fully_consumed();
        let suppress_body =
            *self.method() == Method::Head || matches!(response.status.0, 100..=199 | 204 | 304);
        let deadline = Deadline::new(self.limits.peer_io);
        let result = {
            let mut stream = self.stream.borrow_mut();
            deadline
                .arm_write(&stream)
                .and_then(|()| write_response(&mut *stream, &response, keep_alive, suppress_body))
        };
        self.outcome.set(if result.is_ok() && keep_alive {
            Outcome::KeepAlive
        } else {
            Outcome::Close
        });
        result
    }

    /// Hands the raw connection to the caller for the rest of its lifetime
    /// (SSE: `crate::daemon::serve::serve_sse`). Always closes afterward —
    /// the connection loop stops the moment the handler returns, since the
    /// stream the returned writer holds is the same one the loop would
    /// otherwise try to read the next request from.
    pub fn into_writer(self) -> Box<dyn Write> {
        self.outcome.set(Outcome::Close);
        Box::new(RequestWriter {
            stream: self.stream,
            limits: self.limits,
        })
    }
}

impl Read for Request {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let limits = self.limits;
        let deadline = self
            .body_deadline
            .get_or_insert_with(|| Deadline::new(limits.peer_io));
        self.body.read(&mut self.peer, deadline, buf)
    }
}

/// Whether `headers`/`version` ask for the connection to stay open past this
/// response — HTTP/1.1 defaults to yes, HTTP/1.0 to no, and an explicit
/// `Connection` header always wins over the version default.
fn wants_keep_alive(headers: &[Header], version: (u8, u8)) -> bool {
    let connection = headers
        .iter()
        .find(|h| h.field.equiv("Connection"))
        .map(|h| h.value.to_ascii_lowercase());
    match connection {
        Some(v) if v.split(',').any(|t| t.trim() == "close") => false,
        Some(v) if v.split(',').any(|t| t.trim() == "keep-alive") => true,
        _ => version.1 >= 1,
    }
}

/// The raw connection, handed to [`crate::daemon::serve::serve_sse`] once a
/// request has committed to a streaming response. Every write gets its own
/// fresh [`PEER_IO_TIMEOUT`] budget — an SSE heartbeat fires every 20s
/// (`crate::daemon::serve::heartbeat_interval`), comfortably inside that —
/// so a browser tab that stops reading (a laptop asleep, a dead network) is
/// disconnected rather than holding this thread forever, which is exactly
/// the bound this module exists to give every peer socket, streaming ones
/// included.
struct RequestWriter {
    stream: Rc<RefCell<TcpStream>>,
    limits: Limits,
}

impl Write for RequestWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let deadline = Deadline::new(self.limits.peer_io);
        let mut stream = self.stream.borrow_mut();
        deadline.arm_write(&stream)?;
        stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.borrow_mut().flush()
    }
}

/// What became of one attempt to read a request head.
enum HeadOutcome {
    /// The head, its body's framing, and any bytes read alongside the head
    /// that belong to the body rather than to it — `httparse` reports only
    /// how many bytes of a read were the head, and on a fast loopback
    /// connection a small body routinely arrives in the same read as its
    /// head. These are handed to [`PeerReader`] rather than discarded.
    Complete(Head, Framing, Vec<u8>),
    /// The peer closed the connection (cleanly or otherwise) without
    /// sending a complete head — nothing to answer.
    Closed,
    /// A response with this status must be sent, then the connection closed.
    Refuse(u16),
}

/// Reads and parses one request head from `stream`, bound by
/// [`Limits::peer_io`] in wall-clock time and [`Limits::max_head_bytes`] in
/// size — the bound `tiny_http` could not give this daemon (see the module
/// docs).
fn read_head(stream: &RefCell<TcpStream>, limits: Limits) -> HeadOutcome {
    let deadline = Deadline::new(limits.peer_io);
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read_result = {
            let mut s = stream.borrow_mut();
            match deadline.arm_read(&s) {
                Ok(()) => s.read(&mut chunk),
                Err(e) => Err(e),
            }
        };
        let n = match read_result {
            Ok(n) => n,
            Err(e) if is_deadline_error(&e) => return HeadOutcome::Refuse(408),
            Err(_) => return HeadOutcome::Closed,
        };
        if n == 0 {
            // A clean close mid-head and a clean close between requests look
            // identical here — both are "the peer sent nothing more" — and
            // both get the same answer: nothing to respond to.
            return HeadOutcome::Closed;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > limits.max_head_bytes {
            return HeadOutcome::Refuse(431);
        }
        match parse_head(&buf, limits.max_headers) {
            Ok(ParseOutcome::Partial) => continue,
            // `consumed` is how many bytes of `buf` were the head; anything
            // past it is the start of the body (common on loopback — a
            // small body often arrives in the same read as its head) and is
            // handed to the `Request` as `leftover` rather than discarded.
            // This daemon does not pipeline (see the crate-level module
            // docs), so those bytes are never a second request's head.
            Ok(ParseOutcome::Complete {
                head,
                framing,
                consumed,
            }) => {
                let leftover = buf.split_off(consumed);
                return HeadOutcome::Complete(head, framing, leftover);
            }
            Err(ParseError::TooManyHeaders) => return HeadOutcome::Refuse(431),
            Err(ParseError::Malformed | ParseError::InvalidHeader) => {
                return HeadOutcome::Refuse(400);
            }
            Err(ParseError::AmbiguousFraming | ParseError::InvalidContentLength) => {
                return HeadOutcome::Refuse(400);
            }
            Err(ParseError::UnsupportedTransferEncoding) => return HeadOutcome::Refuse(501),
        }
    }
}

/// Writes an empty response carrying `code`, best-effort, then lets the
/// caller close the connection. Used for every refusal this module answers
/// on its own, without ever building a [`Request`] — a malformed or
/// oversized head, and a connection over [`ConnectionSlots`]'s cap.
///
/// `write_budget` is a parameter rather than always [`Limits::peer_io`]
/// because the over-capacity refusal runs on the *accept* thread, not a
/// connection's own — an unbounded wait there would let the same class of
/// peer this cap exists to stop also stall every other listener's ability to
/// accept, which is why that caller passes the short
/// [`REFUSAL_WRITE_BUDGET`] instead.
fn refuse(stream: &mut TcpStream, write_budget: Duration, code: u16, retry_after: Option<&str>) {
    let mut response = Response::new_empty(code);
    if let Some(secs) = retry_after
        && let Ok(h) = Header::from_bytes("Retry-After", secs)
    {
        response = response.with_header(h);
    }
    let deadline = Deadline::new(write_budget);
    if deadline.arm_write(stream).is_ok() {
        let _ = write_response(stream, &response, false, false);
    }
}

/// Serves every request one accepted connection carries, one at a time, on
/// this thread — see the module docs on why one thread serves a whole
/// connection rather than one request.
fn serve_one_connection<F>(stream: TcpStream, limits: Limits, _permit: Permit, handler: F)
where
    F: Fn(Request),
{
    // Every request/response this daemon exchanges is small (see
    // `crate::api::http::MAX_BODY_BYTES`) and answered as one write, so
    // Nagle's algorithm only ever adds latency here, never saves a packet.
    let _ = stream.set_nodelay(true);
    let stream = Rc::new(RefCell::new(stream));

    loop {
        match read_head(&stream, limits) {
            HeadOutcome::Complete(head, framing, leftover) => {
                let outcome = Rc::new(Cell::new(Outcome::Unanswered));
                let request = Request::new(
                    Rc::clone(&stream),
                    head,
                    framing,
                    leftover,
                    limits,
                    Rc::clone(&outcome),
                );
                handler(request);
                if outcome.get() != Outcome::KeepAlive {
                    return;
                }
            }
            HeadOutcome::Closed => return,
            HeadOutcome::Refuse(code) => {
                let mut s = stream.borrow_mut();
                refuse(&mut s, limits.peer_io, code, None);
                return;
            }
        }
    }
}

/// Accepts connections from `listener` until it errors, handing each one to
/// its own thread running `handler` for as long as the connection stays
/// alive. Two refusals happen on the accept thread itself, *without* a
/// thread ever being spawned for the connection: a peer `admit` refuses
/// (`403`, SH-255 — off-tailnet refusal, checked first so a peer this
/// daemon should never talk to cannot even consume a capacity slot) and a
/// connection past `slots`'s ceiling (`503`, SH-177's bound over SH-172's
/// per-request threads, which had no ceiling at all).
///
/// `admit` is deliberately just `SocketAddr -> bool`, not a richer type this
/// module would have to understand: `conn.rs` knows nothing about
/// loopback/tailnet/CGNAT, and that ignorance is structural (see this
/// module's own doc comment) — the caller decides the policy and any
/// refusal logging it wants from *why*; this loop only ever needs *whether*.
pub fn serve_connections<F, A>(
    listener: TcpListener,
    limits: Limits,
    slots: Arc<ConnectionSlots>,
    admit: A,
    handler: F,
) where
    F: Fn(Request) + Clone + Send + 'static,
    A: Fn(std::net::SocketAddr) -> bool,
{
    loop {
        let (stream, addr) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(e) => {
                eprintln!(
                    "storyhook daemon: accept failed on a bound listener: {e}; \
                     this listener stops accepting"
                );
                return;
            }
        };
        if !admit(addr) {
            let mut stream = stream;
            refuse(&mut stream, REFUSAL_WRITE_BUDGET, 403, None);
            continue;
        }
        match slots.try_acquire() {
            Some(permit) => {
                let handler = handler.clone();
                thread::spawn(move || serve_one_connection(stream, limits, permit, handler));
            }
            None => {
                let mut stream = stream;
                refuse(&mut stream, REFUSAL_WRITE_BUDGET, 503, Some("1"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpStream as StdTcpStream;

    /// Binds an ephemeral loopback listener for a test to connect to.
    fn listen() -> TcpListener {
        TcpListener::bind("127.0.0.1:0").expect("bind")
    }

    /// The test SH-177 demanded before any deadline claim: SH-172's own
    /// attempt at this assumed `SO_RCVTIMEO`/`SO_SNDTIMEO` set on a
    /// `TcpListener` would be inherited by the socket its `accept()`
    /// returns, and shipped a test to prove it — which failed, because
    /// `accept(2)`'s documented inheritance list omits both options on every
    /// platform this daemon runs on. This module sets the deadline
    /// per-connection instead, directly on the accepted socket, which is
    /// what this test actually exercises.
    #[test]
    fn an_accepted_socket_carries_both_deadlines() {
        let listener = listen();
        let addr = listener.local_addr().unwrap();
        let _client = StdTcpStream::connect(addr).unwrap();
        let (accepted, _) = listener.accept().unwrap();

        let deadline = Deadline::new(Duration::from_millis(50));
        deadline.arm_read(&accepted).unwrap();
        deadline.arm_write(&accepted).unwrap();

        assert!(accepted.read_timeout().unwrap().is_some());
        assert!(accepted.write_timeout().unwrap().is_some());
    }

    /// The inequality `PEER_IO_TIMEOUT > ureq's max_idle_age` is what
    /// guarantees a pooled `ureq` client always discards a connection on its
    /// own before this daemon would close it out from under a request that
    /// reused it. Read from `ureq`'s own default config rather than
    /// restating its 15-second constant, so a future `ureq` upgrade that
    /// changes it fails this test instead of silently invalidating the
    /// guarantee.
    #[test]
    fn an_idle_client_gives_up_before_the_daemon_does() {
        let ureq_max_idle_age = ureq::config::Config::default().max_idle_age();
        assert!(
            ureq_max_idle_age < PEER_IO_TIMEOUT,
            "ureq's max_idle_age ({ureq_max_idle_age:?}) must stay below PEER_IO_TIMEOUT \
             ({PEER_IO_TIMEOUT:?}), or a client can be holding a connection this daemon has \
             already closed"
        );
    }

    /// A peer that sends nothing at all — not even a partial request line —
    /// is disconnected once the deadline passes, rather than held forever.
    #[test]
    fn a_silent_peer_is_disconnected_at_the_deadline() {
        let listener = listen();
        let addr = listener.local_addr().unwrap();
        let client = StdTcpStream::connect(addr).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        let limits = Limits::for_tests(Duration::from_millis(100));

        let outcome = read_head(&RefCell::new(accepted), limits);
        assert!(matches!(outcome, HeadOutcome::Refuse(408)));
        drop(client);
    }

    /// A peer that sends one byte and then goes silent — the smallest
    /// reproduction of the story's own scenario — is disconnected at the
    /// deadline rather than held forever, whether it dribbles the head or a
    /// declared body.
    #[test]
    fn a_dribbled_head_is_disconnected_at_the_deadline() {
        let listener = listen();
        let addr = listener.local_addr().unwrap();
        let mut client = StdTcpStream::connect(addr).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        client.write_all(b"G").unwrap();
        let limits = Limits::for_tests(Duration::from_millis(100));

        let outcome = read_head(&RefCell::new(accepted), limits);
        assert!(matches!(outcome, HeadOutcome::Refuse(408)));
    }

    /// One byte sent every little while, always inside a single per-call
    /// timeout, never adds up past the deadline's *total* budget — the exact
    /// gap a bare `SO_RCVTIMEO` cannot close (see the module docs).
    #[test]
    fn a_slow_dribble_still_hits_the_wall_clock_deadline() {
        let listener = listen();
        let addr = listener.local_addr().unwrap();
        let mut client = StdTcpStream::connect(addr).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        let limits = Limits::for_tests(Duration::from_millis(150));
        let stream = RefCell::new(accepted);

        let dribbler = thread::spawn(move || {
            for _ in 0..3 {
                thread::sleep(Duration::from_millis(60));
                if client.write_all(b"G").is_err() {
                    return;
                }
            }
        });

        let started = Instant::now();
        let outcome = read_head(&stream, limits);
        assert!(matches!(outcome, HeadOutcome::Refuse(408)));
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "the deadline should have fired near 150ms, not been reset by every dribbled byte"
        );
        let _ = dribbler.join();
    }

    #[test]
    fn a_head_over_the_size_cap_is_refused() {
        let listener = listen();
        let addr = listener.local_addr().unwrap();
        let mut client = StdTcpStream::connect(addr).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        let mut limits = Limits::for_tests(Duration::from_secs(5));
        limits.max_head_bytes = 16;

        client
            .write_all(b"GET /this-is-a-much-too-long-url-for-the-cap HTTP/1.1\r\n\r\n")
            .unwrap();
        let outcome = read_head(&RefCell::new(accepted), limits);
        assert!(matches!(outcome, HeadOutcome::Refuse(431)));
    }

    #[test]
    fn a_well_formed_head_parses_as_complete() {
        let listener = listen();
        let addr = listener.local_addr().unwrap();
        let mut client = StdTcpStream::connect(addr).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        let limits = Limits::for_tests(Duration::from_secs(5));

        client
            .write_all(b"GET /api/v1/hello HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .unwrap();
        match read_head(&RefCell::new(accepted), limits) {
            HeadOutcome::Complete(head, framing, leftover) => {
                assert_eq!(head.url, "/api/v1/hello");
                assert!(matches!(framing, Framing::None));
                assert!(leftover.is_empty());
            }
            _ => panic!("expected a complete head"),
        }
    }

    /// The bug this module's `PeerReader` exists to prevent: a body byte
    /// that arrives in the *same* read as the head — routine on a loopback
    /// connection for a small body, since nothing stops the kernel from
    /// delivering a head and its body in one segment — must still reach the
    /// body decoder. `httparse` only reports how many bytes of a read were
    /// the head; the rest is not "there was no more data", it is
    /// unread body, and dropping it either desyncs the connection or, worse,
    /// leaves a request looking like it never sent a body it did send.
    #[test]
    fn a_body_byte_arriving_with_the_head_is_not_dropped() {
        let listener = listen();
        let addr = listener.local_addr().unwrap();
        let mut client = StdTcpStream::connect(addr).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        let limits = Limits::for_tests(Duration::from_secs(5));

        // One write, so the head and the whole body land in the same read.
        client
            .write_all(b"POST / HTTP/1.1\r\nContent-Length: 3\r\n\r\nabc")
            .unwrap();

        let stream = Rc::new(RefCell::new(accepted));
        let (head, framing, leftover) = match read_head(&stream, limits) {
            HeadOutcome::Complete(head, framing, leftover) => (head, framing, leftover),
            _ => panic!("expected a complete head"),
        };
        assert_eq!(
            leftover, b"abc",
            "the body must not be dropped as if unread"
        );

        let outcome = Rc::new(Cell::new(Outcome::Unanswered));
        let mut request =
            Request::new(Rc::clone(&stream), head, framing, leftover, limits, outcome);
        let mut body = Vec::new();
        request.as_reader().read_to_end(&mut body).unwrap();
        assert_eq!(body, b"abc");
    }

    /// A connection past the cap is refused before a thread is ever spawned
    /// for it — this is SH-177's actual fix, so it is pinned directly rather
    /// than only indirectly through the daemon-level integration test.
    #[test]
    fn a_connection_past_the_cap_gets_no_permit() {
        let slots = ConnectionSlots::new(1);
        let first = slots.try_acquire();
        assert!(first.is_some());
        assert!(
            slots.try_acquire().is_none(),
            "a second connection must not get a permit while the first is held"
        );
        drop(first);
        assert!(
            slots.try_acquire().is_some(),
            "releasing a permit must free its slot for the next connection"
        );
    }

    /// End-to-end: a connection refused for being over the cap gets told why,
    /// on the wire, rather than a bare reset.
    #[test]
    fn a_refused_connection_is_told_why() {
        let listener = listen();
        let addr = listener.local_addr().unwrap();
        let mut client = StdTcpStream::connect(addr).unwrap();
        let (mut accepted, _) = listener.accept().unwrap();
        let limits = Limits::for_tests(Duration::from_secs(5));

        refuse(&mut accepted, limits.peer_io, 503, Some("1"));

        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(&mut client);
        let mut status_line = String::new();
        reader.read_line(&mut status_line).unwrap();
        assert!(status_line.starts_with("HTTP/1.1 503"), "{status_line}");
        let mut saw_retry_after = false;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
            if line.to_ascii_lowercase().starts_with("retry-after:") {
                saw_retry_after = true;
            }
        }
        assert!(saw_retry_after, "a refusal should carry Retry-After");
    }

    /// End-to-end through the real accept loop, not just `refuse()` called
    /// directly: a peer the `admit` closure refuses gets a `403` on the wire
    /// and never reaches `handler` at all — no thread spawned, no `Request`
    /// built, the same shape [`ConnectionSlots`]'s own over-cap refusal
    /// already has (SH-255, off-tailnet refusal's wiring into this module).
    #[test]
    fn a_peer_the_admit_closure_refuses_never_reaches_the_handler() {
        let listener = listen();
        let addr = listener.local_addr().unwrap();
        let limits = Limits::for_tests(Duration::from_secs(5));
        let slots = ConnectionSlots::new(4);
        let handler_calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&handler_calls);

        thread::spawn(move || {
            serve_connections(
                listener,
                limits,
                slots,
                |_peer_addr| false,
                move |_request: Request| {
                    counted.fetch_add(1, Ordering::SeqCst);
                },
            );
        });

        let mut client = StdTcpStream::connect(addr).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(&mut client);
        let mut status_line = String::new();
        reader.read_line(&mut status_line).unwrap();
        assert!(status_line.starts_with("HTTP/1.1 403"), "{status_line}");

        thread::sleep(Duration::from_millis(50));
        assert_eq!(
            handler_calls.load(Ordering::SeqCst),
            0,
            "a refused peer must never reach the request handler"
        );
    }
}
