//! The HTTP plumbing every storyhook listener shares.
//!
//! Response shaping, the security headers, the mutation guard, body reading and
//! the raw SSE framing — everything that is about *HTTP* rather than about
//! stories. It lives apart from the routes so that the two listeners the daemon
//! runs (the tailnet dashboard and the loopback RPC transport) cannot drift
//! apart on the parts where drifting is a vulnerability.
//!
//! **The guard code in this module is security-load-bearing and moved here
//! verbatim.** [`mutation_guard_ok`], [`host_is_trusted`] and
//! [`host_without_port`] are the CSRF and DNS-rebinding defence; their doc
//! comments explain what each check buys, and changing one without
//! understanding the other two re-opens an attack this dashboard already
//! closed.

use std::io::{self, Read, Write};

use crate::daemon::http1::{Header, Method, Request, Response};

use crate::error::AppError;
use crate::output::render_error;

fn security_header_nosniff() -> Header {
    Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap()
}

fn security_header_frame() -> Header {
    Header::from_bytes("X-Frame-Options", "DENY").unwrap()
}

/// The dashboard's Content-Security-Policy, shared between every normal
/// response (via [`finish`]) and the hand-rolled `GET /api/events` response
/// head, which bypasses `finish` entirely (see [`write_sse_head`]) — so the
/// two paths can never drift apart.
pub const CSP: &str = "default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'";

/// A legitimate same-origin dashboard request keeps sending `Referer`
/// (SH-319's cookie-borne-read fallback,
/// [`crate::api::admission::named_token_ok`], depends on it for `EventSource`
/// on an origin that never gets `Sec-Fetch-Site`), while a cross-origin
/// navigation away from the dashboard sends none — never a foreign origin,
/// which is what the fallback would need to leak nothing. Shared between
/// [`finish`] and [`write_sse_head`] for the same reason [`CSP`] is.
pub const REFERRER_POLICY: &str = "same-origin";

fn security_header_referrer_policy() -> Header {
    Header::from_bytes("Referrer-Policy", REFERRER_POLICY).unwrap()
}

fn security_header_csp() -> Header {
    Header::from_bytes("Content-Security-Policy", CSP).unwrap()
}

fn content_type_header(value: &str) -> Header {
    Header::from_bytes("Content-Type", value).unwrap()
}

/// What `Cache-Control` a [`Reply`] carries.
///
/// One field with three states rather than two booleans, because `no-cache`
/// and `no-store` are different instructions and "both at once" is not a
/// thing a reply should be able to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Caching {
    /// No `Cache-Control` header at all.
    Unset,
    /// `no-cache` — the browser may keep a copy, but must revalidate before
    /// reusing it. Right for dynamic content.
    NoCache,
    /// `no-store` — the browser may not write it down at all. Right for the
    /// one reply in this daemon that carries a credential
    /// ([`crate::api::handoff`]); `no-cache` would permit exactly the durable
    /// copy that reply exists to avoid.
    NoStore,
}

/// A fully-formed HTTP response, decoupled from the connection layer's request type so
/// routing decisions stay pure and easy to reason about (and test) apart from
/// the network layer. Every `Reply` — success or error — flows through
/// [`finish`], which attaches the security headers exactly once, in exactly
/// one place, so no response path can accidentally omit them.
///
/// `PartialEq` so a test can assert two refusals are *the same reply* rather
/// than that they merely share a status code — what SH-251 requires of the
/// handoff route, whose refusals must not tell a caller which part of an
/// attempt was wrong.
#[derive(Debug, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    content_type: &'static str,
    body: String,
    caching: Caching,
    retry_after: Option<u32>,
    /// A pre-formatted `Set-Cookie` header value, verbatim. This module knows
    /// nothing about what a cookie is *for* — a route builds the value
    /// (`crate::api::tokens::set_cookie_header`/`clear_cookie_header`) and
    /// this only carries it to [`finish`], the same division of labour every
    /// other `Reply` field already keeps.
    cookie: Option<String>,
}

impl Reply {
    pub fn new(status: u16, content_type: &'static str, body: impl Into<String>) -> Self {
        Reply {
            status,
            content_type,
            body: body.into(),
            caching: Caching::Unset,
            retry_after: None,
            cookie: None,
        }
    }

    /// Marks this reply as dynamic content that must never be cached by the browser.
    #[must_use]
    pub fn no_cache(mut self) -> Self {
        self.caching = Caching::NoCache;
        self
    }

    /// Marks this reply as one the browser must not write down at all.
    ///
    /// Stronger than [`Self::no_cache`], and reserved for a reply carrying a
    /// credential: `no-cache` still permits a stored copy that is revalidated
    /// before reuse, and a stored copy is precisely what a credential must not
    /// leave behind.
    #[must_use]
    pub fn no_store(mut self) -> Self {
        self.caching = Caching::NoStore;
        self
    }

    /// What this reply will send, before [`finish`] consumes it.
    ///
    /// Read-only, and here for the assertions a credential-issuing route needs:
    /// "this reply carries a capability the daemon knows" and "this reply does
    /// not contain the master token" are both claims about the body, and the
    /// alternative — picking them out of a `Debug` rendering — would be a test
    /// that passes or fails on how `String` chooses to escape itself.
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Advises the client to retry after `secs` seconds (used for 409
    /// LockTimeout, where a concurrent writer briefly held the project lock).
    #[must_use]
    pub fn retry_after(mut self, secs: u32) -> Self {
        self.retry_after = Some(secs);
        self
    }

    /// Attaches a `Set-Cookie: {value}` header, verbatim.
    #[must_use]
    pub fn with_cookie(mut self, value: String) -> Self {
        self.cookie = Some(value);
        self
    }

    /// The `Set-Cookie` value this reply carries, if any — for the same
    /// reason [`Self::body`] is exposed: a test asserting what a
    /// credential-issuing route did needs this without picking it out of a
    /// `Debug` rendering.
    pub fn cookie(&self) -> Option<&str> {
        self.cookie.as_deref()
    }
}

pub fn text_reply(status: u16, body: impl Into<String>) -> Reply {
    Reply::new(status, "text/plain; charset=utf-8", body)
}

pub fn json_reply(status: u16, body: impl Into<String>) -> Reply {
    Reply::new(status, "application/json", body)
}

pub fn html_reply(body: impl Into<String>) -> Reply {
    Reply::new(200, "text/html; charset=utf-8", body)
}

/// Attaches the shared security headers to `reply` and sends it on `request`.
pub fn finish(request: Request, reply: Reply) {
    let mut resp = Response::from_string(reply.body)
        .with_status_code(reply.status)
        .with_header(content_type_header(reply.content_type))
        .with_header(security_header_nosniff())
        .with_header(security_header_frame())
        .with_header(security_header_csp())
        .with_header(security_header_referrer_policy());
    match reply.caching {
        Caching::Unset => {}
        Caching::NoCache => {
            resp = resp.with_header(Header::from_bytes("Cache-Control", "no-cache").unwrap());
        }
        Caching::NoStore => {
            resp = resp.with_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
        }
    }
    if let Some(secs) = reply.retry_after {
        resp = resp.with_header(Header::from_bytes("Retry-After", secs.to_string()).unwrap());
    }
    if let Some(cookie) = reply.cookie {
        resp = resp.with_header(Header::from_bytes("Set-Cookie", cookie).unwrap());
    }
    let _ = request.respond(resp);
}

/// Strips any query string from a request URL, leaving just the path.
pub fn request_path(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

/// The query string portion of a request URL (after `?`, if any), raw and
/// unparsed. Sibling to [`request_path`] — together they cover the two
/// halves `request.url()` can hold. Needed at all because
/// [`crate::api::dispatch::intercept`] runs before the request body is ever
/// read (SH-208's `?auto=1`): a flag that has to be visible before the body
/// exists cannot travel in one.
pub fn request_query(url: &str) -> Option<&str> {
    url.split_once('?').map(|(_, query)| query)
}

/// Splits a request path into non-empty segments, e.g. `/api/story/SH-1` →
/// `["api", "story", "SH-1"]`. A bare `/` (or `""`) yields an empty slice.
pub fn path_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// Maps an application error to the HTTP status code that best represents it
/// for API consumers, mirroring the severity ordering in `AppError::exit_code`.
pub fn status_for(error: &AppError) -> u16 {
    match error {
        AppError::Usage(_) => 400,
        AppError::Validation(_) => 422,
        AppError::NotFound(_) => 404,
        AppError::LockTimeout(_) => 409,
        AppError::Integrity(_) | AppError::Storage(_) => 500,
        AppError::GithubAuth(_) | AppError::GithubApi(_) => 502,
        AppError::StateConflict(..) => 409,
        // 409, like the other "the server is in a state that refuses this
        // write" answers: the request was well-formed and the store is
        // healthy — this build simply may not write to it. Not 500, which
        // would say something is broken, and not 403, which would say the
        // caller lacks permission.
        AppError::ReadOnlyStore(_) => 409,
    }
}

/// Renders an `AppError` as the standard `{"result":"error",...}` JSON
/// envelope, at the status code `status_for` derives from its variant.
pub fn error_reply(error: &AppError) -> Reply {
    let reply = json_reply(status_for(error), render_error(error, true));
    match error {
        // The project write lock was held by another process; the client
        // can safely retry shortly once that writer releases it.
        AppError::LockTimeout(_) => reply.retry_after(1),
        _ => reply,
    }
}

// --- Mutation guard (CSRF / DNS-rebinding) ---

/// Looks up a header's value by name (case-insensitive), as
/// `crate::daemon::http1` itself compares header field names.
pub fn header_value<'a>(headers: &'a [Header], name: &'static str) -> Option<&'a str> {
    headers
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str())
}

/// The value of one cookie named `name` in the request's `Cookie` header, if
/// present.
///
/// A browser's `Cookie` header carries every cookie for the origin as
/// `name1=value1; name2=value2` on one line — this is the one place that
/// syntax is parsed, so every caller reads it the same way. Deliberately
/// permissive about what surrounds the pair it wants: a foreign cookie this
/// daemon did not set (a different store's, or an unrelated site sharing the
/// same loopback port history) is skipped rather than tripping a parse
/// error, because the presence of cookies this function does not recognize
/// is the ordinary case, not a malformed request.
pub fn cookie_value<'a>(headers: &'a [Header], name: &str) -> Option<&'a str> {
    let raw = header_value(headers, "Cookie")?;
    raw.split(';').find_map(|pair| {
        let (key, value) = pair.trim().split_once('=')?;
        (key == name).then_some(value)
    })
}

/// Strips an optional `:port` suffix from a `Host` header value, correctly
/// handling bracketed IPv6 literals (`[::1]:3456` and `[::1]` both -> `::1`).
fn host_without_port(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        host.rsplit_once(':').map_or(host, |(h, _)| h)
    }
}

/// A `Host` header reduced to the form the allowlists are matched against:
/// port stripped, rooted trailing `.` stripped (so a browser sending the rooted
/// form of a MagicDNS FQDN — `psamathe.tail983f02.ts.net.` — still matches the
/// unrooted form stored in the allowlist), lowercased.
fn normalized_host(host: &str) -> String {
    host_without_port(host)
        .trim_end_matches('.')
        .to_ascii_lowercase()
}

/// Whether `host` names loopback and *nothing else*.
///
/// Deliberately not [`host_is_trusted`], which also matches the allowlists —
/// including the reverse-proxy one, which is exactly the hazard this predicate
/// exists to avoid. Anything deciding "is this caller local?" must ask this;
/// anything deciding "may this `Host` mutate?" must ask [`host_is_trusted`].
/// Merging the two would silently let a configured proxy hostname answer the
/// first question, which is how SH-250's loopback exemption would have leaked.
pub fn host_is_loopback(host: &str) -> bool {
    matches!(
        normalized_host(host).as_str(),
        "127.0.0.1" | "localhost" | "::1"
    )
}

/// A `Host` is trusted if it's a loopback address, or explicitly listed in
/// `trusted` (the tailnet's MagicDNS FQDN and IPv4 — see
/// [`crate::daemon::tailnet::TailnetBind::trusted_hosts`] — or
/// `STORYHOOK_WEB_TRUSTED_HOSTS`, for a `web-serve`-style reverse proxy).
pub fn host_is_trusted(host: &str, trusted: &TrustedHosts) -> bool {
    let host_only = normalized_host(host);
    matches!(host_only.as_str(), "127.0.0.1" | "localhost" | "::1") || trusted.allows(&host_only)
}

// --- Locality: the one affirmative grant, and the type that carries it ---

/// The headers a reverse proxy adds when it forwards a request. Their mere
/// presence means this daemon cannot claim the caller is local.
///
/// None of these is authentication and none is trusted — a caller may set any
/// of them freely. That is exactly what makes them safe to consult: under
/// [`local_request`]'s governing rule they can only ever *withhold* locality,
/// so forging one refuses nobody but the forger. What they buy is the *honest*
/// proxy — nginx's own recommended configuration, caddy, traefik and
/// `tailscale serve` each set at least one, which turns every one of them from
/// silently local into "not local, exactly as before".
pub const FORWARDING_HEADERS: [&str; 5] = [
    "X-Forwarded-For",
    "X-Forwarded-Host",
    "X-Forwarded-Proto",
    "Forwarded",
    "X-Real-IP",
];

/// Proof that a request came from **this machine**.
///
/// Zero-sized, with a private field, so [`local_request`] — which is in this
/// module and nowhere else — is the only thing in the entire crate that can
/// produce one. A route that wants to act on locality must therefore *hold*
/// one, and the only way to hold one is to have asked. Deleting the check
/// becomes a compile error rather than a quietly-passing test, which is the
/// property SH-251's council made binding: *"a test is what gets deleted
/// alongside the check."*
///
/// It carries no data on purpose. Everything it could carry — the listener,
/// the `Host`, the headers — is already in scope at every call site; what the
/// caller lacks is not information but *authority to conclude*, and that is
/// exactly what a witness type confers.
pub struct LocalRequest(());

/// Whether this request came from this machine, and the proof if it did.
///
/// # The governing rule
///
/// **An attacker-supplied header may only withhold locality, never confer it.**
///
/// The single affirmative grant is `loopback`, stamped on the listener at bind
/// time — the one input here no caller can supply (and, since SH-253, derived
/// from the bound socket rather than asserted beside it). Every other conjunct
/// can only answer `None`. Keep it that way: an edit that lets a request-borne
/// value *grant* locality breaks the rule however reasonable it looks on its
/// own, because every such value is forgeable by exactly the caller this must
/// not reach.
///
/// # The conjuncts, and what each one is for
///
/// 1. **`loopback`** — it arrived on the listener bound to `127.0.0.1`.
/// 2. **No reverse-proxy allowlist is configured**
///    ([`TrustedHosts::behind_a_proxy`]). A reverse proxy connects to this
///    daemon *over loopback*, so an operator declaring one is telling this
///    daemon that "arrived on loopback" has stopped meaning "came from this
///    machine". Reading it from the `proxy` half alone is what stops a bound
///    tailnet interface — which is not an indirection — from withdrawing
///    locality on every Tailscale machine.
///
///    This also constrains test topology (SH-321): a browser fixture that sets
///    `STORYHOOK_WEB_TRUSTED_HOSTS` to exercise a non-loopback `Host` must own a
///    separate daemon environment. Sharing that daemon with handoff coverage
///    would correctly make the handoff non-local and invalidate the fixture,
///    not reveal a handoff defect.
/// 3. **No [`FORWARDING_HEADERS`]** — see that constant.
/// 4. **`Host` is a loopback literal** — [`host_is_loopback`], deliberately
///    *not* [`host_is_trusted`], which also matches the allowlists. A
///    configured proxy hostname must never be able to answer "is this caller
///    local?". This is also the conjunct that keeps DNS rebinding out: a
///    rebound page is same-origin with `http://127.0.0.1:PORT` and can read
///    what it gets back.
///
/// # Why one function
///
/// [`crate::api::handoff`]'s redemption gate (SH-251) is the one remaining
/// caller — [`crate::api::admission`]'s tokenless read exemption (SH-250)
/// derived it too, before SH-255 deleted the exemption outright. Answering
/// *"the governing principle lives in this one place, not scattered across
/// callers that each spell it inline"* is still the reason it is a function
/// rather than duplicated logic. A second caller is a change to this doc
/// comment and to `tests/handoff_endpoint.rs`'s
/// `locality_is_derived_only_where_it_was_decided_it_should_be`, which pins
/// the allowed set.
pub fn local_request(
    headers: &[Header],
    trusted: &TrustedHosts,
    loopback: bool,
) -> Option<LocalRequest> {
    let local = loopback
        && !trusted.behind_a_proxy()
        && !headers.iter().any(|header| {
            FORWARDING_HEADERS
                .iter()
                .any(|name| header.field.equiv(name))
        })
        && header_value(headers, "Host").is_some_and(host_is_loopback);
    local.then_some(LocalRequest(()))
}

/// The non-loopback `Host` values this daemon will trust for a mutation, kept
/// in **two lists by provenance** because one of them is a security signal
/// about the network and the other is not.
///
/// * `bound` — derived from an interface this daemon actually bound: the
///   tailnet IPv4 and MagicDNS FQDN ([`crate::daemon::tailnet::TailnetBind::trusted_hosts`]).
///   Trust follows bind, so a name is never in here unless the interface it
///   would arrive on is being served.
/// * `proxy` — named by `STORYHOOK_WEB_TRUSTED_HOSTS`, for a `web-serve`-style
///   reverse proxy forwarding under its own hostname.
///
/// # Why they cannot be one list (SH-250)
///
/// **A reverse proxy connects to this daemon over loopback.** So a non-empty
/// `proxy` is the daemon's only evidence that "arrived on the loopback
/// listener" may no longer mean "came from this machine" — which is what
/// [`Self::behind_a_proxy`] answers, and the fifth conjunct of
/// [`crate::api::admission`]'s token exemption.
///
/// Before SH-250 these were one flat `Vec<String>`, merged by two callers who
/// each wrote the same two lines ([`crate::daemon::serve::serve`] and
/// `daemon::lifecycle::run`). Two things made that untenable at once. A
/// merged list cannot answer `behind_a_proxy` at all — on any Tailscale
/// machine it is non-empty with zero proxies configured, so an emptiness test
/// would withhold the exemption from every such machine, and
/// [`crate::daemon::serve`]'s late tailnet rebind extends the same list at
/// runtime, so the answer would *change mid-daemon-life*. And forgetting the
/// merge used to cost only proxy trust; after SH-250 the same omission would
/// silently **grant** the exemption. Hence private fields and one constructor
/// that reads the environment, the same shape and the same reason
/// [`crate::daemon::tailnet::TailnetBind`] has: the value *is* the evidence.
#[derive(Debug, Clone, Default)]
pub struct TrustedHosts {
    bound: Vec<String>,
    proxy: Vec<String>,
}

impl TrustedHosts {
    /// The allowlist for a daemon that bound `bound_hosts`, with the
    /// reverse-proxy half read from `STORYHOOK_WEB_TRUSTED_HOSTS`.
    ///
    /// The only constructor that reads the environment, and the one both the
    /// production daemon and the test seam call — so "forgot the env half" is
    /// not a reachable state.
    pub fn for_daemon(bound_hosts: Vec<String>) -> Self {
        TrustedHosts {
            bound: bound_hosts,
            proxy: proxy_hosts_from_env(),
        }
    }

    /// Adds hosts earned by a *bind* — the late tailnet bind (SH-146), which
    /// is the only thing that ever widens an allowlist after startup.
    ///
    /// Reaches the `bound` half only. That is the invariant that makes
    /// [`Self::behind_a_proxy`] immutable for the daemon's whole life, so the
    /// token exemption cannot flip under a self-heal.
    pub fn add_bound(&mut self, hosts: impl IntoIterator<Item = String>) {
        self.bound.extend(hosts);
    }

    /// Whether a reverse proxy has been declared to this daemon.
    ///
    /// Reads the `proxy` half alone, never `bound`: a bound tailnet interface
    /// is an interface this daemon serves directly, not an indirection it was
    /// told about.
    pub fn behind_a_proxy(&self) -> bool {
        !self.proxy.is_empty()
    }

    /// Whether `host_only` (already [`normalized_host`]-ed) is on either list.
    fn allows(&self, host_only: &str) -> bool {
        self.bound.iter().any(|h| h == host_only) || self.proxy.iter().any(|h| h == host_only)
    }

    /// An allowlist with both halves stated outright, for tests that need a
    /// configured proxy without mutating the process environment — which
    /// `crates/storyhook-test-support` forbids, because the environment is
    /// shared by every test in the binary.
    #[cfg(test)]
    pub(crate) fn from_parts(bound: Vec<String>, proxy: Vec<String>) -> Self {
        TrustedHosts { bound, proxy }
    }
}

/// Localhost-only CSRF / DNS-rebinding guard for mutating requests. Both
/// checks must pass:
///
/// 1. A custom `X-Storyhook` header must be present. Setting a custom header
///    forces a CORS preflight on any cross-origin `fetch`, which this server
///    never answers with `Access-Control-Allow-*`, so the browser blocks the
///    real request — and an HTML `<form>` cannot set custom headers at all.
/// 2. `Host` must resolve to a loopback address (or an explicitly trusted
///    host). `Host` is a forbidden header a page cannot set itself, so a
///    DNS-rebinding attack — where an attacker-controlled domain starts
///    resolving to 127.0.0.1 after the browser's same-origin check passes —
///    still fails here, even though check 1 alone can't stop it (a rebound
///    page is same-origin and so *can* set custom headers).
pub fn mutation_guard_ok(headers: &[Header], trusted: &TrustedHosts) -> bool {
    if header_value(headers, "X-Storyhook").is_none() {
        return false;
    }
    match header_value(headers, "Host") {
        Some(host) => host_is_trusted(host, trusted),
        None => false,
    }
}

/// A request declares a JSON body if its `Content-Type` is `application/json`
/// (ignoring any `; charset=...` parameter).
pub fn content_type_is_json(headers: &[Header]) -> bool {
    header_value(headers, "Content-Type").is_some_and(|ct| {
        ct.split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("application/json")
    })
}

/// Runs `handler(body)` only if the request passes [`mutation_guard_ok`] and
/// declares a JSON content type; otherwise returns 403 or 415.
pub fn guarded(
    headers: &[Header],
    trusted_hosts: &TrustedHosts,
    body: &str,
    handler: impl FnOnce(&str) -> Reply,
) -> Reply {
    if !mutation_guard_ok(headers, trusted_hosts) {
        return text_reply(403, "Forbidden");
    }
    if !content_type_is_json(headers) {
        return text_reply(415, "Content-Type must be application/json");
    }
    handler(body)
}

/// Like [`guarded`], for routes that take no request body (so the
/// `Content-Type` check doesn't apply).
pub fn guarded_no_body(
    headers: &[Header],
    trusted_hosts: &TrustedHosts,
    handler: impl FnOnce() -> Reply,
) -> Reply {
    if !mutation_guard_ok(headers, trusted_hosts) {
        return text_reply(403, "Forbidden");
    }
    handler()
}

/// Parses `STORYHOOK_WEB_TRUSTED_HOSTS` into a lowercase host allowlist,
/// permitting a `web-serve`-style reverse proxy to reach the write API under
/// its own (non-loopback) hostname.
///
/// Private, and reached only through [`TrustedHosts::for_daemon`]: since
/// SH-250 the *emptiness* of this list is a security signal
/// ([`TrustedHosts::behind_a_proxy`]), so a caller that read it separately and
/// merged the result into a flat list would destroy the one fact the exemption
/// depends on.
fn proxy_hosts_from_env() -> Vec<String> {
    std::env::var("STORYHOOK_WEB_TRUSTED_HOSTS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

// --- Request body reading and parsing ---

/// Maximum request body size accepted from a mutation route. Well above any
/// legitimate story field (titles, comments, label lists), while bounding
/// how much a single request can force the server to buffer.
pub const MAX_BODY_BYTES: u64 = 64 * 1024;

/// Reads a request body up to [`MAX_BODY_BYTES`]. Reads one byte past the cap
/// so an oversized body is *detected* (and rejected with 400) rather than
/// silently truncated.
pub fn read_body(request: &mut Request) -> Option<String> {
    let mut buf = Vec::new();
    request
        .as_reader()
        .take(MAX_BODY_BYTES + 1)
        .read_to_end(&mut buf)
        .ok()?;
    if buf.len() as u64 > MAX_BODY_BYTES {
        return None;
    }
    String::from_utf8(buf).ok()
}

/// Parses `body` as a JSON object. An empty (or all-whitespace) body is
/// treated as an empty object, so field-level validation below produces a
/// clear "`x` is required" error rather than a raw JSON-parse error.
pub fn parse_json_object(
    body: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, AppError> {
    if body.trim().is_empty() {
        return Ok(serde_json::Map::new());
    }
    match serde_json::from_str(body) {
        Ok(serde_json::Value::Object(map)) => Ok(map),
        Ok(_) => Err(AppError::Usage(
            "request body must be a JSON object".to_string(),
        )),
        Err(e) => Err(AppError::Usage(format!("invalid JSON body: {e}"))),
    }
}

pub fn get_str<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<&'a str> {
    obj.get(key).and_then(|v| v.as_str())
}

pub fn require_str<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<&'a str, AppError> {
    match get_str(obj, key) {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err(AppError::Usage(format!(
            "`{key}` is required and must be a non-empty string"
        ))),
    }
}

/// Reads an optional boolean field, defaulting to `false` when absent or not
/// a boolean — the shape every optional flag in a request body takes.
pub fn get_bool(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> bool {
    obj.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

pub fn get_str_array(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Vec<String> {
    obj.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Serializes `value` as the body of a JSON reply, turning a serialization
/// failure into an application error rather than a panic.
pub fn to_json(value: &impl serde::Serialize) -> Result<String, AppError> {
    serde_json::to_string(value)
        .map_err(|e| AppError::Storage(format!("JSON serialization failed: {e}")))
}

// --- Server-Sent Events framing ---

/// Writes the fixed HTTP/1.1 response head for a `GET /api/events`
/// connection. This bypasses [`finish`] entirely — an SSE body is written
/// frame-by-frame over the connection's lifetime rather than as one `Reply`
/// — so it re-emits the same security headers (and the shared [`CSP`]) by
/// hand to keep the two paths from drifting apart. `Transfer-Encoding:
/// chunked` lets the body stay open indefinitely while remaining correctly
/// framed for HTTP/1.1.
pub fn write_sse_head(w: &mut dyn Write) -> io::Result<()> {
    write!(
        w,
        "HTTP/1.1 200 OK\r\n\
         Content-Type: text/event-stream\r\n\
         Cache-Control: no-cache\r\n\
         Connection: keep-alive\r\n\
         Transfer-Encoding: chunked\r\n\
         X-Content-Type-Options: nosniff\r\n\
         X-Frame-Options: DENY\r\n\
         Content-Security-Policy: {CSP}\r\n\
         Referrer-Policy: {REFERRER_POLICY}\r\n\
         \r\n"
    )
}

/// Writes `payload` as a single HTTP chunk and flushes immediately.
/// Flushing after every frame — rather than trusting any buffering layer to
/// do it eventually — is what makes this genuinely live: a small SSE frame
/// left sitting unflushed until a buffer happens to fill would defeat the
/// entire feature (this is exactly the trap `Response`'s
/// `chunked_transfer::Encoder` falls into, which is why this connection
/// writes directly to `Request::into_writer`'s raw socket instead).
pub fn write_sse_frame(w: &mut dyn Write, payload: &str) -> io::Result<()> {
    write!(w, "{:x}\r\n{payload}\r\n", payload.len())?;
    w.flush()
}

/// Whether a request is a body-carrying method, and therefore one whose body
/// the accept loop must read before routing.
pub fn carries_body(method: &Method) -> bool {
    matches!(method, Method::Post | Method::Patch | Method::Delete)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_carries_no_cookie_by_default() {
        assert_eq!(text_reply(200, "ok").cookie(), None);
    }

    #[test]
    fn with_cookie_attaches_the_value_verbatim() {
        let reply = text_reply(200, "ok").with_cookie("storyhook_abc=secret".to_string());
        assert_eq!(reply.cookie(), Some("storyhook_abc=secret"));
    }

    fn header(name: &'static str, value: &str) -> Header {
        Header::from_bytes(name, value).unwrap()
    }

    #[test]
    fn cookie_value_finds_one_pair_among_several() {
        let headers = vec![header(
            "Cookie",
            "unrelated=1; storyhook_abc=the-secret; other=2",
        )];
        assert_eq!(cookie_value(&headers, "storyhook_abc"), Some("the-secret"));
    }

    #[test]
    fn cookie_value_is_none_without_a_cookie_header() {
        assert_eq!(cookie_value(&[], "storyhook_abc"), None);
    }

    #[test]
    fn cookie_value_is_none_for_a_name_not_present() {
        let headers = vec![header("Cookie", "other=1")];
        assert_eq!(cookie_value(&headers, "storyhook_abc"), None);
    }

    fn tailnet_trusted_hosts() -> TrustedHosts {
        TrustedHosts::from_parts(
            vec![
                "100.71.206.33".to_string(),
                "psamathe.tail983f02.ts.net".to_string(),
            ],
            Vec::new(),
        )
    }

    #[test]
    fn host_is_trusted_accepts_magic_dns_fqdn_with_and_without_port() {
        let hosts = tailnet_trusted_hosts();
        assert!(host_is_trusted("psamathe.tail983f02.ts.net:3456", &hosts));
        assert!(host_is_trusted("psamathe.tail983f02.ts.net", &hosts));
    }

    #[test]
    fn host_is_trusted_is_case_insensitive() {
        let hosts = tailnet_trusted_hosts();
        assert!(host_is_trusted("PSAMATHE.TAIL983F02.TS.NET:3456", &hosts));
    }

    #[test]
    fn host_is_trusted_accepts_trailing_dot_rooted_form() {
        let hosts = tailnet_trusted_hosts();
        assert!(host_is_trusted("psamathe.tail983f02.ts.net.:3456", &hosts));
        assert!(host_is_trusted("psamathe.tail983f02.ts.net.", &hosts));
    }

    #[test]
    fn host_is_trusted_accepts_tailnet_ip() {
        let hosts = tailnet_trusted_hosts();
        assert!(host_is_trusted("100.71.206.33:3456", &hosts));
    }

    #[test]
    fn host_is_trusted_accepts_loopback_forms() {
        let hosts = tailnet_trusted_hosts();
        assert!(host_is_trusted("127.0.0.1:3456", &hosts));
        assert!(host_is_trusted("localhost:3456", &hosts));
        assert!(host_is_trusted("[::1]:3456", &hosts));
        assert!(host_is_trusted("[::1]", &hosts));
    }

    #[test]
    fn host_is_trusted_rejects_bare_short_label() {
        // Locks in the FQDN-only trust decision: even though the FQDN is
        // trusted, its bare first label must not be — see
        // TailnetIdentity::trusted_hosts's doc comment for why.
        let hosts = tailnet_trusted_hosts();
        assert!(!host_is_trusted("psamathe:3456", &hosts));
        assert!(!host_is_trusted("psamathe", &hosts));
    }

    #[test]
    fn host_is_trusted_rejects_foreign_hosts() {
        let hosts = tailnet_trusted_hosts();
        assert!(!host_is_trusted("evil.example", &hosts));
        assert!(!host_is_trusted("psamathe.evil.com:3456", &hosts));
        assert!(!host_is_trusted("", &hosts));
        assert!(!host_is_trusted("evil.tail983f02.ts.net", &hosts));
    }

    // --- host_is_loopback, and the provenance split (SH-250) ---

    #[test]
    fn host_is_loopback_accepts_every_spelling_of_loopback() {
        for host in [
            "127.0.0.1",
            "127.0.0.1:3456",
            "localhost",
            "LOCALHOST:3456",
            "[::1]",
            "[::1]:3456",
            "localhost.",
        ] {
            assert!(host_is_loopback(host), "{host} names loopback");
        }
    }

    #[test]
    fn host_is_loopback_rejects_everything_else_including_a_trusted_host() {
        // The last two are the point of the predicate existing: a tailnet
        // FQDN and a configured proxy name are both *trusted* for a mutation,
        // and neither means "this caller is local". `host_is_trusted` says
        // yes to them; this must say no.
        for host in [
            "evil.example",
            "evil.example:3456",
            "",
            "127.0.0.2",
            "psamathe.tail983f02.ts.net",
            "proxy.example",
        ] {
            assert!(!host_is_loopback(host), "{host} does not name loopback");
        }
    }

    #[test]
    fn behind_a_proxy_is_false_for_a_bind_only_allowlist() {
        // The regression test for the naive implementation of SH-250's
        // conjunct 5. On every Tailscale machine the allowlist is non-empty
        // with zero proxies configured, so `!is_empty()` would report a proxy
        // that does not exist and withhold the loopback exemption from the
        // one machine class most likely to want it.
        let bound_only = tailnet_trusted_hosts();
        assert!(!bound_only.behind_a_proxy());
        assert!(bound_only.allows("psamathe.tail983f02.ts.net"));
    }

    #[test]
    fn behind_a_proxy_is_true_only_for_the_env_derived_half() {
        let proxied = TrustedHosts::from_parts(Vec::new(), vec!["proxy.example".to_string()]);
        assert!(proxied.behind_a_proxy());
        assert!(proxied.allows("proxy.example"));
    }

    #[test]
    fn a_late_bound_host_cannot_change_the_proxy_verdict() {
        // `add_bound` is the late tailnet bind (SH-146), the only thing that
        // widens an allowlist after startup. It must never be able to flip
        // `behind_a_proxy` in either direction, or an authorization decision
        // would change mid-daemon-life on a self-heal.
        let mut bound_only = TrustedHosts::for_daemon(Vec::new());
        let before = bound_only.behind_a_proxy();
        bound_only.add_bound(["100.71.206.33".to_string()]);
        assert_eq!(bound_only.behind_a_proxy(), before);
        assert!(bound_only.allows("100.71.206.33"));

        let mut proxied = TrustedHosts::from_parts(Vec::new(), vec!["proxy.example".to_string()]);
        proxied.add_bound(["100.71.206.33".to_string()]);
        assert!(
            proxied.behind_a_proxy(),
            "a late bind must not retire a configured proxy"
        );
    }

    // --- The locality predicate, and its witness (SH-251) ---
    //
    // A truth table over `local_request`'s four conjuncts: one local case,
    // then one case per conjunct falsifying that conjunct alone. The same
    // shape `api::admission`'s own tests already use for the exemption that
    // now derives from this function — and the reason those tests could stay
    // byte-identical across the extraction.

    fn headers(pairs: &[(&str, &str)]) -> Vec<Header> {
        pairs
            .iter()
            .map(|(name, value)| Header::from_bytes(*name, *value).unwrap())
            .collect()
    }

    /// What a browser on this machine actually sends: a loopback `Host`, and
    /// nothing else that bears on locality.
    fn local_headers() -> Vec<Header> {
        headers(&[("Host", "127.0.0.1:3456")])
    }

    #[test]
    fn a_loopback_request_with_a_loopback_host_is_local() {
        assert!(local_request(&local_headers(), &TrustedHosts::default(), true).is_some());
    }

    #[test]
    fn conjunct_1_the_same_request_on_another_listener_is_not_local() {
        assert!(local_request(&local_headers(), &TrustedHosts::default(), false).is_none());
    }

    #[test]
    fn conjunct_2_a_configured_proxy_allowlist_withdraws_locality() {
        let behind_proxy = TrustedHosts::from_parts(Vec::new(), vec!["proxy.example".to_string()]);
        assert!(local_request(&local_headers(), &behind_proxy, true).is_none());
    }

    #[test]
    fn conjunct_2_a_bound_host_allowlist_does_not() {
        // A bound tailnet interface is an interface this daemon serves
        // directly, not an indirection it was told about — so on every
        // Tailscale machine, where the allowlist is non-empty with zero
        // proxies configured, loopback stays local.
        assert!(local_request(&local_headers(), &tailnet_trusted_hosts(), true).is_some());
    }

    #[test]
    fn conjunct_3_any_forwarding_header_withdraws_locality() {
        for name in FORWARDING_HEADERS {
            let mut forwarded = local_headers();
            forwarded.push(Header::from_bytes(name, "203.0.113.1").unwrap());
            assert!(
                local_request(&forwarded, &TrustedHosts::default(), true).is_none(),
                "{name}"
            );
        }
    }

    #[test]
    fn conjunct_4_a_rebound_host_is_not_local() {
        let rebound = headers(&[("Host", "evil.example:3456")]);
        assert!(local_request(&rebound, &TrustedHosts::default(), true).is_none());
    }

    #[test]
    fn conjunct_4_a_missing_host_is_not_local() {
        // An absent input can never grant. HTTP/1.0 clients may omit `Host`.
        assert!(local_request(&[], &TrustedHosts::default(), true).is_none());
    }

    #[test]
    fn conjunct_4_a_merely_trusted_host_is_not_thereby_local() {
        // `host_is_trusted` would happily accept this for a mutation. It
        // still must not read as "this caller is local" — the whole reason
        // `host_is_loopback` exists apart from it.
        let trusted = TrustedHosts::from_parts(vec!["proxy.example".to_string()], Vec::new());
        assert!(
            local_request(&headers(&[("Host", "proxy.example")]), &trusted, true).is_none(),
            "a merely-trusted Host is not a local one"
        );
    }

    #[test]
    fn request_query_extracts_everything_after_the_first_question_mark() {
        assert_eq!(
            request_query("/api/repos/x/story/y/dispatch?auto=1"),
            Some("auto=1")
        );
        assert_eq!(
            request_query("/api/repos/x/story/y/dispatch?a=1&b=2"),
            Some("a=1&b=2")
        );
    }

    #[test]
    fn request_query_is_none_without_a_question_mark() {
        assert_eq!(request_query("/api/repos/x/story/y/dispatch"), None);
    }

    #[test]
    fn request_query_is_some_empty_string_for_a_bare_trailing_question_mark() {
        // Distinct from None: the caller asked a question, just an empty
        // one — a client that sent `?` with nothing after it, not one that
        // never sent `?` at all.
        assert_eq!(request_query("/api/repos/x/story/y/dispatch?"), Some(""));
    }

    #[test]
    fn request_path_and_request_query_partition_the_url_with_nothing_lost() {
        let url = "/api/repos/x/story/y/dispatch?auto=1&extra=2";
        assert_eq!(request_path(url), "/api/repos/x/story/y/dispatch");
        assert_eq!(request_query(url), Some("auto=1&extra=2"));
    }
}
