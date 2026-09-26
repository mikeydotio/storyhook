//! `launchctl`, in one place (SH-136, SH-784).
//!
//! Every direct invocation of the `launchctl` binary lives here: registering
//! an agent (`story daemon install`/`uninstall`, via [`run`]) and asking an
//! already-registered agent to actually run the daemon
//! ([`LaunchdLauncher`]). One low-level wrapper, function-pointer-injectable
//! wherever the logic above it needs testing, so nothing in this codebase
//! ever bootstraps a real agent into the developer's own login session —
//! `commands`'s own tests already avoid that, and this module is what lets
//! [`LaunchdLauncher`]'s tests do the same rather than restating the pattern.

use std::process::{Command, Output};

use crate::env::Environment;
use crate::error::AppError;

use super::lifecycle::{DaemonLauncher, DaemonOwner};

/// Runs `launchctl` with `args`, exactly as installed on `$PATH`. The one
/// `Command::new("launchctl")` site in this codebase (`tests/spawn_inventory.rs`
/// pins that).
pub(crate) fn run(args: &[&str]) -> std::io::Result<Output> {
    Command::new("launchctl").args(args).output()
}

/// `launchctl`'s exit status for "no such service" — reported identically by
/// `bootout` (a service that was never loaded) and by `kickstart` (a service
/// that is not loaded *yet*), so both readers here share one constant rather
/// than two copies of the same fact (SH-136).
pub(crate) const LAUNCHCTL_SERVICE_NOT_FOUND: i32 = 113;

/// Starts the daemon by asking launchd, never by forking (SH-784's approved
/// design). [`super::lifecycle::choose_launcher`] selects this whenever a
/// launchd agent is installed for the exact store being asked about.
pub(crate) struct LaunchdLauncher;

impl DaemonLauncher for LaunchdLauncher {
    fn launch(&self, env: &Environment) -> Result<DaemonOwner, AppError> {
        // The same invariant `spawn_child` keeps for the fork path: a
        // failure recorded here belongs to *this* attempt, so
        // `await_launchd_healthy` can trust whatever it finds afterward
        // instead of reporting an old cause with total confidence.
        let _ = std::fs::remove_file(env.daemon_failure());
        let label = super::agent::label(env);
        ensure_running(env, &label, &run)?;
        super::lifecycle::await_launchd_healthy(env)?;
        Ok(DaemonOwner::Launchd { label })
    }
}

/// `launchctl kickstart <target>`, bootstrapping first if the job is not yet
/// loaded, then retrying once.
///
/// **Never `-k`.** `kickstart -k` kills and restarts an already-running
/// service; every ordinary `ensure`/`start` call against a *healthy*
/// launchd-owned daemon would then be disruptive, which defeats the whole
/// point of routing starts through launchd instead of forking — there would
/// be no race-free "start if not running" left, only a route back to the
/// same kind of surprise restart SH-784 exists to remove. Plain `kickstart`
/// is a no-op when the service is already running, which is what makes
/// launchd the sole, race-free creator of `daemon --serve` processes for an
/// installed agent (the exact race that produced this story's `exit code 2`
/// evidence cannot recur once nothing else ever forks one).
///
/// `launchctl` is a parameter for the same reason
/// [`commands::bootstrap_with_launchctl`](super::commands)'s own is: a
/// fixture that ran the real binary would register an agent pointing at the
/// test binary into the developer's own login session, under the one label
/// this project owns.
fn ensure_running(
    env: &Environment,
    label: &str,
    launchctl: &dyn Fn(&[&str]) -> std::io::Result<Output>,
) -> Result<(), AppError> {
    let target = format!("gui/{}/{label}", super::commands::user_id());
    match launchctl(&["kickstart", &target]) {
        Ok(output) if output.status.success() => return Ok(()),
        Ok(output) if output.status.code() == Some(LAUNCHCTL_SERVICE_NOT_FOUND) => {
            bootstrap(env, launchctl)?;
        }
        Ok(output) => {
            return Err(AppError::Storage(format!(
                "launchctl refused to start {target}: {}",
                describe_output(&output)
            )));
        }
        Err(e) => return Err(AppError::Storage(format!("failed to run launchctl: {e}"))),
    }
    // Retried exactly once, immediately after a successful bootstrap: the
    // job is now loaded, so this attempt either starts it or reports a real
    // refusal — never silently falls back to a fork (the approved design's
    // "if the agent is installed but launchd refuses, the command fails
    // loudly").
    match launchctl(&["kickstart", &target]) {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(AppError::Storage(format!(
            "launchctl refused to start {target} after loading it: {}",
            describe_output(&output)
        ))),
        Err(e) => Err(AppError::Storage(format!("failed to run launchctl: {e}"))),
    }
}

/// `launchctl bootstrap gui/<uid> <plist>` for this store's own plist.
fn bootstrap(
    env: &Environment,
    launchctl: &dyn Fn(&[&str]) -> std::io::Result<Output>,
) -> Result<(), AppError> {
    let gui = format!("gui/{}", super::commands::user_id());
    let plist = super::agent::path(env);
    let path = plist.to_string_lossy();
    let loaded = launchctl(&["bootstrap", &gui, &path])
        .map_err(|e| AppError::Storage(format!("failed to run launchctl: {e}")))?;
    if loaded.status.success() {
        return Ok(());
    }
    Err(AppError::Storage(format!(
        "launchctl refused to load {}: {}",
        plist.display(),
        describe_output(&loaded)
    )))
}

fn describe_output(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    match (stderr.trim(), stdout.trim()) {
        ("", "") => "no diagnostic output".to_string(),
        ("", stdout) => stdout.to_string(),
        (stderr, _) => stderr.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("storyhook-launchd-")
            .tempdir_in("/private/tmp")
            .expect("a scratch directory")
    }

    /// A real, inert child process producing exactly the exit status and
    /// stderr a fixture wants — the same pattern `commands::tests::shell_output`
    /// uses, so an `Output` here needs no platform-specific `ExitStatus`
    /// construction of its own (SH-136).
    fn shell_output(script: &str) -> Output {
        std::process::Command::new("/bin/sh")
            .args(["-c", script])
            .output()
            .expect("running the inert launchctl-output fixture")
    }

    fn success() -> Output {
        shell_output("exit 0")
    }

    fn failure(code: i32, stderr: &str) -> Output {
        shell_output(&format!("printf '%s' {stderr:?} >&2; exit {code}"))
    }

    /// Owned, so it can outlive the borrowed `&str` arguments each call
    /// receives (`ensure_running`'s own locals, dropped when it returns).
    fn record(calls: &std::sync::Mutex<Vec<Vec<String>>>, args: &[&str]) {
        calls
            .lock()
            .unwrap()
            .push(args.iter().map(|s| (*s).to_string()).collect());
    }

    #[test]
    fn kickstart_succeeds_when_the_service_is_already_loaded() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let calls = std::sync::Mutex::new(Vec::new());
        let fake = |args: &[&str]| -> std::io::Result<Output> {
            record(&calls, args);
            Ok(success())
        };
        ensure_running(&env, "io.mikey.storyhook.daemon", &fake).expect("kickstart succeeds");
        let calls = calls.into_inner().unwrap();
        assert_eq!(calls.len(), 1, "no bootstrap needed: {calls:?}");
        assert_eq!(calls[0][0], "kickstart");
        assert!(
            !calls[0].iter().any(|arg| arg == "-k"),
            "kickstart must never carry -k, or a healthy daemon gets killed and restarted \
             on every ordinary command: {calls:?}"
        );
    }

    #[test]
    fn a_not_loaded_service_is_bootstrapped_then_kickstarted_again() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let calls: std::sync::Mutex<Vec<Vec<String>>> = std::sync::Mutex::new(Vec::new());
        let fake = |args: &[&str]| -> std::io::Result<Output> {
            let already_kickstarted = calls.lock().unwrap().iter().any(|c| c[0] == "kickstart");
            record(&calls, args);
            match args[0] {
                "kickstart" if !already_kickstarted => {
                    Ok(failure(LAUNCHCTL_SERVICE_NOT_FOUND, "not loaded"))
                }
                "bootstrap" => Ok(success()),
                "kickstart" => Ok(success()),
                other => panic!("unexpected launchctl action: {other}"),
            }
        };
        ensure_running(&env, "io.mikey.storyhook.daemon", &fake)
            .expect("bootstrap then retried kickstart succeeds");
        let calls = calls.into_inner().unwrap();
        assert_eq!(
            calls.iter().map(|c| c[0].as_str()).collect::<Vec<&str>>(),
            vec!["kickstart", "bootstrap", "kickstart"],
            "must load the job before retrying, and must retry exactly once: {calls:?}"
        );
    }

    #[test]
    fn a_bootstrap_failure_is_reported_loudly_never_silently_forked() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let fake = |args: &[&str]| -> std::io::Result<Output> {
            match args[0] {
                "kickstart" => Ok(failure(LAUNCHCTL_SERVICE_NOT_FOUND, "not loaded")),
                "bootstrap" => Ok(failure(1, "launchd refused to load it")),
                other => panic!("unexpected launchctl action: {other}"),
            }
        };
        let failed = ensure_running(&env, "io.mikey.storyhook.daemon", &fake)
            .expect_err("a bootstrap refusal must surface, not be swallowed");
        assert!(
            failed.to_string().contains("launchd refused to load it"),
            "{failed}"
        );
    }

    #[test]
    fn a_kickstart_refusal_after_loading_is_reported_loudly() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        // The first `kickstart` reports not-loaded (driving the bootstrap
        // path), the second — after a successful bootstrap — is refused.
        let attempts = std::sync::atomic::AtomicUsize::new(0);
        let fake = |args: &[&str]| -> std::io::Result<Output> {
            match args[0] {
                "kickstart" if attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 => {
                    Ok(failure(LAUNCHCTL_SERVICE_NOT_FOUND, "not loaded"))
                }
                "kickstart" => Ok(failure(1, "launchd refused it")),
                "bootstrap" => Ok(success()),
                other => panic!("unexpected launchctl action: {other}"),
            }
        };
        let failed = ensure_running(&env, "io.mikey.storyhook.daemon", &fake)
            .expect_err("a post-bootstrap kickstart refusal must surface");
        assert!(
            failed.to_string().contains("launchd refused it"),
            "{failed}"
        );
    }
}
