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
use std::thread;
use std::time::Duration;

use tiny_http::{Method, Request, Server};

use crate::api::http::{carries_body, finish, read_body, request_path, text_reply};
use crate::api::rest::{self, Changed};
use crate::daemon::bus::{Change, ChangeBus};
use crate::daemon::tailnet::tailnet_identity;
use crate::env::Environment;
use crate::error::AppError;
use crate::store::{ReadOps, Store};

/// How often a heartbeat is published to every connected client, so a
/// connection that vanished without a clean close (laptop sleep, network drop)
/// is noticed — its next write fails — rather than lingering forever.
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
/// serve — a git hook running `story --local`, a second machine, a `sqlite3`
/// prompt. One pragma per tick, so the interval can be short without mattering.
const CHANGE_TOKEN_POLL: Duration = Duration::from_millis(250);

/// How often a background thread looks up to see whether it should stop.
const SHUTDOWN_CHECK: Duration = Duration::from_millis(250);

/// Everything the accept loop needs, shared by every listener.
struct Serving<'a, S: Store> {
    store: &'a S,
    env: Environment,
    bus: ChangeBus,
    trusted_hosts: Vec<String>,
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
    listeners: Vec<TcpListener>,
    trusted_hosts: Vec<String>,
    bus: ChangeBus,
    ready: F,
) -> Result<(), AppError>
where
    F: FnOnce(),
{
    let mut servers = Vec::new();
    for listener in listeners {
        servers
            .push(Server::from_listener(listener, None).map_err(|e| {
                AppError::Storage(format!("failed to serve a bound listener: {e}"))
            })?);
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
    };

    // Every background thread lives inside this scope, which is what lets the
    // change-token poller borrow the store rather than being handed a raw
    // pointer to it. The scope joins them on the way out, so the accept loop
    // signals `stop` before it returns and the joins take one poll interval
    // rather than forever.
    thread::scope(|scope| {
        {
            let (bus, stop) = (bus.clone(), Arc::clone(&stop));
            scope.spawn(move || heartbeat(&bus, &stop));
        }
        {
            let (bus, stop) = (bus.clone(), Arc::clone(&stop));
            scope.spawn(move || poll_change_token(store, &bus, &stop));
        }

        ready();

        for server in servers {
            let serving = &serving;
            scope.spawn(move || accept_loop(serving, server));
        }
        accept_loop(&serving, primary);
        stop.store(true, Ordering::Relaxed);
    });
    Ok(())
}

/// Binds loopback (and the tailnet interface, when there is one) and serves.
///
/// `port` may be 0, in which case the kernel picks; `ready` is told the address
/// actually bound. Two things only the server itself can report, and which no
/// probe from outside can establish: *which* address it got, and that the
/// address is its own — a caller that merely connects to a port cannot tell this
/// server apart from some other process holding it.
pub fn bind_and_serve<S: Store, F>(
    store: &S,
    env: &Environment,
    port: u16,
    ready: F,
) -> Result<(), AppError>
where
    F: FnOnce(std::net::SocketAddr),
{
    let (listeners, bound, mut trusted_hosts) = bind_listeners(port)?;
    eprintln!("Storyhook dashboard: http://127.0.0.1:{}", bound.port());
    trusted_hosts.extend(crate::api::http::trusted_hosts_from_env());
    serve(
        store,
        env,
        listeners,
        trusted_hosts,
        ChangeBus::new(),
        move || ready(bound),
    )
}

/// Binds loopback on `port`, then the tailnet interface on whatever port
/// loopback actually got, and reports the hosts the tailnet bind earns trust
/// for.
///
/// A tailnet bind failure is a warning, never fatal: no tailnet is a degraded
/// dashboard, and a dashboard that refuses to start is a broken one.
pub fn bind_listeners(
    port: u16,
) -> Result<(Vec<TcpListener>, std::net::SocketAddr, Vec<String>), AppError> {
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

    let mut listeners = vec![loopback];
    let mut trusted_hosts = Vec::new();

    if let Some(identity) = tailnet_identity() {
        let tailnet_addr = format!("{}:{}", identity.bind_ip, bound.port());
        match TcpListener::bind(&tailnet_addr) {
            Ok(listener) => {
                eprintln!("Storyhook dashboard (tailnet): http://{tailnet_addr}");
                // We deliberately bound this interface ourselves, so trust its
                // identity for mutations too — the same standing as loopback —
                // without requiring STORYHOOK_WEB_TRUSTED_HOSTS. Trust follows
                // bind, so a name is never trusted unless the interface it
                // would arrive on is actually being served.
                trusted_hosts.extend(identity.trusted_hosts());
                listeners.push(listener);
            }
            Err(e) => eprintln!(
                "warning: could not bind tailnet interface {tailnet_addr}: {e}; \
                 the dashboard is only reachable via localhost"
            ),
        }
    }

    Ok((listeners, bound, trusted_hosts))
}

/// Runs the request-accept loop for one bound server.
///
/// `GET /api/events` is intercepted here, before body-reading or routing, and
/// handed off to its own thread rather than answered inline: it is a long-lived
/// streaming connection, and this loop must move on to the next request
/// immediately rather than block on it for as long as the browser tab stays
/// open. `tiny_http` already reads connections into an internal queue on its own
/// thread pool, so detaching this one request costs nothing to the ones behind
/// it.
fn accept_loop<S: Store>(serving: &Serving<'_, S>, server: Server) {
    for mut request in server.incoming_requests() {
        let method = request.method().clone();
        let path = request_path(request.url()).to_string();

        if method == Method::Get && path == "/api/events" {
            let bus = serving.bus.clone();
            thread::spawn(move || serve_sse(request, bus));
            continue;
        }

        let headers = request.headers().to_vec();
        let body = if carries_body(&method) {
            match read_body(&mut request) {
                Some(b) => b,
                None => {
                    finish(
                        request,
                        text_reply(400, "request body invalid or too large"),
                    );
                    continue;
                }
            }
        } else {
            String::new()
        };

        let routed = rest::route(
            serving.store,
            &serving.env,
            &method,
            &path,
            &headers,
            &body,
            &serving.trusted_hosts,
        );
        // Published here, at the request boundary: the write has committed and
        // its transaction is over, so a subscriber woken by this can read what
        // just happened rather than what was there before it.
        match &routed.changed {
            Some(Changed::Project(slug)) => serving.bus.publish(Change::Project(slug.clone())),
            Some(Changed::Catalog) => serving.bus.publish(Change::Catalog),
            None => {}
        }
        finish(request, routed.reply);
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
/// [`Change::Project`], so a `story --local` write reaches a browser as the same
/// event a dashboard write does.
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
