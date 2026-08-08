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

fn security_header_csp() -> Header {
    Header::from_bytes("Content-Security-Policy", CSP).unwrap()
}

fn content_type_header(value: &str) -> Header {
    Header::from_bytes("Content-Type", value).unwrap()
}

/// A fully-formed HTTP response, decoupled from the connection layer's request type so
/// routing decisions stay pure and easy to reason about (and test) apart from
/// the network layer. Every `Reply` — success or error — flows through
/// [`finish`], which attaches the security headers exactly once, in exactly
/// one place, so no response path can accidentally omit them.
pub struct Reply {
    pub status: u16,
    content_type: &'static str,
    body: String,
    no_cache: bool,
    retry_after: Option<u32>,
}

impl Reply {
    pub fn new(status: u16, content_type: &'static str, body: impl Into<String>) -> Self {
        Reply {
            status,
            content_type,
            body: body.into(),
            no_cache: false,
            retry_after: None,
        }
    }

    /// Marks this reply as dynamic content that must never be cached by the browser.
    #[must_use]
    pub fn no_cache(mut self) -> Self {
        self.no_cache = true;
        self
    }

    /// Advises the client to retry after `secs` seconds (used for 409
    /// LockTimeout, where a concurrent writer briefly held the project lock).
    #[must_use]
    pub fn retry_after(mut self, secs: u32) -> Self {
        self.retry_after = Some(secs);
        self
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
        .with_header(security_header_csp());
    if reply.no_cache {
        resp = resp.with_header(Header::from_bytes("Cache-Control", "no-cache").unwrap());
    }
    if let Some(secs) = reply.retry_after {
        resp = resp.with_header(Header::from_bytes("Retry-After", secs.to_string()).unwrap());
    }
    let _ = request.respond(resp);
}

/// Strips any query string from a request URL, leaving just the path.
pub fn request_path(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
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
        AppError::SyncConflict(_) => 409,
        AppError::StateConflict(..) => 409,
        // Built from one or more failed GitHub calls, same as GithubApi —
        // the aggregate form of the same upstream-failure shape.
        AppError::SyncErrors(_) => 502,
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

/// Strips an optional `:port` suffix from a `Host` header value, correctly
/// handling bracketed IPv6 literals (`[::1]:3456` and `[::1]` both -> `::1`).
fn host_without_port(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        host.rsplit_once(':').map_or(host, |(h, _)| h)
    }
}

/// A `Host` is trusted if it's a loopback address, or explicitly listed in
/// `trusted_hosts` (the tailnet's MagicDNS FQDN and IPv4 — see
/// [`crate::daemon::tailnet::TailnetIdentity::trusted_hosts`] — or
/// `STORYHOOK_WEB_TRUSTED_HOSTS`, for a `web-serve`-style reverse proxy). A
/// trailing `.` is stripped before matching so a browser sending the rooted form
/// of a MagicDNS FQDN (e.g. `psamathe.tail983f02.ts.net.`) still matches the
/// unrooted form stored in `trusted_hosts`.
pub fn host_is_trusted(host: &str, trusted_hosts: &[String]) -> bool {
    let host_only = host_without_port(host)
        .trim_end_matches('.')
        .to_ascii_lowercase();
    matches!(host_only.as_str(), "127.0.0.1" | "localhost" | "::1")
        || trusted_hosts.contains(&host_only)
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
pub fn mutation_guard_ok(headers: &[Header], trusted_hosts: &[String]) -> bool {
    if header_value(headers, "X-Storyhook").is_none() {
        return false;
    }
    match header_value(headers, "Host") {
        Some(host) => host_is_trusted(host, trusted_hosts),
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
    trusted_hosts: &[String],
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
    trusted_hosts: &[String],
    handler: impl FnOnce() -> Reply,
) -> Reply {
    if !mutation_guard_ok(headers, trusted_hosts) {
        return text_reply(403, "Forbidden");
    }
    handler()
}

/// Parses `STORYHOOK_WEB_TRUSTED_HOSTS` into a lowercase host allowlist for
/// the mutation guard, used to permit a `web-serve`-style reverse proxy to
/// reach the write API under its own (non-loopback) hostname. Read-only
/// requests are never subject to this check.
pub fn trusted_hosts_from_env() -> Vec<String> {
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

    fn tailnet_trusted_hosts() -> Vec<String> {
        vec![
            "100.71.206.33".to_string(),
            "psamathe.tail983f02.ts.net".to_string(),
        ]
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
}
