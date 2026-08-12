//! `story web …` — the deprecated aliases, and the browser seam.
//!
//! The clipboard seam moved to [`crate::clipboard`] when `story daemon token`
//! became its second caller (SH-250); `story web address` reaches it there.
//!
//! The dashboard is not a separate program any more: it is a surface of the
//! storyhook daemon, and `story daemon` is what manages that. These aliases
//! survive because scripts, muscle memory and this project's own plugin all
//! type `story web start`, and breaking them to make a point would be a poor
//! trade.
//!
//! **Their output is unchanged.** Every one of them prints the bytes it always
//! printed, and says on *stderr* where it moved to — so a script reading stdout
//! keeps working and a human reading a terminal learns something.
//!
//! The catalog commands (`register`/`deregister`/`list`) are *not* here. They
//! had a second implementation over `~/.storyhook/registry.toml` for as long as
//! the quarantined legacy path existed; that path is gone and
//! [`crate::service::CatalogService`] is the only one left.

use std::env;
use std::process::Command;

use crate::daemon::commands;
use crate::daemon::lifecycle::{self, DaemonInfo};
use crate::env::Environment;
use crate::error::AppError;

/// Tells the user, once, where a `story web` command moved to.
///
/// On stderr rather than stdout: the aliases exist so that a script reading
/// stdout keeps working, and a deprecation notice mixed into its output would
/// defeat that in the same breath as announcing it.
fn deprecation(alias: &str, replacement: &str) {
    eprintln!("note: `story {alias}` is now `story {replacement}`; the old spelling still works.");
}

/// The environment these commands act in.
///
/// Re-resolved rather than threaded, and `None` for the flag on purpose: `main`
/// publishes any `--store-path` into `$STORYHOOK_STORE_PATH` before anything
/// dispatches, so this reads the store the caller named.
fn environment() -> Result<Environment, AppError> {
    Environment::from_process(None)
}

/// `story web start [--port N]` — an alias for `story daemon start`.
///
/// `port` passes straight through, `None` included: an alias that substituted a
/// default of its own would override `$STORYHOOK_DAEMON_ADDR` and the store's
/// resolved preference, which is the whole of SH-249.
pub fn handle_start(port: Option<u16>) -> Result<String, AppError> {
    deprecation("web start", "daemon start");
    let env = environment()?;
    let info = commands::start(&env, port)?;
    Ok(format!(
        "Web UI started at {} (PID {})",
        info.dashboard_url(),
        info.pid
    ))
}

/// `story web stop` — an alias for `story daemon stop`.
pub fn handle_stop() -> Result<String, AppError> {
    deprecation("web stop", "daemon stop");
    let env = environment()?;
    Ok(
        match lifecycle::stop(&env, lifecycle::StopMode::Graceful)? {
            Some(info) => format!("Web UI stopped (PID {})", info.pid),
            None => "Web UI is not running".to_string(),
        },
    )
}

/// `story web status` — an alias for `story daemon status`.
pub fn handle_status() -> Result<String, AppError> {
    deprecation("web status", "daemon status");
    let env = environment()?;
    Ok(match running_daemon(&env) {
        Some(info) => format!(
            "Web UI running at {} (PID {})",
            info.dashboard_url(),
            info.pid
        ),
        None => "Web UI is not running".to_string(),
    })
}

/// The running daemon, if there is one that published where it is.
fn running_daemon(env: &Environment) -> Option<DaemonInfo> {
    lifecycle::is_live(env)
        .then(|| lifecycle::read_info(env))
        .flatten()
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
";

/// The error returned when a `web` command requires the dashboard to be running
/// but it is not. Carries [`WEB_COMMANDS_SUMMARY`] as its help-like body.
fn web_not_running_error() -> AppError {
    AppError::Usage(format!(
        "web dashboard is not running — start it with: story web start\n\n{WEB_COMMANDS_SUMMARY}"
    ))
}

/// `story web open` — open the running dashboard in the system-default browser.
///
/// Targets loopback (`http://127.0.0.1:<port>/`), which is always reachable from
/// a browser on this machine. Fails with a help-like summary when the dashboard
/// isn't running.
pub fn handle_open() -> Result<String, AppError> {
    let env = environment()?;
    let info = running_daemon(&env).ok_or_else(web_not_running_error)?;
    let url = format!("http://127.0.0.1:{}/", info.port);
    open_in_browser(&url)?;
    Ok(format!("Opening dashboard at {url}"))
}

/// `story web address` — copy the running dashboard's URL to the system clipboard.
///
/// Targets [`DaemonInfo::dashboard_url`] — the daemon's MagicDNS FQDN when it
/// *bound* its tailnet interface, else loopback — so a copied URL is usable
/// from other tailnet devices whenever there is a tailnet listener to reach,
/// and is never a URL that refuses connections. Matches what `story web status`
/// prints, because both read the same published fact. Fails with a help-like
/// summary when the dashboard isn't running.
pub fn handle_address() -> Result<String, AppError> {
    let env = environment()?;
    let info = running_daemon(&env).ok_or_else(web_not_running_error)?;
    let url = format!("{}/", info.dashboard_url());
    crate::clipboard::copy_to_clipboard(&url)?;
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
