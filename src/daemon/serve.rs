//! The daemon's listeners and its accept loop.
//!
//! # Two interfaces, one process
//!
//! Loopback is always bound — hardcoded, never configurable — so the dashboard
//! can never become reachable from beyond this machine by accident. When the
//! `tailscale` CLI reports an IP, a second listener is bound to it on the same
//! port, making the dashboard reachable from the rest of the tailnet without a
//! third-party reverse proxy. Nothing is ever bound to `0.0.0.0`, to any other
//! wildcard, or to a plain LAN address.
//!
//! Binding the tailnet interface is also what *grants* trust to its names: the
//! mutation guard's allowlist gains the tailnet IP and the MagicDNS FQDN only
//! when the interface they would arrive on is actually being served. Trust
//! follows bind — whenever that bind happens. A login-time daemon start can
//! race `tailscaled` coming up and miss its tailnet interface entirely
//! (SH-146), so [`serve`] retries in the background until one succeeds,
//! however long that takes; each request's `trusted_hosts` snapshot is read
//! fresh, so the moment a late bind commits it, the very next request already
//! sees it. The retry is one-shot: once a bind succeeds, `trusted_hosts` is
//! never touched again for the rest of this process's life.
//!
//! # Listeners are passed in, not opened here
//!
//! [`serve`] takes bound [`TcpListener`]s. That is a testability requirement
//! made structural: a caller can bind port 0, learn the port the kernel actually
//! gave it, publish that, and only then start serving — which deletes the "port
//! already taken" failure mode in production and the "which port did the test
//! get" guessing game in the suite.

use std::collections::BTreeMap;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::thread;
use std::time::Duration;

use crate::api::http::{
    Reply, carries_body, finish, path_segments, read_body, request_path, text_reply,
};
use crate::api::rest::{self, Changed};
use crate::api::rpc;
use crate::daemon::bus::{Change, ChangeBus};
use crate::daemon::http1::{self, Header, Method, Request};
use crate::daemon::lifecycle::Hello;
use crate::daemon::tailnet::{TailnetBind, tailnet_identity};
use crate::env::Environment;
use crate::error::AppError;
use crate::store::{ReadOps, Store};

/// How often a heartbeat is published to every connected client. A server-side
/// write failure prunes a connection that vanished without a clean close, and a
/// silence longer than this on the client side is what `web_dashboard.html`'s
/// `sseWatchdog` treats as the same thing when no write ever fails (SH-145).
/// Overridable via `STORYHOOK_SSE_HEARTBEAT_MS` so integration tests do not have
/// to wait out the production interval.
fn heartbeat_interval() -> Duration {
    std::env::var("STORYHOOK_SSE_HEARTBEAT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(20))
}

/// How often the daemon asks the store whether somebody else has written to it.
///
/// This is the safety net rather than the mechanism: a write the daemon serves
/// is published the instant it commits, and this catches the ones it did not
/// serve — a `story tui` session, a second machine, a `sqlite3` prompt. One
/// pragma per tick, so the interval can be short without mattering.
const CHANGE_TOKEN_POLL: Duration = Duration::from_millis(250);

/// How often a background thread looks up to see whether it should stop.
const SHUTDOWN_CHECK: Duration = Duration::from_millis(250);

/// How many requests this daemon routes at once (SH-173).
///
/// Derived, not chosen: [`crate::api::dispatch::MAX_RUNNING`] (4) dashboard
/// dispatches may run at once, and each one's `story.sh` makes several
/// nested `story` CLI calls that arrive at `hook_depth` 0 (a dispatch child
/// sets nothing that would mark it otherwise), so up to four of them can
/// occupy four dispatchers simultaneously. Plus a dashboard tab's own data
/// fetch, an interactive CLI command, and a health probe — one each. 4 + 4 =
/// 8. Falling short does not fail loudly: it silently starves the dashboard
/// behind the CLI, or the reverse, whichever arrives second — enforced at
/// compile time below, since both operands are constants.
///
/// [`crate::store::sqlite::DEFAULT_POOL_SIZE`] is tied to this constant for
/// the same reason: falling short there does not fail either, it silently
/// re-runs every connection pragma on every request past the idle cap.
const DISPATCHERS: usize = 8;

// Both relations above are pure compile-time facts, so they are enforced at
// compile time rather than left to a test somebody has to remember to run.
const _: () = assert!(DISPATCHERS > crate::api::dispatch::MAX_RUNNING);
const _: () = assert!(crate::store::sqlite::DEFAULT_POOL_SIZE == DISPATCHERS + 2);

/// [`DISPATCHERS`], overridable via `STORYHOOK_DISPATCHERS` so a test can
/// prove the hook-depth lane below is unconditional without needing eight
/// real concurrent commands to do it — the same shape [`heartbeat_interval`]
/// already uses to let a suite drive a production interval down.
fn dispatchers() -> usize {
    std::env::var("STORYHOOK_DISPATCHERS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DISPATCHERS)
}

/// Everything the accept loop needs, shared by every listener.
struct Serving<'a, S: Store> {
    store: &'a S,
    env: Environment,
    bus: ChangeBus,
    /// The mutation guard's allowlist. Set once at construction from
    /// whatever [`bind_listeners`] earned at startup; [`tailnet_reprobe`]
    /// is the only thing that ever writes it again, and only once, the
    /// instant its own late bind succeeds — never touched after that
    /// (SH-146). A lock rather than a plain field for exactly that one
    /// possible write; every read takes it for as long as a clone, never
    /// longer. `Arc`-wrapped (SH-177) so [`accept_loop`]'s handler — which
    /// `http1::serve_connections` requires to be `'static`, so it cannot
    /// hold `&Serving` the way [`dispatch`] and [`tailnet_reprobe`] do — can
    /// still read the live value on every request rather than a snapshot
    /// frozen when the listener started.
    trusted_hosts: Arc<RwLock<Vec<String>>>,
    /// The bearer token `/api/v1/*` requires — and, since SH-50,
    /// `/api/repos/*/story/*/dispatch` too.
    token: String,
    /// This daemon's identity, answered by `/api/v1/hello`.
    hello: Hello,
    /// Every dispatch this daemon has started, and their outcomes (SH-50).
    /// An `Arc` because `worker` threads reach it directly — dispatch is
    /// answered off the store thread, so it is not reached through
    /// [`dispatch`] the way everything else in this struct is.
    dispatch_registry: Arc<crate::api::dispatch::DispatchRegistry>,
    /// Everything this daemon is serving right now (SH-173, SH-144). An `Arc`
    /// so the detached thread [`worker`] spawns on the shutdown path — which
    /// has no `'scope` of its own — can still poll it while draining.
    inflight: Arc<crate::daemon::lifecycle::InFlight>,
    /// Set once, when the shutdown request itself is answered. Every
    /// dispatcher checks this before routing a job it dequeues, so work
    /// still queued when shutdown was requested is refused rather than
    /// started — "new work" stops here, not at the accept loop, so a peer
    /// still gets an answer rather than a reset connection.
    draining: AtomicBool,
}

/// Serves `listeners` until the process ends.
///
/// The first listener is served on this thread; the rest get one thread each.
/// `ready` fires once every background thread is up and the loops are about to
/// accept, so a caller can treat its return as "answering requests", not merely
/// "bound".
///
/// `on_late_tailnet_bind` fires at most once, from the background thread that
/// retries a tailnet bind `listeners` did not already include (SH-146) — never
/// from a request thread, so nothing a peer sends can trigger it. It is the
/// only way a caller finds out a late bind happened at all: `listeners` and
/// `trusted_hosts` are a snapshot of what was true when `serve` was called,
/// and everything after that lives inside this function.
#[allow(clippy::too_many_arguments)]
pub fn serve<S: Store, F, L>(
    store: &S,
    env: &Environment,
    listeners: Vec<Listener>,
    trusted_hosts: Vec<String>,
    bus: ChangeBus,
    token: String,
    ready: F,
    on_late_tailnet_bind: L,
) -> Result<(), AppError>
where
    F: FnOnce(),
    L: Fn(&BoundAddress) + Send + Sync,
{
    let has_tailnet = listeners.iter().any(|l| !l.loopback);
    let loopback_addr = listeners
        .iter()
        .find(|l| l.loopback)
        .and_then(|l| l.listener.local_addr().ok());

    if listeners.is_empty() {
        return Err(AppError::Usage(
            "the daemon needs at least one bound listener to serve".to_string(),
        ));
    }
    let http_limits = http1::Limits::production();
    // Shared across every listener this daemon binds, so the connection cap
    // is per daemon rather than per interface (SH-177) — a machine with a
    // tailnet interface bound has two listeners, and a peer should not be
    // able to double its share of the cap by splitting connections across
    // both.
    let connection_slots = http1::ConnectionSlots::production();

    let stop = Arc::new(AtomicBool::new(false));
    let serving = Serving {
        store,
        env: env.clone(),
        bus: bus.clone(),
        trusted_hosts: Arc::new(RwLock::new(trusted_hosts)),
        token,
        hello: Hello {
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol: crate::daemon::lifecycle::PROTOCOL,
            pid: std::process::id(),
            started_at: env.now(),
        },
        dispatch_registry: Arc::new(crate::api::dispatch::DispatchRegistry::new()),
        inflight: Arc::new(crate::daemon::lifecycle::InFlight::new(env.clone())),
        draining: AtomicBool::new(false),
    };
    // Before `ready()`, so no listener has accepted a request a client could
    // poll a stale record from: a record surviving to here can only be one a
    // *previous* daemon left behind (SH-173).
    serving.inflight.harvest_stale();

    // One job channel for the whole daemon, no matter how many listeners are
    // bound — a rendezvous channel, so a worker's `send` blocks until a
    // dispatcher is ready for the next job, which is what keeps at most
    // `dispatchers()` jobs in flight to the store at a time (SH-173).
    let (jobs_tx, jobs_rx) = mpsc::sync_channel::<Job>(0);
    let jobs_rx = Mutex::new(jobs_rx);
    // A request whose envelope names `hook_depth > 0` never queues behind the
    // pool above: it is nested inside an event hook a pool thread is already
    // running, so if enough of them queued for the pool at once they could
    // occupy every dispatcher and deadlock waiting on each other.
    // `hook_depth` caps nesting at one, so this lane can never recurse — an
    // unbounded channel, because back-pressure here would reintroduce the
    // deadlock this lane exists to remove.
    let (nested_tx, nested_rx) = mpsc::channel::<Job>();

    // Every background thread lives inside this scope, which is what lets the
    // change-token poller and every dispatcher borrow the store rather than
    // being handed a raw pointer to it. The scope joins them on the way out,
    // so the accept loop signals `stop` before it returns and the joins take
    // one poll interval rather than forever.
    thread::scope(|scope| {
        {
            let (bus, stop) = (bus.clone(), Arc::clone(&stop));
            scope.spawn(move || heartbeat(&bus, &stop));
        }
        {
            let (bus, stop) = (bus.clone(), Arc::clone(&stop));
            scope.spawn(move || poll_change_token(store, &bus, &stop));
        }
        {
            let stop = Arc::clone(&stop);
            scope.spawn(move || watch_parent(&stop));
        }
        if !has_tailnet && let Some(loopback_addr) = loopback_addr {
            let stop = Arc::clone(&stop);
            let serving = &serving;
            let on_late_tailnet_bind = &on_late_tailnet_bind;
            let jobs_tx = jobs_tx.clone();
            let nested_tx = nested_tx.clone();
            let slots = Arc::clone(&connection_slots);
            scope.spawn(move || {
                tailnet_reprobe(
                    scope,
                    serving,
                    loopback_addr,
                    http_limits,
                    slots,
                    jobs_tx,
                    nested_tx,
                    on_late_tailnet_bind,
                    &stop,
                )
            });
        }
        {
            let serving = &serving;
            let jobs_rx = &jobs_rx;
            for _ in 0..dispatchers() {
                scope.spawn(move || dispatch(serving, jobs_rx));
            }
        }
        {
            let serving = &serving;
            scope.spawn(move || nested_lane(scope, serving, nested_rx));
        }

        ready();

        let mut listeners = listeners;
        // The last listener is served on this thread, so `serve`'s caller
        // blocks for as long as the daemon is up; every other listener (the
        // tailnet interface, when bound) gets its own thread. Which one is
        // "last" carries no meaning — every listener runs the identical loop.
        let primary = listeners.pop().expect("checked non-empty above");
        for listener in listeners {
            let serving = &serving;
            let jobs_tx = jobs_tx.clone();
            let nested_tx = nested_tx.clone();
            let slots = Arc::clone(&connection_slots);
            scope.spawn(move || {
                accept_loop(
                    serving,
                    listener.listener,
                    listener.loopback,
                    http_limits,
                    slots,
                    jobs_tx,
                    nested_tx,
                )
            });
        }
        accept_loop(
            &serving,
            primary.listener,
            primary.loopback,
            http_limits,
            connection_slots,
            jobs_tx,
            nested_tx,
        );
        stop.store(true, Ordering::Relaxed);
    });
    Ok(())
}

/// Binds loopback (and the tailnet interface, when there is one) and serves —
/// the harness's sanctioned test seam, not a production entry point (SH-148).
///
/// The only production entry point is [`super::lifecycle::run`], which claims
/// the pidfile lock, mints a real token, writes the portfile, and runs the
/// startup backup. This function skips all four: it exists solely for
/// `storyhook-test-support`'s `try_serve_on`, which needs a dashboard serving
/// a store that is already open, in-process, without any of that daemon
/// lifecycle ceremony. Gated behind the `test-seam` feature — never in
/// `default` — so it does not exist in a build that lacks the feature, no
/// matter how `pub` its signature is; see that feature's doc in `Cargo.toml`.
///
/// `port` may be 0, in which case the kernel picks; `ready` is told every
/// address actually bound, loopback and tailnet alike. Three things only the
/// server itself can report, and which no probe from outside can establish:
/// *which* address it got, that the address is its own — a caller that merely
/// connects to a port cannot tell this server apart from some other process
/// holding it — and whether the best-effort tailnet bind succeeded, which is
/// what a caller must not guess by probing the machine (SH-110).
#[cfg(feature = "test-seam")]
pub fn bind_and_serve<S: Store, F>(
    store: &S,
    env: &Environment,
    port: u16,
    ready: F,
) -> Result<(), AppError>
where
    F: FnOnce(BoundAddress),
{
    let (listeners, bound) = bind_listeners(port)?;
    eprintln!("Storyhook dashboard: http://127.0.0.1:{}", bound.port());
    let mut trusted_hosts = bound.trusted_hosts();
    trusted_hosts.extend(crate::api::http::trusted_hosts_from_env());
    // No portfile and no real token: nothing this test seam serves reaches
    // `/api/v1/*`, so an empty `token` is never presented, let alone checked.
    // It is *not* a control-surface guard in its own right — `token_ok`'s
    // constant-time comparison admits an explicit empty offered header
    // against an empty expected one — so the thing actually keeping this
    // function out of a real deployment is the `test-seam` gate above, not
    // this string.
    serve(
        store,
        env,
        listeners,
        trusted_hosts,
        ChangeBus::new(),
        String::new(),
        move || ready(bound),
        |_| {},
    )
}

/// Where a server is answering: the loopback address it bound, and the tailnet
/// interface it bound beside it, if any.
///
/// The whole of what may legitimately be advertised, and the reason it is a
/// struct rather than a loose `SocketAddr`: the tailnet answer used to be
/// computed inside [`bind_listeners`] and never leave it, so every process that
/// wanted to name the dashboard re-derived it from a fresh probe of the machine
/// instead of reading what this server actually bound (SH-110).
///
/// One value crosses both boundaries — the in-process `ready` callback and the
/// daemon's portfile — so one fact has exactly one representation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BoundAddress {
    /// The loopback address. Always present: loopback is bound unconditionally.
    pub loopback: std::net::SocketAddr,
    /// The tailnet interface, when one was bound. `None` is the honest answer
    /// both for a machine with no tailnet and for a probe that missed its
    /// deadline — in either case there is no tailnet listener to reach.
    #[serde(default)]
    pub tailnet: Option<TailnetBind>,
}

impl BoundAddress {
    /// The port both listeners share — the tailnet one is bound to whatever
    /// port loopback actually got.
    pub fn port(&self) -> u16 {
        self.loopback.port()
    }

    /// The host to show or copy for reaching this server: the tailnet's
    /// advertised name when this server bound one, else loopback.
    ///
    /// Loopback is the correct answer rather than a fallback: it is the only
    /// address this server is certain to be answering on.
    pub fn advertise_host(&self) -> String {
        self.tailnet
            .as_ref()
            .map(TailnetBind::advertise_host)
            .unwrap_or_else(|| "127.0.0.1".to_string())
    }

    /// The dashboard URL to show or copy, built from [`Self::advertise_host`].
    pub fn dashboard_url(&self) -> String {
        format!("http://{}:{}", self.advertise_host(), self.port())
    }

    /// The non-loopback `Host` values this bind earns trust for. Empty unless a
    /// tailnet interface was actually bound — trust follows bind.
    pub fn trusted_hosts(&self) -> Vec<String> {
        self.tailnet
            .as_ref()
            .map(TailnetBind::trusted_hosts)
            .unwrap_or_default()
    }
}

/// A bound listener and whether it is the loopback one.
///
/// The distinction is security-relevant rather than cosmetic: `/api/v1/*` is a
/// full-privilege surface and is answered on loopback only, so the accept loop
/// has to know which interface a request came in on.
pub struct Listener {
    /// The bound socket.
    pub listener: TcpListener,
    /// Whether this is the loopback interface.
    pub loopback: bool,
}

/// Binds loopback on `port`, then the tailnet interface on whatever port
/// loopback actually got, and reports the hosts the tailnet bind earns trust
/// for.
///
/// A tailnet bind failure is a warning, never fatal: no tailnet is a degraded
/// dashboard, and a dashboard that refuses to start is a broken one.
///
/// The `?` below is load-bearing beyond its own error: it is also what keeps
/// [`super::lifecycle::bind_preferred`]'s retry from ever probing the tailnet
/// identity twice (SH-147) — a port-taken failure returns here, before
/// `probe_and_bind_tailnet` is reached, so the fallback call is the *only*
/// one that pays the probe's cost. Move the probe ahead of this bind and that
/// stops being true.
pub fn bind_listeners(port: u16) -> Result<(Vec<Listener>, BoundAddress), AppError> {
    let loopback = TcpListener::bind(("127.0.0.1", port)).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            AppError::Usage(format!(
                "Port {port} already in use. Try a different port with --port."
            ))
        } else {
            AppError::Storage(format!("Failed to start web server: {e}"))
        }
    })?;
    let bound = loopback
        .local_addr()
        .map_err(|e| AppError::Storage(format!("bound listener has no address: {e}")))?;

    let mut listeners = vec![Listener {
        listener: loopback,
        loopback: true,
    }];
    let mut tailnet = None;

    if let Some((listener, bind)) = probe_and_bind_tailnet(bound.port()) {
        tailnet = Some(bind);
        listeners.push(listener);
    }

    Ok((
        listeners,
        BoundAddress {
            loopback: bound,
            tailnet,
        },
    ))
}

/// Probes `tailscale` and, if it names an identity, binds the tailnet
/// interface on `port` — the same port loopback is already bound to.
///
/// `None` covers three cases alike, and deliberately does not distinguish
/// them: no tailnet on this machine, a probe that missed its deadline, and a
/// bind that failed (logged here, since this is the only place that knows
/// which). All three mean the same thing to every caller — no tailnet
/// listener exists — and [`tailnet_reprobe`] relies on that: it calls this
/// again later without needing to know which case it was.
fn probe_and_bind_tailnet(port: u16) -> Option<(Listener, TailnetBind)> {
    let identity = tailnet_identity()?;
    let tailnet_addr = format!("{}:{port}", identity.bind_ip);
    match TcpListener::bind(&tailnet_addr) {
        Ok(listener) => {
            eprintln!("Storyhook dashboard (tailnet): http://{tailnet_addr}");
            // We deliberately bound this interface ourselves, so trust its
            // identity for mutations too — the same standing as loopback —
            // without requiring STORYHOOK_WEB_TRUSTED_HOSTS. Trust follows
            // bind, so a name is never trusted unless the interface it would
            // arrive on is actually being served. `into_bound` is where that
            // rule is enforced: it is reachable only from here, on the
            // success arm.
            let bind = identity.into_bound();
            Some((
                Listener {
                    listener,
                    loopback: false,
                },
                bind,
            ))
        }
        Err(e) => {
            eprintln!(
                "warning: could not bind tailnet interface {tailnet_addr}: {e}; \
                 the dashboard is only reachable via localhost"
            );
            None
        }
    }
}

/// One accepted request, stripped of everything socket-shaped, on its way to
/// the thread that owns the store.
///
/// Built by [`worker`] only after the head has cleared admission and the body
/// has been fully read — nothing past that point ever touches the network
/// again, which is this struct's whole reason to exist (SH-172): the thread
/// that receives it can route purely in memory. Answered by sending exactly
/// one [`Verdict`] back over `reply`.
struct Job {
    method: Method,
    path: String,
    headers: Vec<Header>,
    body: String,
    loopback: bool,
    reply: mpsc::Sender<Verdict>,
}

/// What [`dispatch`] decided for one [`Job`].
enum Verdict {
    /// Answer it.
    Reply(Reply),
    /// Answer it, then the daemon exits once nothing is left in flight. The
    /// dispatch thread never holds `request` — [`worker`] does — so the
    /// environment needed to clear the portfile, and the registry needed to
    /// know when it is safe to exit, travel inside the verdict instead of
    /// being re-read or re-derived.
    Shutdown {
        reply: Reply,
        env: Environment,
        inflight: Arc<crate::daemon::lifecycle::InFlight>,
    },
}

/// Runs the request-accept loop for one bound listener.
///
/// This loop does exactly one thing: hand each accepted connection to
/// [`http1::serve_connections`], which owns it — deadlines, the connection
/// cap, and parsing — for as long as it stays open, calling [`worker`] once
/// per request the connection carries. Accepting itself must never block on
/// a peer, and it never does: [`TcpListener::accept`] only ever waits on the
/// *next* connection arriving, never on anything a peer already connected
/// controls the pace of.
///
/// Everything peer-paced — reading a head, reading a body, writing a reply —
/// happens inside `http1`, one connection per thread, never here and never on
/// [`dispatch`]. That split is the fix for SH-172: a peer that stalls mid-body
/// ties up one thread, never the thread every other client's command is
/// queued behind. `http1`'s own deadlines and connection cap are the fix for
/// SH-177: that one tied-up thread cannot survive past `PEER_IO_TIMEOUT`, and
/// an attacker cannot grow the daemon past `MAX_CONNECTIONS` of them at once,
/// across every listener this daemon binds combined.
///
/// `jobs_tx` and `nested_tx` are shared with every other bound listener's
/// accept loop — each is built once, in [`serve`], rather than one channel
/// and one [`dispatch`] thread per listener. Before this split, a machine
/// with a tailnet interface bound *two* listeners and therefore had two
/// threads already serving the store concurrently, by accident of interface
/// count rather than by design; one channel per lane means one place the
/// [`DISPATCHERS`] pool has to reason about.
fn accept_loop<S: Store>(
    serving: &Serving<'_, S>,
    listener: TcpListener,
    loopback: bool,
    limits: http1::Limits,
    slots: Arc<http1::ConnectionSlots>,
    jobs_tx: mpsc::SyncSender<Job>,
    nested_tx: mpsc::Sender<Job>,
) {
    let token: Arc<str> = Arc::from(serving.token.as_str());
    let bus = serving.bus.clone();
    let trusted_hosts = Arc::clone(&serving.trusted_hosts);
    let env = serving.env.clone();
    let dispatch_registry = Arc::clone(&serving.dispatch_registry);

    // One closure per listener, cloned by `http1::serve_connections` once per
    // accepted connection (and, within a kept-alive connection, called again
    // for each request it carries) — every capture here is `Clone`, so the
    // closure is too, without needing any of them to be `Sync`. `trusted_hosts`
    // is read fresh on every call rather than snapshotted once: a late
    // tailnet bind (SH-146) can update it for as long as this listener runs,
    // and the very next request must see it.
    let handler = move |request: Request| {
        let trusted_hosts = trusted_hosts.read().unwrap_or_else(PoisonError::into_inner);
        worker(
            request,
            loopback,
            &token,
            bus.clone(),
            jobs_tx.clone(),
            nested_tx.clone(),
            &trusted_hosts,
            &env,
            &dispatch_registry,
        )
    };

    http1::serve_connections(listener, limits, slots, handler);
}

/// The one field `worker` peeks from an as-yet-unparsed `/api/v1/invoke`
/// body, to decide which lane a request takes.
///
/// The envelope (`crate::api::wire::WireRequest`) stays the single source of
/// truth for *behaviour* — this peek decides only *scheduling*, so a
/// disagreement between the two (a malformed body this parses leniently but
/// the real parse inside `rpc::invoke` refuses) is a scheduling
/// suboptimality, never a correctness bug: the job is misrouted to the wrong
/// lane, not misinterpreted once it gets there.
#[derive(serde::Deserialize, Default)]
struct Nesting {
    #[serde(default)]
    hook_depth: u32,
}

/// Whether `body` is a request nested inside an event hook.
///
/// A malformed or absent `hook_depth` reads as `false` — the ordinary
/// pool, never the unbounded lane — because misrouting a genuinely nested
/// request to the pool costs a queueing delay this design already accepts
/// elsewhere, while misrouting an ordinary request to the unbounded lane
/// would let a client bypass the pool's back-pressure by sending malformed
/// JSON.
fn is_nested_invoke(path: &str, body: &str) -> bool {
    path == "/api/v1/invoke"
        && serde_json::from_str::<Nesting>(body)
            .map(|n| n.hook_depth > 0)
            .unwrap_or(false)
}

/// Handles one accepted connection's request — everything that touches the
/// network — on its own detached thread, so a peer that stalls mid-head or
/// mid-body blocks only this thread and the one file descriptor it owns.
///
/// Admission for the control surface (`/api/v1/*`) is decided here, from the
/// head alone, *before* any body is read: an unauthenticated peer must never
/// be able to make this daemon wait on a body it has no right to send in the
/// first place (SH-172). `GET /api/events` is answered here in full too,
/// without ever reaching [`dispatch`]: it needs nothing from the store, and it
/// is a long-lived streaming connection that must not tie up the job channel
/// for as long as the browser tab stays open.
#[allow(clippy::too_many_arguments)]
fn worker(
    request: Request,
    loopback: bool,
    token: &str,
    bus: ChangeBus,
    jobs: mpsc::SyncSender<Job>,
    nested_jobs: mpsc::Sender<Job>,
    trusted_hosts: &[String],
    env: &Environment,
    dispatch_registry: &Arc<crate::api::dispatch::DispatchRegistry>,
) {
    let mut request = request;
    let method = request.method().clone();
    let path = request_path(request.url()).to_string();
    let headers = request.headers().to_vec();

    let segments = path_segments(&path);
    if let Some(reply) = rpc::admission(&segments, &headers, token, loopback) {
        finish(request, reply);
        return;
    }

    if method == Method::Get && path == "/api/events" {
        serve_sse(request, bus);
        return;
    }

    // Answered here, off the store thread, exactly like the SSE branch
    // above: a dispatch runs for tens of seconds and makes its own nested
    // `story` CLI calls back into this daemon, so handling it on the
    // single thread that owns the store (`dispatch`, below) would
    // deadlock on the first one (SH-50).
    if let Some(reply) = crate::api::dispatch::intercept(
        &segments,
        &method,
        &headers,
        trusted_hosts,
        token,
        env,
        &bus,
        dispatch_registry,
    ) {
        finish(request, reply);
        return;
    }

    let body = if carries_body(&method) {
        match read_body(&mut request) {
            Some(b) => b,
            None => {
                finish(
                    request,
                    text_reply(400, "request body invalid or too large"),
                );
                return;
            }
        }
    } else {
        String::new()
    };

    // A request nested inside an event hook must never queue behind the
    // dispatcher pool: a hook runs *inside* a dispatcher, so if enough of
    // them fired `story` back into this daemon at once, their nested calls
    // could occupy every remaining dispatcher and deadlock waiting on each
    // other. `hook_depth` caps nesting at one, so this lane can never
    // recurse — structurally deadlock-free, the same move `GET /api/events`
    // and the dispatch-endpoint intercept above already make (SH-173).
    let nested = is_nested_invoke(&path, &body);

    let (reply_tx, reply_rx) = mpsc::channel::<Verdict>();
    let job = Job {
        method,
        path,
        headers,
        body,
        loopback,
        reply: reply_tx,
    };
    let sent = if nested {
        nested_jobs.send(job).map_err(|_| ())
    } else {
        jobs.send(job).map_err(|_| ())
    };
    if sent.is_err() {
        // Every dispatcher is gone — the daemon is exiting. Answer rather
        // than leaving the peer to time out against a socket that will never
        // write anything.
        finish(
            request,
            text_reply(503, "storyhook daemon is shutting down"),
        );
        return;
    }
    match reply_rx.recv() {
        Ok(Verdict::Reply(reply)) => finish(request, reply),
        Ok(Verdict::Shutdown {
            reply,
            env,
            inflight,
        }) => {
            finish(request, reply);
            thread::spawn(move || {
                // Uncapped: `draining` already refuses every job dequeued
                // after this point, so this can only shrink. A daemon with
                // nothing in flight exits after one `SHUTDOWN_CHECK` —
                // faster than the blind sleep this replaced — and one that
                // is genuinely wedged is `story daemon stop --force`'s
                // problem, not this loop's: it does not wait for this loop
                // to notice anything, it signals the pid directly.
                while !inflight.is_empty() {
                    thread::sleep(SHUTDOWN_CHECK);
                }
                crate::daemon::lifecycle::clear_info(&env);
                std::process::exit(0);
            });
        }
        Err(_) => finish(
            request,
            text_reply(503, "storyhook daemon is shutting down"),
        ),
    }
}

/// One of [`DISPATCHERS`] threads that own the store between them. Every
/// [`Job`] the pool's channel carries is routed by [`route_job`] on whichever
/// dispatcher happens to be free — nothing peer-paced happens on any of
/// these threads, so a slow or stalled *client* can no longer make a
/// *dispatcher* slow for everyone else.
///
/// `jobs` is behind a [`Mutex`] because [`mpsc::Receiver`] is not `Sync`:
/// only one dispatcher is ever inside `recv()` at a time, but that dequeue is
/// the only moment the lock is held, so it is never held across a job's
/// actual routing. A fixed pool needs nothing more elaborate — no idle
/// tracking, no retirement — because there is nothing elastic to track.
fn dispatch<S: Store>(serving: &Serving<'_, S>, jobs: &Mutex<mpsc::Receiver<Job>>) {
    loop {
        let job = {
            let jobs = jobs.lock().unwrap_or_else(PoisonError::into_inner);
            jobs.recv()
        };
        match job {
            Ok(job) => route_job(serving, job),
            // The channel closed: every sender is gone, which means `serve`
            // is tearing down. Nothing left to route.
            Err(_) => return,
        }
    }
}

/// Feeds every request nested inside an event hook its own thread,
/// unconditionally — never through [`DISPATCHERS`]' shared channel, and
/// never sharing one thread across nested requests either, because a second
/// hook's own nested call must not queue behind a first one's.
///
/// Bounded by construction rather than by a limit: a nested request can only
/// exist inside a dispatcher that is already running a hook
/// (`hook_depth` caps nesting at one — [`crate::service::Ctx::hooks_enabled`]),
/// so at most [`DISPATCHERS`] of these can ever be alive at once.
///
/// Lives inside `serve`'s own [`thread::scope`] — recursive scoped spawning
/// is sound (`&'scope Scope<'scope, 'env>` is `Send + Sync`, and each spawn
/// is a fresh OS thread rather than stack recursion) — which is what a
/// detached `worker` thread cannot do for itself: it has no `'scope` to
/// spawn into, only a channel to hand the job to something that does.
fn nested_lane<'scope, S: Store>(
    scope: &'scope thread::Scope<'scope, '_>,
    serving: &'scope Serving<'_, S>,
    jobs: mpsc::Receiver<Job>,
) {
    for job in jobs {
        scope.spawn(move || route_job(serving, job));
    }
}

/// Routes one [`Job`] to its answer and sends the answer back — the body
/// every dispatcher runs, whether pooled ([`dispatch`]) or ad hoc
/// ([`nested_lane`]).
///
/// The whole thing runs inside `catch_unwind`. [`thread::scope`] re-panics in
/// its caller once every spawned thread is joined if any of them panicked,
/// so an uncaught panic here would take down the whole daemon — a pooled
/// dispatcher's *loop* would also simply stop, shrinking the pool by one for
/// the life of the process. Caught, `job` (and its `reply` sender) drops
/// during the unwind without answering, which `worker`'s own
/// `reply_rx.recv()` already turns into a 503 — the same fallback a closed
/// channel gets on shutdown, and an honest one: the daemon does not know
/// whether the work it was routing completed.
fn route_job<S: Store>(serving: &Serving<'_, S>, job: Job) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        route_job_inner(serving, job)
    }))
    .is_err()
    {
        eprintln!(
            "storyhook daemon: a dispatcher panicked while routing a request; the \
             client sees a 503 and the daemon keeps running"
        );
    }
}

fn route_job_inner<S: Store>(serving: &Serving<'_, S>, job: Job) {
    // Shutdown was already answered and accepted: this job was either
    // queued behind the pool when that happened, or arrived after — either
    // way it is "new work", and new work stops here rather than at the
    // accept loop, so its peer gets an answer instead of a reset connection.
    if serving.draining.load(Ordering::Relaxed) {
        let _ = job.reply.send(Verdict::Reply(text_reply(
            503,
            "storyhook daemon is shutting down",
        )));
        return;
    }

    // Opened for every job; `rpc::invoke` names an RPC job once its
    // envelope has parsed (SH-144's own reasoning for publishing where it
    // does). A REST job is named here, generically, the moment it is
    // known to be one: there is no per-request `cwd` on that surface the
    // way an RPC envelope carries one, so it gets the ordinary deadline
    // rather than one widened by a project's own hook configuration.
    let entry = serving.inflight.enter();
    let surface = rpc::Surface {
        store: serving.store,
        env: &serving.env,
        token: &serving.token,
        hello: &serving.hello,
        entry: &entry,
    };
    let segments = path_segments(&job.path);
    if let Some(answer) = rpc::route(
        &surface,
        &segments,
        &job.method,
        &job.headers,
        &job.body,
        job.loopback,
    ) {
        let verdict = match answer {
            rpc::Answer::Reply(reply) => Verdict::Reply(reply),
            rpc::Answer::Shutdown(reply) => {
                // Set before the reply goes out, so no dispatcher that sees
                // this daemon's own answer to the shutdown request can also
                // see a dequeue racing ahead of this store.
                serving.draining.store(true, Ordering::Relaxed);
                // Tell every connected browser to reconnect *before*
                // answering, so a client that is about to lose its stream
                // knows why.
                serving.bus.publish(Change::Reload);
                Verdict::Shutdown {
                    reply,
                    env: serving.env.clone(),
                    inflight: Arc::clone(&serving.inflight),
                }
            }
        };
        // Closed before the reply is sent, matching the property SH-144
        // relies on: the record changes exactly when the daemon finishes
        // something, not a moment after its client already knows that.
        drop(entry);
        let _ = job.reply.send(verdict);
        return;
    }
    // A fixed name: a browser never polls this record the way
    // `HttpInvoker::send` does, so nothing depends on it being unique
    // across concurrent dashboard requests — only on it never colliding
    // with a real client's own request id, which no CLI-generated UUID
    // ever will.
    entry.name(crate::daemon::lifecycle::CurrentRequest {
        request_id: "dashboard".to_string(),
        command: "dashboard".to_string(),
        project: None,
        pid: std::process::id(),
        started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        served_deadline_secs: crate::daemon::lifecycle::SERVED_DEADLINE.as_secs(),
        // No client directory on this surface — a browser names a project
        // and a story, never a `cwd` — so this is never a `migrate`'s own
        // concurrency check could match against.
        cwd: std::path::PathBuf::new(),
    });

    let trusted_hosts = serving
        .trusted_hosts
        .read()
        .unwrap_or_else(PoisonError::into_inner);
    let routed = rest::route(
        serving.store,
        &serving.env,
        &job.method,
        &job.path,
        &job.headers,
        &job.body,
        &trusted_hosts,
    );
    drop(trusted_hosts);
    drop(entry);
    // Published here, at the request boundary: the write has committed and
    // its transaction is over, so a subscriber woken by this can read what
    // just happened rather than what was there before it.
    match &routed.changed {
        Some(Changed::Project(slug)) => serving.bus.publish(Change::Project(slug.clone())),
        Some(Changed::Catalog) => serving.bus.publish(Change::Catalog),
        None => {}
    }
    let _ = job.reply.send(Verdict::Reply(routed.reply));
}

/// Serves one `GET /api/events` connection for its entire lifetime, on its own
/// thread. Subscribes to the change feed, streams every change it receives as an
/// SSE frame until a write fails (the client disconnected), then unsubscribes —
/// which the subscription's own `Drop` does, including on an early return.
fn serve_sse(request: Request, bus: ChangeBus) {
    let subscription = bus.subscribe();

    let mut writer = request.into_writer();
    if crate::api::http::write_sse_head(&mut writer).is_err() {
        return;
    }
    // `retry: 3000` tells the browser's `EventSource` how long to wait before
    // auto-reconnecting after a drop; the leading comment gives the client an
    // immediate, distinguishable "connected" frame to act on (see
    // `connectEvents`'s `onopen` resync in web_dashboard.html).
    if crate::api::http::write_sse_frame(&mut writer, "retry: 3000\n: connected\n\n").is_err() {
        return;
    }

    loop {
        // The wait is bounded so a subscriber whose queue never fills still
        // wakes often enough to notice its own process shutting down.
        let Some(change) = subscription.recv(Duration::from_millis(500)) else {
            continue;
        };
        if crate::api::http::write_sse_frame(&mut writer, &change.to_sse()).is_err() {
            break;
        }
    }
    // `subscription` drops here, unsubscribing; `writer` drops, closing the
    // socket.
}

/// Publishes a heartbeat on [`heartbeat_interval`] until `stop` is signalled.
///
/// The sleep is chopped into short naps rather than taken whole: the production
/// interval is twenty seconds, and a shutdown should not have to wait out the
/// remainder of one.
fn heartbeat(bus: &ChangeBus, stop: &AtomicBool) {
    let interval = heartbeat_interval();
    let mut waited = Duration::ZERO;
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(SHUTDOWN_CHECK);
        waited += SHUTDOWN_CHECK;
        if waited >= interval {
            waited = Duration::ZERO;
            bus.publish(Change::Ping);
        }
    }
}

/// Watches the store's change token and reports what moved when it changes.
///
/// The token is the cheap trigger — one pragma per tick, whatever the store
/// holds. Only when it moves does this do any real work, and then it asks the
/// sharper question: which projects' histories grew? Those get a precise
/// [`Change::Project`], so a write this daemon did not serve reaches a browser as
/// the same event one it did serve does.
///
/// **A change it cannot attribute becomes a [`Change::Resync`].** Editing a
/// state definition appends no story event, so nothing's sequence moves and the
/// only honest answer is "something changed, refetch". Guessing at a project
/// would be worse than the resync: a dashboard showing something untrue is the
/// failure this whole feed exists to prevent.
fn poll_change_token<S: Store>(store: &S, bus: &ChangeBus, stop: &AtomicBool) {
    let mut last = store.change_token().ok();
    let mut seqs = project_sequences(store);
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(CHANGE_TOKEN_POLL);
        let Ok(token) = store.change_token() else {
            continue;
        };
        if last == Some(token) {
            continue;
        }
        last = Some(token);

        let fresh = project_sequences(store);
        // A change nobody is listening for is still a change: the baseline has
        // to move, or the first client to connect — which has just fetched
        // everything — would be told to refetch it again.
        if bus.subscriber_count() == 0 {
            seqs = fresh;
            continue;
        }

        let mut attributed = false;
        for (slug, seq) in &fresh {
            if seqs.get(slug) != Some(seq) {
                bus.publish(Change::Project(slug.clone()));
                attributed = true;
            }
        }
        if fresh.len() != seqs.len() {
            bus.publish(Change::Catalog);
            attributed = true;
        }
        if !attributed {
            bus.publish(Change::Resync);
        }
        seqs = fresh;
    }
}

/// Exits when the process named by `STORYHOOK_PARENT_PID` goes away.
///
/// The suicide contract, and the layer of orphan defence that catches what the
/// other three miss. A test binary names itself; every `story` it runs inherits
/// the variable, so a daemon started by one of them inherits it too. When the
/// binary ends — cleanly, by panic, or by `kill -9` — the daemon notices and
/// exits, rather than surviving to answer the *next* run's requests out of a
/// store that no longer exists. That failure has happened, and it cost 78 of 139
/// tests and an afternoon.
///
/// Production sets nothing, so nothing watches.
fn watch_parent(stop: &AtomicBool) {
    let Some(parent) = crate::daemon::lifecycle::parent_pid() else {
        return;
    };
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(SHUTDOWN_CHECK);
        if !crate::daemon::lifecycle::pid_is_live(parent) {
            eprintln!("storyhook daemon: parent process {parent} is gone; exiting");
            std::process::exit(0);
        }
    }
}

/// How [`tailnet_reprobe`] paces itself. Four durations rather than four
/// stray env lookups scattered through the retry loop, and a struct rather
/// than reading the environment inside [`next_reprobe_delay`] itself so that
/// function stays a pure, unit-testable calculation over explicit inputs.
struct ReprobeSchedule {
    /// The delay before the first retry attempt.
    initial: Duration,
    /// The most the exponential backoff climbs to during the aggressive
    /// phase below.
    cap: Duration,
    /// How long the aggressive phase runs (summed delay already waited out,
    /// not wall-clock spent probing) before falling back to `steady_state`.
    aggressive_window: Duration,
    /// The interval once the aggressive phase gives up climbing. Never zero
    /// and never "stop": a login-time miss must still self-heal whenever
    /// `tailscaled` finally comes up, however late, so this is a slower
    /// cadence, not a bounded retry count.
    steady_state: Duration,
}

impl ReprobeSchedule {
    /// Reads every knob from its `STORYHOOK_TAILNET_REPROBE_*_MS` override,
    /// falling back to the production default. Overridable so a test can
    /// observe a self-heal without waiting out the production interval —
    /// the same shape [`heartbeat_interval`] already uses.
    fn from_env() -> Self {
        fn ms(name: &str, default: Duration) -> Duration {
            std::env::var(name)
                .ok()
                .and_then(|s| s.parse().ok())
                .map(Duration::from_millis)
                .unwrap_or(default)
        }
        ReprobeSchedule {
            initial: ms(
                "STORYHOOK_TAILNET_REPROBE_INITIAL_MS",
                Duration::from_secs(2),
            ),
            cap: ms("STORYHOOK_TAILNET_REPROBE_CAP_MS", Duration::from_secs(60)),
            aggressive_window: ms(
                "STORYHOOK_TAILNET_REPROBE_WINDOW_MS",
                Duration::from_secs(600),
            ),
            steady_state: ms(
                "STORYHOOK_TAILNET_REPROBE_STEADY_MS",
                Duration::from_secs(300),
            ),
        }
    }
}

/// The delay before [`tailnet_reprobe`]'s next attempt, given `elapsed` (the
/// sum of every delay already waited out — not wall-clock time, which also
/// includes each probe's own duration) and how many attempts have run.
///
/// Doubles from `schedule.initial` up to `schedule.cap` for the first
/// `schedule.aggressive_window` of retrying — long enough to catch
/// `tailscaled` starting anywhere from a few seconds to a few minutes after
/// login without hammering it — then falls back to `schedule.steady_state`
/// forever. Never returns "stop": a background thread doing nothing but this
/// is unbounded on purpose, because the alternative is a login-time miss
/// that outlives a hard retry limit and goes back to lasting until restart,
/// which is the defect this exists to fix (SH-146).
fn next_reprobe_delay(schedule: &ReprobeSchedule, elapsed: Duration, attempt: u32) -> Duration {
    if elapsed >= schedule.aggressive_window {
        return schedule.steady_state;
    }
    let scaled = schedule.initial.saturating_mul(1u32 << attempt.min(20));
    scaled.min(schedule.cap)
}

/// Sleeps for `total`, checking `stop` every [`SHUTDOWN_CHECK`] rather than
/// in one long nap — the same chopped-sleep shape [`heartbeat`] uses — so a
/// shutdown does not have to wait out whatever the current backoff delay is.
/// Returns `false` if `stop` fired before `total` elapsed, so the caller can
/// bail immediately rather than going on to probe.
fn sleep_chopped(total: Duration, stop: &AtomicBool) -> bool {
    let mut waited = Duration::ZERO;
    while waited < total {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        let nap = SHUTDOWN_CHECK.min(total - waited);
        thread::sleep(nap);
        waited += nap;
    }
    !stop.load(Ordering::Relaxed)
}

/// Retries a tailnet bind [`serve`] did not already have when it started —
/// SH-146. A login-time daemon start races `tailscaled` coming up, and
/// before this nothing ever retried a miss: the daemon served loopback only
/// until somebody restarted it by hand.
///
/// Strictly timer-driven, never triggered by request arrival: a probe shells
/// out to `tailscale`, and tying that to an unauthenticated request would let
/// any peer that can reach loopback force repeated subprocess spawns — a
/// self-inflicted local DoS vector for no benefit, since an idle daemon still
/// needs to self-heal whether or not anyone is asking it anything.
///
/// One-shot: the loop returns the instant a bind succeeds and never probes
/// again for the rest of this process's life. `trusted_hosts` is written
/// — once — strictly *before* the new listener's `accept_loop` is spawned,
/// so no request can ever be accepted on an interface the allowlist does not
/// yet recognize; `host_is_trusted` is default-deny, so the reverse order
/// would only ever fail safe, but this ordering closes the question rather
/// than relying on that.
#[allow(clippy::too_many_arguments)]
fn tailnet_reprobe<'scope, S: Store, L>(
    scope: &'scope thread::Scope<'scope, '_>,
    serving: &'scope Serving<'_, S>,
    loopback_addr: SocketAddr,
    limits: http1::Limits,
    slots: Arc<http1::ConnectionSlots>,
    jobs_tx: mpsc::SyncSender<Job>,
    nested_tx: mpsc::Sender<Job>,
    on_late_tailnet_bind: &'scope L,
    stop: &AtomicBool,
) where
    L: Fn(&BoundAddress) + Send + Sync,
{
    let schedule = ReprobeSchedule::from_env();
    let mut elapsed = Duration::ZERO;
    let mut attempt = 0u32;
    loop {
        let delay = next_reprobe_delay(&schedule, elapsed, attempt);
        if !sleep_chopped(delay, stop) {
            return;
        }
        elapsed += delay;
        attempt += 1;

        let Some((listener, bind)) = probe_and_bind_tailnet(loopback_addr.port()) else {
            continue;
        };

        {
            let mut hosts = serving
                .trusted_hosts
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            hosts.extend(bind.trusted_hosts());
        }

        let bound = BoundAddress {
            loopback: loopback_addr,
            tailnet: Some(bind),
        };
        on_late_tailnet_bind(&bound);

        scope.spawn(move || {
            accept_loop(
                serving,
                listener.listener,
                false,
                limits,
                slots,
                jobs_tx,
                nested_tx,
            )
        });
        return;
    }
}

/// Each project's slug and the head of its change feed.
///
/// An unreadable store yields an empty map rather than an error: the poller is
/// a background safety net, and a transient read failure must not end it.
fn project_sequences<S: Store>(store: &S) -> BTreeMap<String, i64> {
    store
        .read(|tx| {
            let mut seqs: BTreeMap<String, i64> = BTreeMap::new();
            for project in tx.projects()? {
                seqs.insert(project.slug, tx.max_global_seq(project.id)?.get());
            }
            Ok(seqs)
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `hook_depth` is peeked from the body only for `/api/v1/invoke`; every
    /// other path is never nested, whatever its body says.
    #[test]
    fn only_invoke_requests_can_be_nested() {
        assert!(!is_nested_invoke("/api/repos", r#"{"hook_depth":1}"#));
        assert!(!is_nested_invoke("/api/v1/hello", r#"{"hook_depth":1}"#));
    }

    /// The ordinary case — no hook in progress — never takes the unbounded
    /// lane.
    #[test]
    fn a_top_level_invoke_is_not_nested() {
        assert!(!is_nested_invoke("/api/v1/invoke", r#"{"hook_depth":0}"#));
        assert!(!is_nested_invoke("/api/v1/invoke", r#"{}"#));
    }

    /// The one case the lane exists for.
    #[test]
    fn an_invoke_from_inside_a_hook_is_nested() {
        assert!(is_nested_invoke("/api/v1/invoke", r#"{"hook_depth":1}"#));
    }

    /// A malformed body reads as *not* nested — the pool, never the
    /// unbounded lane — because misrouting a genuinely nested request to the
    /// pool costs a queueing delay; misrouting an ordinary one to the
    /// unbounded lane would let malformed JSON bypass the pool's
    /// back-pressure entirely.
    #[test]
    fn a_malformed_body_defaults_to_the_ordinary_pool() {
        assert!(!is_nested_invoke("/api/v1/invoke", "{ not json"));
        assert!(!is_nested_invoke("/api/v1/invoke", ""));
    }

    fn schedule(
        initial_secs: u64,
        cap_secs: u64,
        window_secs: u64,
        steady_secs: u64,
    ) -> ReprobeSchedule {
        ReprobeSchedule {
            initial: Duration::from_secs(initial_secs),
            cap: Duration::from_secs(cap_secs),
            aggressive_window: Duration::from_secs(window_secs),
            steady_state: Duration::from_secs(steady_secs),
        }
    }

    /// SH-146's retry must not hammer `tailscale`: each attempt waits longer
    /// than the last, starting from the schedule's own initial delay.
    #[test]
    fn reprobe_delay_doubles_from_the_initial_delay() {
        let s = schedule(1, 3600, 3600, 300);
        assert_eq!(
            next_reprobe_delay(&s, Duration::ZERO, 0),
            Duration::from_secs(1)
        );
        assert_eq!(
            next_reprobe_delay(&s, Duration::from_secs(1), 1),
            Duration::from_secs(2)
        );
        assert_eq!(
            next_reprobe_delay(&s, Duration::from_secs(3), 2),
            Duration::from_secs(4)
        );
    }

    /// The backoff must never exceed the configured cap, however many
    /// attempts have run — an unbounded exponent would eventually overflow
    /// `Duration` on top of being pointless.
    #[test]
    fn reprobe_delay_never_exceeds_the_cap() {
        let s = schedule(1, 8, 3600, 300);
        assert_eq!(
            next_reprobe_delay(&s, Duration::from_secs(7), 3),
            Duration::from_secs(8)
        );
        assert_eq!(
            next_reprobe_delay(&s, Duration::from_secs(15), 4),
            Duration::from_secs(8)
        );
        assert_eq!(
            next_reprobe_delay(&s, Duration::from_secs(15), 63),
            Duration::from_secs(8),
            "a huge attempt count must not overflow the left shift"
        );
    }

    /// Past the aggressive window, the schedule falls back to the steady
    /// state — and stays there. This is what makes "never a bounded retry
    /// count" true: a daemon started days before its tailnet ever comes up
    /// still keeps trying, just slowly.
    #[test]
    fn reprobe_delay_falls_back_to_steady_state_past_the_aggressive_window() {
        let s = schedule(1, 60, 10, 300);
        assert_eq!(
            next_reprobe_delay(&s, Duration::from_secs(9), 5),
            Duration::from_secs(32),
            "still inside the aggressive window"
        );
        assert_eq!(
            next_reprobe_delay(&s, Duration::from_secs(10), 6),
            Duration::from_secs(300)
        );
        assert_eq!(
            next_reprobe_delay(&s, Duration::from_secs(999_999), 999),
            Duration::from_secs(300),
            "must keep returning a real delay arbitrarily far into a daemon's life, never \
             a signal to stop retrying"
        );
    }

    /// [`sleep_chopped`] must return promptly once `stop` is already set,
    /// rather than sleeping out the full duration first — a real backoff
    /// delay easily exceeds what a shutdown should ever have to wait for.
    #[test]
    fn sleep_chopped_returns_immediately_once_stop_is_already_set() {
        let stop = AtomicBool::new(true);
        let started = std::time::Instant::now();
        let completed = sleep_chopped(Duration::from_secs(30), &stop);
        assert!(
            !completed,
            "a pre-stopped sleep must report it did not finish"
        );
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "must not sleep at all once stop is already set"
        );
    }

    /// The ordinary case: nothing stops it, so the full duration elapses and
    /// the sleep reports completion.
    #[test]
    fn sleep_chopped_completes_when_never_stopped() {
        let stop = AtomicBool::new(false);
        let started = std::time::Instant::now();
        let completed = sleep_chopped(Duration::from_millis(20), &stop);
        assert!(completed);
        assert!(started.elapsed() >= Duration::from_millis(20));
    }
}
