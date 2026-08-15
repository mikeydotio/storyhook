//! `story daemon …` — the commands *about* the daemon, as opposed to the daemon.
//!
//! Starting, stopping, reporting, and registering it with the system's service
//! manager so it comes back at login. The `story web start|stop|status` family
//! are aliases for the first three; they kept their wording, because scripts
//! read it, and gained a line on stderr saying where they moved.
//!
//! [`status`] is also where the backups — both the daily schedule and the
//! unpruned maintenance/hand-taken directory `story store backup` writes to
//! (SH-135) — are reported. The plan said `story doctor`, and this is a
//! deliberate departure: `doctor`'s output is pinned byte-for-byte by the
//! golden corpus, and its exit code *means* something — a project's
//! integrity. How old a copy of the database is a fact about the machine, not
//! about the project, and adding it to `doctor` would either move bytes the
//! whole rearchitecture is measured against or turn "this machine has not run
//! a daemon lately" into an integrity failure.

use std::path::PathBuf;

use crate::daemon::lifecycle::{self, DaemonInfo};
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
        Some(port) => env.clone().daemon_port(port),
        None => env.clone(),
    };
    lifecycle::ensure(&env)
}

/// Notes, on stderr, that `info`'s tailnet bind is not known yet.
///
/// SH-186 moved the tailnet probe off the daemon's startup path onto a
/// background thread (`serve::tailnet_reprobe`), so a caller of [`start`]
/// that just spawned a fresh daemon gets back a `DaemonInfo` whose `tailnet`
/// is `None` regardless of whether this machine has one — the probe has not
/// had a chance to answer yet. `info.dashboard_url()` is still correct in
/// that instant (loopback is always true), but printing only that would let
/// a stale answer read as a confirmed one on a tailnet-connected machine.
/// `story daemon status`/`story daemon address`, run any time after, read
/// the same portfile fresh and report the tailnet host once
/// `tailnet_reprobe` lands it — this note exists only to say why the URL
/// just printed might not be the final one, not to make the caller wait.
///
/// Silent when `info.tailnet` is already known: a daemon that was already
/// running, and had already resolved its tailnet before this call, has
/// nothing pending to note.
pub fn note_tailnet_pending(info: &DaemonInfo) {
    if info.tailnet.is_none() {
        eprintln!(
            "note: resolving the tailnet address in the background; `story daemon status` \
             will show it once bound."
        );
    }
}

/// Stops the running daemon.
///
/// `force` selects [`lifecycle::StopMode::Force`]: a short grace period,
/// then a kill signal, abandoning whatever the daemon was still serving.
/// Without it, this waits however long an orderly drain takes — nothing is
/// abandoned, but there is no deadline of its own; `--force` is the escape
/// hatch for a daemon that is not draining.
pub fn stop(env: &Environment, force: bool) -> Result<String, AppError> {
    let mode = if force {
        lifecycle::StopMode::Force
    } else {
        lifecycle::StopMode::Graceful
    };
    match lifecycle::stop(env, mode)? {
        Some(info) => Ok(format!("storyhook daemon stopped (PID {})", info.pid)),
        None => Ok("storyhook daemon is not running".to_string()),
    }
}

/// Reports whether a daemon is running, and what it is.
pub fn status(env: &Environment) -> Result<String, AppError> {
    if !lifecycle::is_live(env) {
        return Ok(format!(
            "storyhook daemon is not running\n\n{}\n{}\n{}",
            lifecycle::describe_paths(env),
            crate::daemon::backup::describe(env),
            crate::daemon::backup::describe_maintenance(env)
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
                "storyhook daemon {} running at {} (PID {}){}\n\n{}\n{}\n{}",
                info.version,
                info.dashboard_url(),
                info.pid,
                staleness,
                lifecycle::describe_paths(env),
                crate::daemon::backup::describe(env),
                crate::daemon::backup::describe_maintenance(env)
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

/// Prints the running daemon's bearer token.
///
/// The token gates every `/api/v1/*` request (`src/api/rpc.rs`) and, since
/// SH-50, the dashboard's dispatch endpoint too — this is how an operator
/// gets a copy into the dashboard's token prompt, or a phone reaching the
/// dashboard over Tailscale, without reading the 0600 portfile by hand.
///
/// Refuses rather than starting a daemon: unlike `start`, this is a question
/// about a daemon presumably already serving the dashboard the token is for,
/// and silently starting a second one under a caller who only wanted to read
/// a value would be a surprising side effect for what looks like a query —
/// the same reasoning `status` already follows.
///
/// # Standard output is the token, and only the token (SH-250)
///
/// Every side effect this grew — the clipboard copy, the OSC 52 escape
/// sequence, the note saying what happened, and the rotation warning that
/// used to be printed on stdout — goes to **stderr**, and only when stderr is
/// a terminal. `story daemon token | pbcopy`, `TOKEN=$(story daemon token)`
/// and every script already written against this command keep working
/// unchanged, because what they read never had anything added to it.
///
/// The TTY test is on **stderr**, not stdout: stdout is very often a pipe
/// here (that is the command's whole shape), while stderr is what a person
/// watching would be reading. Testing stdout would switch the convenience off
/// in exactly the case it is most wanted — a human running
/// `story daemon token | pbcopy` on a machine they are sitting at.
pub fn token(env: &Environment) -> Result<String, AppError> {
    if !lifecycle::is_live(env) {
        return Err(AppError::Usage(format!(
            "storyhook daemon is not running — start one with `story daemon start` first.\n\n{}",
            lifecycle::describe_paths(env)
        )));
    }
    match lifecycle::read_info(env) {
        Some(info) => {
            offer_token_to_the_clipboard(&info.token);
            Ok(info.token)
        }
        None => Err(AppError::Storage(
            "a storyhook daemon holds the pidfile but published no portfile".to_string(),
        )),
    }
}

/// Copies `token` to the clipboard and says so, when there is a human there
/// to be told. A no-op otherwise.
///
/// The OSC 52 sequence is written to stderr rather than stdout for the reason
/// in [`token`]'s own doc: stdout carries a value callers parse. Stderr is
/// the same terminal device in every interactive session, so the escape
/// reaches the emulator either way — and when stderr is redirected instead,
/// the guard below has already declined to write anything.
fn offer_token_to_the_clipboard(token: &str) {
    use std::io::IsTerminal;
    if !std::io::stderr().is_terminal() {
        return;
    }
    let mut stderr = std::io::stderr();
    let copied = crate::clipboard::copy_everywhere(token, &mut stderr);
    match copied.describe() {
        Some(what) => eprintln!(
            "note: {what}. It rotates every time the daemon restarts — fetch a \
             fresh one after a `story daemon stop`/`start`."
        ),
        None => eprintln!(
            "note: could not reach a clipboard. The token rotates every time the \
             daemon restarts — fetch a fresh one after a `story daemon stop`/`start`."
        ),
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
{store}        <string>daemon</string>
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
        // A login agent for the default store needs no flag, and leaving it off
        // keeps the plist identical to the one every existing installation has.
        // For any other store the flag is required rather than tidy: the log
        // path below is already this store's, so an agent without it would run
        // one store's daemon while writing into another's directory.
        store = if env.store().is_default() {
            String::new()
        } else {
            format!(
                "        <string>--store-path</string>\n        <string>{}</string>\n",
                env.store_path().display()
            )
        },
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
        assert!(stop(&env, false).expect("stop").contains("not running"));
    }

    /// **`RunAtLoad` yes, `KeepAlive` no**, and both halves are decisions rather
    /// than defaults.
    ///
    /// It was worth re-taking once the daemon became mandatory (SH-114): an
    /// optional process that nothing restarts is a choice, and a *required* one
    /// that nothing restarts sounds like an oversight. It is not, and the
    /// reasons got stronger rather than weaker.
    ///
    /// - **Availability is already self-healing.** Every client calls
    ///   `lifecycle::ensure`, so a daemon that is not running is started by
    ///   whoever needs it next. A restarter would be a second mechanism for
    ///   something that has one.
    /// - **A restarter is now more dangerous than it was.** `spawn_locked`
    ///   stands the old daemon down and *then* claims the pidfile, so launchd
    ///   racing that window turns a deterministic failure into an intermittent
    ///   one — and since SH-114 that window is on every command's path, not on
    ///   the subset that chose the daemon.
    /// - **The version-upgrade race survives verbatim**: an agent that restarts
    ///   the daemon immediately races the client that just asked it to stop
    ///   because the binary moved.
    /// - **`KeepAlive{SuccessfulExit:false}`**, the surgical variant that looks
    ///   like it avoids all of the above, is the worst of them. It turns "the
    ///   store is damaged and the daemon cannot open it" — the exact scenario
    ///   the cannot-start diagnostic exists for — into a respawn loop on
    ///   launchd's ten-second throttle.
    #[test]
    fn the_agent_runs_the_daemon_at_login_and_never_resurrects_it() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let plist = agent_plist(std::path::Path::new("/usr/local/bin/story"), &env);
        assert!(plist.contains("<string>daemon</string>"));
        assert!(plist.contains("<string>--serve</string>"));
        assert!(plist.contains(LAUNCHD_LABEL));
        assert!(
            plist.contains("<key>RunAtLoad</key>"),
            "the daemon is started at login, so a machine that has just booted \
             has a dashboard without anybody typing a command: {plist}"
        );
        assert!(
            !plist.contains("KeepAlive"),
            "and it is never restarted. A restarter races `spawn_locked`'s \
             stand-down window on every command's path, and the surgical \
             `SuccessfulExit: false` variant turns a damaged store into a \
             respawn loop on launchd's ten-second throttle: {plist}"
        );
    }

    /// The agent's log path is already this store's. An agent that ran the
    /// *default* store's daemon while writing into a named store's directory
    /// would be describing one process and starting another.
    #[test]
    fn the_agent_names_a_store_that_is_not_the_default_one() {
        let dir = scratch();
        let named = dir.path().join("named.db");
        let env = Environment::at(dir.path());
        let default_plist = agent_plist(std::path::Path::new("/usr/local/bin/story"), &env);
        assert!(
            !default_plist.contains("--store-path"),
            "the default store's agent must stay byte-identical to the one every \
             existing installation has"
        );

        let env = env.with_store(
            crate::env::StoreLocation::resolve(
                Some(&named),
                &crate::env::StoreVars::default(),
                dir.path(),
            )
            .expect("resolving a named store"),
        );
        let plist = agent_plist(std::path::Path::new("/usr/local/bin/story"), &env);
        assert!(plist.contains("<string>--store-path</string>"), "{plist}");
        assert!(
            plist.contains(&format!("<string>{}</string>", env.store_path().display())),
            "{plist}"
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
