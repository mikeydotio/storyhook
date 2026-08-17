//! An in-tree HTTP/1.1 connection layer that owns its sockets (SH-177).
//!
//! SH-172 moved peer-paced I/O (reading a head, reading a body, writing a
//! reply) off the daemon's dispatch thread and onto a worker per request, so
//! one stalled peer could no longer wedge every other client's command. It
//! could not bound how long that one worker stayed stalled: the plan called
//! for `SO_RCVTIMEO`/`SO_SNDTIMEO` on every accepted socket, inherited from
//! the listener, but `accept(2)`'s documented inheritance list omits both
//! options on every platform this daemon runs on — a test written to pin the
//! assumption caught it before it shipped. `tiny_http` 0.12, the transport
//! SH-172 built on, gave no other way to reach an accepted socket at all:
//! `Server::from_listener` owns its whole accept loop internally, and a
//! `Request`'s body reader was an opaque `Box<dyn Read>` with no file
//! descriptor behind it.
//!
//! This module replaces that transport with one that does expose the
//! accepted socket, built on `httparse` (already in `Cargo.lock` via `ureq`,
//! so this adds no new dependency) for head parsing. It closes the gap two
//! ways at once:
//!
//! - [`conn::Deadline`] bounds every read and write on a peer socket in
//!   wall-clock time — not per syscall, which a peer that dribbles one byte
//!   at a time can stay under forever, but as a total budget per phase (a
//!   request head, a request body, a response write).
//! - [`ConnectionSlots`] bounds how many connections this daemon serves at
//!   once, across every listener it binds. This is what actually answers the
//!   story's stated consequence — "an attacker who opens many such
//!   connections over time can still grow the daemon's thread count without
//!   limit" — because a per-connection deadline alone does not: `tiny_http`
//!   0.12's own `TaskPool` (`util/task_pool.rs`) spawned one thread per
//!   connection with no ceiling, and a thread blocked reading a dribbled
//!   *head* stalls before a `Request` ever exists — before any
//!   application-level counter could see it.
//!
//! # Scope
//!
//! Deliberately not supported, and not a regression: `tiny_http` never
//! supported HTTP/2 either. **Pipelining** — a second request's bytes
//! arriving before the first has been answered — is not handled: this
//! daemon's clients (`ureq`, `EventSource`, the `story` CLI) never pipeline,
//! and one request answered at a time removes the response-sequencing
//! bookkeeping pipelining would otherwise require. A pipelined second
//! request sent behind a body-less first one is not rejected with an error —
//! its bytes are simply never read as a request, so the client that sent it
//! sees the same thing it would see from a slow server: its own timeout.
//! **TLS** was never wired up here either (`Server::from_listener(l, None)`,
//! the call this module replaces) — the tailnet dashboard runs in the clear
//! today, and adding TLS is separate work if it's ever wanted.
//!
//! # Types match `tiny_http`'s subset on purpose
//!
//! [`Header`], [`Method`], [`Request`], and [`Response`] carry the same
//! names and methods as the `tiny_http` types this module replaced, so
//! `src/api/{http,rpc,rest,dispatch}.rs` changed only their `use` line.

mod body;
mod conn;
mod message;
mod parse;

pub use conn::{ConnectionSlots, Limits, PEER_IO_TIMEOUT, Request, serve_connections};
pub use message::{Header, Method, Response};
