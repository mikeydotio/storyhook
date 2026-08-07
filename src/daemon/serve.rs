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
//! follows bind.
//!
//! # Listeners are passed in, not opened here
//!
//! [`serve`] takes bound [`TcpListener`]s. That is a testability requirement
//! made structural: a caller can bind port 0, learn the port the kernel actually
//! gave it, publish that, and only then start serving — which deletes the "port
//! already taken" failure mode in production and the "which port did the test
//! get" guessing game in the suite.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use tiny_http::{Header, Method, Request, Server};

use crate::api::http::{
    Reply, carries_body, finish, path_segments, read_body, request_path, text_reply,
};
use crate::api::rest::{self, Changed};
use crate::api::rpc;
use crate::daemon::bus::{Change, ChangeBus};
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

/// Everything the accept loop needs, shared by every listener.
struct Serving<'a, S: Store> {
    store: &'a S,
    env: Environment,
    bus: ChangeBus,
    trusted_hosts: Vec<String>,
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
    /// Everything this daemon is serving right now (SH-173, SH-144).
    inflight: crate::daemon::lifecycle::InFlight,
}

/// Serves `listeners` until the process ends.
///
/// The first listener is served on this thread; the rest get one thread each.
/// `ready` fires once every background thread is up and the loops are about to
/// accept, so a caller can treat its return as "answering requests", not merely
/// "bound".
pub fn serve<S: Store, F>(
    store: &S,
    env: &Environment,
    listeners: Vec<Listener>,
    trusted_hosts: Vec<String>,
    bus: ChangeBus,
    token: String,
    ready: F,
) -> Result<(), AppError>
where
    F: FnOnce(),
{
    let mut servers = Vec::new();
    for listener in listeners {
        let loopback = listener.loopback;
        let server = Server::from_listener(listener.listener, None)
            .map_err(|e| AppError::Storage(format!("failed to serve a bound listener: {e}")))?;
        servers.push((server, loopback));
    }

    let Some(primary) = servers.pop() else {
        return Err(AppError::Usage(
            "the daemon needs at least one bound listener to serve".to_string(),
        ));
    };

    let stop = Arc::new(AtomicBool::new(false));
    let serving = Serving {
        store,
        env: env.clone(),
        bus: bus.clone(),
        trusted_hosts,
        token,
        hello: Hello {
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol: crate::daemon::lifecycle::PROTOCOL,
            pid: std::process::id(),
            started_at: env.now(),
        },
        dispatch_registry: Arc::new(crate::api::dispatch::DispatchRegistry::new()),
        inflight: crate::daemon::lifecycle::InFlight::new(env.clone()),
    };
    // Before `ready()`, so no listener has accepted a request a client could
    // poll a stale record from: a record surviving to here can only be one a
    // *previous* daemon left behind (SH-173).
    serving.inflight.harvest_stale();

    // One job channel and one dispatch thread for the whole daemon, no matter
    // how many listeners are bound (SH-173) — a rendezvous channel, so a
    // worker's `send` blocks until dispatch is ready for the next job, which
    // is what keeps at most one job in flight to the store at a time.
    let (jobs_tx, jobs_rx) = mpsc::sync_channel::<Job>(0);

    // Every background thread lives inside this scope, which is what lets the
    // change-token poller and the dispatcher borrow the store rather than
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
        {
            let serving = &serving;
            scope.spawn(move || dispatch(serving, jobs_rx));
        }

        ready();

        for (server, loopback) in servers {
            let serving = &serving;
            let jobs_tx = jobs_tx.clone();
            scope.spawn(move || accept_loop(serving, server, loopback, jobs_tx));
        }
        accept_loop(&serving, primary.0, primary.1, jobs_tx);
        stop.store(true, Ordering::Relaxed);
    });
    Ok(())
}

/// Binds loopback (and the tailnet interface, when there is one) and serves.
///
/// `port` may be 0, in which case the kernel picks; `ready` is told every
/// address actually bound, loopback and tailnet alike. Three things only the
/// server itself can report, and which no probe from outside can establish:
/// *which* address it got, that the address is its own — a caller that merely
/// connects to a port cannot tell this server apart from some other process
/// holding it — and whether the best-effort tailnet bind succeeded, which is
/// what a caller must not guess by probing the machine (SH-110).
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
    // No portfile and no token: this entry point serves the dashboard for a
    // caller that already has a store open, and an empty token is one no
    // request can present, so the control surface refuses every one.
    serve(
        store,
        env,
        listeners,
        trusted_hosts,
        ChangeBus::new(),
        String::new(),
        move || ready(bound),
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

    if let Some(identity) = tailnet_identity() {
        let tailnet_addr = format!("{}:{}", identity.bind_ip, bound.port());
        match TcpListener::bind(&tailnet_addr) {
            Ok(listener) => {
                eprintln!("Storyhook dashboard (tailnet): http://{tailnet_addr}");
                // We deliberately bound this interface ourselves, so trust its
                // identity for mutations too — the same standing as loopback —
                // without requiring STORYHOOK_WEB_TRUSTED_HOSTS. Trust follows
                // bind, so a name is never trusted unless the interface it
                // would arrive on is actually being served. `into_bound` is
                // where that rule is enforced: it is reachable only from here,
                // on the success arm.
                tailnet = Some(identity.into_bound());
                listeners.push(Listener {
                    listener,
                    loopback: false,
                });
            }
            Err(e) => eprintln!(
                "warning: could not bind tailnet interface {tailnet_addr}: {e}; \
                 the dashboard is only reachable via localhost"
            ),
        }
    }

    Ok((
        listeners,
        BoundAddress {
            loopback: bound,
            tailnet,
        },
    ))
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
    /// Answer it, then the daemon exits. The dispatch thread never holds
    /// `request` — [`worker`] does — so the environment needed to clear the
    /// portfile travels inside the verdict instead of being re-read.
    Shutdown { reply: Reply, env: Environment },
}

/// Runs the request-accept loop for one bound server.
///
/// This loop does exactly one thing: pop a request off `tiny_http`'s queue and
/// hand it to a fresh [`worker`] thread. It is the only part of request
/// handling that must never block, because `tiny_http`'s
/// [`Server::incoming_requests`] is itself a single shared queue — if this
/// loop stalled, every listener sharing it would too.
///
/// Everything peer-paced — reading a head, reading a body, writing a reply —
/// happens on a worker, one per request, never here and never on
/// [`dispatch`]. That split is the fix for SH-172: a peer that stalls mid-body
/// now ties up one detached thread, never the thread every other client's
/// command is queued behind.
///
/// `jobs_tx` is shared with every other bound listener's accept loop (SH-173)
/// — it is built once, in [`serve`], rather than one channel and one
/// [`dispatch`] thread per listener. Before this split, a machine with a
/// tailnet interface bound *two* listeners and therefore had two threads
/// already serving the store concurrently, by accident of interface count
/// rather than by design; one channel means one place SH-173's dispatch pool
/// has to reason about.
///
/// **What this does not bound.** `tiny_http` exposes no way to configure an
/// accepted socket — a request's body reader is an opaque `Box<dyn Read>`
/// with no accessible file descriptor, and `SO_RCVTIMEO`/`SO_SNDTIMEO` set on
/// the *listener* are not inherited by sockets `accept(2)` returns (confirmed
/// against this daemon's own listener on both macOS and — per `accept(2)`'s
/// documented inheritance list, which omits them — Linux; an earlier version
/// of this fix assumed otherwise and a test written to pin that assumption is
/// what caught it before it shipped). So a single stalled worker still blocks
/// forever in the worst case, tying up one thread and one fd rather than the
/// whole daemon. Bounding that is SH-177's problem, not this one's.
fn accept_loop<S: Store>(
    serving: &Serving<'_, S>,
    server: Server,
    loopback: bool,
    jobs_tx: mpsc::SyncSender<Job>,
) {
    let token: Arc<str> = Arc::from(serving.token.as_str());

    for request in server.incoming_requests() {
        let jobs_tx = jobs_tx.clone();
        let bus = serving.bus.clone();
        let token = Arc::clone(&token);
        let trusted_hosts = serving.trusted_hosts.clone();
        let env = serving.env.clone();
        let dispatch_registry = Arc::clone(&serving.dispatch_registry);
        thread::spawn(move || {
            worker(
                request,
                loopback,
                &token,
                bus,
                jobs_tx,
                &trusted_hosts,
                &env,
                &dispatch_registry,
            )
        });
    }
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

    let (reply_tx, reply_rx) = mpsc::channel::<Verdict>();
    let job = Job {
        method,
        path,
        headers,
        body,
        loopback,
        reply: reply_tx,
    };
    if jobs.send(job).is_err() {
        // The dispatch thread is gone — the daemon is exiting. Answer rather
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
        Ok(Verdict::Shutdown { reply, env }) => {
            finish(request, reply);
            thread::spawn(move || {
                thread::sleep(crate::daemon::lifecycle::DRAIN_DEADLINE);
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

/// The one thread that owns the store. Every [`Job`] a [`worker`] hands off is
/// routed here, serially — exactly as the whole accept loop used to run — but
/// nothing peer-paced happens on this thread any more, so a slow or stalled
/// *client* can no longer make the *dispatcher* slow for everyone else.
fn dispatch<S: Store>(serving: &Serving<'_, S>, jobs: mpsc::Receiver<Job>) {
    for job in jobs {
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
                    // Tell every connected browser to reconnect *before*
                    // answering, so a client that is about to lose its stream
                    // knows why.
                    serving.bus.publish(Change::Reload);
                    Verdict::Shutdown {
                        reply,
                        env: serving.env.clone(),
                    }
                }
            };
            // Closed before the reply is sent, matching the property SH-144
            // relies on: the record changes exactly when the daemon finishes
            // something, not a moment after its client already knows that.
            drop(entry);
            let _ = job.reply.send(verdict);
            continue;
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
        });

        let routed = rest::route(
            serving.store,
            &serving.env,
            &job.method,
            &job.path,
            &job.headers,
            &job.body,
            &serving.trusted_hosts,
        );
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
