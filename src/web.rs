use std::collections::HashMap;
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use fs4::FileExt;
use notify::Watcher;
use tiny_http::{Header, Method, Request, Response, Server};

use crate::app::{self, build_report_data};
use crate::cli::{CliOptions, Invocation, StateAction};
use crate::domain::Priority;
use crate::error::AppError;
use crate::output::{render_error, render_response};
use crate::registry;
use crate::storage;

const DASHBOARD_HTML: &str = include_str!("web_dashboard.html");

/// All priority levels, in the order the frontend should offer them.
const PRIORITIES: [Priority; 5] = [
    Priority::Critical,
    Priority::High,
    Priority::Medium,
    Priority::Low,
    Priority::None,
];

/// All relationship kinds a story can be linked with, in canonical form (not
/// including the `related-to` alias of `relates-to`). See
/// `domain::relation_edges` for the authoritative parser.
const RELATIONS: [&str; 8] = [
    "relates-to",
    "blocks",
    "blocked-by",
    "parent-of",
    "child-of",
    "duplicate-of",
    "obviates",
    "obviated-by",
];

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
const CSP: &str = "default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'";

fn security_header_csp() -> Header {
    Header::from_bytes("Content-Security-Policy", CSP).unwrap()
}

fn content_type_header(value: &str) -> Header {
    Header::from_bytes("Content-Type", value).unwrap()
}

/// A fully-formed HTTP response, decoupled from `tiny_http`'s request type so
/// routing decisions stay pure and easy to reason about (and test) apart from
/// the network layer. Every `Reply` — success or error — flows through
/// [`finish`], which attaches the security headers exactly once, in exactly
/// one place, so no response path can accidentally omit them.
struct Reply {
    status: u16,
    content_type: &'static str,
    body: String,
    no_cache: bool,
    retry_after: Option<u32>,
}

impl Reply {
    fn new(status: u16, content_type: &'static str, body: impl Into<String>) -> Self {
        Reply {
            status,
            content_type,
            body: body.into(),
            no_cache: false,
            retry_after: None,
        }
    }

    /// Marks this reply as dynamic content that must never be cached by the browser.
    fn no_cache(mut self) -> Self {
        self.no_cache = true;
        self
    }

    /// Advises the client to retry after `secs` seconds (used for 409
    /// LockTimeout, where a concurrent writer briefly held the project lock).
    fn retry_after(mut self, secs: u32) -> Self {
        self.retry_after = Some(secs);
        self
    }
}

fn text_reply(status: u16, body: impl Into<String>) -> Reply {
    Reply::new(status, "text/plain; charset=utf-8", body)
}

fn json_reply(status: u16, body: impl Into<String>) -> Reply {
    Reply::new(status, "application/json", body)
}

fn html_reply(body: impl Into<String>) -> Reply {
    Reply::new(200, "text/html; charset=utf-8", body)
}

/// Attaches the shared security headers to `reply` and sends it on `request`.
fn finish(request: Request, reply: Reply) {
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
fn request_path(url: &str) -> &str {
    url.split('?').next().unwrap_or(url)
}

/// Splits a request path into non-empty segments, e.g. `/api/story/SH-1` →
/// `["api", "story", "SH-1"]`. A bare `/` (or `""`) yields an empty slice.
fn path_segments(path: &str) -> Vec<&str> {
    path.split('/').filter(|s| !s.is_empty()).collect()
}

/// Maps an application error to the HTTP status code that best represents it
/// for API consumers, mirroring the severity ordering in `AppError::exit_code`.
fn status_for(error: &AppError) -> u16 {
    match error {
        AppError::Usage(_) => 400,
        AppError::Validation(_) => 422,
        AppError::NotFound(_) => 404,
        AppError::LockTimeout(_) => 409,
        AppError::Integrity(_) | AppError::Storage(_) => 500,
        AppError::GithubAuth(_) | AppError::GithubApi(_) => 502,
        AppError::SyncConflict(_) => 409,
        AppError::StateConflict(..) => 409,
    }
}

/// Renders an `AppError` as the standard `{"result":"error",...}` JSON
/// envelope, at the status code `status_for` derives from its variant.
fn error_reply(error: &AppError) -> Reply {
    let reply = json_reply(status_for(error), render_error(error, true));
    match error {
        // The project write lock was held by another process; the client
        // can safely retry shortly once that writer releases it.
        AppError::LockTimeout(_) => reply.retry_after(1),
        _ => reply,
    }
}

/// Dispatches `invocation` through `app::run` — the same entrypoint the CLI
/// uses — and renders the result as a `Reply` at `status` on success, or the
/// standard error envelope on failure. Centralizing this means every
/// mutation route gets locking, validation, event-hook firing, and
/// CLOSED-state archiving for free, identically to its CLI counterpart.
fn run_and_reply(root: &Path, status: u16, invocation: Invocation) -> Reply {
    let options = CliOptions {
        json: true,
        quiet: false,
        no_hooks: false,
        invocation,
    };
    match app::run(root, options) {
        Ok(response) => json_reply(status, render_response(&response, true, false)),
        Err(e) => error_reply(&e),
    }
}

/// Like [`run_and_reply`], but for invocations (namely `SetFields`, which
/// backs `PATCH`) that report success as a plain text `Response::Message`
/// rather than the updated story. Every mutation route's success body should
/// be the story so the frontend can reconcile optimistic UI state uniformly,
/// so on success this re-fetches `id` via a second, independent `app::run`
/// call (its own lock acquired and released in turn — never nested inside
/// the first) and returns that instead.
fn run_mutation_and_reply_with_story(root: &Path, id: &str, invocation: Invocation) -> Reply {
    let options = CliOptions {
        json: true,
        quiet: false,
        no_hooks: false,
        invocation,
    };
    match app::run(root, options) {
        Ok(_) => run_and_reply(root, 200, Invocation::Show { id: id.to_string() }),
        Err(e) => error_reply(&e),
    }
}

/// Decides how to respond to a request against the *registry* at
/// `registry_path` — the file listing every repo the dashboard knows about.
/// `/` always serves the single-page app; `/api/repos` (list/register) and
/// `/api/repos/<id>` (deregister) operate on the registry itself; every
/// other `/api/repos/<id>/...` path resolves `<id>` to that repo's root and
/// delegates to [`route_repo`], the entire per-repo API surface.
fn route(
    registry_path: &Path,
    method: &Method,
    path: &str,
    headers: &[Header],
    body: &str,
    trusted_hosts: &[String],
) -> Reply {
    match path_segments(path).as_slice() {
        [] => match method {
            Method::Get => html_reply(DASHBOARD_HTML).no_cache(),
            _ => text_reply(405, "Method not allowed"),
        },
        ["api", "repos"] => match method {
            Method::Get => match build_repos_json(registry_path) {
                Ok(json) => json_reply(200, json).no_cache(),
                Err(e) => error_reply(&e),
            },
            Method::Post => guarded(headers, trusted_hosts, body, |b| {
                route_register_repo(registry_path, b)
            }),
            _ => text_reply(405, "Method not allowed"),
        },
        ["api", "repos", id] => match method {
            Method::Delete => guarded_no_body(headers, trusted_hosts, || {
                route_deregister_repo(registry_path, id)
            }),
            _ => text_reply(405, "Method not allowed"),
        },
        ["api", "repos", id, rest @ ..] => match resolve_repo_root(registry_path, id) {
            Ok(Some(root)) => route_repo(&root, method, rest, headers, body, trusted_hosts),
            Ok(None) => text_reply(404, "Not found"),
            Err(e) => error_reply(&e),
        },
        _ => text_reply(404, "Not found"),
    }
}

/// The per-repo API surface — everything under `/api/repos/<id>/...` once
/// `<id>` has resolved to `root`. Identical to the routes the single-repo
/// server used to expose at the top level; `rest` is the path *after*
/// `/api/repos/<id>`, e.g. `["data"]` or `["story", "SH-1", "move"]`.
fn route_repo(
    root: &Path,
    method: &Method,
    rest: &[&str],
    headers: &[Header],
    body: &str,
    trusted_hosts: &[String],
) -> Reply {
    match rest {
        ["data"] => match method {
            Method::Get => match build_api_json(root) {
                Ok(json) => json_reply(200, json).no_cache(),
                Err(e) => error_reply(&e),
            },
            _ => text_reply(405, "Method not allowed"),
        },
        ["story"] => match method {
            Method::Post => guarded(headers, trusted_hosts, body, |b| {
                route_create_story(root, b)
            }),
            _ => text_reply(405, "Method not allowed"),
        },
        ["story", id] => match method {
            Method::Get => run_and_reply(
                root,
                200,
                Invocation::Show {
                    id: (*id).to_string(),
                },
            ),
            Method::Patch => guarded(headers, trusted_hosts, body, |b| {
                route_patch_story(root, id, b)
            }),
            Method::Delete => guarded(headers, trusted_hosts, body, |b| {
                route_delete_story(root, id, b)
            }),
            _ => text_reply(405, "Method not allowed"),
        },
        ["story", id, action] => match (method, *action) {
            (Method::Post, "move") => guarded(headers, trusted_hosts, body, |b| {
                route_move_story(root, id, b)
            }),
            (Method::Post, "comment") => guarded(headers, trusted_hosts, body, |b| {
                route_comment_story(root, id, b)
            }),
            (Method::Post, "priority") => guarded(headers, trusted_hosts, body, |b| {
                route_priority_story(root, id, b)
            }),
            (Method::Post, "assign") => guarded(headers, trusted_hosts, body, |b| {
                route_assign_story(root, id, b)
            }),
            (Method::Post, "labels") => guarded(headers, trusted_hosts, body, |b| {
                route_labels_story(root, id, b)
            }),
            (Method::Post, "block") => guarded(headers, trusted_hosts, body, |b| {
                route_block_story(root, id, b)
            }),
            (Method::Post, "unblock") => {
                guarded_no_body(headers, trusted_hosts, || route_unblock_story(root, id))
            }
            (Method::Post, "reopen") => guarded(headers, trusted_hosts, body, |b| {
                route_reopen_story(root, id, b)
            }),
            (Method::Post, _) => text_reply(404, "Not found"),
            _ => text_reply(405, "Method not allowed"),
        },
        // Reordering is a PATCH of the *collection*, not a
        // `/states/reorder` sub-path: `reorder` is a legal state slug, and
        // that route would shadow the state with that name.
        ["states"] => match method {
            Method::Get => match build_states_json(root, None) {
                Ok(json) => json_reply(200, json).no_cache(),
                Err(e) => error_reply(&e),
            },
            Method::Post => guarded(headers, trusted_hosts, body, |b| {
                route_create_state(root, b)
            }),
            Method::Patch => guarded(headers, trusted_hosts, body, |b| {
                route_reorder_states(root, b)
            }),
            _ => text_reply(405, "Method not allowed"),
        },
        ["states", slug] => match method {
            Method::Patch => guarded(headers, trusted_hosts, body, |b| {
                route_patch_state(root, slug, b)
            }),
            Method::Delete => guarded(headers, trusted_hosts, body, |b| {
                route_delete_state(root, slug, b)
            }),
            _ => text_reply(405, "Method not allowed"),
        },
        ["relate"] => match method {
            Method::Post => guarded(headers, trusted_hosts, body, |b| {
                route_relate(root, b, false)
            }),
            _ => text_reply(405, "Method not allowed"),
        },
        ["unrelate"] => match method {
            Method::Post => guarded(headers, trusted_hosts, body, |b| {
                route_relate(root, b, true)
            }),
            _ => text_reply(405, "Method not allowed"),
        },
        _ => text_reply(404, "Not found"),
    }
}

/// Resolves `id` to its registered repo's root path, or `None` if no repo
/// with that id is registered. A registry-load failure (e.g. corrupt TOML)
/// is a distinct, real error from an unknown id, so callers can tell a 404
/// (bad id) from a 500 (broken registry).
fn resolve_repo_root(registry_path: &Path, id: &str) -> Result<Option<PathBuf>, AppError> {
    let registry = registry::Registry::load_from(registry_path)?;
    Ok(registry.find(id).map(|r| r.path.clone()))
}

// --- Registry route handlers: GET/POST /api/repos, DELETE /api/repos/<id> ---

/// `GET /api/repos` — one entry per registered repo, driving the
/// repo-selector dropdown, the home screen's summary cards, and the
/// settings screen's repo list. A repo whose data can't currently be loaded
/// (moved, deleted, or otherwise broken) is reported as `available: false`
/// with an `error` message rather than failing the whole request — one bad
/// repo must never take down the view of every other one.
fn build_repos_json(registry_path: &Path) -> Result<String, AppError> {
    let registry = registry::Registry::load_from(registry_path)?;
    let repos: Vec<serde_json::Value> = registry
        .repos
        .iter()
        .map(|repo| match build_report_data(&repo.path) {
            Ok(data) => serde_json::json!({
                "id": repo.id,
                "name": repo.name,
                "path": repo.path,
                "available": true,
                "summary": data.summary,
            }),
            Err(e) => serde_json::json!({
                "id": repo.id,
                "name": repo.name,
                "path": repo.path,
                "available": false,
                "error": e.to_string(),
            }),
        })
        .collect();

    serde_json::to_string(&repos)
        .map_err(|e| AppError::Storage(format!("JSON serialization failed: {e}")))
}

/// `POST /api/repos` — register a repo. Body: `{"path": "...", "name"?: "..."}`.
fn route_register_repo(registry_path: &Path, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let path = require_str(&obj, "path")?;
        let name = get_str(&obj, "name");
        let repo = registry::with_lock_at(registry_path, |r| r.register(Path::new(path), name))?;
        let json = serde_json::to_string(&repo)
            .map_err(|e| AppError::Storage(format!("JSON serialization failed: {e}")))?;
        Ok(json_reply(201, json))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// `DELETE /api/repos/{id}` — deregister a repo. Never touches the target
/// repo's own files, only the registry entry (see `Registry::deregister`).
fn route_deregister_repo(registry_path: &Path, id: &str) -> Reply {
    match registry::with_lock_at(registry_path, |r| r.deregister(id)) {
        Ok(repo) => {
            let json = serde_json::json!({"result": "ok", "repo": repo}).to_string();
            json_reply(200, json)
        }
        Err(e) => error_reply(&e),
    }
}

// --- Mutation guard (CSRF / DNS-rebinding) ---

/// Looks up a header's value by name (case-insensitive), as `tiny_http`
/// itself compares header field names.
fn header_value<'a>(headers: &'a [Header], name: &'static str) -> Option<&'a str> {
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
/// [`TailnetIdentity::trusted_hosts`] — or `STORYHOOK_WEB_TRUSTED_HOSTS`, for
/// a `web-serve`-style reverse proxy). A trailing `.` is stripped before
/// matching so a browser sending the rooted form of a MagicDNS FQDN (e.g.
/// `psamathe.tail983f02.ts.net.`) still matches the unrooted form stored in
/// `trusted_hosts`.
fn host_is_trusted(host: &str, trusted_hosts: &[String]) -> bool {
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
fn mutation_guard_ok(headers: &[Header], trusted_hosts: &[String]) -> bool {
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
fn content_type_is_json(headers: &[Header]) -> bool {
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
fn guarded(
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
fn guarded_no_body(
    headers: &[Header],
    trusted_hosts: &[String],
    handler: impl FnOnce() -> Reply,
) -> Reply {
    if !mutation_guard_ok(headers, trusted_hosts) {
        return text_reply(403, "Forbidden");
    }
    handler()
}

// --- Request body parsing helpers ---

/// Parses `body` as a JSON object. An empty (or all-whitespace) body is
/// treated as an empty object, so field-level validation below produces a
/// clear "`x` is required" error rather than a raw JSON-parse error.
fn parse_json_object(body: &str) -> Result<serde_json::Map<String, serde_json::Value>, AppError> {
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

fn get_str<'a>(obj: &'a serde_json::Map<String, serde_json::Value>, key: &str) -> Option<&'a str> {
    obj.get(key).and_then(|v| v.as_str())
}

fn require_str<'a>(
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
fn get_bool(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> bool {
    obj.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn get_str_array(obj: &serde_json::Map<String, serde_json::Value>, key: &str) -> Vec<String> {
    obj.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

// --- Mutation route handlers ---
//
// Each parses its JSON body, builds the matching `Invocation`, and dispatches
// through `run_and_reply` — the same validation, locking, and event-hook path
// the CLI uses. Body-parsing failures short-circuit to `error_reply` via the
// `?`-in-a-closure pattern below.

/// `POST /api/story` — create a new story.
fn route_create_story(root: &Path, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let title = require_str(&obj, "title")?.to_string();
        let state = get_str(&obj, "state").map(str::to_string);
        let story_type = get_str(&obj, "type").map(str::to_string);
        let description = get_str(&obj, "description").map(str::to_string);
        let priority = get_str(&obj, "priority").map(str::to_string);
        let labels = if obj.contains_key("labels") {
            Some(get_str_array(&obj, "labels"))
        } else {
            None
        };
        Ok(run_and_reply(
            root,
            201,
            Invocation::New {
                title,
                state,
                story_type,
                description,
                priority,
                labels,
                assignee: None,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// `PATCH /api/story/{id}` — update title/state/priority/assignee/type.
/// Label changes go through the dedicated `/labels` route instead: `SetFields`
/// only ever *adds* labels via this field, which would be a confusing PATCH
/// semantic (callers would reasonably expect a full replace).
fn route_patch_story(root: &Path, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let invocation = Invocation::SetFields {
            id: id.to_string(),
            title: get_str(&obj, "title").map(str::to_string),
            state: get_str(&obj, "state").map(str::to_string),
            priority: get_str(&obj, "priority").map(str::to_string),
            assignee: get_str(&obj, "assignee").map(str::to_string),
            labels: None,
            blocked: None,
            unblocked: false,
            json: None,
            story_type: get_str(&obj, "type").map(str::to_string),
            description: get_str(&obj, "description").map(str::to_string),
        };
        Ok(run_mutation_and_reply_with_story(root, id, invocation))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// `POST /api/story/{id}/move` — the board's drag-and-drop endpoint. Moving
/// into a CLOSED state archives the story (handled inside `SetState`).
fn route_move_story(root: &Path, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let state = require_str(&obj, "state")?.to_string();
        let comment = get_str(&obj, "comment").map(str::to_string);
        Ok(run_and_reply(
            root,
            200,
            Invocation::SetState {
                id: id.to_string(),
                state,
                comment,
                if_state: None,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

fn route_comment_story(root: &Path, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let text = require_str(&obj, "text")?.to_string();
        Ok(run_and_reply(
            root,
            200,
            Invocation::Comment {
                id: id.to_string(),
                text,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

fn route_priority_story(root: &Path, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let priority = require_str(&obj, "priority")?.to_string();
        Ok(run_and_reply(
            root,
            200,
            Invocation::SetPriority {
                id: id.to_string(),
                priority,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

fn route_assign_story(root: &Path, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let member = require_str(&obj, "member")?.to_string();
        Ok(run_and_reply(
            root,
            200,
            Invocation::Assign {
                id: id.to_string(),
                member,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

fn route_labels_story(root: &Path, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let add = get_str_array(&obj, "add");
        let remove = get_str_array(&obj, "remove");
        if add.is_empty() && remove.is_empty() {
            return Err(AppError::Usage(
                "request body must include a non-empty `add` or `remove` array".to_string(),
            ));
        }
        Ok(run_and_reply(
            root,
            200,
            Invocation::SetLabels {
                id: id.to_string(),
                add,
                remove,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

fn route_block_story(root: &Path, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let awaiting = require_str(&obj, "reason")?.to_string();
        Ok(run_and_reply(
            root,
            200,
            Invocation::SetAwaiting {
                id: id.to_string(),
                awaiting,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

fn route_unblock_story(root: &Path, id: &str) -> Reply {
    run_and_reply(root, 200, Invocation::ClearAwaiting { id: id.to_string() })
}

/// `POST /api/story/{id}/reopen` — reopens a closed story. An optional
/// `"force": true` in the JSON body undeletes a soft-deleted story, mirroring
/// the CLI's `story reopen <id> --force`; absent or `false` performs the
/// guarded (non-force) reopen. An empty body is treated as `{}`.
fn route_reopen_story(root: &Path, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let force = get_bool(&obj, "force");
        Ok(run_and_reply(
            root,
            200,
            Invocation::Reopen {
                id: id.to_string(),
                force,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// `DELETE /api/story/{id}` — a required, non-empty `reason` is enforced the
/// same way the CLI's `story delete` requires one.
fn route_delete_story(root: &Path, id: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let reason = require_str(&obj, "reason")?.to_string();
        Ok(run_and_reply(
            root,
            200,
            Invocation::Delete {
                id: id.to_string(),
                reason,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

// --- Per-repo state configuration: /api/repos/<id>/states ---

/// The states payload every state route replies with: the configured states
/// in `states.toml` order — which is the board's column order — each with
/// the story counts the editor needs to decide whether a change is
/// destructive.
///
/// `open_count` stories can be migrated elsewhere; `archived_count` ones
/// cannot, so the two stay separate rather than summed.
fn build_states_json(root: &Path, message: Option<&str>) -> Result<String, AppError> {
    let usage = storage::state_usage(root)?;
    let states: Vec<serde_json::Value> = storage::load_states(root)?
        .into_iter()
        .map(|state| {
            let counts = usage.get(&state.slug).copied().unwrap_or_default();
            serde_json::json!({
                "slug": state.slug,
                "super_state": state.super_state.as_str(),
                "role": state.role,
                "description": state.description,
                "open_count": counts.open,
                "archived_count": counts.archived,
            })
        })
        .collect();

    let payload = serde_json::json!({
        "result": "ok",
        "message": message,
        "states": states,
    });
    serde_json::to_string(&payload)
        .map_err(|e| AppError::Storage(format!("JSON serialization failed: {e}")))
}

/// Dispatches a state mutation through `app::run` — same locking, migration,
/// and validation as the CLI — then replies with the refreshed list rather
/// than the bare success message, so the editor always redraws from server
/// truth instead of guessing what its own edit did (an edit can move
/// stories, which changes counts on *two* states at once).
fn run_state_mutation_and_reply(root: &Path, status: u16, invocation: Invocation) -> Reply {
    let options = CliOptions {
        json: true,
        quiet: false,
        no_hooks: false,
        invocation,
    };
    match app::run(root, options) {
        Ok(response) => {
            // `Response` here is tiny_http's; the app's lives in `output`.
            let message = match response {
                crate::output::Response::Message(text) => text,
                _ => String::new(),
            };
            match build_states_json(root, Some(&message)) {
                Ok(json) => json_reply(status, json).no_cache(),
                Err(e) => error_reply(&e),
            }
        }
        Err(e) => error_reply(&e),
    }
}

/// `POST /api/repos/{id}/states` — add a state.
fn route_create_state(root: &Path, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let slug = require_str(&obj, "slug")?.to_string();
        let superstate = require_str(&obj, "super_state")?.to_string();
        Ok(run_state_mutation_and_reply(
            root,
            201,
            Invocation::State {
                action: StateAction::Add {
                    slug,
                    superstate,
                    role: get_str(&obj, "role").map(str::to_string),
                    description: get_str(&obj, "description").map(str::to_string),
                },
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// `PATCH /api/repos/{id}/states/{slug}` — edit one state.
///
/// `role` and `description` are three-valued: absent leaves the field alone,
/// `null` clears it, a string sets it. `move_stories_to` names where open
/// stories go when the edit reclassifies the state they sit in — required by
/// `storage::update_state` in exactly that case.
fn route_patch_state(root: &Path, slug: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let field_edit = |key: &str| match obj.get(key) {
            None => Ok(None),
            Some(serde_json::Value::Null) => Ok(Some(None)),
            Some(serde_json::Value::String(value)) => Ok(Some(Some(value.clone()))),
            Some(_) => Err(AppError::Usage(format!("`{key}` must be a string or null"))),
        };
        let role = field_edit("role")?;
        let description = field_edit("description")?;

        Ok(run_state_mutation_and_reply(
            root,
            200,
            Invocation::State {
                action: StateAction::Set {
                    slug: slug.to_string(),
                    superstate: get_str(&obj, "super_state").map(str::to_string),
                    // `app::run` reads a literal "none" as "clear the role".
                    role: role.map(|value| value.unwrap_or_else(|| "none".to_string())),
                    description: description.clone().flatten(),
                    clear_description: matches!(description, Some(None)),
                    move_stories_to: get_str(&obj, "move_stories_to").map(str::to_string),
                },
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// `DELETE /api/repos/{id}/states/{slug}` — remove a state, optionally
/// migrating the open stories still in it.
fn route_delete_state(root: &Path, slug: &str, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        Ok(run_state_mutation_and_reply(
            root,
            200,
            Invocation::State {
                action: StateAction::Remove {
                    slug: slug.to_string(),
                    move_stories_to: get_str(&obj, "move_stories_to").map(str::to_string),
                },
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// `PATCH /api/repos/{id}/states` — reorder the whole set, i.e. the board's
/// column order. The body must list every state; a partial order is refused
/// rather than interpreted as a deletion.
fn route_reorder_states(root: &Path, body: &str) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let order = get_str_array(&obj, "order");
        if order.is_empty() {
            return Err(AppError::Usage(
                "`order` is required and must be a non-empty array of state slugs".to_string(),
            ));
        }
        Ok(run_state_mutation_and_reply(
            root,
            200,
            Invocation::State {
                action: StateAction::Reorder { order },
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

/// Backs both `POST /api/relate` (`remove: false`) and `POST /api/unrelate`
/// (`remove: true`).
fn route_relate(root: &Path, body: &str, remove: bool) -> Reply {
    (|| -> Result<Reply, AppError> {
        let obj = parse_json_object(body)?;
        let a = require_str(&obj, "a")?.to_string();
        let relation = require_str(&obj, "relation")?.to_string();
        let b = require_str(&obj, "b")?.to_string();
        Ok(run_and_reply(
            root,
            200,
            Invocation::Relate {
                a,
                relation,
                b,
                remove,
            },
        ))
    })()
    .unwrap_or_else(|e| error_reply(&e))
}

// --- Live updates: Server-Sent Events (`GET /api/events`) ---
//
// Every other endpoint is intentionally stateless — `/data` and `/api/repos`
// re-read from disk on every request (see `build_api_json`,
// `build_repos_json`) — so "live" here means telling connected browsers
// *when* to re-fetch, not pushing story data itself. A single filesystem
// watcher (`spawn_change_watcher`) observes every registered repo's
// open-stories directory plus the registry file itself, and publishes a
// coarse event — which repo id changed, or "the registry changed" — to every
// subscriber. The browser already holds the last snapshot it fetched, so it
// diffs the fresh one against it client-side to decide *what* changed and
// pick the matching animation (see `diffSnapshots` in web_dashboard.html).

/// A message pushed to every connected `/api/events` client. Deliberately
/// coarse — never a server-computed diff — so the server stays as stateless
/// about UI concerns as every other endpoint.
#[derive(Clone, Debug)]
enum SseEvent {
    /// A registered repo's stories changed; `id` matches `RegisteredRepo::id`.
    RepoChanged(String),
    /// The registry itself changed (a repo was registered or deregistered).
    ReposChanged,
    /// Keep-alive comment. A failed write while sending this is how a
    /// connection that vanished without a clean close (sleep, network drop)
    /// is detected and its subscriber pruned, rather than lingering forever.
    Ping,
}

#[derive(Default)]
struct BroadcastState {
    next_id: u64,
    subs: Vec<(u64, Sender<SseEvent>)>,
}

/// The live set of connected `/api/events` clients, shared — every clone
/// refers to the same underlying list — between the change watcher (which
/// publishes), the heartbeat thread (which publishes), and every
/// `accept_loop`'s `GET /api/events` handler (which subscribes).
#[derive(Clone, Default)]
struct Broadcaster {
    inner: Arc<Mutex<BroadcastState>>,
}

impl Broadcaster {
    /// Registers a new subscriber, returning its id (pass to [`Self::unsubscribe`]
    /// once the connection ends) and the receiver it should block on.
    fn subscribe(&self) -> (u64, Receiver<SseEvent>) {
        let (tx, rx) = mpsc::channel();
        let mut state = self.inner.lock().unwrap();
        let id = state.next_id;
        state.next_id += 1;
        state.subs.push((id, tx));
        (id, rx)
    }

    /// Removes a subscriber by id. A no-op if it's already gone — e.g. a
    /// concurrent [`Self::publish`] already pruned it because its receiver
    /// had dropped.
    fn unsubscribe(&self, id: u64) {
        self.inner
            .lock()
            .unwrap()
            .subs
            .retain(|(sub, _)| *sub != id);
    }

    /// Sends `event` to every subscriber, dropping any whose receiver is
    /// gone. This is a backstop alongside each connection's own
    /// `Drop`-guard unsubscribe (see `serve_sse`) — either alone prevents a
    /// leak, but a failed send here also notices a dead connection
    /// immediately rather than waiting on that connection's own cleanup.
    fn publish(&self, event: SseEvent) {
        let mut state = self.inner.lock().unwrap();
        state.subs.retain(|(_, tx)| tx.send(event.clone()).is_ok());
    }

    /// Number of connected clients, so the watcher can skip publishing
    /// (cheaply) when nobody is listening.
    fn subscriber_count(&self) -> usize {
        self.inner.lock().unwrap().subs.len()
    }
}

/// Writes the fixed HTTP/1.1 response head for a `GET /api/events`
/// connection. This bypasses [`finish`] entirely — an SSE body is written
/// frame-by-frame over the connection's lifetime rather than as one `Reply`
/// — so it re-emits the same security headers (and the shared [`CSP`]) by
/// hand to keep the two paths from drifting apart. `Transfer-Encoding:
/// chunked` lets the body stay open indefinitely while remaining correctly
/// framed for HTTP/1.1.
fn write_sse_head(w: &mut dyn Write) -> io::Result<()> {
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
fn write_sse_frame(w: &mut dyn Write, payload: &str) -> io::Result<()> {
    write!(w, "{:x}\r\n{payload}\r\n", payload.len())?;
    w.flush()
}

/// Formats one [`SseEvent`] as a complete SSE message (event name + `data:`
/// line, or a `:`-prefixed comment for [`SseEvent::Ping`]), terminated by
/// the blank line that tells the client's `EventSource` the message is
/// complete.
fn format_sse_event(event: &SseEvent) -> String {
    match event {
        SseEvent::Ping => ": ping\n\n".to_string(),
        SseEvent::ReposChanged => "event: repos-changed\ndata: {}\n\n".to_string(),
        SseEvent::RepoChanged(id) => format!(
            "event: repo-changed\ndata: {}\n\n",
            serde_json::json!({ "repo_id": id })
        ),
    }
}

/// Serves one `GET /api/events` connection for its entire lifetime, on its
/// own thread (spawned by `accept_loop`, which immediately moves on to the
/// next request — see the comment there; `tiny_http`'s internal task pool
/// means this never stalls other clients). Subscribes to `broadcaster`,
/// streams every event it receives as an SSE frame until a write fails (the
/// client disconnected), then unsubscribes.
fn serve_sse(request: Request, broadcaster: Broadcaster) {
    let (id, rx) = broadcaster.subscribe();

    // Unsubscribes on drop — including an early `return` below — so a
    // connection that ends can never leak its subscriber slot.
    struct UnsubscribeGuard {
        broadcaster: Broadcaster,
        id: u64,
    }
    impl Drop for UnsubscribeGuard {
        fn drop(&mut self) {
            self.broadcaster.unsubscribe(self.id);
        }
    }
    let _guard = UnsubscribeGuard { broadcaster, id };

    let mut writer = request.into_writer();
    if write_sse_head(&mut writer).is_err() {
        return;
    }
    // `retry: 3000` tells the browser's `EventSource` how long to wait
    // before auto-reconnecting after a drop; the leading comment gives the
    // client an immediate, distinguishable "connected" frame to act on (see
    // `connectEvents`'s `onopen` resync in web_dashboard.html).
    if write_sse_frame(&mut writer, "retry: 3000\n: connected\n\n").is_err() {
        return;
    }

    while let Ok(event) = rx.recv() {
        if write_sse_frame(&mut writer, &format_sse_event(&event)).is_err() {
            break;
        }
    }
    // `_guard` drops here, unsubscribing; `writer` drops, closing the socket.
}

/// Debounce window for a single repo's (or the registry's) change events:
/// several rapid filesystem events — e.g. a `story move` appending one JSONL
/// line, or several stories mutated back-to-back — collapse into one
/// `publish` rather than one per underlying `notify::Event`. Matches the
/// TUI's file watcher (`tui/event.rs`).
const WATCH_DEBOUNCE: Duration = Duration::from_millis(200);

/// How often a heartbeat `Ping` is published to every connected client, so a
/// connection that vanished without a clean close (laptop sleep, network
/// drop) is noticed — its next write fails — rather than lingering forever.
/// Overridable via `STORYHOOK_SSE_HEARTBEAT_MS` so integration tests don't
/// have to wait out the production interval.
fn heartbeat_interval() -> Duration {
    env::var("STORYHOOK_SSE_HEARTBEAT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(20))
}

/// Publishes a [`SseEvent::Ping`] on [`heartbeat_interval`] until `stop` is
/// signaled.
fn spawn_heartbeat(broadcaster: Broadcaster, stop: Arc<AtomicBool>) -> JoinHandle<()> {
    thread::spawn(move || {
        while !stop.load(Ordering::Relaxed) {
            thread::sleep(heartbeat_interval());
            if stop.load(Ordering::Relaxed) {
                break;
            }
            broadcaster.publish(SseEvent::Ping);
        }
    })
}

/// Returns `true` (and records `Instant::now()`) the first time it's called
/// for `key` in longer than `window`; otherwise returns `false`. Used to
/// collapse a burst of filesystem events for the same repo (or the
/// registry, keyed by `""`) into a single published event.
fn debounce(clocks: &Mutex<HashMap<String, Instant>>, key: &str, window: Duration) -> bool {
    let mut clocks = clocks.lock().unwrap();
    let now = Instant::now();
    let fires = match clocks.get(key) {
        Some(last) => now.duration_since(*last) >= window,
        None => true,
    };
    if fires {
        clocks.insert(key.to_string(), now);
    }
    fires
}

/// Runs the single filesystem watcher backing every `/api/events`
/// connection, for the lifetime of the process (`stop` exists for symmetry
/// with `tui::event::EventSource` and for tests; production never signals
/// it, since the dashboard daemon has no clean-shutdown path today — `story
/// web stop` sends `SIGTERM` to the whole process). Watches every registered
/// repo's open-stories directory — creating, appending to, or deleting a
/// file there covers every story mutation (create, any field change, close,
/// reopen; see `storage::write_story_events`/`archive_story`/
/// `unarchive_story`) — plus the registry file's parent directory, so
/// registering or deregistering a repo at runtime is itself detected and
/// triggers a re-scan.
///
/// `ready_tx` is signaled once the *initial* scan has established a watch
/// on every currently-registered repo (or the watcher failed to start at
/// all) — see `start_server`, which blocks on the matching receiver before
/// it starts serving any request. On the FSEvents backend (macOS), every
/// individual `watch()` call tears down and rebuilds the *entire* event
/// stream (see the deadlock-avoidance comment on the rescan channel below),
/// so setting up N registered repos takes N real, not-instant stream
/// restarts; without this handshake, a client could connect and mutate a
/// repo before its watch is actually live, and permanently miss that
/// change — FSEvents doesn't replay history for a stream once it starts.
fn spawn_change_watcher(
    registry_path: PathBuf,
    broadcaster: Broadcaster,
    stop: Arc<AtomicBool>,
    ready_tx: Sender<()>,
) -> JoinHandle<()> {
    thread::spawn(move || run_change_watcher(registry_path, broadcaster, stop, ready_tx))
}

/// Rebuilds `watched` (watched directory -> repo id) from the registry at
/// `registry_path`, `watch`-ing every directory newly present and
/// `unwatch`-ing every one no longer registered. A single repo whose
/// directory is missing or otherwise unwatchable (moved, deleted, permission
/// error) is skipped rather than aborting the whole rescan — one bad repo
/// must never break live updates for the others.
///
/// Watches each repo's open-stories directory, which covers every story
/// mutation (create, any field change, close, reopen — see
/// `storage::write_story_events`/`archive_story`/`unarchive_story`).
///
/// Project *configuration* (states, types, members) deliberately isn't
/// watched: it lives beside the stories directory, next to the SQLite
/// archive whose WAL and shared-memory files are rewritten merely by
/// reading the archive — which every `/data` request does — so watching that
/// directory turns each client's refetch into another change event for
/// every client. Configuration changes made *through the dashboard* are
/// published directly by `accept_loop` instead; ones made behind its back
/// are picked up by the client's safety poll.
fn rescan_watched_repos(
    registry_path: &Path,
    watcher: &mut notify::RecommendedWatcher,
    watched: &Mutex<HashMap<PathBuf, String>>,
) {
    let repos = match registry::Registry::load_from(registry_path) {
        Ok(r) => r.repos,
        Err(_) => return, // Registry momentarily unreadable (e.g. mid-write); the next change event retries.
    };

    let mut watched = watched.lock().unwrap();
    let desired: HashMap<PathBuf, String> = repos
        .into_iter()
        .map(|r| (storage::ProjectPaths::new(&r.path).open_stories_dir(), r.id))
        .collect();

    for dir in watched.keys() {
        if !desired.contains_key(dir) {
            let _ = watcher.unwatch(dir);
        }
    }
    for dir in desired.keys() {
        if !watched.contains_key(dir) {
            let _ = watcher.watch(dir, notify::RecursiveMode::NonRecursive);
        }
    }
    *watched = desired;
}

fn run_change_watcher(
    registry_path: PathBuf,
    broadcaster: Broadcaster,
    stop: Arc<AtomicBool>,
    ready_tx: Sender<()>,
) {
    // open-stories dir -> repo id, kept in sync with the registry by
    // `rescan_watched_repos`, and consulted by the event callback below to
    // attribute a raw filesystem path to the repo it belongs to.
    let watched: Arc<Mutex<HashMap<PathBuf, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let debounce_clocks: Arc<Mutex<HashMap<String, Instant>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let registry_dir = registry_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let registry_file_name: Option<OsString> = registry_path.file_name().map(|n| n.to_os_string());

    // A registry-file change means the set of watched repo directories must
    // be rescanned — but the callback below must NEVER call `watch`/
    // `unwatch` itself: on the FSEvents backend (macOS), those calls block
    // until the event stream quiesces, which can't happen until the *very
    // callback invocation making the call* returns — a self-deadlock. So
    // the callback only *signals* a rescan over this channel; the plain
    // `notify::RecommendedWatcher` value below is owned solely by this
    // function's own control loop, which is the only place that ever calls
    // `watch`/`unwatch` on it.
    let (rescan_tx, rescan_rx) = mpsc::channel::<()>();

    let cb_watched = Arc::clone(&watched);
    let cb_debounce = Arc::clone(&debounce_clocks);
    let cb_broadcaster = broadcaster.clone();
    let cb_registry_file_name = registry_file_name.clone();
    let cb_rescan_tx = rescan_tx.clone();

    let make_watcher =
        notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
            let Ok(event) = res else { return };
            for path in &event.paths {
                let is_registry_file = cb_registry_file_name
                    .as_deref()
                    .is_some_and(|name| path.file_name() == Some(name));
                if is_registry_file {
                    if debounce(&cb_debounce, "", WATCH_DEBOUNCE) {
                        // Wake the control loop to actually rescan/re-watch;
                        // never touch the watcher from this thread. The
                        // control loop publishes `ReposChanged` itself, only
                        // once the rescan has finished — publishing it here
                        // instead would tell a client to refetch (and, for
                        // a brand new repo, start expecting live updates
                        // from it) before that repo's directory is actually
                        // being watched yet.
                        let _ = cb_rescan_tx.send(());
                    }
                    continue;
                }
                if cb_broadcaster.subscriber_count() == 0 {
                    continue;
                }
                let repo_id = cb_watched
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(dir, _)| path.starts_with(dir))
                    .map(|(_, id)| id.clone());
                if let Some(id) = repo_id
                    && debounce(&cb_debounce, &id, WATCH_DEBOUNCE)
                {
                    cb_broadcaster.publish(SseEvent::RepoChanged(id));
                }
            }
        });

    let mut watcher = match make_watcher {
        Ok(w) => w,
        // No live-update capability if the watcher itself can't be created
        // (e.g. the platform's file-event backend is unavailable) — the
        // dashboard still functions via its slower safety poll. Still
        // signal readiness so `start_server` doesn't block forever waiting
        // for a watcher that will never come.
        Err(_) => {
            let _ = ready_tx.send(());
            return;
        }
    };

    rescan_watched_repos(&registry_path, &mut watcher, &watched);
    let _ = watcher.watch(&registry_dir, notify::RecursiveMode::NonRecursive);
    // Every currently-registered repo (and the registry itself) is now
    // being watched — safe for `start_server` to start serving.
    let _ = ready_tx.send(());

    // Control loop: the sole owner of `watcher` for its entire lifetime.
    // Wakes on a rescan signal from the callback above, or every 250ms to
    // check `stop` — the same keep-alive cadence as `tui::event::EventSource`.
    // `ReposChanged` is published from *here*, after `rescan_watched_repos`
    // returns, so a client never sees the event before the repos it
    // describes are actually being watched.
    loop {
        match rescan_rx.recv_timeout(Duration::from_millis(250)) {
            Ok(()) => {
                rescan_watched_repos(&registry_path, &mut watcher, &watched);
                if broadcaster.subscriber_count() > 0 {
                    broadcaster.publish(SseEvent::ReposChanged);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
    }
    drop(watcher);
}

/// The repo whose configuration a just-answered request changed, if any —
/// i.e. a successful write to `/api/repos/<id>/states…`.
///
/// Read as: which clients need to redraw because the *state set* moved under
/// them. Only successful (2xx) writes count; a rejected edit changed
/// nothing, and telling every client to refetch for it would be noise.
fn config_change_repo_id(method: &Method, path: &str, reply: &Reply) -> Option<String> {
    if !matches!(method, Method::Post | Method::Patch | Method::Delete) {
        return None;
    }
    if !(200..300).contains(&reply.status) {
        return None;
    }
    match path_segments(path).as_slice() {
        ["api", "repos", id, "states", ..] => Some((*id).to_string()),
        _ => None,
    }
}

/// Maximum request body size accepted from a mutation route. Well above any
/// legitimate story field (titles, comments, label lists), while bounding
/// how much a single request can force the server to buffer.
const MAX_BODY_BYTES: u64 = 64 * 1024;

/// Reads a request body up to `MAX_BODY_BYTES`. Reads one byte past the cap
/// so an oversized body is *detected* (and rejected with 400) rather than
/// silently truncated.
fn read_body(request: &mut Request) -> Option<String> {
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

/// Parses `STORYHOOK_WEB_TRUSTED_HOSTS` into a lowercase host allowlist for
/// the mutation guard, used to permit a `web-serve`-style reverse proxy to
/// reach the write API under its own (non-loopback) hostname. Read-only
/// requests are never subject to this check.
fn trusted_hosts_from_env() -> Vec<String> {
    env::var("STORYHOOK_WEB_TRUSTED_HOSTS")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Runs the request-accept loop for one bound `Server`, dispatching each
/// request through [`route`]/[`finish`]. Shared between the primary
/// (loopback) listener and the optional tailnet listener started by
/// [`start_server`] — both serve identical logic against the same registry
/// (and, through it, every repo registered in it).
///
/// `GET /api/events` is intercepted here, before body-reading or `route`,
/// and handed off to its own thread ([`serve_sse`]) rather than answered
/// inline: it's a long-lived streaming connection, and this loop must move
/// on to the next request immediately rather than block on it for as long
/// as the browser tab stays open. `tiny_http`'s `Server` already reads
/// connections into an internal queue on its own thread pool, so detaching
/// this one `Request` and `continue`-ing costs nothing to the requests
/// behind it.
fn accept_loop(
    server: Server,
    registry_path: PathBuf,
    trusted_hosts: Vec<String>,
    broadcaster: Broadcaster,
) {
    for mut request in server.incoming_requests() {
        let method = request.method().clone();
        let path = request_path(request.url()).to_string();

        if method == Method::Get && path == "/api/events" {
            let broadcaster = broadcaster.clone();
            thread::spawn(move || serve_sse(request, broadcaster));
            continue;
        }

        let headers = request.headers().to_vec();

        let body = if matches!(method, Method::Post | Method::Patch | Method::Delete) {
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

        let reply = route(
            &registry_path,
            &method,
            &path,
            &headers,
            &body,
            &trusted_hosts,
        );
        // Story writes reach other clients through the filesystem watcher,
        // which watches the open-stories directory. Configuration writes
        // can't: the directory holding states.toml also holds the SQLite
        // archive, which rewrites its WAL files whenever it is merely read
        // (see `rescan_watched_repos`). The server knows perfectly well what
        // it just changed, so it says so directly.
        if let Some(repo_id) = config_change_repo_id(&method, &path, &reply) {
            broadcaster.publish(SseEvent::RepoChanged(repo_id));
        }
        finish(request, reply);
    }
}

/// Binds the dashboard's listeners and serves forever, against the registry
/// at `registry_path` (production always uses [`registry::default_registry_path`];
/// tests point this at a temp file so the suite never touches the
/// developer's real `~/.storyhook/registry.toml`).
///
/// The primary listener is always `127.0.0.1` — hardcoded, never
/// configurable — so the dashboard can never become reachable from beyond
/// this machine by accident. If the `tailscale` CLI reports an IP, a
/// second listener is bound to that address (best-effort: a failure here
/// is logged to stderr and does not stop the primary listener from
/// serving), making the dashboard reachable from the rest of the tailnet
/// without a third-party reverse proxy. Nothing is ever bound to `0.0.0.0`
/// or any other wildcard/public-facing address, and no plain LAN IP is
/// bound either — only loopback and, when present, the tailnet.
pub fn start_server(registry_path: &Path, port: u16) -> Result<(), AppError> {
    let loopback_addr = format!("127.0.0.1:{port}");
    let loopback_server = Server::http(&loopback_addr).map_err(|e| {
        if e.to_string().contains("Address already in use")
            || e.to_string().contains("AddrInUse")
            || e.to_string().contains("address already in use")
        {
            AppError::Usage(format!(
                "Port {port} already in use. Try a different port with --port."
            ))
        } else {
            AppError::Storage(format!("Failed to start web server: {e}"))
        }
    })?;

    eprintln!("Storyhook dashboard: http://127.0.0.1:{port}");

    let mut trusted_hosts = trusted_hosts_from_env();

    // One broadcaster and one filesystem watcher for the whole process,
    // shared by every listener (loopback and, if bound, tailnet) — a repo
    // change should reach every connected browser regardless of which
    // interface it's connected through. `stop` is never signaled in
    // production (the daemon's only shutdown path is `SIGTERM` to the whole
    // process via `story web stop`); it exists so tests can tear background
    // threads down cleanly.
    let broadcaster = Broadcaster::default();
    let stop = Arc::new(AtomicBool::new(false));
    let (watcher_ready_tx, watcher_ready_rx) = mpsc::channel::<()>();
    spawn_change_watcher(
        registry_path.to_path_buf(),
        broadcaster.clone(),
        stop.clone(),
        watcher_ready_tx,
    );
    spawn_heartbeat(broadcaster.clone(), stop);

    // Bind the tailnet listener (if any) right away, same as before this
    // server ever waited on the watcher below — only *serving* on it is
    // gated on watcher readiness, not the bind (and the diagnostic it
    // prints either way), so a slow watcher setup can't delay reporting
    // where the dashboard will be reachable.
    let tailnet_listener = tailnet_identity().and_then(|identity| {
        let tailnet_addr = format!("{}:{port}", identity.bind_ip);
        match Server::http(&tailnet_addr) {
            Ok(server) => {
                eprintln!("Storyhook dashboard (tailnet): http://{tailnet_addr}");
                // We deliberately bound this interface ourselves, so trust
                // its identity for mutations too — the same standing as
                // loopback — without requiring STORYHOOK_WEB_TRUSTED_HOSTS.
                // `trusted_hosts` gains an entry only for a Host a tailnet
                // browser can actually send for this machine (see
                // `TailnetIdentity::trusted_hosts`); trust follows bind, so
                // a name is never trusted unless the interface it would
                // arrive on is actually being served.
                trusted_hosts.extend(identity.trusted_hosts());
                Some(server)
            }
            Err(e) => {
                eprintln!(
                    "warning: could not bind tailnet interface {tailnet_addr}: {e}; \
                     the dashboard is only reachable via localhost"
                );
                None
            }
        }
    });

    // Block until every currently-registered repo's watch is actually
    // established before either listener accepts a single request —
    // otherwise a client could connect and mutate a repo in the gap while
    // the watcher (still working through its one-time, per-repo FSEvents
    // stream setup — see `spawn_change_watcher`'s doc comment) hasn't
    // gotten to it yet, and silently miss that change forever. `recv()`
    // only ever returns `Err` if the watcher thread's sender dropped
    // without sending — i.e. it's already given up — so there's nothing to
    // keep waiting for either way.
    let _ = watcher_ready_rx.recv();

    if let Some(tailnet_server) = tailnet_listener {
        let tailnet_registry_path = registry_path.to_path_buf();
        let tailnet_trusted = trusted_hosts.clone();
        let tailnet_broadcaster = broadcaster.clone();
        std::thread::spawn(move || {
            accept_loop(
                tailnet_server,
                tailnet_registry_path,
                tailnet_trusted,
                tailnet_broadcaster,
            )
        });
    }

    accept_loop(
        loopback_server,
        registry_path.to_path_buf(),
        trusted_hosts,
        broadcaster,
    );
    Ok(())
}

// --- Daemon lifecycle ---
//
// One dashboard daemon serves every registered repo, so its pid/lock/log
// live alongside the registry itself (`~/.storyhook/`) rather than under
// any single repo's `.storyhook/` — there no longer is a single repo to
// scope them to.

fn pid_file() -> Result<PathBuf, AppError> {
    Ok(registry::registry_dir()?.join("web.pid"))
}

fn lock_file() -> Result<PathBuf, AppError> {
    Ok(registry::registry_dir()?.join("web.lock"))
}

fn log_file() -> Result<PathBuf, AppError> {
    Ok(registry::registry_dir()?.join("web.log"))
}

/// Read PID and port from the PID file. Format: "{pid}\n{port}"
fn read_pid_file() -> Option<(u32, u16)> {
    let content = fs::read_to_string(pid_file().ok()?).ok()?;
    let mut lines = content.lines();
    let pid: u32 = lines.next()?.parse().ok()?;
    let port: u16 = lines.next()?.parse().ok()?;
    Some((pid, port))
}

/// Check if a PID is alive and belongs to a `story` process.
fn is_process_alive(pid: u32) -> bool {
    // Check /proc/{pid}/cmdline on Linux
    let cmdline_path = format!("/proc/{pid}/cmdline");
    if let Ok(cmdline) = fs::read_to_string(&cmdline_path) {
        cmdline.contains("story")
    } else {
        // Fallback: use kill -0 to check process existence
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .is_ok_and(|o| o.status.success())
    }
}

/// This machine's Tailscale identity, derived from a single `tailscale
/// status --json` invocation — the IPv4 to bind the tailnet listener to and,
/// when MagicDNS is enabled, the fully-qualified MagicDNS name a tailnet
/// peer's browser sends as `Host` when it reaches this machine by name
/// rather than by raw IP (see [`parse_tailnet_identity`], which is what
/// actually derives this).
#[derive(Debug, Clone, PartialEq, Eq)]
struct TailnetIdentity {
    bind_ip: String,
    magic_dns: Option<String>,
}

impl TailnetIdentity {
    /// `Host` values a browser may legitimately send for *this* machine that
    /// an external attacker can never forge an origin for: the tailnet IPv4
    /// itself, and — when MagicDNS is on — its fully-qualified name. A
    /// `*.ts.net` FQDN only ever resolves (off-tailnet) to NXDOMAIN, so no
    /// attacker can serve a page from that origin — DNS rebinding can't make
    /// a hostile page same-origin with it. The bare single-label short name
    /// (e.g. `psamathe`) is deliberately *not* included: unlike the FQDN, it
    /// can resolve through a DNS search domain that isn't the tailnet's, so
    /// an attacker who influences that search path could rebind exactly that
    /// name to this machine's IP — the very attack class this allowlist
    /// exists to stop.
    fn trusted_hosts(&self) -> Vec<String> {
        let mut hosts = vec![self.bind_ip.clone()];
        hosts.extend(self.magic_dns.clone());
        hosts
    }

    /// The best host to show or copy for reaching this machine: the MagicDNS
    /// name when available — memorable, and, since it's also in
    /// [`Self::trusted_hosts`], guaranteed to work for mutations too — else
    /// the bare tailnet IPv4.
    fn advertise_host(&self) -> &str {
        self.magic_dns.as_deref().unwrap_or(&self.bind_ip)
    }
}

/// Mirrors only the fields this module reads from `tailscale status --json`.
#[derive(serde::Deserialize)]
struct TailscaleStatus {
    #[serde(rename = "Self")]
    self_node: Option<TailscaleSelf>,
}

#[derive(serde::Deserialize)]
struct TailscaleSelf {
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Vec<String>,
    #[serde(rename = "DNSName", default)]
    dns_name: String,
}

/// Parses `tailscale status --json`'s output into this machine's
/// [`TailnetIdentity`]. Pure and dependency-free — exercised by unit tests
/// below against captured fixtures, without a live tailnet. Returns `None`
/// if the JSON has no `Self` entry, isn't valid JSON, or `Self` has no IPv4
/// in `TailscaleIPs` (a v6-only tailnet has no bind target, matching
/// `tailscale ip -4`'s own behavior — no bind, no trust).
fn parse_tailnet_identity(status_json: &str) -> Option<TailnetIdentity> {
    let status: TailscaleStatus = serde_json::from_str(status_json).ok()?;
    let me = status.self_node?;
    let bind_ip = me
        .tailscale_ips
        .iter()
        .find(|ip| ip.parse::<std::net::Ipv4Addr>().is_ok())?
        .clone();
    // `DNSName` is reported rooted (a trailing `.`) and MagicDNS names are
    // already lowercase, but normalize defensively rather than assume.
    let fqdn = me.dns_name.trim_end_matches('.').to_ascii_lowercase();
    let magic_dns = if fqdn.is_empty() { None } else { Some(fqdn) };
    Some(TailnetIdentity { bind_ip, magic_dns })
}

/// Shells out to `tailscale status --json` and parses this machine's
/// [`TailnetIdentity`]. `None` if the CLI is absent, exits non-zero, or
/// reports nothing usable (see [`parse_tailnet_identity`]).
fn tailnet_identity() -> Option<TailnetIdentity> {
    let output = Command::new("tailscale")
        .args(["status", "--json"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_tailnet_identity(&String::from_utf8_lossy(&output.stdout))
}

/// The best host to show or copy for reaching this machine, used by `story
/// web start`/`status`/`address` output: this machine's MagicDNS FQDN when
/// [`tailnet_identity`] reports one (memorable, and guaranteed to work for
/// mutations too — see [`TailnetIdentity::trusted_hosts`]), else its bare
/// tailnet IPv4, else loopback if no tailnet identity is available at all.
fn reachable_host() -> String {
    tailnet_identity()
        .map(|identity| identity.advertise_host().to_string())
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

/// Check if web-serve is available in PATH
fn has_web_serve() -> bool {
    Command::new("which")
        .arg("web-serve")
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn handle_start(port: u16) -> Result<String, AppError> {
    fs::create_dir_all(registry::registry_dir()?)?;

    // Acquire exclusive lock to prevent race conditions
    let lock_path = lock_file()?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| AppError::Storage(format!("Failed to open lock file: {e}")))?;

    match lock.try_lock_exclusive() {
        Ok(()) => {} // Lock acquired — no other instance running
        Err(_) => {
            // Lock held by another process — check PID for user message
            if let Some((pid, existing_port)) = read_pid_file() {
                return Err(AppError::Usage(format!(
                    "Web UI already running (PID {pid} on port {existing_port}). Run 'story web stop' first."
                )));
            }
            return Err(AppError::Usage(
                "Web UI already running. Run 'story web stop' first.".to_string(),
            ));
        }
    }

    // Check for stale PID file (lock acquired but PID file exists from a crashed instance)
    if let Some((pid, _)) = read_pid_file()
        && !is_process_alive(pid)
    {
        let _ = fs::remove_file(pid_file()?);
    }

    // Release lock before spawning child (child will acquire its own lock)
    let _ = lock.unlock();

    // Spawn background server process
    let exe = env::current_exe()
        .map_err(|e| AppError::Storage(format!("Failed to find current executable: {e}")))?;

    let log = fs::File::create(log_file()?)
        .map_err(|e| AppError::Storage(format!("Failed to create web log file: {e}")))?;

    let child = Command::new(exe)
        .args(["web", "--serve", "--port", &port.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .spawn()
        .map_err(|e| AppError::Storage(format!("Failed to spawn web server: {e}")))?;

    let pid = child.id();

    // Write PID file
    fs::write(pid_file()?, format!("{pid}\n{port}"))
        .map_err(|e| AppError::Storage(format!("Failed to write PID file: {e}")))?;

    // Register with web-serve if available
    if has_web_serve() {
        let _ = Command::new("web-serve")
            .args(["register", &port.to_string()])
            .output();
    }

    let host = reachable_host();
    Ok(format!(
        "Web UI started at http://{host}:{port} (PID {pid})"
    ))
}

pub fn handle_stop() -> Result<String, AppError> {
    let pid_path = pid_file()?;
    if !pid_path.exists() {
        return Ok("Web UI is not running".to_string());
    }

    let (pid, _port) =
        read_pid_file().ok_or_else(|| AppError::Storage("Failed to read PID file".to_string()))?;

    if !is_process_alive(pid) {
        // Stale PID
        let _ = fs::remove_file(&pid_path);
        let _ = fs::remove_file(lock_file()?);
        return Ok("Cleaned up stale PID file".to_string());
    }

    // Send SIGTERM to the process
    #[cfg(unix)]
    {
        // SAFETY: libc::kill with a valid PID and SIGTERM is safe
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = Command::new("kill").arg(pid.to_string()).output();
    }

    let _ = fs::remove_file(&pid_path);
    let _ = fs::remove_file(lock_file()?);

    // Unregister from web-serve if available
    if has_web_serve() {
        let _ = Command::new("web-serve").arg("unregister").output();
    }

    Ok(format!("Web UI stopped (PID {pid})"))
}

pub fn handle_status() -> Result<String, AppError> {
    let pid_path = pid_file()?;
    if !pid_path.exists() {
        return Ok("Web UI is not running".to_string());
    }

    let (pid, port) =
        read_pid_file().ok_or_else(|| AppError::Storage("Failed to read PID file".to_string()))?;

    if !is_process_alive(pid) {
        let _ = fs::remove_file(&pid_path);
        let _ = fs::remove_file(lock_file()?);
        return Ok("Web UI is not running (stale PID file cleaned up)".to_string());
    }

    let host = reachable_host();
    Ok(format!(
        "Web UI running at http://{host}:{port} (PID {pid})"
    ))
}

/// Help-like summary of the `web` command group, shown when a command needs the
/// dashboard running but it isn't — so the user immediately sees how to start it.
const WEB_COMMANDS_SUMMARY: &str = "\
web commands:
  story web start [--port <PORT>]   start the dashboard daemon
  story web stop                    stop the dashboard daemon
  story web status                  show whether it's running and its URL
  story web open                    open the dashboard in your browser
  story web address                 copy the dashboard URL to the clipboard
  story web register [<PATH>]       add a repo to the dashboard
  story web deregister <ID|PATH>    remove a repo from the dashboard
  story web list                    list registered repos";

/// The error returned when a `web` command requires the dashboard to be running
/// but it is not. Carries [`WEB_COMMANDS_SUMMARY`] as its help-like body.
fn web_not_running_error() -> AppError {
    AppError::Usage(format!(
        "web dashboard is not running — start it with: story web start\n\n{WEB_COMMANDS_SUMMARY}"
    ))
}

/// Resolve the running dashboard's `(pid, port)`, or a graceful not-running
/// error. Mirrors [`handle_status`]'s liveness check and stale-PID cleanup, so
/// a crashed daemon's leftover files never masquerade as a running dashboard.
fn running_dashboard() -> Result<(u32, u16), AppError> {
    let pid_path = pid_file()?;
    if !pid_path.exists() {
        return Err(web_not_running_error());
    }
    let (pid, port) = read_pid_file().ok_or_else(web_not_running_error)?;
    if !is_process_alive(pid) {
        let _ = fs::remove_file(&pid_path);
        let _ = fs::remove_file(lock_file()?);
        return Err(web_not_running_error());
    }
    Ok((pid, port))
}

/// `story web open` — open the running dashboard in the system-default browser.
///
/// Targets loopback (`http://127.0.0.1:<port>/`), which is always reachable from
/// a browser on this machine. Fails with a help-like summary when the dashboard
/// isn't running.
pub fn handle_open() -> Result<String, AppError> {
    let (_pid, port) = running_dashboard()?;
    let url = format!("http://127.0.0.1:{port}/");
    open_in_browser(&url)?;
    Ok(format!("Opening dashboard at {url}"))
}

/// `story web address` — copy the running dashboard's URL to the system clipboard.
///
/// Targets [`reachable_host`] (this machine's MagicDNS FQDN when Tailscale
/// reports one, else its bare tailnet IPv4, else loopback), so a copied URL
/// is usable from other tailnet devices — matching what `story web status`
/// prints. Fails with a help-like summary when the dashboard isn't running.
pub fn handle_address() -> Result<String, AppError> {
    let (_pid, port) = running_dashboard()?;
    let url = format!("http://{}:{port}/", reachable_host());
    copy_to_clipboard(&url)?;
    Ok(format!("Copied dashboard URL to clipboard: {url}"))
}

/// The default browser-opener argv for the host OS, with `url` appended. Empty
/// on unsupported platforms (the caller turns that into a clear error).
#[cfg(target_os = "macos")]
fn default_open_argv(url: &str) -> Vec<String> {
    vec!["open".to_string(), url.to_string()]
}
#[cfg(target_os = "linux")]
fn default_open_argv(url: &str) -> Vec<String> {
    vec!["xdg-open".to_string(), url.to_string()]
}
#[cfg(target_os = "windows")]
fn default_open_argv(url: &str) -> Vec<String> {
    // `start` is a `cmd` builtin; the empty "" is its (ignored) window-title arg.
    vec![
        "cmd".to_string(),
        "/C".to_string(),
        "start".to_string(),
        String::new(),
        url.to_string(),
    ]
}
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn default_open_argv(_url: &str) -> Vec<String> {
    Vec::new()
}

/// Open `url` in the system-default browser. Honors `$BROWSER` (a non-empty
/// value is run as `<$BROWSER> <url>`); otherwise uses the platform opener. A
/// missing opener maps to an actionable error rather than a raw IO error.
fn open_in_browser(url: &str) -> Result<(), AppError> {
    let argv: Vec<String> = match env::var("BROWSER") {
        Ok(b) if !b.trim().is_empty() => vec![b, url.to_string()],
        _ => default_open_argv(url),
    };
    if argv.is_empty() {
        return Err(AppError::Storage(
            "opening a browser isn't supported on this platform — use `story web address` to copy the URL instead"
                .to_string(),
        ));
    }

    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AppError::Storage(format!(
                    "could not open a browser: `{}` not found on PATH. Set $BROWSER to your browser command, or use `story web address` to copy the URL.",
                    argv[0]
                ))
            } else {
                AppError::Storage(format!("failed to launch browser `{}`: {e}", argv[0]))
            }
        })?;
    if !status.success() {
        return Err(AppError::Storage(format!(
            "browser command `{}` exited with status {status}",
            argv[0]
        )));
    }
    Ok(())
}

/// Clipboard-writer command candidates for the host OS, tried in order. Empty on
/// unsupported platforms (the caller turns that into a clear error).
#[cfg(target_os = "macos")]
fn default_clipboard_argv() -> Vec<Vec<String>> {
    vec![vec!["pbcopy".to_string()]]
}
#[cfg(target_os = "linux")]
fn default_clipboard_argv() -> Vec<Vec<String>> {
    vec![
        vec![
            "xclip".to_string(),
            "-selection".to_string(),
            "clipboard".to_string(),
        ],
        vec![
            "xsel".to_string(),
            "--clipboard".to_string(),
            "--input".to_string(),
        ],
    ]
}
#[cfg(target_os = "windows")]
fn default_clipboard_argv() -> Vec<Vec<String>> {
    vec![vec!["clip".to_string()]]
}
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn default_clipboard_argv() -> Vec<Vec<String>> {
    Vec::new()
}

/// Pipe `text` to a clipboard command's stdin. Stdout/stderr are discarded. The
/// caller distinguishes a missing binary (`ErrorKind::NotFound`) to try the next
/// candidate.
fn pipe_to_command(argv: &[String], text: &str) -> std::io::Result<std::process::ExitStatus> {
    use std::io::Write;
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
        // `stdin` drops here, closing the pipe so the child sees EOF.
    }
    child.wait()
}

/// Copy `text` to the system clipboard. Honors `$STORYHOOK_CLIPBOARD_CMD` (a
/// non-empty value is split on whitespace and used verbatim, e.g. `wl-copy`);
/// otherwise tries the platform utilities in order. A total absence of any
/// clipboard utility maps to an actionable error.
fn copy_to_clipboard(text: &str) -> Result<(), AppError> {
    let candidates: Vec<Vec<String>> = match env::var("STORYHOOK_CLIPBOARD_CMD") {
        Ok(c) if !c.trim().is_empty() => {
            vec![c.split_whitespace().map(str::to_string).collect()]
        }
        _ => default_clipboard_argv(),
    };
    if candidates.is_empty() {
        return Err(AppError::Storage(
            "copying to the clipboard isn't supported on this platform — set $STORYHOOK_CLIPBOARD_CMD to a clipboard command"
                .to_string(),
        ));
    }

    let mut tried = Vec::new();
    for argv in &candidates {
        match pipe_to_command(argv, text) {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => {
                return Err(AppError::Storage(format!(
                    "clipboard command `{}` exited with status {status}",
                    argv[0]
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tried.push(argv[0].clone());
            }
            Err(e) => {
                return Err(AppError::Storage(format!(
                    "failed to run clipboard command `{}`: {e}",
                    argv[0]
                )));
            }
        }
    }
    Err(AppError::Storage(format!(
        "no clipboard utility found (tried: {}). Set $STORYHOOK_CLIPBOARD_CMD to your clipboard command (e.g. wl-copy).",
        tried.join(", ")
    )))
}

/// `story web register [PATH] [--name NAME]` — registers `path` with the
/// default registry (`~/.storyhook/registry.toml`). A relative `path`
/// resolves against the CLI process's actual working directory (the same
/// place any other relative CLI path argument resolves), via
/// `Path::canonicalize` inside `Registry::register`.
pub fn handle_register(path: &Path, name: Option<&str>) -> Result<String, AppError> {
    let repo = registry::with_lock(|r| r.register(path, name))?;
    Ok(format!(
        "Registered `{}` as `{}`",
        repo.path.display(),
        repo.id
    ))
}

/// `story web deregister <ID|PATH>`.
pub fn handle_deregister(target: &str) -> Result<String, AppError> {
    let repo = registry::with_lock(|r| r.deregister(target))?;
    Ok(format!(
        "Deregistered `{}` ({})",
        repo.id,
        repo.path.display()
    ))
}

/// `story web list` — human-readable summary of every registered repo.
pub fn handle_list() -> Result<String, AppError> {
    let registry = registry::Registry::load()?;
    if registry.repos.is_empty() {
        return Ok(
            "No repos registered. Run `story web register` from a project to add one.".to_string(),
        );
    }
    let mut lines = vec![format!("{} registered repo(s):", registry.repos.len())];
    for repo in &registry.repos {
        lines.push(format!(
            "  {} — {} ({})",
            repo.id,
            repo.name,
            repo.path.display()
        ));
    }
    Ok(lines.join("\n"))
}

fn build_api_json(root: &Path) -> Result<String, AppError> {
    let data = build_report_data(root)?;

    // Build a JSON response that includes is_ready and is_blocked per story.
    // Soft-deleted stories are excluded here rather than in `build_report_data`
    // — `story report`/`story list` intentionally still surface them (marked
    // deleted), but the dashboard has no such treatment and would otherwise
    // show them as live cards in whichever column matches their last state.
    let stories_json: Vec<serde_json::Value> = data
        .stories
        .iter()
        .filter(|view| !view.story.deleted)
        .map(|view| {
            let mut val = serde_json::to_value(view).unwrap_or(serde_json::Value::Null);
            if let serde_json::Value::Object(ref mut map) = val {
                map.insert(
                    "is_ready".to_string(),
                    serde_json::Value::Bool(data.ready_ids.contains(&view.story.id)),
                );
                map.insert(
                    "is_blocked".to_string(),
                    serde_json::Value::Bool(data.blocked_ids.contains(&view.story.id)),
                );
            }
            val
        })
        .collect();

    let response = serde_json::json!({
        "summary": data.summary,
        "stories": stories_json,
        "ready_ids": data.ready_ids,
        "blocked_ids": data.blocked_ids,
        "meta": build_meta_json(root)?,
    });

    serde_json::to_string(&response)
        .map_err(|e| AppError::Storage(format!("JSON serialization failed: {e}")))
}

/// Builds the `meta` object describing the project's configuration — states
/// (in `states.toml` order, which the board's columns must follow),
/// types, members, and the fixed priority/relation vocabularies — so the
/// frontend never has to hardcode anything project-specific.
fn build_meta_json(root: &Path) -> Result<serde_json::Value, AppError> {
    let states: Vec<serde_json::Value> = storage::load_states(root)?
        .into_iter()
        .map(|s| {
            serde_json::json!({
                "slug": s.slug,
                "super_state": s.super_state.as_str(),
                "role": s.role,
                "description": s.description,
            })
        })
        .collect();

    let types: Vec<serde_json::Value> = storage::load_types(root)?
        .into_iter()
        .map(|t| serde_json::json!({ "slug": t.slug, "description": t.description }))
        .collect();

    let members: Vec<serde_json::Value> = storage::load_members(root)?
        .into_iter()
        .map(|m| {
            serde_json::json!({
                "id": m.id,
                "display_name": m.display_name,
                "github": m.github,
            })
        })
        .collect();

    let priorities: Vec<&str> = PRIORITIES.iter().map(Priority::as_str).collect();
    let labels = storage::distinct_labels(root)?;

    Ok(serde_json::json!({
        "states": states,
        "types": types,
        "members": members,
        "priorities": priorities,
        "relations": RELATIONS,
        "labels": labels,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_tailnet_identity ---

    /// A `tailscale status --json` fixture matching this project's own
    /// machine (captured while writing the #35 fix): dual-stack
    /// `TailscaleIPs`, a rooted `DNSName`, and an uppercase `HostName` that
    /// must NOT leak into the parsed identity (see
    /// `parse_tailnet_identity_ignores_host_name`).
    const STATUS_JSON: &str = r#"{
        "Self": {
            "HostName": "Psamathe",
            "DNSName": "psamathe.tail983f02.ts.net.",
            "TailscaleIPs": ["100.71.206.33", "fd7a:115c:a1e0::6701:ce21"]
        }
    }"#;

    #[test]
    fn parse_tailnet_identity_happy_path() {
        let identity = parse_tailnet_identity(STATUS_JSON).unwrap();
        assert_eq!(identity.bind_ip, "100.71.206.33");
        assert_eq!(
            identity.magic_dns.as_deref(),
            Some("psamathe.tail983f02.ts.net")
        );
    }

    #[test]
    fn parse_tailnet_identity_strips_trailing_dot_and_lowercases() {
        let json = r#"{"Self": {"DNSName": "Psamathe.Tail983F02.TS.NET.", "TailscaleIPs": ["100.71.206.33"]}}"#;
        let identity = parse_tailnet_identity(json).unwrap();
        assert_eq!(
            identity.magic_dns.as_deref(),
            Some("psamathe.tail983f02.ts.net")
        );
    }

    #[test]
    fn parse_tailnet_identity_ignores_host_name() {
        // HostName ("Psamathe") must never surface anywhere in the parsed
        // identity — only DNSName is a real, browser-addressable name.
        let identity = parse_tailnet_identity(STATUS_JSON).unwrap();
        assert_ne!(identity.magic_dns.as_deref(), Some("psamathe"));
        assert_ne!(identity.magic_dns.as_deref(), Some("Psamathe"));
    }

    #[test]
    fn parse_tailnet_identity_missing_dns_name_is_ip_only() {
        let json = r#"{"Self": {"DNSName": "", "TailscaleIPs": ["100.71.206.33"]}}"#;
        let identity = parse_tailnet_identity(json).unwrap();
        assert_eq!(identity.bind_ip, "100.71.206.33");
        assert_eq!(identity.magic_dns, None);
    }

    #[test]
    fn parse_tailnet_identity_missing_self_is_none() {
        assert_eq!(parse_tailnet_identity(r#"{}"#), None);
    }

    #[test]
    fn parse_tailnet_identity_ipv6_only_is_none() {
        let json = r#"{"Self": {"DNSName": "foo.ts.net.", "TailscaleIPs": ["fd7a:115c:a1e0::6701:ce21"]}}"#;
        assert_eq!(parse_tailnet_identity(json), None);
    }

    #[test]
    fn parse_tailnet_identity_malformed_json_is_none() {
        assert_eq!(parse_tailnet_identity("not json"), None);
        assert_eq!(parse_tailnet_identity(""), None);
    }

    #[test]
    fn parse_tailnet_identity_picks_first_ipv4_among_mixed_order() {
        let json = r#"{"Self": {"DNSName": "", "TailscaleIPs": ["fd7a:115c:a1e0::6701:ce21", "100.71.206.33"]}}"#;
        let identity = parse_tailnet_identity(json).unwrap();
        assert_eq!(identity.bind_ip, "100.71.206.33");
    }

    // --- TailnetIdentity::trusted_hosts / advertise_host ---

    #[test]
    fn trusted_hosts_includes_ip_and_fqdn_but_not_short_label_or_ipv6() {
        let identity = parse_tailnet_identity(STATUS_JSON).unwrap();
        let hosts = identity.trusted_hosts();
        assert_eq!(
            hosts,
            vec![
                "100.71.206.33".to_string(),
                "psamathe.tail983f02.ts.net".to_string(),
            ]
        );
        // The bare single-label short name is deliberately excluded — see
        // TailnetIdentity::trusted_hosts's doc comment for why.
        assert!(!hosts.iter().any(|h| h == "psamathe"));
        // Never bound, so never trusted (see TailnetIdentity's doc comment).
        assert!(
            !hosts
                .iter()
                .any(|h| h.contains(':') && h != "psamathe.tail983f02.ts.net")
        );
    }

    #[test]
    fn trusted_hosts_is_ip_only_without_magic_dns() {
        let identity = TailnetIdentity {
            bind_ip: "100.71.206.33".to_string(),
            magic_dns: None,
        };
        assert_eq!(identity.trusted_hosts(), vec!["100.71.206.33".to_string()]);
    }

    #[test]
    fn advertise_host_prefers_magic_dns_over_ip() {
        let identity = parse_tailnet_identity(STATUS_JSON).unwrap();
        assert_eq!(identity.advertise_host(), "psamathe.tail983f02.ts.net");
    }

    #[test]
    fn advertise_host_falls_back_to_ip_without_magic_dns() {
        let identity = TailnetIdentity {
            bind_ip: "100.71.206.33".to_string(),
            magic_dns: None,
        };
        assert_eq!(identity.advertise_host(), "100.71.206.33");
    }

    // --- config_change_repo_id ---

    fn ok_reply() -> Reply {
        json_reply(200, "{}")
    }

    #[test]
    fn config_change_is_detected_for_every_state_write() {
        for (method, path) in [
            (Method::Post, "/api/repos/app/states"),
            (Method::Patch, "/api/repos/app/states"),
            (Method::Patch, "/api/repos/app/states/todo"),
            (Method::Delete, "/api/repos/app/states/todo"),
        ] {
            assert_eq!(
                config_change_repo_id(&method, path, &ok_reply()).as_deref(),
                Some("app"),
                "{method:?} {path}"
            );
        }
    }

    #[test]
    fn reads_and_story_writes_are_not_config_changes() {
        assert_eq!(
            config_change_repo_id(&Method::Get, "/api/repos/app/states", &ok_reply()),
            None,
            "listing states changes nothing"
        );
        assert_eq!(
            config_change_repo_id(&Method::Post, "/api/repos/app/story", &ok_reply()),
            None,
            "story writes reach clients through the filesystem watcher"
        );
        assert_eq!(
            config_change_repo_id(&Method::Post, "/api/repos", &ok_reply()),
            None
        );
    }

    /// A rejected edit changed nothing — telling every client to refetch for
    /// it would be pure noise.
    #[test]
    fn failed_state_writes_are_not_config_changes() {
        for status in [400, 403, 404, 422, 500] {
            assert_eq!(
                config_change_repo_id(
                    &Method::Patch,
                    "/api/repos/app/states/todo",
                    &json_reply(status, "{}")
                ),
                None,
                "status {status}"
            );
        }
    }

    // --- host_is_trusted ---

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
        // TailnetIdentity::trusted_hosts's doc comment.
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
