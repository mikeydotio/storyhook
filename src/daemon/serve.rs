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

use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::thread;
use std::time::Duration;

use crate::api::http::{
    Reply, TrustedHosts, carries_body, finish, path_segments, read_body, request_path,
    request_query, text_reply,
};
use crate::api::rest::{self, Changed};
use crate::api::rpc;
use crate::daemon::bus::{Change, ChangeBus};
use crate::daemon::http1::{self, Header, Method, Request};
use crate::daemon::lifecycle::Hello;
use crate::daemon::tailnet::{TailnetBind, tailnet_identity};
use crate::env::Environment;
use crate::error::AppError;
use crate::store::Store;

/// How often a heartbeat is published to every connected client. A server-side
/// write failure prunes a connection that vanished without a clean close, and a
/// silence longer than this on the client side is what `web_dashboard.html`'s
/// `sseWatchdog` treats as the same thing when no write ever fails (SH-145).
/// Overridable via `STORYHOOK_SSE_HEARTBEAT_MS` so integration tests do not have
/// to wait out the production interval.
///
/// `pub(crate)` rather than private: [`crate::daemon::subscribe`] derives its
/// watchdog from this same constant, so the client and server sides of the
/// feed cannot drift apart the way `web_dashboard.html`'s separately-declared
/// `SSE_STALE_AFTER_MS` did before SH-145.
pub(crate) fn heartbeat_interval() -> Duration {
    std::env::var("STORYHOOK_SSE_HEARTBEAT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(20))
}

/// How often the daemon asks the store whether somebody else has written to it.
///
/// This is the safety net rather than the mechanism, now that
/// [`route_job_inner`]'s RPC arm calls
/// [`ChangeWatcher::notice`](crate::daemon::watch::ChangeWatcher::notice) at
/// the request boundary too (SH-202) — so a write over `POST /api/v1/invoke`,
/// the only way an ordinary `story` command reaches the store, is published
/// the instant it commits, the same as a REST-served write already was via
/// `rest::route`'s own precise `Changed` signal. (The REST arm does not also
/// call `notice()` — SH-202's own council record found that doing so could
/// attribute an unrelated out-of-band commit to the wrong response and
/// collide with `ChangeBus`'s coalescing window; see `daemon::bus`'s module
/// doc and SH-216.) This poll catches only the writes that boundary never saw
/// at all: a `story tui` session on the same store, a second machine, a
/// `sqlite3` prompt. One pragma per tick, so the interval can be short
/// without mattering.
///
/// Overridable via `STORYHOOK_CHANGE_POLL_MS`, the same seam
/// [`heartbeat_interval`] and [`dispatchers`] already use — set high enough in
/// a test, this is what proves the request boundary alone carries a write,
/// with the safety net not ticking in time to help it.
fn change_poll_interval() -> Duration {
    std::env::var("STORYHOOK_CHANGE_POLL_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_millis(250))
}

/// How often a background thread looks up to see whether it should stop.
///
/// `pub(crate)` rather than private: [`crate::daemon::github_poll`] chops its
/// own sleep into the same naps, for the same reason [`heartbeat`] and
/// [`poll_change_token`] do — a shutdown should not have to wait out a whole
/// poll interval.
pub(crate) const SHUTDOWN_CHECK: Duration = Duration::from_millis(250);

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
    /// Attributes a store-wide change to specific projects, at the request
    /// boundary in [`route_job_inner`] (SH-202) and, sharing the same
    /// baseline, in [`poll_change_token`]'s safety-net poll.
    watcher: crate::daemon::watch::ChangeWatcher,
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
    trusted_hosts: Arc<RwLock<TrustedHosts>>,
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
    /// The exact verification generation currently owned by the serialized
    /// verifier (SH-549). Shared with the progress publisher and REST board;
    /// queue ordering alone cannot answer this once priorities change.
    verification_activity: crate::daemon::verification::VerificationActivity,
    /// Engine controls are answered on per-connection workers, never the
    /// fixed store-dispatch pool. This controller owns the persistent store
    /// handle those `'static` workers require.
    engine: Arc<crate::api::engine::EngineController>,
    /// Every handoff coupon this daemon has armed and not yet spent (SH-251).
    /// An `Arc` for the same reason `dispatch_registry` is one: redemption
    /// needs nothing from the store, so it is answered on the `worker` thread
    /// and must never queue behind the dispatcher pool.
    handoff: Arc<crate::api::handoff::HandoffRegistry>,
    /// Every named, persistent, revocable dashboard token (SH-255). An `Arc`
    /// for `handoff`'s reason: `story token new|list|revoke` need nothing
    /// from the store, so they are answered on the `worker` thread and must
    /// never queue behind the dispatcher pool — `story token revoke` most of
    /// all, per this registry's own module doc.
    tokens: Arc<crate::api::tokens::TokenRegistry>,
    /// The `Set-Cookie` name a browser holds a named token in for this store
    /// (SH-255) — computed once here rather than per request, since it never
    /// changes for the life of this daemon. See
    /// [`crate::api::tokens::cookie_name`].
    cookie_name: String,
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
/// the allowlist are a snapshot of what was true when `serve` was called,
/// and everything after that lives inside this function.
///
/// `bound_hosts` are the `Host` values the *bind* earned — nothing else. The
/// reverse-proxy half of the allowlist is read from the environment here,
/// exactly once, rather than by each caller: a caller who forgot to merge it
/// in would silently narrow which proxied `Host` a mutation trusts. Both
/// callers used to write that merge by hand.
#[allow(clippy::too_many_arguments)]
pub fn serve<S: Store, F, L>(
    store: &S,
    env: &Environment,
    listeners: Vec<Listener>,
    bound_hosts: Vec<String>,
    bus: ChangeBus,
    token: String,
    ready: F,
    on_late_tailnet_bind: L,
) -> Result<(), AppError>
where
    F: FnOnce(),
    L: Fn(&BoundAddress) + Send + Sync,
{
    let has_tailnet = listeners.iter().any(|l| !l.is_loopback());
    let loopback_addr = listeners
        .iter()
        .find(|l| l.is_loopback())
        .map(Listener::addr);

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
    // Baselined here, before any listener has accepted a request and before
    // the poller below starts — the same "read the baseline before anything
    // can write" ordering `serving.inflight.harvest_stale()` relies on for
    // its own reason, so a write that landed before this daemon started is
    // history, never news.
    let watcher = crate::daemon::watch::ChangeWatcher::new(store);
    let verification_activity = crate::daemon::verification::VerificationActivity::new();
    let serving = Serving {
        store,
        env: env.clone(),
        bus: bus.clone(),
        watcher: watcher.clone(),
        trusted_hosts: Arc::new(RwLock::new(TrustedHosts::for_daemon(bound_hosts))),
        token,
        hello: Hello {
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol: crate::daemon::lifecycle::PROTOCOL,
            pid: std::process::id(),
            started_at: env.now(),
        },
        dispatch_registry: Arc::new(crate::api::dispatch::DispatchRegistry::load(env)),
        verification_activity: verification_activity.clone(),
        engine: Arc::new(crate::api::engine::EngineController::open(env)?),
        handoff: Arc::new(crate::api::handoff::HandoffRegistry::new()),
        tokens: Arc::new(crate::api::tokens::TokenRegistry::load(env)),
        cookie_name: crate::api::tokens::cookie_name(env),
        inflight: Arc::new(crate::daemon::lifecycle::InFlight::new(env.clone())),
        draining: AtomicBool::new(false),
    };
    // Before `ready()`, so no listener has accepted a request a client could
    // poll a stale record from: a record surviving to here can only be one a
    // *previous* daemon left behind (SH-173).
    serving.inflight.harvest_stale();

    // Only in a build carrying live crash points, and only when a test asked
    // for it — see `crash::maybe_trigger_test_panic`'s own doc for why this is
    // the right moment (SH-287).
    #[cfg(feature = "fault-injection")]
    crate::daemon::crash::maybe_trigger_test_panic();

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
            let watcher = watcher.clone();
            scope.spawn(move || poll_change_token(store, &watcher, &bus, &stop));
        }
        {
            let stop = Arc::clone(&stop);
            let env = env.clone();
            scope.spawn(move || watch_parent(&env, &stop));
        }
        // The unattended GitHub poll (SH-212) — absent entirely without the
        // `github-pr` feature, the same way `pr_check::run_check`, the
        // engine it spends its credential on, is unreachable without it.
        #[cfg(feature = "github-pr")]
        {
            let stop = Arc::clone(&stop);
            let env = env.clone();
            scope.spawn(move || crate::daemon::github_poll::poll_github(store, &env, &stop));
        }
        {
            let stop = Arc::clone(&stop);
            let env = env.clone();
            let bus = bus.clone();
            let activity = verification_activity.clone();
            let inflight = Arc::clone(&serving.inflight);
            scope.spawn(move || {
                crate::daemon::verification::poll_verification(
                    store, &env, &bus, &stop, &activity, &inflight,
                )
            });
        }
        {
            // SH-549: queue rank can change while a lower-priority candidate
            // is already running, so the publisher shares exact ownership
            // with the verifier rather than inferring it from queue position.
            let stop = Arc::clone(&stop);
            let env = env.clone();
            let activity = verification_activity.clone();
            scope.spawn(move || {
                crate::daemon::verification_progress::poll_verification_progress(
                    store, &env, &stop, &activity,
                )
            });
        }
        // The Full Auto engine trigger (SH-466): a restart sweep once, then
        // the ordinary reconcile pass on every bus wake or coarse tick. One
        // thread, sequential, so nothing else can race the restart sweep
        // over the same lane rows.
        {
            let stop = Arc::clone(&stop);
            let env = env.clone();
            let bus = bus.clone();
            scope.spawn(move || crate::daemon::engine::poll_engine(store, &env, &bus, &stop));
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

        // One-shot, not a loop like the threads above — files whatever
        // crashes the startup harvest found, then ends. After `ready()` so a
        // store write, however unlikely to matter, never delays this daemon
        // answering its first request (SH-287).
        {
            let env = env.clone();
            scope.spawn(move || crate::daemon::crash::file_pending(store, &env));
        }

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
            let bind_addr = listener.addr();
            scope.spawn(move || {
                accept_loop(
                    serving,
                    listener.listener,
                    bind_addr,
                    http_limits,
                    slots,
                    jobs_tx,
                    nested_tx,
                )
            });
        }
        let primary_addr = primary.addr();
        accept_loop(
            &serving,
            primary.listener,
            primary_addr,
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
/// address actually bound, loopback and tailnet alike, plus the token this
/// call minted for itself. Three things about the address only the server
/// itself can report, and which no probe from outside can establish: *which*
/// address it got, that the address is its own — a caller that merely
/// connects to a port cannot tell this server apart from some other process
/// holding it — and whether the best-effort tailnet bind succeeded, which is
/// what a caller must not guess by probing the machine (SH-110).
///
/// The token is real (`lifecycle::mint_token`, not an empty placeholder) —
/// every `/api/**` route requires one since SH-187, and [`crate::api::rpc::token_ok`] fails
/// closed on an empty `expected` on purpose, so an empty string here would
/// admit nothing a test could ever authenticate. It lives only in this
/// process's memory and in `ready`'s argument: no portfile, no pidfile, none
/// of production's ceremony, matching every other way this seam differs
/// from [`super::lifecycle::run`].
///
/// Fifth departure from production, added by SH-186: this seam probes the
/// tailnet *synchronously*, right here, before `ready` fires — production no
/// longer does that anywhere. A caller of this seam needs the tailnet answer
/// available the instant `ready` runs (`storyhook-test-support::try_serve_on`
/// hands it straight back as `BoundAddress`, and a whole class of tests —
/// `web_serve_binds_tailnet_ip_when_available` and its siblings in
/// `tests/web_test.rs` — assert on it), and there is no analogue of
/// `tailnet_reprobe`'s background settle to wait on inside a synchronous test
/// helper without reintroducing a wait budget this seam has no reason to
/// carry. The daemon's own self-heal path (`tailnet_reprobe`) is exercised
/// separately and end-to-end by `tests/tailnet_rebind.rs`.
#[cfg(feature = "test-seam")]
pub fn bind_and_serve<S: Store, F>(
    store: &S,
    env: &Environment,
    port: u16,
    ready: F,
) -> Result<(), AppError>
where
    F: FnOnce(BoundAddress, String),
{
    let (mut listeners, mut bound) = bind_listeners(port)?;
    if let Some((listener, bind)) = probe_and_bind_tailnet(bound.port()) {
        bound.tailnet = Some(bind);
        listeners.push(listener);
    }
    eprintln!("Storyhook dashboard: http://127.0.0.1:{}", bound.port());
    let token = crate::daemon::lifecycle::mint_token();
    serve(
        store,
        env,
        listeners,
        bound.trusted_hosts(),
        ChangeBus::new(),
        token.clone(),
        move || ready(bound, token),
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

/// What [`Listener::adopt`] requires the socket it is given to have bound.
///
/// Named rather than a bare `IpAddr`, so a loopback expectation can never be
/// satisfied by coincidentally matching `127.0.0.1` as a literal — it is
/// checked with [`std::net::IpAddr::is_loopback`], the same broad test
/// [`Listener::is_loopback`] uses, so `::1` is an equally valid loopback bind.
#[derive(Debug, Clone, Copy)]
pub enum Expected {
    /// The socket must have bound a loopback address.
    Loopback,
    /// The socket must have bound exactly this address — the tailnet IPv4
    /// [`tailnet_identity`] just reported, passed back in rather than
    /// re-derived, so this checks agreement with a specific probe result
    /// rather than "is this address tailnet-shaped" in the abstract.
    Tailnet(std::net::IpAddr),
}

impl std::fmt::Display for Expected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Expected::Loopback => write!(f, "a loopback address"),
            Expected::Tailnet(ip) => write!(f, "the tailnet address {ip}"),
        }
    }
}

/// A bound listener, and the address it actually bound.
///
/// The loopback distinction is security-relevant rather than cosmetic:
/// `/api/v1/*` is a full-privilege surface answered on loopback only, and
/// [`crate::api::rpc::admission`]'s first conjunct is exactly that answer. So
/// the accept loop has to know which interface a request came in on.
///
/// **There is no `loopback` field, and that is the point (SH-253).** There used
/// to be one, stamped `true` beside a hardcoded `127.0.0.1` literal — a *claim*
/// about the bind sitting next to it rather than a *consequence* of it. The two
/// could not disagree only because the literal was a literal; the day anything
/// varied the bind address, a listener still stamped `loopback: true` would have
/// started admitting tokenless reads from anywhere, and no test would have
/// noticed. [`Self::is_loopback`] is now computed from [`Self::addr`], so the
/// disagreement is unrepresentable rather than merely unlikely.
pub struct Listener {
    /// The bound socket.
    listener: TcpListener,
    /// The address [`Self::listener`] is bound to, read from the socket after
    /// the bind rather than copied from what the bind was asked for — a
    /// port-0 bind gets a real port here, and a host with an unusual routing
    /// table gets whatever the kernel actually gave it.
    addr: SocketAddr,
}

impl Listener {
    /// Adopts an already-bound socket, reading its address back off it and
    /// refusing to serve it unless it matches `expected`.
    ///
    /// The only constructor, so every `Listener` in existence is one this
    /// module itself decided to serve. Fails if the socket cannot report an
    /// address (not bound — the same failure this used to raise inline) or if
    /// it *is* bound, but to something other than what the caller expected:
    /// a wildcard address, a LAN address, or a tailnet address that does not
    /// match the identity that was just probed. Both of today's callers
    /// ([`bind_listeners`], [`probe_and_bind_tailnet`]) only ever pass an
    /// address that already matches — this check has nothing to catch in
    /// production as written — but SH-255 makes the module's own doc comment
    /// ("nothing is ever bound to `0.0.0.0`... or a plain LAN address") a
    /// property of this function rather than a fact about its two current
    /// callers' good behavior. A third caller that got its address wrong
    /// fails here instead of silently serving it.
    pub fn adopt(listener: TcpListener, expected: Expected) -> Result<Self, AppError> {
        let addr = listener
            .local_addr()
            .map_err(|e| AppError::Storage(format!("bound listener has no address: {e}")))?;
        let matches = match expected {
            Expected::Loopback => addr.ip().is_loopback(),
            Expected::Tailnet(ip) => addr.ip() == ip,
        };
        if !matches {
            return Err(AppError::Storage(format!(
                "refusing to serve {addr}: expected {expected}"
            )));
        }
        Ok(Listener { listener, addr })
    }

    /// The address this listener bound.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Whether this listener answers on the loopback interface.
    ///
    /// Derived, never asserted: `127.0.0.1` and `::1` are loopback, `0.0.0.0`
    /// and a tailnet address are not. A caller cannot set this to disagree with
    /// the socket, because there is nothing to set.
    pub fn is_loopback(&self) -> bool {
        self.addr.ip().is_loopback()
    }
}

/// Binds loopback on `port`. Nothing else — no tailnet probe, no tailnet
/// bind attempt.
///
/// Before SH-186 this also shelled out to `tailscale status --json` (capped
/// at [`TAILNET_PROBE_TIMEOUT`](super::tailnet::TAILNET_PROBE_TIMEOUT))
/// before returning, so for as long as that probe blocked — including a
/// `tailscaled` wedge, observed stuck for minutes on this machine — the
/// socket above accepted connections and answered nothing at all, and no
/// portfile existed yet for any client to find. A production caller has
/// exactly one path to a tailnet bind now: [`tailnet_reprobe`], spawned
/// unconditionally by [`serve`] on a background thread, whose first attempt
/// runs immediately rather than after its retry schedule's `initial` delay.
/// This function's only job is to make the socket real; the tailnet question
/// is answered later and never blocks it.
pub fn bind_listeners(port: u16) -> Result<(Vec<Listener>, BoundAddress), AppError> {
    let socket = TcpListener::bind(("127.0.0.1", port)).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            AppError::Usage(format!(
                "Port {port} already in use. Try a different port with --port."
            ))
        } else {
            AppError::Storage(format!("Failed to start web server: {e}"))
        }
    })?;
    let loopback = Listener::adopt(socket, Expected::Loopback)?;
    let bound = loopback.addr();

    Ok((
        vec![loopback],
        BoundAddress {
            loopback: bound,
            tailnet: None,
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
            // `adopt` can fail if the socket cannot report its own address,
            // or if what it reports disagrees with the identity that was
            // just probed (SH-255) -- neither should happen for a bind that
            // just succeeded against that exact address, but a tailnet bind
            // is best-effort throughout this function, so either failure
            // joins the other three cases `None` covers rather than becoming
            // the one that is fatal.
            let expected = Expected::Tailnet(std::net::IpAddr::V4(bind.ip()));
            Some((Listener::adopt(listener, expected).ok()?, bind))
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

/// Tailscale's CGNAT range: every tailnet IPv4 address is drawn from here.
/// Documented at <https://tailscale.com/kb/1015/100.x-addresses> — a `/10`,
/// so the second octet's top two bits are fixed and the low six vary
/// (`100.64.0.0` through `100.127.255.255`).
const TAILSCALE_CGNAT_OCTET0: u8 = 100;
const TAILSCALE_CGNAT_OCTET1_RANGE: std::ops::RangeInclusive<u8> = 64..=127;

/// Whether `ip` falls in [`TAILSCALE_CGNAT_OCTET0`]/[`TAILSCALE_CGNAT_OCTET1_RANGE`].
fn in_tailscale_cgnat(ip: std::net::Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == TAILSCALE_CGNAT_OCTET0 && TAILSCALE_CGNAT_OCTET1_RANGE.contains(&b)
}

/// Tailscale's IPv6 ULA range: every tailnet IPv6 address is drawn from
/// here. Documented at the same URL as the CGNAT range above — a `/48`, so
/// the first six bytes are fixed and the remaining ten vary.
const TAILSCALE_ULA_PREFIX: [u8; 6] = [0xfd, 0x7a, 0x11, 0x5c, 0xa1, 0xe0];

/// Whether `ip` falls in [`TAILSCALE_ULA_PREFIX`]'s `/48`.
fn in_tailscale_ula(ip: std::net::Ipv6Addr) -> bool {
    ip.octets()[..6] == TAILSCALE_ULA_PREFIX
}

/// Proof that a peer was admitted to connect to the interface it reached.
///
/// Zero-sized, with a private field, so [`peer_admitted`] is the only thing
/// that can produce one — the same idiom [`crate::api::http::LocalRequest`]
/// uses, for the same reason: a caller cannot construct one to skip the
/// check, only to prove they already ran it.
#[derive(Debug)]
pub struct PeerAdmitted(());

/// Why [`peer_admitted`] refused a connection, named rather than a bare
/// `bool` — the design this implements requires the refusal to say which
/// range it failed, for [`accept_loop`]'s own log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerRefused {
    /// The loopback listener admits only `127.0.0.0/8` and `::1`.
    NotLoopback,
    /// The tailnet listener admits loopback, [`TAILSCALE_CGNAT_OCTET0`]'s
    /// `/10`, and [`TAILSCALE_ULA_PREFIX`]'s `/48` — this peer was in none
    /// of the three.
    OutsideTailnetRanges,
}

impl std::fmt::Display for PeerRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeerRefused::NotLoopback => {
                write!(f, "the loopback listener only admits 127.0.0.0/8 and ::1")
            }
            PeerRefused::OutsideTailnetRanges => write!(
                f,
                "the tailnet listener only admits loopback, Tailscale's CGNAT \
                 range (100.64.0.0/10) and its IPv6 ULA range (fd7a:115c:a1e0::/48)"
            ),
        }
    }
}

/// Whether `peer_addr` may connect to a listener bound at `bind_addr` — the
/// off-tailnet refusal SH-255's council decided, composed with bind-time
/// refusal ([`Listener::adopt`]) rather than replacing it: bind-time keeps
/// this daemon from ever *listening* somewhere it should not, and this
/// closes the gap that leaves open — a tailnet is not a firewall, so binding
/// only the tailnet interface does not by itself stop an off-tailnet peer
/// that can still route packets to it (a misconfigured subnet router, a
/// bridged VM, `iptables` forwarding) from connecting anyway.
///
/// # Pure, on purpose
///
/// Zero syscalls, zero locks, zero shelling out to `tailscale` — the two
/// `SocketAddr`s already carry everything this needs. That last property is
/// not a style preference: [`tailnet_reprobe`]'s own doc comment forbids
/// tying a `tailscale` probe to request arrival at all, because doing so
/// would let any peer that can reach a listener force repeated subprocess
/// spawns — a self-inflicted local DoS (SH-186). A function that cannot
/// shell out cannot reopen that hole no matter where it is called from.
///
/// # The two rules
///
/// * **The loopback listener** (`bind_addr.ip().is_loopback()`) admits only
///   a loopback peer. Belt-and-braces: the kernel will not route a
///   non-loopback peer to a socket bound to `127.0.0.1` in the first place,
///   but the check costs nothing and this function's contract should not
///   quietly depend on that being true on every platform forever.
/// * **The tailnet listener** admits a loopback peer, this machine's own
///   `bind_addr` itself as a peer address (the OS does not always route a
///   locally-owned destination through `lo` — connecting to your own
///   interface's address can arrive with that same address as the source,
///   and it is the identical "this machine's own process" case as the
///   loopback one just named), plus anything in Tailscale's own CGNAT or
///   ULA range. It does **not** check the peer against the *specific*
///   tailnet this daemon joined — any tailnet-shaped address is admitted,
///   the same way [`Listener::adopt`] only ever bound the address this
///   daemon's own probe reported, not a claim about who else might be on it.
///
/// # What this does not, and cannot, enforce
///
/// A subnet router or an exit node can put a genuinely off-tailnet peer's
/// traffic inside these ranges; Tailscale ACLs, not this daemon, are the
/// real membership authority. Recorded here rather than implied away —
/// see the README's Network exposure section.
pub fn peer_admitted(
    bind_addr: SocketAddr,
    peer_addr: SocketAddr,
) -> Result<PeerAdmitted, PeerRefused> {
    let peer_is_loopback = peer_addr.ip().is_loopback();
    if bind_addr.ip().is_loopback() {
        return if peer_is_loopback {
            Ok(PeerAdmitted(()))
        } else {
            Err(PeerRefused::NotLoopback)
        };
    }
    let admitted = peer_is_loopback
        // A process on this machine reaching the tailnet-bound socket by its
        // own interface address, rather than through loopback, sees itself
        // as the peer -- the OS picks the outbound interface's own address
        // as the source when the destination is a locally-owned one, and
        // does not always route that through `lo`. Ordinary, not an attack,
        // for the identical reason a loopback peer is admitted just above:
        // it is this daemon's own machine. `bind_addr` is not a claim being
        // trusted here either -- `Listener::adopt` already refused any bind
        // that was not either loopback or the exact identity this daemon's
        // own probe reported.
        || peer_addr.ip() == bind_addr.ip()
        || match peer_addr.ip() {
            std::net::IpAddr::V4(ip) => in_tailscale_cgnat(ip),
            std::net::IpAddr::V6(ip) => in_tailscale_ula(ip),
        };
    if admitted {
        Ok(PeerAdmitted(()))
    } else {
        Err(PeerRefused::OutsideTailnetRanges)
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
    bind_addr: SocketAddr,
    limits: http1::Limits,
    slots: Arc<http1::ConnectionSlots>,
    jobs_tx: mpsc::SyncSender<Job>,
    nested_tx: mpsc::Sender<Job>,
) {
    // Derived here rather than passed in, same reasoning as
    // `Listener::is_loopback` (SH-253): a caller with `bind_addr` in hand
    // cannot separately assert a `loopback` that disagrees with it.
    let loopback = bind_addr.ip().is_loopback();
    let token: Arc<str> = Arc::from(serving.token.as_str());
    let cookie_name: Arc<str> = Arc::from(serving.cookie_name.as_str());
    let bus = serving.bus.clone();
    let trusted_hosts = Arc::clone(&serving.trusted_hosts);
    let env = serving.env.clone();
    let dispatch_registry = Arc::clone(&serving.dispatch_registry);
    let engine = Arc::clone(&serving.engine);
    let handoff = Arc::clone(&serving.handoff);
    let tokens = Arc::clone(&serving.tokens);

    // One closure per listener, cloned by `http1::serve_connections` once per
    // accepted connection (and, within a kept-alive connection, called again
    // for each request it carries) — every capture here is `Clone`, so the
    // closure is too, without needing any of them to be `Sync`. `trusted_hosts`
    // is read fresh on every call rather than snapshotted once at listener
    // startup: a late tailnet bind (SH-146) can update it for as long as this
    // listener runs, and the very next request must see it.
    //
    // The read lock is held only long enough to `.clone()` the small value
    // out — never across `worker`'s call, which two of its branches can hold
    // open far longer than a lock should ever be held: `GET /api/events`
    // streams for as long as the client's tab is open, and a dispatch
    // intercept runs "for tens of seconds" by its own doc comment. Before
    // SH-186, `tailnet_reprobe`'s write lock (the only writer) almost never
    // ran on a machine whose tailnet was already bound at startup, so this
    // was latent; SH-186 makes that write run for nearly every daemon, which
    // turned an occasional wait into a request that can no longer complete
    // until the SSE tab closes — observed as `story daemon stop` timing out
    // against a daemon serving an open dashboard tab. A cloned, owned value
    // gives `worker` the same "fresh per request" guarantee (SH-146's own
    // requirement — one snapshot per request, not one for the listener's
    // whole life) without asking any reader to hold the lock for longer than
    // a `Vec` copy takes.
    let handler = move |request: Request| {
        let trusted_hosts = trusted_hosts
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
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
            &engine,
            &handoff,
            &tokens,
            &cookie_name,
        )
    };

    // Off-tailnet refusal (SH-255): a peer this daemon should never talk to
    // is refused on the accept thread itself, before a `Request` — before
    // even a thread — exists for it. See `peer_admitted`'s own doc comment
    // for why this composes with, rather than replaces, `Listener::adopt`'s
    // bind-time refusal.
    let admit = move |peer_addr: SocketAddr| match peer_admitted(bind_addr, peer_addr) {
        Ok(_witness) => true,
        Err(reason) => {
            eprintln!(
                "storyhook daemon: refused a connection from {peer_addr} on \
                 {bind_addr}: {reason}"
            );
            false
        }
    };

    http1::serve_connections(listener, limits, slots, admit, handler);
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
    trusted_hosts: &TrustedHosts,
    env: &Environment,
    dispatch_registry: &Arc<crate::api::dispatch::DispatchRegistry>,
    engine: &Arc<crate::api::engine::EngineController>,
    handoff: &Arc<crate::api::handoff::HandoffRegistry>,
    tokens: &Arc<crate::api::tokens::TokenRegistry>,
    cookie_name: &str,
) {
    let mut request = request;
    let method = request.method().clone();
    let url = request.url().to_string();
    let path = request_path(&url).to_string();
    let query = request_query(&url).map(str::to_string);
    let headers = request.headers().to_vec();

    let segments = path_segments(&path);
    if let Some(reply) = rpc::admission(&segments, &headers, token, loopback) {
        finish(request, reply);
        return;
    }

    // Every other `/api/**` route's own gate (SH-187): the CSRF/DNS-rebinding
    // guard for a mutation, the bearer token for everything, on both
    // listeners. Checked here, ahead of the SSE and dispatch branches below
    // (and so ahead of `dispatch::intercept`'s own, narrower copy of the
    // same two checks — harmless, deliberate redundancy that keeps that
    // module's own test suite, which calls `intercept` directly, exercising
    // real logic rather than dead code), for the identical reason
    // `rpc::admission` runs first: an unauthenticated peer must never reach
    // even a read, let alone make this daemon wait on a body it has no
    // right to send.
    if let Some(reply) = crate::api::admission::admission(
        &segments,
        &method,
        &headers,
        trusted_hosts,
        token,
        std::time::Instant::now(),
        tokens,
        cookie_name,
        chrono::Utc::now(),
    ) {
        finish(request, reply);
        return;
    }

    // The handoff routes (SH-251), answered here for the same reason the SSE
    // branch below is: neither needs anything from the store, so neither may
    // queue behind the dispatcher pool. A browser redeeming its coupon on
    // page load, stuck behind eight long-running commands, would time out for
    // no reason at all.
    //
    // Immediately after `admission::admission` and before everything else, so
    // that `POST /api/v1/handoff` has already passed `rpc::admission`'s
    // loopback-and-token gate above, and `POST /handoff` — which is outside
    // `/api` and so passes through both gates untouched — reaches its own.
    if let Some(reply) = crate::api::handoff::intercept(
        &segments,
        &method,
        &headers,
        trusted_hosts,
        token,
        loopback,
        std::time::Instant::now(),
        handoff,
        tokens,
        cookie_name,
        chrono::Utc::now(),
    ) {
        finish(request, reply);
        return;
    }

    // The pasted-token exchange (SH-255): also outside `/api`, for
    // `handoff::intercept`'s reason immediately above — the caller has no
    // credential yet, so this cannot sit behind `admission::admission`'s
    // gate.
    if let Some(reply) = crate::api::tokens::intercept_exchange(
        &segments,
        &method,
        &headers,
        trusted_hosts,
        cookie_name,
        token,
        tokens,
        chrono::Utc::now(),
        std::time::Instant::now(),
    ) {
        finish(request, reply);
        return;
    }

    // Minting, listing and revoking named tokens (SH-255), answered here for
    // `handoff::intercept`'s reason: none of the three needs the store, so
    // none may queue behind the dispatcher pool. `rpc::admission` above has
    // already required loopback and the master token for anything under
    // `/api/v1/*`, which is where this route lives.
    if let Some(reply) = crate::api::tokens::intercept(
        &segments,
        &method,
        query.as_deref(),
        &headers,
        token,
        chrono::Utc::now(),
        std::time::Instant::now(),
        tokens,
    ) {
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
        query.as_deref(),
        &headers,
        trusted_hosts,
        token,
        env,
        &bus,
        dispatch_registry,
        tokens,
        cookie_name,
        chrono::Utc::now(),
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

    // Engine controls can synchronously run `story.sh unclaim`, whose own
    // `story` calls return through `/api/v1/invoke`. Intercept after admission
    // and body acquisition, but before a `Job` can occupy the fixed store
    // pool, so that nested work always has a dispatcher available.
    if let Some(reply) = crate::api::engine::intercept(
        &segments,
        &method,
        query.as_deref(),
        &headers,
        &body,
        trusted_hosts,
        token,
        engine,
        &bus,
        tokens,
        cookie_name,
        chrono::Utc::now(),
    ) {
        finish(request, reply);
        return;
    }

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
        // Named ahead of the match, which consumes `answer`: the shutdown
        // arm below publishes `Change::Reload` itself and the daemon is on
        // its way out, so it is the one case that skips the ordinary notice
        // after this match.
        let is_shutdown = matches!(answer, rpc::Answer::Shutdown(_));
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
        // Published here, at the request boundary, for the RPC surface —
        // `rest::route`'s writes already publish below; this is what makes
        // `POST /api/v1/invoke`, the only way an ordinary `story` CLI
        // command reaches the store, do the same (SH-202). `rpc::route`
        // carries no `Changed` signal the way `rest::route` does, so every
        // reply pays one `PRAGMA data_version` rather than only a write —
        // cheap, and the alternative is threading "what changed" through
        // every one of `StoreInvoker`'s call sites for a surface that never
        // needed to know.
        if !is_shutdown {
            serving.watcher.notice(serving.store, &serving.bus);
        }
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
    let routed = rest::route_with_activity(
        serving.store,
        &serving.env,
        &serving.verification_activity,
        rest::RouteRequest::new(&job.method, &job.path, &job.headers, &job.body),
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
    // Deliberately does NOT also call `serving.watcher.notice()` here.
    // Every mutating REST route is built through `Routed::changing`, so its
    // `Changed` coverage above is already exhaustive and `notice()`'s
    // store-wide diff would add none. A council vote scoped SH-202's fix to
    // the RPC arm alone, which is the gap that story was actually filed
    // about. That vote's trail was written to an untracked, worktree-local
    // directory and is gone (SH-363), and it belonged to no story, so the
    // sentence above is now the whole of the record: the boundary is the RPC
    // arm, and this comment is why.
    //
    // Its second reason has since expired: a `notice()` call here could
    // attribute an unrelated out-of-band commit to this response and consume
    // `ChangeBus`'s 200ms coalesce window, silently dropping a *later*,
    // genuinely relevant publish for the same project. SH-216 deleted that
    // window, so an incidental publish now costs one extra refetch instead.
    // The first reason still stands on its own, so the behaviour here is
    // unchanged. The residual cost is also unchanged: the safety-net poll may
    // re-publish a REST change up to one poll interval after this precise
    // publish already did.
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

/// Watches the store's change token on a schedule and reports what moved
/// when it changes — [`ChangeWatcher::notice`](crate::daemon::watch::ChangeWatcher::notice)'s
/// attribution, run as the safety net beside [`route_job_inner`]'s own RPC-arm
/// call at the request boundary, sharing `watcher`'s baseline with it so a
/// commit both notice is attributed once, by whichever notices it first
/// (SH-202).
///
/// The sleep is chopped into [`SHUTDOWN_CHECK`] naps rather than taken whole,
/// the same reason [`heartbeat`] does: `thread::scope`'s join should not have
/// to wait out a whole poll interval on shutdown, whatever
/// [`change_poll_interval`] is set to.
fn poll_change_token<S: Store>(
    store: &S,
    watcher: &crate::daemon::watch::ChangeWatcher,
    bus: &ChangeBus,
    stop: &AtomicBool,
) {
    let interval = change_poll_interval();
    let mut waited = Duration::ZERO;
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(SHUTDOWN_CHECK);
        waited += SHUTDOWN_CHECK;
        if waited >= interval {
            waited = Duration::ZERO;
            watcher.notice(store, bus);
        }
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
///
/// Clears the portfile before exiting, the same as the shutdown route
/// (`route_job_inner`'s `Verdict::Shutdown` arm) — this is an orderly exit,
/// not a crash, and a portfile left behind here would be indistinguishable
/// from one a real crash left, misleading the crash detector the next daemon
/// runs at startup (SH-287).
fn watch_parent(env: &Environment, stop: &AtomicBool) {
    let Some(parent) = crate::daemon::lifecycle::parent_pid() else {
        return;
    };
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(SHUTDOWN_CHECK);
        if !crate::daemon::lifecycle::pid_is_live(parent) {
            eprintln!("storyhook daemon: parent process {parent} is gone; exiting");
            crate::daemon::lifecycle::clear_info(env);
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

/// Binds the tailnet interface [`serve`] did not already have when it
/// started — SH-146's self-heal path, and since SH-186 the *only* path any
/// tailnet bind takes, first attempt included. `bind_listeners` no longer
/// probes at all, so nothing before `serve` waits on `tailscale`; this
/// background thread is where that question gets answered, whenever it
/// answers.
///
/// The first attempt runs immediately — probe, then sleep only on a miss —
/// rather than waiting out `schedule.initial` first, which is what a login-
/// time daemon or a `story web --serve` that has never had a tailnet bind
/// needs: SH-146's original shape only ever ran this loop *after* a startup
/// miss already happened, so paying `initial` before the first retry cost
/// nothing extra. Now this loop's first attempt often *is* the only attempt
/// on a healthy machine, and delaying it would trade the bug this story fixes
/// for a flat regression on the common case.
///
/// Strictly timer-driven after that, never triggered by request arrival: a
/// probe shells out to `tailscale`, and tying that to an unauthenticated
/// request would let any peer that can reach loopback force repeated
/// subprocess spawns — a self-inflicted local DoS vector for no benefit,
/// since an idle daemon still needs to self-heal whether or not anyone is
/// asking it anything.
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
        if let Some((listener, bind)) = probe_and_bind_tailnet(loopback_addr.port()) {
            {
                let mut hosts = serving
                    .trusted_hosts
                    .write()
                    .unwrap_or_else(PoisonError::into_inner);
                hosts.add_bound(bind.trusted_hosts());
            }

            let bound = BoundAddress {
                loopback: loopback_addr,
                tailnet: Some(bind),
            };
            on_late_tailnet_bind(&bound);

            // `addr()` rather than the `false` `loopback` literal that used
            // to be written here. A tailnet bind is never loopback, so the
            // literal was right — but it was right by assertion, which is
            // the whole of what SH-253 removes: this listener now answers
            // from the address it bound, exactly as the two in `serve` do.
            let bind_addr = listener.addr();
            scope.spawn(move || {
                accept_loop(
                    serving,
                    listener.listener,
                    bind_addr,
                    limits,
                    slots,
                    jobs_tx,
                    nested_tx,
                )
            });
            return;
        }

        // Backoff belongs to a failed attempt, never to the next one to try
        // — which is what keeps this the same schedule SH-146 already
        // proved out, just entered one probe earlier.
        let delay = next_reprobe_delay(&schedule, elapsed, attempt);
        if !sleep_chopped(delay, stop) {
            return;
        }
        elapsed += delay;
        attempt += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- The loopback label follows the bind (SH-253) ---

    /// The flag is a consequence of the bind, not a claim made beside it.
    ///
    /// Binds for real rather than fabricating a `SocketAddr`: the property
    /// under test is that the label follows *the socket*, and a hand-built
    /// address would test only that `is_loopback()` calls `is_loopback()`.
    #[test]
    fn a_listener_that_bound_loopback_is_marked_loopback() {
        let local = Listener::adopt(
            TcpListener::bind(("127.0.0.1", 0)).expect("binding loopback"),
            Expected::Loopback,
        )
        .expect("a freshly bound socket reports its address");

        assert!(local.is_loopback(), "127.0.0.1 is the loopback interface");
    }

    // --- A wildcard or mismatched bind is unserveable, not merely
    // --- never-written (SH-255) ---

    /// `0.0.0.0` — the wildcard — is reachable from every interface this
    /// machine has, so it is emphatically not loopback. It is also the one
    /// non-loopback address any machine can bind without a network to be on,
    /// which is what keeps this test hermetic.
    ///
    /// Before SH-255 this constructed a `Listener` and asserted
    /// `!is_loopback()` on it — proving the label followed the bind, but
    /// also proving a wildcard-bound `Listener` could exist at all.
    /// `adopt` refusing it outright is the stronger property: the module's
    /// own doc comment ("nothing is ever bound to `0.0.0.0`") is now
    /// something this function enforces, not just a fact about who calls it.
    #[test]
    fn adopt_refuses_a_wildcard_bind_expected_to_be_loopback() {
        let wildcard = TcpListener::bind(("0.0.0.0", 0)).expect("binding the wildcard address");
        assert!(
            Listener::adopt(wildcard, Expected::Loopback).is_err(),
            "a wildcard bind must never become a servable Listener"
        );
    }

    // --- The tailnet probe never runs on the pre-serve path (SH-186) ---

    /// `bind_listeners` must never itself produce a tailnet bind — every
    /// tailnet bind happens on [`tailnet_reprobe`]'s background thread,
    /// `bind_and_serve`'s test-seam probe, or nowhere at all. Deliberately a
    /// pure return-value check rather than a `tailscale` shim on `PATH`:
    /// mutating `PATH` for one unit test would leak into every other test
    /// sharing this process, and the property under test does not need a
    /// shim at all — `bind_listeners` contains no call to
    /// `probe_and_bind_tailnet` any more, so this holds on a machine with a
    /// real tailnet exactly as it does on one with none. Before SH-186 this
    /// was false on any tailnet-equipped machine (this repo's own dev
    /// machine included): `bind_listeners` probed synchronously and
    /// `bound.tailnet` came back `Some(..)`.
    #[test]
    fn bind_listeners_never_itself_produces_a_tailnet_bind() {
        let (listeners, bound) =
            bind_listeners(0).expect("binding loopback on an OS-assigned port");
        assert_eq!(
            listeners.len(),
            1,
            "bind_listeners must return loopback and nothing else"
        );
        assert!(
            bound.tailnet.is_none(),
            "bind_listeners must never bind a tailnet interface itself, on any machine: got {:?}",
            bound.tailnet
        );
    }

    /// The tailnet arm of the same property: a real, non-loopback socket
    /// this machine can genuinely bind (no live Tailscale required — this is
    /// any bindable local address), refused because it does not match the
    /// specific IP `Expected::Tailnet` names. Mirrors `tailnet_rebind.rs`'s
    /// routing-lookup trick for finding one hermetically.
    #[test]
    fn adopt_refuses_a_tailnet_bind_that_does_not_match_the_probed_identity() {
        let Some(real_ip) = a_bindable_non_loopback_ip() else {
            return; // no non-loopback interface on this machine/CI runner
        };
        let bound = TcpListener::bind((real_ip, 0)).expect("binding a real local address");
        let wrong_identity = std::net::IpAddr::V4(std::net::Ipv4Addr::new(100, 64, 0, 1));
        assert!(
            Listener::adopt(bound, Expected::Tailnet(wrong_identity)).is_err(),
            "a bind that disagrees with the probed tailnet identity must be refused"
        );
    }

    /// The positive control: the same kind of bind, this time matching
    /// exactly what was expected, succeeds — so the refusal above is the
    /// mismatch being caught, not `adopt` refusing every non-loopback bind
    /// unconditionally.
    #[test]
    fn adopt_admits_a_tailnet_bind_that_matches_the_probed_identity() {
        let Some(real_ip) = a_bindable_non_loopback_ip() else {
            return;
        };
        let bound = TcpListener::bind((real_ip, 0)).expect("binding a real local address");
        assert!(
            Listener::adopt(bound, Expected::Tailnet(real_ip)).is_ok(),
            "a bind that matches the expected identity must be admitted"
        );
    }

    /// A real bindable, non-loopback IPv4 address on this machine, found by a
    /// routing lookup rather than an actual connection — no packet leaves the
    /// machine, so this needs no network access and no live Tailscale.
    /// `None` on a machine with no such interface at all (a bare CI runner),
    /// which the two tests above treat as "nothing to test here" rather than
    /// a failure.
    fn a_bindable_non_loopback_ip() -> Option<std::net::IpAddr> {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        socket.connect("8.8.8.8:80").ok()?;
        let ip = socket.local_addr().ok()?.ip();
        (!ip.is_loopback()).then_some(ip)
    }

    // --- Off-tailnet refusal at accept time (SH-255) ---

    fn addr(ip: &str) -> SocketAddr {
        SocketAddr::new(ip.parse().expect("a literal IP parses"), 0)
    }

    const LOOPBACK_BIND: &str = "127.0.0.1";
    const TAILNET_BIND: &str = "100.71.206.33"; // any address inside the CGNAT range

    #[test]
    fn the_loopback_listener_admits_only_a_loopback_peer() {
        assert!(peer_admitted(addr(LOOPBACK_BIND), addr("127.0.0.1")).is_ok());
        assert!(peer_admitted(addr(LOOPBACK_BIND), addr("::1")).is_ok());
        assert_eq!(
            peer_admitted(addr(LOOPBACK_BIND), addr("100.71.206.1")).unwrap_err(),
            PeerRefused::NotLoopback,
            "even a Tailscale-shaped peer must not reach the loopback listener"
        );
        assert_eq!(
            peer_admitted(addr(LOOPBACK_BIND), addr("192.168.1.5")).unwrap_err(),
            PeerRefused::NotLoopback
        );
    }

    #[test]
    fn the_tailnet_listener_admits_loopback_too() {
        // This machine's own processes reaching the tailnet-bound socket
        // directly (e.g. `curl` against the tailnet IP from the box that
        // owns it) is ordinary, not an attack.
        assert!(peer_admitted(addr(TAILNET_BIND), addr("127.0.0.1")).is_ok());
        assert!(peer_admitted(addr(TAILNET_BIND), addr("::1")).is_ok());
    }

    /// The same self-connection case, arriving the other way: the OS does
    /// not always route a locally-owned destination through `lo`, so a
    /// same-machine caller can show up *as the bind address itself* rather
    /// than as loopback -- `tests/tailnet_rebind.rs`'s own SH-146 self-heal
    /// test connects exactly this way, to a real local IP it found by
    /// routing lookup rather than a genuine Tailscale identity. The bind
    /// address here is deliberately a plain LAN-shaped one, outside
    /// 100.64.0.0/10, so this only passes if the bind-address check is
    /// doing the admitting, not the CGNAT range it happens to also satisfy
    /// for [`TAILNET_BIND`]'s own value.
    #[test]
    fn the_tailnet_listener_admits_a_peer_that_is_the_bind_address_itself() {
        let non_tailnet_shaped_bind = addr("192.168.1.50");
        assert!(peer_admitted(non_tailnet_shaped_bind, non_tailnet_shaped_bind).is_ok());
    }

    #[test]
    fn the_tailnet_listener_admits_the_whole_cgnat_range_at_its_exact_edges() {
        for ip in ["100.64.0.0", "100.64.0.1", "100.127.255.255"] {
            assert!(
                peer_admitted(addr(TAILNET_BIND), addr(ip)).is_ok(),
                "{ip} is inside 100.64.0.0/10"
            );
        }
    }

    #[test]
    fn the_tailnet_listener_refuses_one_address_on_each_side_of_the_cgnat_range() {
        for ip in ["100.63.255.255", "100.128.0.0"] {
            assert_eq!(
                peer_admitted(addr(TAILNET_BIND), addr(ip)).unwrap_err(),
                PeerRefused::OutsideTailnetRanges,
                "{ip} is outside 100.64.0.0/10"
            );
        }
    }

    #[test]
    fn the_tailnet_listener_admits_the_whole_ula_range_at_its_exact_edges() {
        for ip in [
            "fd7a:115c:a1e0::",
            "fd7a:115c:a1e0::1",
            "fd7a:115c:a1e0:ffff:ffff:ffff:ffff:ffff",
        ] {
            assert!(
                peer_admitted(addr(TAILNET_BIND), addr(ip)).is_ok(),
                "{ip} is inside fd7a:115c:a1e0::/48"
            );
        }
    }

    #[test]
    fn the_tailnet_listener_refuses_the_adjacent_48_on_each_side() {
        for ip in ["fd7a:115c:a1df:ffff::", "fd7a:115c:a1e1::"] {
            assert_eq!(
                peer_admitted(addr(TAILNET_BIND), addr(ip)).unwrap_err(),
                PeerRefused::OutsideTailnetRanges,
                "{ip} is outside fd7a:115c:a1e0::/48"
            );
        }
    }

    #[test]
    fn the_tailnet_listener_refuses_a_foreign_address() {
        for ip in ["8.8.8.8", "192.168.1.5", "10.0.0.1", "2001:4860:4860::8888"] {
            assert_eq!(
                peer_admitted(addr(TAILNET_BIND), addr(ip)).unwrap_err(),
                PeerRefused::OutsideTailnetRanges,
                "{ip} is not this machine, not CGNAT, not Tailscale's ULA"
            );
        }
    }

    /// The refusal names which check failed, for the accept loop's own log
    /// line -- pinned so a future edit cannot silently collapse the two
    /// reasons into one generic refusal.
    #[test]
    fn the_two_refusal_reasons_are_distinguishable() {
        assert_ne!(
            PeerRefused::NotLoopback.to_string(),
            PeerRefused::OutsideTailnetRanges.to_string()
        );
    }

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
