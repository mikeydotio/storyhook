//! The `story web …` command group's process management.
//!
//! What is left here is the CLI's side of the dashboard: starting and stopping
//! the background process, reporting where it is, opening it in a browser, and
//! copying its address. The server itself moved to [`crate::daemon`] and the
//! routes to [`crate::api::rest`].
//!
//! The catalog commands (`register`/`deregister`/`list`) survive only for the
//! quarantined legacy path in [`crate::app`]; every live caller reaches
//! [`crate::service::CatalogService`] instead.

use std::env;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;

use fs4::FileExt;

use crate::daemon::tailnet::reachable_host;
use crate::error::AppError;
use crate::registry;

// --- Daemon lifecycle ---
//
// The runtime files live in the XDG state home. They used to sit in
// `~/.storyhook/` beside the registry, which was where storyhook's global
// state went before locked decision 6; a pid file and a log are the textbook
// contents of a state home, and leaving them in a directory the store no
// longer reads would leave that directory looking live.

/// The directory the daemon's runtime files live in.
fn state_dir() -> Result<PathBuf, AppError> {
    Ok(crate::env::Environment::from_process()?
        .state_home()
        .to_path_buf())
}

fn pid_file() -> Result<PathBuf, AppError> {
    Ok(state_dir()?.join("web.pid"))
}

fn lock_file() -> Result<PathBuf, AppError> {
    Ok(state_dir()?.join("web.lock"))
}

fn log_file() -> Result<PathBuf, AppError> {
    Ok(state_dir()?.join("web.log"))
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

/// Check if web-serve is available in PATH
fn has_web_serve() -> bool {
    Command::new("which")
        .arg("web-serve")
        .output()
        .is_ok_and(|o| o.status.success())
}

pub fn handle_start(port: u16) -> Result<String, AppError> {
    fs::create_dir_all(state_dir()?)?;

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
