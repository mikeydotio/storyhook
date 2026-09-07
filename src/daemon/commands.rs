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

use crate::daemon::agent;
use crate::daemon::install_guard;
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::daemon::lifecycle::{self, DaemonInfo};
use crate::env::Environment;
use crate::error::AppError;

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
    let pidfile_pid = (force && lifecycle::is_live(env))
        .then(|| lifecycle::read_daemon_identity(env).map(|identity| identity.pid))
        .flatten();
    let mode = if force {
        lifecycle::StopMode::Force
    } else {
        lifecycle::StopMode::Graceful
    };
    match lifecycle::stop(env, mode)? {
        Some(info) => Ok(format!("storyhook daemon stopped (PID {})", info.pid)),
        None => Ok(pidfile_pid.map_or_else(
            || "storyhook daemon is not running".to_string(),
            |pid| format!("storyhook daemon stopped (PID {pid})"),
        )),
    }
}

/// Reports whether a daemon is running, and what it is.
///
/// The login agent is described in **every** branch, the not-running one
/// included — that branch is precisely where a deleted or misdirected agent
/// exe is the likeliest cause of what the reader came to ask about, and
/// "there is no daemon, and the agent that should have started one names a
/// binary that is gone" is the answer it owes. [`agent::describe`] renders
/// every state, including the ones [`agent::warning`] stays quiet about: a
/// reader who came to look is owed the whole answer.
pub fn status(env: &Environment) -> Result<String, AppError> {
    if !lifecycle::is_live(env) {
        return Ok(format!(
            "storyhook daemon is not running\n\n{}\n{}\n{}\n{}",
            lifecycle::describe_paths(env),
            crate::daemon::backup::describe(env),
            crate::daemon::backup::describe_maintenance(env),
            agent::report(env)
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
                "storyhook daemon {} running at {} (PID {}){}\n\n{}\n{}\n{}\n{}",
                info.version,
                info.dashboard_url(),
                info.pid,
                staleness,
                lifecycle::describe_paths(env),
                crate::daemon::backup::describe(env),
                crate::daemon::backup::describe_maintenance(env),
                agent::report(env)
            ))
        }
        // The lock is held by something that published nothing. Say so plainly
        // rather than reporting "not running", which would be false.
        None => Ok(format!(
            "a storyhook daemon holds the pidfile but published no portfile\n\n{}\n{}",
            lifecycle::describe_paths(env),
            agent::report(env)
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

/// What an install would do, once the gate has permitted it.
///
/// Separated from [`apply`] so the decision is a pure function of the
/// environment: a test can provoke a refusal and then assert that **no file
/// exists**, which is the only observable that distinguishes a gate placed
/// ahead of every side effect from one placed a line too low. Both produce the
/// same exit code and the same message.
///
/// `label` travels with the plan rather than being re-derived by `apply` and
/// `bootstrap_via_launchctl` separately, so the bootout target, the plist
/// bytes, and the success message are three uses of one fact (SH-136) instead
/// of three derivations that could drift.
#[derive(Debug)]
struct Plan {
    label: String,
    path: PathBuf,
    contents: String,
}

/// What a login-agent install or uninstall did, including non-fatal caveats.
///
/// Warnings remain separate from the success message so JSON callers receive
/// them as data through [`crate::output::Response::MessageWithWarnings`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoginAgentReport {
    message: String,
    warnings: Vec<String>,
}

impl LoginAgentReport {
    pub(crate) fn new(message: String, warning: Option<String>) -> Self {
        Self {
            message,
            warnings: warning.into_iter().collect(),
        }
    }

    /// The successful operation summary.
    #[must_use]
    pub fn message(&self) -> String {
        self.message.clone()
    }

    /// Non-fatal failures encountered while completing the operation.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        self.warnings.clone()
    }
}

/// Writes the launchd agent and loads it.
///
/// Idempotent: an existing agent is replaced, because the common reason to run
/// this twice is that the binary moved.
///
/// `this_binary` carries `--this-binary` — see [`install_guard`] for what it
/// overrides and, deliberately, what it does not.
///
/// # Errors
///
/// [`AppError::Usage`] when [`install_guard::decide`] refuses, when a
/// temporary store would leave a durable agent behind, or off macOS.
/// [`AppError::Storage`] when the plist cannot be written or `launchctl`
/// refuses it.
pub fn install(env: &Environment, this_binary: bool) -> Result<LoginAgentReport, AppError> {
    if !cfg!(target_os = "macos") {
        return Err(AppError::Usage(
            "`story daemon install` registers a launchd agent, which is macOS only. \
             On other systems, run `story daemon start` from your login shell or write \
             a unit for your own service manager."
                .to_string(),
        ));
    }
    let running = crate::path_identity::running_exe()
        .ok_or_else(|| AppError::Storage("failed to find the running executable".to_string()))?;
    let inputs = install_guard::gather(user_id(), this_binary, running);
    let mut report = apply(&install_plan(env, &inputs)?, &bootstrap_via_launchctl)?;
    // Named at the moment a machine grows past one store, not only when
    // someone happens to run `status` later.
    let others = agent::describe_others(env);
    if !others.is_empty() {
        report.message = format!("{}\n\n{others}", report.message);
    }
    Ok(report)
}

/// The gate, then the bytes. No side effects.
///
/// # Errors
///
/// [`AppError::Usage`] when the executable gate refuses, or when a temporary
/// store would leave a durable login agent behind.
fn install_plan(env: &Environment, inputs: &install_guard::Inputs) -> Result<Plan, AppError> {
    let verdict =
        install_guard::decide(inputs).map_err(|refusal| AppError::Usage(refusal.to_string()))?;
    let path = agent::path(env);
    refuse_temporary_store_for_durable_agent(env.store_path(), &path)?;
    Ok(Plan {
        label: agent::label(env),
        path,
        contents: agent::plist(&verdict.enthrone, env),
    })
}

/// Refuses a login agent that can outlive the store it serves (SH-426).
///
/// The mismatch is the defect, not either temporary path by itself. The test
/// harness roots both its store and fake `~/Library/LaunchAgents` under
/// `/private/tmp`; those artifacts disappear together and are safe. A plist
/// in a durable home that names a temporary store instead survives after the
/// operating system reclaims that store, so launchd retries a defunct daemon
/// at every later login until somebody uninstalls it by hand.
///
/// Kept separate from [`install_guard`]: that module decides which executable
/// may receive a permanent seat on the machine, while this function decides
/// whether the seat itself can outlive the store. The executable decision runs
/// first, preserving its root and path-identity refusal precedence.
fn refuse_temporary_store_for_durable_agent(
    store_path: &Path,
    agent_path: &Path,
) -> Result<(), AppError> {
    if !crate::service::project::is_under_temp(store_path)
        || crate::service::project::is_under_temp(agent_path)
    {
        return Ok(());
    }
    Err(AppError::Usage(format!(
        "refusing to register a durable login agent at `{}` for the temporary store at `{}`.\n\n\
         Nothing has been written. The operating system may reclaim that store while the agent \
         remains, causing launchd to retry a defunct daemon at every later login. Move the store \
         to a durable location and run `story daemon install` again. To run this store only for \
         the current session, use `story --store-path {} daemon start`.",
        agent_path.display(),
        store_path.display(),
        store_path.display(),
    )))
}

/// Writes the plist and hands it to launchd.
///
/// `load` is a parameter so both failure paths below are testable without
/// bootstrapping anything into the developer's own login session — a fixture
/// that ran real `launchctl` would register an agent pointing at the test
/// binary, under a label this project owns. It takes the whole [`Plan`],
/// not just the path, because `bootstrap_via_launchctl` needs the label to
/// boot out the *right* agent — never always the default store's.
fn apply(
    plan: &Plan,
    load: &dyn Fn(&Plan) -> Result<Option<String>, AppError>,
) -> Result<LoginAgentReport, AppError> {
    if let Some(parent) = plan.path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Read before writing: `bootout` below unloads whatever is there, so a
    // failure past this point has already cost the operator their working
    // agent. See `undo`.
    let previous = std::fs::read(&plan.path).ok();
    std::fs::write(&plan.path, &plan.contents)
        .map_err(|e| AppError::Storage(format!("failed to write {}: {e}", plan.path.display())))?;
    match load(plan) {
        Ok(warning) => Ok(LoginAgentReport::new(
            format!(
                "installed the storyhook daemon as a launchd agent ({})\n  {}",
                plan.label,
                plan.path.display()
            ),
            warning,
        )),
        Err(failure) => Err(undo(&plan.path, previous.as_deref(), failure)),
    }
}

/// `bootout` the old agent, then `bootstrap` the new one.
///
/// `bootout` first so a reinstall replaces rather than conflicts. A missing
/// service is expected; every other refusal is returned as a warning while
/// bootstrap still gets its chance. Both operations target `plan`'s own label
/// — never [`agent::LAUNCHD_LABEL`] directly — so a non-default store's install
/// can never boot out the default store's running agent.
fn bootstrap_via_launchctl(plan: &Plan) -> Result<Option<String>, AppError> {
    bootstrap_with_launchctl(plan, &|args| {
        std::process::Command::new("launchctl").args(args).output()
    })
}

fn bootstrap_with_launchctl(
    plan: &Plan,
    launchctl: &dyn Fn(&[&str]) -> std::io::Result<Output>,
) -> Result<Option<String>, AppError> {
    let target = format!("gui/{}", user_id());
    let service_target = format!("{target}/{}", plan.label);
    let warning = bootout_warning(&service_target, launchctl(&["bootout", &service_target]));
    let path = plan.path.to_string_lossy();
    let loaded = launchctl(&["bootstrap", &target, &path]).map_err(|e| {
        AppError::Storage(with_prior_bootout(
            format!("failed to run launchctl: {e}"),
            warning.as_deref(),
        ))
    })?;
    if loaded.status.success() {
        return Ok(warning);
    }
    Err(AppError::Storage(with_prior_bootout(
        format!(
            "launchctl refused to load {}: {}",
            plan.path.display(),
            String::from_utf8_lossy(&loaded.stderr).trim()
        ),
        warning.as_deref(),
    )))
}

fn with_prior_bootout(message: String, warning: Option<&str>) -> String {
    match warning {
        Some(warning) => format!("{message}\n\nBefore this failure, {warning}"),
        None => message,
    }
}

/// Puts the machine back the way it was after `launchctl` refused, and says
/// which way that was.
///
/// **Restoring rather than deleting is the point.** By the time `load` fails,
/// the new plist is already on disk and `bootout` has already unloaded whatever
/// was running — and `RunAtLoad` means launchd honours a plist in
/// `~/Library/LaunchAgents` at the next login whether or not anything
/// bootstrapped it. So leaving the new file behind makes a *failed* install a
/// durable one, and plain removal silently uninstalls a previously working
/// agent nobody asked to lose. Errors travel with context; this one names what
/// happened to the file.
fn undo(path: &std::path::Path, previous: Option<&[u8]>, failure: AppError) -> AppError {
    let note = match previous {
        Some(bytes) => match std::fs::write(path, bytes) {
            Ok(()) => "\n\nThe agent that was there before has been put back, but launchd has \
                       already unloaded it — run `story daemon install` again once launchctl is \
                       happy."
                .to_string(),
            Err(e) => format!(
                "\n\nThe agent that was there before could NOT be put back ({e}); {} now holds \
                 the new plist, which launchd will honour at the next login.",
                path.display()
            ),
        },
        None => match std::fs::remove_file(path) {
            Ok(()) => "\n\nNothing was left behind.".to_string(),
            Err(e) => format!(
                "\n\nThe plist could not be removed ({e}); {} will be honoured at the next \
                 login even though launchctl refused it now.",
                path.display()
            ),
        },
    };
    match failure {
        AppError::Storage(message) => AppError::Storage(format!("{message}{note}")),
        other => other,
    }
}

/// Unloads the launchd agent and removes its plist.
pub fn uninstall(env: &Environment) -> Result<LoginAgentReport, AppError> {
    uninstall_with(env, &bootout_via_launchctl)
}

/// `unload` is a parameter for the same reason [`apply`]'s `load` is: a
/// fixture that ran real `launchctl` would touch the developer's own login
/// session.
///
/// Reads the plist before removing it — the reader already exists
/// ([`agent::health`]) — so a pre-SH-414 leftover (a bare-label agent
/// actually serving a different store) is named rather than silently
/// discarded under a report that only ever says "removed".
fn uninstall_with(
    env: &Environment,
    unload: &dyn Fn(&str) -> Option<String>,
) -> Result<LoginAgentReport, AppError> {
    let path = agent::path(env);
    if !path.exists() {
        return Ok(LoginAgentReport::new(
            "the storyhook daemon is not installed as a launchd agent".to_string(),
            None,
        ));
    }
    let note = match agent::health(env) {
        agent::Health::ServesAnotherStore { serves, .. } => format!(
            "\n  note: that agent was actually serving {} — if it still needs its own \
             login agent, run `story --store-path {} daemon install`",
            serves.display(),
            serves.display()
        ),
        _ => String::new(),
    };
    let label = agent::label(env);
    let warning = unload(&label);
    std::fs::remove_file(&path).map_err(|e| {
        AppError::Storage(with_prior_bootout(
            format!("failed to remove {}: {e}", path.display()),
            warning.as_deref(),
        ))
    })?;
    let others = agent::describe_others(env);
    let others = if others.is_empty() {
        String::new()
    } else {
        format!("\n\n{others}")
    };
    Ok(LoginAgentReport::new(
        format!(
            "removed the storyhook daemon's launchd agent\n  {}{}{}",
            path.display(),
            note,
            others
        ),
        warning,
    ))
}

/// `bootout` this store's own agent — never always the default store's.
fn bootout_via_launchctl(label: &str) -> Option<String> {
    let target = format!("gui/{}/{label}", user_id());
    bootout_warning(
        &target,
        std::process::Command::new("launchctl")
            .args(["bootout", &target])
            .output(),
    )
}

/// launchctl maps its BOOTSTRAP_UNKNOWN_SERVICE result to process status 113.
const LAUNCHCTL_SERVICE_NOT_FOUND: i32 = 113;

fn bootout_warning(target: &str, result: std::io::Result<Output>) -> Option<String> {
    let output = match result {
        Ok(output)
            if output.status.success()
                || output.status.code() == Some(LAUNCHCTL_SERVICE_NOT_FOUND) =>
        {
            return None;
        }
        Ok(output) => output,
        Err(error) => {
            return Some(format!(
                "failed to run `launchctl bootout {target}`: {error}; the login agent may still be loaded"
            ));
        }
    };
    let status = describe_failed_status(&output.status);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let diagnostic = match (stderr.trim(), stdout.trim()) {
        ("", "") => "no diagnostic output",
        ("", stdout) => stdout,
        (stderr, _) => stderr,
    };
    Some(format!(
        "`launchctl bootout {target}` failed ({status}): {diagnostic}; the login agent may still be loaded"
    ))
}

fn describe_failed_status(status: &std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("status {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("signal {signal}");
        }
    }
    "no exit status".to_string()
}

/// This process's user id, which names the launchd domain to load into.
///
/// Also what [`install_guard`] decides root on, so the guard and the domain the
/// install targets are two readings of one fact rather than two facts (SH-136).
pub fn user_id() -> u32 {
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

    fn shell_output(script: &str) -> std::process::Output {
        std::process::Command::new("/bin/sh")
            .args(["-c", script])
            .output()
            .expect("running the inert launchctl-output fixture")
    }

    #[test]
    fn a_successful_or_absent_bootout_needs_no_warning() {
        let target = "gui/501/io.mikey.storyhook.daemon";
        assert_eq!(bootout_warning(target, Ok(shell_output("exit 0"))), None);
        assert_eq!(
            bootout_warning(target, Ok(shell_output("exit 113"))),
            None,
            "launchctl 113 means the service was not loaded, which is expected"
        );
    }

    #[test]
    fn a_refused_bootout_preserves_its_target_status_and_diagnostic() {
        let target = "gui/501/io.mikey.storyhook.daemon";
        let warning = bootout_warning(
            target,
            Ok(shell_output(
                "printf 'operation not permitted' >&2; exit 77",
            )),
        )
        .expect("a real refusal must be reported");

        assert!(warning.contains(target), "{warning}");
        assert!(warning.contains("77"), "{warning}");
        assert!(warning.contains("operation not permitted"), "{warning}");
    }

    #[test]
    fn an_unspawnable_bootout_preserves_its_target_and_io_error() {
        let target = "gui/501/io.mikey.storyhook.daemon";
        let warning = bootout_warning(
            target,
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "launchctl vanished",
            )),
        )
        .expect("a spawn failure must be reported");

        assert!(warning.contains(target), "{warning}");
        assert!(warning.contains("launchctl vanished"), "{warning}");
    }

    #[cfg(unix)]
    #[test]
    fn a_signalled_bootout_is_not_mistaken_for_an_absent_service() {
        let target = "gui/501/io.mikey.storyhook.daemon";
        let warning = bootout_warning(target, Ok(shell_output("kill -TERM $$")))
            .expect("signal termination must be reported");

        assert!(warning.contains(target), "{warning}");
        assert!(warning.contains("signal"), "{warning}");
    }

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

    fn permitting_inputs() -> install_guard::Inputs {
        let story = crate::path_identity::InstalledStory {
            spelling: PathBuf::from("/home/dev/.local/bin/story"),
            canonical: PathBuf::from("/home/dev/.local/bin/story"),
        };
        install_guard::Inputs {
            uid: 501,
            running: story.clone(),
            installed_story: Some(story),
            this_binary: false,
        }
    }

    fn refusing_inputs() -> install_guard::Inputs {
        let mut inputs = permitting_inputs();
        inputs.running = crate::path_identity::InstalledStory {
            spelling: PathBuf::from("/home/dev/repo/target/debug/story"),
            canonical: PathBuf::from("/home/dev/repo/target/debug/story"),
        };
        inputs
    }

    /// **The test this fix would ship broken without.**
    ///
    /// A gate placed one line *below* `fs::write` is indistinguishable from a
    /// correct one in the exit code, in the printed message, and in every row
    /// of `install_guard::decide`'s truth table. The only observable that
    /// separates them is whether the file is absent afterwards — and it has to
    /// be absent, because `RunAtLoad` means launchd honours a plist sitting in
    /// `~/Library/LaunchAgents` at the next login whether or not anything ever
    /// bootstrapped it. A refusal that left the file behind would be a
    /// successful install wearing an error message.
    ///
    /// `load` panics rather than returning an error, so this also proves
    /// `launchctl` is never reached — which is what makes the test safe to run
    /// here at all, under the one launchd label this project owns.
    #[test]
    fn a_refused_install_writes_no_plist_and_never_reaches_launchctl() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let refused = install_plan(&env, &refusing_inputs()).expect_err("must refuse");
        assert!(matches!(refused, AppError::Usage(_)), "{refused:?}");
        assert!(
            !agent::path(&env).exists(),
            "a refusal must leave no plist: RunAtLoad honours one at the next login \
             whether or not launchctl ever loaded it"
        );
    }

    /// The control. Without it, the test above passes equally well for a
    /// command that refuses everything.
    #[test]
    fn a_permitted_install_writes_the_plist_and_hands_launchd_that_path() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let handed = std::cell::RefCell::new(None);
        let plan = install_plan(&env, &permitting_inputs()).expect("must permit");
        apply(&plan, &|plan| {
            *handed.borrow_mut() = Some(plan.path.clone());
            Ok(None)
        })
        .expect("install");
        assert!(agent::path(&env).exists());
        assert_eq!(
            handed.into_inner().as_deref(),
            Some(agent::path(&env).as_path())
        );
        let written = std::fs::read_to_string(agent::path(&env)).expect("the plist");
        assert_eq!(
            agent::registered_exe(&written),
            Some(PathBuf::from("/home/dev/.local/bin/story")),
            "the plist must name the path the gate enthroned"
        );
    }

    #[test]
    fn a_bootout_warning_survives_a_successful_install() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let plan = install_plan(&env, &permitting_inputs()).expect("must permit");
        let report = apply(&plan, &|_| {
            Ok(Some("launchctl refused the bootout".to_string()))
        })
        .expect("bootstrap still succeeded");

        assert!(report.message().contains("installed"));
        assert_eq!(
            report.warnings(),
            vec!["launchctl refused the bootout".to_string()]
        );
        assert!(agent::path(&env).exists());
    }

    #[test]
    fn a_failed_bootstrap_keeps_the_earlier_bootout_diagnostic() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let plan = install_plan(&env, &permitting_inputs()).expect("must permit");
        let failed = bootstrap_with_launchctl(&plan, &|args| match args[0] {
            "bootout" => Ok(shell_output("printf 'permission denied' >&2; exit 77")),
            "bootstrap" => Ok(shell_output("printf 'bad plist' >&2; exit 78")),
            other => panic!("unexpected launchctl action: {other}"),
        })
        .expect_err("bootstrap must fail");
        let message = failed.to_string();

        assert!(message.contains("bad plist"), "{message}");
        assert!(message.contains("permission denied"), "{message}");
        assert!(message.contains("bootout"), "{message}");
    }

    /// A failed `bootstrap` must not leave the machine holding a plist launchd
    /// refused — `RunAtLoad` would honour it at the next login regardless.
    #[test]
    fn a_failed_bootstrap_leaves_no_plist_behind() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let plan = install_plan(&env, &permitting_inputs()).expect("must permit");
        let failed = apply(&plan, &|_| {
            Err(AppError::Storage(
                "launchctl refused to load it".to_string(),
            ))
        })
        .expect_err("must fail");
        assert!(failed.to_string().contains("launchctl"), "{failed}");
        assert!(!agent::path(&env).exists());
        assert!(
            failed.to_string().contains("Nothing was left behind"),
            "the error must say what happened to the file: {failed}"
        );
    }

    /// And it must not *delete* one either. By the time the load fails,
    /// `bootout` has already unloaded whatever was running, so removing the
    /// file would silently uninstall a working agent nobody asked to lose.
    #[test]
    fn a_failed_bootstrap_restores_the_agent_it_replaced() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let path = agent::path(&env);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("the LaunchAgents dir");
        std::fs::write(&path, b"the agent that was already working").expect("seeding");

        let plan = install_plan(&env, &permitting_inputs()).expect("must permit");
        let failed = apply(&plan, &|_| {
            Err(AppError::Storage(
                "launchctl refused to load it".to_string(),
            ))
        })
        .expect_err("must fail");

        assert_eq!(
            std::fs::read(&path).expect("the plist"),
            b"the agent that was already working",
            "a launchctl hiccup must not become an uninstall nobody asked for"
        );
        assert!(
            failed.to_string().contains("put back"),
            "and the error must say so, rather than reporting only the load failure: {failed}"
        );
    }

    #[test]
    fn uninstalling_what_was_never_installed_is_not_an_error() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        assert!(
            uninstall(&env)
                .expect("uninstall")
                .message()
                .contains("not installed")
        );
    }

    fn named_store_env(dir: &std::path::Path, file_name: &str) -> Environment {
        let named = dir.join(file_name);
        Environment::at(dir).with_store(
            crate::env::StoreLocation::resolve(
                Some(&named),
                &crate::env::StoreVars::default(),
                dir,
            )
            .expect("resolving a named store"),
        )
    }

    /// SH-426: a store the operating system may reclaim must not leave a
    /// login agent in a durable home. `--this-binary` answers which executable
    /// to enthrone; it must not override the independent lifetime mismatch.
    #[test]
    fn a_temporary_store_cannot_create_a_durable_agent_even_with_this_binary() {
        let temporary = scratch();
        let home = storyhook_test_support::non_temporary_dir("sh426-durable-agent-home");
        let env = named_store_env(&home, &temporary.path().join("store.db").to_string_lossy());
        let mut inputs = permitting_inputs();
        inputs.this_binary = true;

        let refused = install_plan(&env, &inputs).expect_err("must refuse");

        assert!(matches!(refused, AppError::Usage(_)), "{refused:?}");
        assert!(refused.to_string().contains("temporary"), "{refused}");
        assert!(
            refused
                .to_string()
                .contains(&env.store_path().display().to_string()),
            "the refusal must name the temporary store: {refused}"
        );
        assert!(
            refused
                .to_string()
                .contains(&agent::path(&env).display().to_string()),
            "the refusal must name the durable plist: {refused}"
        );
        assert!(
            !agent::path(&env).exists(),
            "the lifetime guard must run before the plist is written"
        );
    }

    /// The test harness deliberately puts both sides under `/private/tmp`.
    /// That pair is self-cleaning, so refusing it would disable the tests that
    /// prove installation without protecting a durable machine artifact.
    #[test]
    fn a_temporary_store_may_create_a_temporary_agent() {
        let dir = scratch();
        let env = named_store_env(dir.path(), "named.db");

        install_plan(&env, &permitting_inputs()).expect("both artifacts are temporary");
    }

    /// The positive production case: per-store agents remain supported when
    /// both the store and its plist have durable locations.
    #[test]
    fn a_durable_named_store_may_create_a_durable_agent() {
        let home = storyhook_test_support::non_temporary_dir("sh426-durable-store-home");
        let env = named_store_env(&home, "named.db");

        install_plan(&env, &permitting_inputs()).expect("both artifacts are durable");
    }

    /// **The defect this story exists to fix.** Before it: `agent::path`
    /// ignored the store, so installing for a second store overwrote the
    /// first store's plist with no notice. Never touches the real
    /// `~/Library/LaunchAgents` or real `launchctl` — `Environment::at`
    /// fakes `$HOME`, and `apply`'s `load` is an injected closure.
    #[test]
    fn installing_for_a_second_store_leaves_the_first_stores_agent_intact() {
        let dir = scratch();
        let env_a = Environment::at(dir.path());
        let plan_a = install_plan(&env_a, &permitting_inputs()).expect("must permit");
        apply(&plan_a, &|_| Ok(None)).expect("install a");
        let bytes_a = std::fs::read(agent::path(&env_a)).expect("plist a");

        let env_b = named_store_env(dir.path(), "b.db");
        let plan_b = install_plan(&env_b, &permitting_inputs()).expect("must permit");
        apply(&plan_b, &|_| Ok(None)).expect("install b");

        assert_ne!(
            agent::path(&env_a),
            agent::path(&env_b),
            "the two stores must not share a plist path"
        );
        assert_eq!(
            std::fs::read(agent::path(&env_a)).expect("plist a still there"),
            bytes_a,
            "installing for store b must not touch store a's plist"
        );
        assert!(agent::path(&env_b).exists());
    }

    /// The launchctl-only version of the same defect: a `bootstrap_via_
    /// launchctl` that kept the label hardcoded would write store b's plist
    /// correctly while unloading store a's *running* agent. `load` here
    /// records what it was handed rather than touching real `launchctl`.
    #[test]
    fn installing_for_a_second_store_never_targets_the_default_label() {
        let dir = scratch();
        let env_b = named_store_env(dir.path(), "b.db");
        let plan_b = install_plan(&env_b, &permitting_inputs()).expect("must permit");
        let handed = std::cell::RefCell::new(None);
        apply(&plan_b, &|plan| {
            *handed.borrow_mut() = Some(plan.label.clone());
            Ok(None)
        })
        .expect("install b");
        assert_eq!(handed.into_inner(), Some(plan_b.label.clone()));
        assert_ne!(plan_b.label, agent::LAUNCHD_LABEL);
    }

    /// The mirror of the regression test above, for `uninstall`: removing a
    /// named store's agent must not touch the default store's.
    #[test]
    fn uninstalling_from_a_named_store_leaves_the_default_stores_plist_alone() {
        let dir = scratch();
        let env_a = Environment::at(dir.path());
        let plan_a = install_plan(&env_a, &permitting_inputs()).expect("must permit");
        apply(&plan_a, &|_| Ok(None)).expect("install a");
        let bytes_a = std::fs::read(agent::path(&env_a)).expect("plist a");

        let env_b = named_store_env(dir.path(), "b.db");
        let plan_b = install_plan(&env_b, &permitting_inputs()).expect("must permit");
        apply(&plan_b, &|_| Ok(None)).expect("install b");

        uninstall_with(&env_b, &|_| None).expect("uninstall b");

        assert!(
            !agent::path(&env_b).exists(),
            "store b's plist must be gone"
        );
        assert!(agent::path(&env_a).exists(), "store a's plist must survive");
        assert_eq!(
            std::fs::read(agent::path(&env_a)).expect("plist a still there"),
            bytes_a
        );
    }

    #[test]
    fn a_bootout_warning_survives_uninstall_while_the_plist_is_removed() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let path = agent::path(&env);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("creating parent");
        std::fs::write(&path, "a plist").expect("seeding the plist");

        let report = uninstall_with(&env, &|_| Some("launchctl refused the bootout".to_string()))
            .expect("uninstall");

        assert!(report.message().contains("removed"));
        assert_eq!(
            report.warnings(),
            vec!["launchctl refused the bootout".to_string()]
        );
        assert!(!path.exists(), "the plist removal remains successful");
    }

    #[test]
    fn a_failed_plist_removal_keeps_the_earlier_bootout_diagnostic() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let path = agent::path(&env);
        std::fs::create_dir_all(&path).expect("making the plist path unremovable as a file");

        let failed = uninstall_with(&env, &|_| Some("launchctl refused the bootout".to_string()))
            .expect_err("removing a directory as a file must fail");
        let message = failed.to_string();

        assert!(message.contains("failed to remove"), "{message}");
        assert!(
            message.contains("launchctl refused the bootout"),
            "{message}"
        );
    }

    #[test]
    fn uninstalling_without_a_plist_never_reaches_launchctl_or_warns() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let called = std::cell::Cell::new(false);

        let report = uninstall_with(&env, &|_| {
            called.set(true);
            Some("must not exist".to_string())
        })
        .expect("uninstall");

        assert!(!called.get(), "an absent plist needs no bootout");
        assert!(report.warnings().is_empty());
        assert!(report.message().contains("not installed"));
    }

    /// The migration seam a council found this fix must handle explicitly:
    /// a pre-SH-414 `--store-path X daemon install` left X's agent at the
    /// bare label. `story daemon uninstall` for the default store must say
    /// which store it was actually serving, not just "removed".
    #[test]
    fn uninstalling_a_pre_fix_leftover_names_the_store_it_actually_served() {
        let dir = scratch();
        let env_a = Environment::at(dir.path());
        let other = dir.path().join("other.db");
        let path = agent::path(&env_a);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("the LaunchAgents dir");
        std::fs::write(
            &path,
            format!(
                "<key>ProgramArguments</key><array><string>/usr/local/bin/story</string>\
                 <string>--store-path</string><string>{}</string></array>",
                other.display()
            ),
        )
        .expect("planting a pre-fix leftover");

        let reported = uninstall_with(&env_a, &|_| None).expect("uninstall");
        let reported = reported.message();
        assert!(reported.contains("removed"), "{reported}");
        assert!(
            reported.contains(&other.display().to_string()),
            "the report must name the store the removed agent actually served: {reported}"
        );
        assert!(!path.exists());
    }
}
