use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tiny_http::{Header, Method, Response, Server};

use crate::app::build_report_data;
use crate::error::AppError;
use crate::storage;

const DASHBOARD_HTML: &str = include_str!("web_dashboard.html");

pub fn start_server(root: &Path, port: u16) -> Result<(), AppError> {
    let addr = format!("0.0.0.0:{port}");
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
        let url = request.url().to_string();

        if method != Method::Get {
            let resp = Response::from_string("Method not allowed")
                .with_status_code(405)
                .with_header(
                    Header::from_bytes("Content-Type", "text/plain; charset=utf-8").unwrap(),
                );
            let _ = request.respond(resp);
            continue;
        }

        match url.as_str() {
            "/" => {
                let resp = Response::from_string(DASHBOARD_HTML)
                    .with_header(
                        Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap(),
                    )
                    .with_header(Header::from_bytes("Cache-Control", "no-cache").unwrap());
                let _ = request.respond(resp);
            }
            "/api/data" => {
                let json = match build_api_json(root) {
                    Ok(j) => j,
                    Err(e) => {
                        let body = format!("{{\"error\": \"{}\"}}", e);
                        let resp = Response::from_string(body)
                            .with_status_code(500)
                            .with_header(
                                Header::from_bytes("Content-Type", "application/json")
                                    .unwrap(),
                            );
                        let _ = request.respond(resp);
                        continue;
                    }
                };
                let resp = Response::from_string(json)
                    .with_header(
                        Header::from_bytes("Content-Type", "application/json").unwrap(),
                    )
                    .with_header(Header::from_bytes("Cache-Control", "no-cache").unwrap());
                let _ = request.respond(resp);
            }
            _ => {
                let resp = Response::from_string("Not found")
                    .with_status_code(404)
                    .with_header(
                        Header::from_bytes("Content-Type", "text/plain; charset=utf-8").unwrap(),
                    );
                let _ = request.respond(resp);
            }
        }
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
    if let Ok(output) = Command::new("tailscale").args(["ip", "-4"]).output() {
        if output.status.success() {
            let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !ip.is_empty() {
                return ip;
            }
        }
    }

    // Fallback: first non-loopback IPv4 from hostname -I
    if let Ok(output) = Command::new("hostname").arg("-I").output() {
        if output.status.success() {
            let ips = String::from_utf8_lossy(&output.stdout);
            for tok in ips.split_whitespace() {
                if !tok.starts_with("127.") && !tok.contains(':') {
                    return tok.to_string();
                }
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

    // Check for existing running instance
    let lock_path = lock_file(root);
    if lock_path.exists() {
        // Try to verify via PID
        if let Some((pid, existing_port)) = read_pid_file(root) {
            if is_process_alive(pid) {
                return Err(AppError::Usage(format!(
                    "Web UI already running (PID {pid} on port {existing_port}). Run 'story web stop' first."
                )));
            }
            // Stale — clean up
            let _ = fs::remove_file(pid_file(root));
            let _ = fs::remove_file(&lock_path);
        }
    }

    // Create lock file
    let _ = fs::write(&lock_path, "");

    // Spawn background server process
    let exe = env::current_exe().map_err(|e| {
        AppError::Storage(format!("Failed to find current executable: {e}"))
    })?;

    let root_abs = root
        .canonicalize()
        .map_err(|e| AppError::Storage(format!("Failed to resolve project path: {e}")))?;

    let log = fs::File::create(log_file(root)).map_err(|e| {
        AppError::Storage(format!("Failed to create web log file: {e}"))
    })?;

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
    fs::write(pid_file(root), format!("{pid}\n{port}")).map_err(|e| {
        AppError::Storage(format!("Failed to write PID file: {e}"))
    })?;

    // Register with web-serve if available
    if has_web_serve() {
        let _ = Command::new("web-serve")
            .args(["register", &port.to_string()])
            .output();
    }

    let ip = reachable_ip();
    Ok(format!(
        "Web UI started at http://{ip}:{port} (PID {pid})"
    ))
}

pub fn handle_stop(root: &Path) -> Result<String, AppError> {
    let pid_path = pid_file(root);
    if !pid_path.exists() {
        return Ok("Web UI is not running".to_string());
    }

    let (pid, _port) = read_pid_file(root).ok_or_else(|| {
        AppError::Storage("Failed to read PID file".to_string())
    })?;

    if !is_process_alive(pid) {
        // Stale PID
        let _ = fs::remove_file(&pid_path);
        let _ = fs::remove_file(lock_file(root));
        return Ok("Cleaned up stale PID file".to_string());
    }

    // Kill the process
    let _ = Command::new("kill")
        .arg(pid.to_string())
        .output();

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

    let (pid, port) = read_pid_file(root).ok_or_else(|| {
        AppError::Storage("Failed to read PID file".to_string())
    })?;

    if !is_process_alive(pid) {
        let _ = fs::remove_file(&pid_path);
        let _ = fs::remove_file(lock_file(root));
        return Ok("Web UI is not running (stale PID file cleaned up)".to_string());
    }

    let ip = reachable_ip();
    Ok(format!(
        "Web UI running at http://{ip}:{port} (PID {pid})"
    ))
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
    });

    serde_json::to_string(&response)
        .map_err(|e| AppError::Storage(format!("JSON serialization failed: {e}")))
}
