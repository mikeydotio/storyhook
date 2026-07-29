//! `story daemon …` — the commands *about* the daemon, as opposed to the daemon.
//!
//! Starting, stopping, reporting, and registering it with the system's service
//! manager so it comes back at login. The `story web start|stop|status` family
//! are aliases for the first three; they kept their wording, because scripts
//! read it, and gained a line on stderr saying where they moved.
//!
//! [`status`] is also where the backups are reported. The plan said `story
//! doctor`, and this is a deliberate departure: `doctor`'s output is pinned
//! byte-for-byte by the golden corpus, and its exit code *means* something —
//! a project's integrity. How old a copy of the database is a fact about the
//! machine, not about the project, and adding it to `doctor` would either move
//! bytes the whole rearchitecture is measured against or turn "this machine has
//! not run a daemon lately" into an integrity failure.

use std::path::PathBuf;

use crate::daemon::lifecycle::{self, DaemonInfo};
use crate::daemon::tailnet::reachable_host;
use crate::env::Environment;
use crate::error::AppError;

/// The launchd label for the user agent.
///
/// Reverse-DNS under the author's own domain, which is the convention every
/// other bundle identifier in this ecosystem follows.
pub const LAUNCHD_LABEL: &str = "io.mikey.storyhook.daemon";

/// Starts a daemon in the background, or reports the one already running.
///
/// `port` overrides the environment's preferred port for the daemon this
/// *starts*; it does not move an already-running one, because a request to start
/// something that is already started is not a request to restart it.
///
/// Deliberately nothing more than [`lifecycle::ensure`] with that override.
/// `ensure` already returns a matching daemon untouched and replaces one that
/// does not match, and an earlier version of this function short-circuited on
/// "something is running" *before* that check — so `story daemon start` happily
/// reported a daemon serving a different build as though it were the right one.
/// Pinned by `a_daemon_from_another_build_is_replaced_rather_than_reused`.
pub fn start(env: &Environment, port: Option<u16>) -> Result<DaemonInfo, AppError> {
    let env = match port {
        Some(port) => env
            .clone()
            .daemon_addr(std::net::SocketAddr::from(([127, 0, 0, 1], port))),
        None => env.clone(),
    };
    lifecycle::ensure(&env)
}

/// Stops the running daemon.
pub fn stop(env: &Environment) -> Result<String, AppError> {
    match lifecycle::stop(env)? {
        Some(info) => Ok(format!("storyhook daemon stopped (PID {})", info.pid)),
        None => Ok("storyhook daemon is not running".to_string()),
    }
}

/// Reports whether a daemon is running, and what it is.
pub fn status(env: &Environment) -> Result<String, AppError> {
    if !lifecycle::is_live(env) {
        return Ok(format!(
            "storyhook daemon is not running\n\n{}\n{}",
            lifecycle::describe_paths(env),
            crate::daemon::backup::describe(env)
        ));
    }
    match lifecycle::read_info(env) {
        Some(info) => {
            let staleness = if info.is_this_binary() {
                String::new()
            } else {
                // Worth saying out loud rather than leaving to be discovered:
                // the next command will restart it, and a user watching the pid
                // change deserves to know why.
                format!(
                    "\n  serving storyhook {}, which is not the build you are running — \
                     the next command will restart it",
                    info.version
                )
            };
            Ok(format!(
                "storyhook daemon {} running at http://{}:{} (PID {}){}\n\n{}\n{}",
                info.version,
                reachable_host(),
                info.port,
                info.pid,
                staleness,
                lifecycle::describe_paths(env),
                crate::daemon::backup::describe(env)
            ))
        }
        // The lock is held by something that published nothing. Say so plainly
        // rather than reporting "not running", which would be false.
        None => Ok(format!(
            "a storyhook daemon holds the pidfile but published no portfile\n\n{}",
            lifecycle::describe_paths(env)
        )),
    }
}

/// Where the launchd agent's plist goes.
fn agent_path(env: &Environment) -> PathBuf {
    env.home()
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist"))
}

/// The launchd agent definition.
///
/// `KeepAlive` is deliberately absent. The daemon is *supposed* to exit on a
/// version-skew restart, and an agent that resurrected it immediately would race
/// the client that just asked for a newer one. `RunAtLoad` is what this is for:
/// the dashboard is there after a reboot without anybody running a command.
fn agent_plist(exe: &std::path::Path, env: &Environment) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LAUNCHD_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>daemon</string>
        <string>--serve</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        exe = exe.display(),
        log = env.daemon_log().display(),
    )
}

/// Writes the launchd agent and loads it.
///
/// Idempotent: an existing agent is replaced, because the common reason to run
/// this twice is that the binary moved.
pub fn install(env: &Environment) -> Result<String, AppError> {
    if !cfg!(target_os = "macos") {
        return Err(AppError::Usage(
            "`story daemon install` registers a launchd agent, which is macOS only. \
             On other systems, run `story daemon start` from your login shell or write \
             a unit for your own service manager."
                .to_string(),
        ));
    }
    let exe = std::env::current_exe()
        .map_err(|e| AppError::Storage(format!("failed to find the running executable: {e}")))?;
    let path = agent_path(env);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, agent_plist(&exe, env))
        .map_err(|e| AppError::Storage(format!("failed to write {}: {e}", path.display())))?;

    // `bootout` first so a reinstall replaces rather than conflicts; its failure
    // is expected when nothing was loaded, so it is ignored.
    let target = format!("gui/{}", user_id());
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &format!("{target}/{LAUNCHD_LABEL}")])
        .output();
    let loaded = std::process::Command::new("launchctl")
        .args(["bootstrap", &target, &path.to_string_lossy()])
        .output()
        .map_err(|e| AppError::Storage(format!("failed to run launchctl: {e}")))?;
    if !loaded.status.success() {
        return Err(AppError::Storage(format!(
            "launchctl refused to load {}: {}",
            path.display(),
            String::from_utf8_lossy(&loaded.stderr).trim()
        )));
    }
    Ok(format!(
        "installed the storyhook daemon as a launchd agent ({LAUNCHD_LABEL})\n  {}",
        path.display()
    ))
}

/// Unloads the launchd agent and removes its plist.
pub fn uninstall(env: &Environment) -> Result<String, AppError> {
    let path = agent_path(env);
    if !path.exists() {
        return Ok("the storyhook daemon is not installed as a launchd agent".to_string());
    }
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &format!("gui/{}/{LAUNCHD_LABEL}", user_id())])
        .output();
    std::fs::remove_file(&path)
        .map_err(|e| AppError::Storage(format!("failed to remove {}: {e}", path.display())))?;
    Ok(format!(
        "removed the storyhook daemon's launchd agent\n  {}",
        path.display()
    ))
}

/// This process's user id, which names the launchd domain to load into.
fn user_id() -> u32 {
    // SAFETY: `getuid` takes no arguments, cannot fail, and cannot be made to
    // touch memory this process does not own.
    #[cfg(unix)]
    unsafe {
        libc::getuid()
    }
    #[cfg(not(unix))]
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("storyhook-daemon-commands-")
            .tempdir_in("/private/tmp")
            .expect("a scratch directory")
    }

    #[test]
    fn status_reports_nothing_running_and_says_where_it_looked() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let reported = status(&env).expect("status");
        assert!(reported.contains("not running"), "{reported}");
        assert!(
            reported.contains(&env.daemon_file().display().to_string()),
            "a status that does not say where it looked is unactionable: {reported}"
        );
    }

    #[test]
    fn stopping_nothing_says_so_rather_than_failing() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        assert!(stop(&env).expect("stop").contains("not running"));
    }

    #[test]
    fn the_agent_runs_the_daemon_and_never_resurrects_it() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let plist = agent_plist(std::path::Path::new("/usr/local/bin/story"), &env);
        assert!(plist.contains("<string>daemon</string>"));
        assert!(plist.contains("<string>--serve</string>"));
        assert!(plist.contains(LAUNCHD_LABEL));
        assert!(
            !plist.contains("KeepAlive"),
            "an agent that restarts the daemon immediately races the client that \
             just asked it to stop for a version upgrade"
        );
    }

    #[test]
    fn the_agent_label_follows_the_bundle_convention() {
        assert_eq!(LAUNCHD_LABEL, "io.mikey.storyhook.daemon");
    }

    #[test]
    fn uninstalling_what_was_never_installed_is_not_an_error() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        assert!(
            uninstall(&env)
                .expect("uninstall")
                .contains("not installed")
        );
    }
}
