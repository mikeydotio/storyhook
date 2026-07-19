use std::env;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

use fs4::FileExt;
use tiny_http::{Header, Method, Request, Response, Server};

use crate::app::{self, build_report_data};
use crate::cli::{CliOptions, Invocation};
use crate::domain::Priority;
use crate::error::AppError;
use crate::output::{render_error, render_response};
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
}

impl Reply {
    fn new(status: u16, content_type: &'static str, body: impl Into<String>) -> Self {
        Reply {
            status,
            content_type,
            body: body.into(),
            no_cache: false,
        }
    }

    /// Marks this reply as dynamic content that must never be cached by the browser.
    fn no_cache(mut self) -> Self {
        self.no_cache = true;
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
    json_reply(status_for(error), render_error(error, true))
}

/// Decides how to respond to a request. Only GET routes exist so far — write
/// routes are added on top of this scaffold in a follow-up commit.
fn route(root: &Path, method: &Method, path: &str) -> Reply {
    if *method != Method::Get {
        return text_reply(405, "Method not allowed");
    }

    match path_segments(path).as_slice() {
        [] => html_reply(DASHBOARD_HTML).no_cache(),
        ["api", "data"] => match build_api_json(root) {
            Ok(json) => json_reply(200, json).no_cache(),
            Err(e) => error_reply(&e),
        },
        ["api", "story", id] => match show_story_json(root, id) {
            Ok(json) => json_reply(200, json),
            Err(e) => error_reply(&e),
        },
        _ => text_reply(404, "Not found"),
    }
}

/// Renders a single story as the standard JSON envelope, dispatching through
/// `app::run` with `Invocation::Show` so the web API sees exactly the same
/// validation and shape as `story show <id>` on the CLI.
fn show_story_json(root: &Path, id: &str) -> Result<String, AppError> {
    let options = CliOptions {
        json: true,
        quiet: false,
        no_hooks: false,
        invocation: Invocation::Show { id: id.to_string() },
    };
    let response = app::run(root, options)?;
    Ok(render_response(&response, true, false))
}

pub fn start_server(root: &Path, port: u16) -> Result<(), AppError> {
    let addr = format!("127.0.0.1:{port}");
    let server = Server::http(&addr).map_err(|e| {
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

    for request in server.incoming_requests() {
        let method = request.method().clone();
        let path = request_path(request.url()).to_string();
        let reply = route(root, &method, &path);
        finish(request, reply);
    }

    Ok(())
}

// --- Daemon lifecycle ---

fn pid_file(root: &Path) -> PathBuf {
    root.join(".storyhook/web.pid")
}

fn lock_file(root: &Path) -> PathBuf {
    root.join(".storyhook/web.lock")
}

fn log_file(root: &Path) -> PathBuf {
    root.join(".storyhook/web.log")
}

/// Read PID and port from the PID file. Format: "{pid}\n{port}"
fn read_pid_file(root: &Path) -> Option<(u32, u16)> {
    let content = fs::read_to_string(pid_file(root)).ok()?;
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

/// Get the best reachable IP: Tailscale IP if available, otherwise LAN IP, fallback to 127.0.0.1.
fn reachable_ip() -> String {
    // Try tailscale IPv4 first
    if let Ok(output) = Command::new("tailscale").args(["ip", "-4"]).output()
        && output.status.success()
    {
        let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !ip.is_empty() {
            return ip;
        }
    }

    // Fallback: first non-loopback IPv4 from hostname -I
    if let Ok(output) = Command::new("hostname").arg("-I").output()
        && output.status.success()
    {
        let ips = String::from_utf8_lossy(&output.stdout);
        for tok in ips.split_whitespace() {
            if !tok.starts_with("127.") && !tok.contains(':') {
                return tok.to_string();
            }
        }
    }

    "127.0.0.1".to_string()
}

/// Check if web-serve is available in PATH
fn has_web_serve() -> bool {
    Command::new("which")
        .arg("web-serve")
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn handle_start(root: &Path, port: u16) -> Result<String, AppError> {
    storage::ensure_project(root)?;

    // Acquire exclusive lock to prevent race conditions
    let lock_path = lock_file(root);
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
            if let Some((pid, existing_port)) = read_pid_file(root) {
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
    if let Some((pid, _)) = read_pid_file(root)
        && !is_process_alive(pid)
    {
        let _ = fs::remove_file(pid_file(root));
    }

    // Release lock before spawning child (child will acquire its own lock)
    let _ = lock.unlock();

    // Spawn background server process
    let exe = env::current_exe()
        .map_err(|e| AppError::Storage(format!("Failed to find current executable: {e}")))?;

    let root_abs = root
        .canonicalize()
        .map_err(|e| AppError::Storage(format!("Failed to resolve project path: {e}")))?;

    let log = fs::File::create(log_file(root))
        .map_err(|e| AppError::Storage(format!("Failed to create web log file: {e}")))?;

    let child = Command::new(exe)
        .args([
            "web",
            "--serve",
            "--port",
            &port.to_string(),
            "--root",
            &root_abs.to_string_lossy(),
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(log)
        .spawn()
        .map_err(|e| AppError::Storage(format!("Failed to spawn web server: {e}")))?;

    let pid = child.id();

    // Write PID file
    fs::write(pid_file(root), format!("{pid}\n{port}"))
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

pub fn handle_stop(root: &Path) -> Result<String, AppError> {
    let pid_path = pid_file(root);
    if !pid_path.exists() {
        return Ok("Web UI is not running".to_string());
    }

    let (pid, _port) = read_pid_file(root)
        .ok_or_else(|| AppError::Storage("Failed to read PID file".to_string()))?;

    if !is_process_alive(pid) {
        // Stale PID
        let _ = fs::remove_file(&pid_path);
        let _ = fs::remove_file(lock_file(root));
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
    let _ = fs::remove_file(lock_file(root));

    // Unregister from web-serve if available
    if has_web_serve() {
        let _ = Command::new("web-serve").arg("unregister").output();
    }

    Ok(format!("Web UI stopped (PID {pid})"))
}

pub fn handle_status(root: &Path) -> Result<String, AppError> {
    let pid_path = pid_file(root);
    if !pid_path.exists() {
        return Ok("Web UI is not running".to_string());
    }

    let (pid, port) = read_pid_file(root)
        .ok_or_else(|| AppError::Storage("Failed to read PID file".to_string()))?;

    if !is_process_alive(pid) {
        let _ = fs::remove_file(&pid_path);
        let _ = fs::remove_file(lock_file(root));
        return Ok("Web UI is not running (stale PID file cleaned up)".to_string());
    }

    let ip = reachable_ip();
    Ok(format!("Web UI running at http://{ip}:{port} (PID {pid})"))
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
