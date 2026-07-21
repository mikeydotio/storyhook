use std::env;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use fs4::FileExt;
use tiny_http::{Header, Method, Request, Response, Server};

use crate::app::{self, build_report_data};
use crate::cli::{CliOptions, Invocation};
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

fn security_header_csp() -> Header {
    Header::from_bytes(
        "Content-Security-Policy",
        "default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'",
    )
    .unwrap()
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
/// `STORYHOOK_WEB_TRUSTED_HOSTS` (for a `web-serve`-style reverse proxy).
fn host_is_trusted(host: &str, trusted_hosts: &[String]) -> bool {
    let host_only = host_without_port(host).to_ascii_lowercase();
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
        Ok(run_and_reply(
            root,
            201,
            Invocation::New {
                title,
                state,
                story_type,
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
fn accept_loop(server: Server, registry_path: PathBuf, trusted_hosts: Vec<String>) {
    for mut request in server.incoming_requests() {
        let method = request.method().clone();
        let path = request_path(request.url()).to_string();
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

    if let Some(tailnet_ip) = tailscale_ip() {
        let tailnet_addr = format!("{tailnet_ip}:{port}");
        match Server::http(&tailnet_addr) {
            Ok(tailnet_server) => {
                eprintln!("Storyhook dashboard (tailnet): http://{tailnet_addr}");
                // We deliberately bound this interface ourselves, so trust
                // it for mutations too — the same standing as loopback —
                // without requiring STORYHOOK_WEB_TRUSTED_HOSTS.
                trusted_hosts.push(tailnet_ip);
                let tailnet_registry_path = registry_path.to_path_buf();
                let tailnet_trusted = trusted_hosts.clone();
                std::thread::spawn(move || {
                    accept_loop(tailnet_server, tailnet_registry_path, tailnet_trusted)
                });
            }
            Err(e) => {
                eprintln!(
                    "warning: could not bind tailnet interface {tailnet_addr}: {e}; \
                     the dashboard is only reachable via localhost"
                );
            }
        }
    }

    accept_loop(loopback_server, registry_path.to_path_buf(), trusted_hosts);
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

/// This machine's Tailscale IPv4 address, if the `tailscale` CLI is present
/// and reports one. Used both to decide whether to bind a second,
/// tailnet-reachable listener and to display its URL — deliberately not a
/// generic LAN IP: the dashboard must only ever be reachable via loopback or
/// the tailnet, never a bare local-network address that might itself be
/// exposed further (e.g. by router port-forwarding).
fn tailscale_ip() -> Option<String> {
    let output = Command::new("tailscale").args(["ip", "-4"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if ip.is_empty() { None } else { Some(ip) }
}

/// The best reachable IP for display: the Tailscale IP if bound, else loopback.
fn reachable_ip() -> String {
    tailscale_ip().unwrap_or_else(|| "127.0.0.1".to_string())
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

    let ip = reachable_ip();
    Ok(format!("Web UI started at http://{ip}:{port} (PID {pid})"))
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

    let ip = reachable_ip();
    Ok(format!("Web UI running at http://{ip}:{port} (PID {pid})"))
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
/// Targets [`reachable_ip`] (the Tailscale IP when the `tailscale` CLI reports
/// one, else loopback), so a copied URL is usable from other tailnet devices —
/// matching what `story web status` prints. Fails with a help-like summary when
/// the dashboard isn't running.
pub fn handle_address() -> Result<String, AppError> {
    let (_pid, port) = running_dashboard()?;
    let url = format!("http://{}:{port}/", reachable_ip());
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

    // Build a JSON response that includes is_ready and is_blocked per story
    let stories_json: Vec<serde_json::Value> = data
        .stories
        .iter()
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

    Ok(serde_json::json!({
        "states": states,
        "types": types,
        "members": members,
        "priorities": priorities,
        "relations": RELATIONS,
    }))
}
