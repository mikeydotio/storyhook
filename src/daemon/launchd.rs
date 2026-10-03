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
use std::time::{Duration, Instant};

use crate::env::Environment;
use crate::error::AppError;

use super::lifecycle::{DaemonLauncher, DaemonOwner};

pub(crate) mod install;
mod install_runtime;
pub(crate) mod registration;
pub(crate) use install_runtime::install_agent;

/// Runs `launchctl` with `args`, exactly as installed on `$PATH`. The one
/// `Command::new("launchctl")` site in this codebase (`tests/spawn_inventory.rs`
/// pins that).
pub(crate) fn run(args: &[&str]) -> std::io::Result<Output> {
    run_bounded(args, registration::REGISTRATION_DEADLINE)
}

/// One bounded process boundary, shared by normal starts and registration.
fn run_bounded(args: &[&str], timeout: Duration) -> std::io::Result<Output> {
    let mut command = Command::new("launchctl");
    command.args(args);
    let result = crate::process::run_captured_quiet(command, timeout)
        .map_err(|error| std::io::Error::other(error.detail()))?;
    if result.stdout_truncated {
        return Err(std::io::Error::other(
            "launchctl returned truncated evidence",
        ));
    }
    Ok(Output {
        status: result.status,
        stdout: result.stdout,
        stderr: result.stderr,
    })
}

/// Production controls; tests inject the process boundary and monotonic clock.
fn control() -> registration::Control<'static> {
    registration::Control {
        run: &run_bounded,
        now: &Instant::now,
        sleep: &std::thread::sleep,
    }
}

fn target(env: &Environment) -> String {
    format!(
        "gui/{}/{}",
        super::commands::user_id(),
        super::agent::label(env)
    )
}

/// Clears startup evidence before a new attempt; an unreadable record cannot
/// be mistaken for evidence from the new daemon.
fn clear_failure(env: &Environment) -> Result<(), AppError> {
    match std::fs::remove_file(env.daemon_failure()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::Storage(format!(
            "clear startup failure {}: {error}",
            env.daemon_failure().display()
        ))),
    }
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
        let label = super::agent::label(env);
        start_with_recovery(
            env,
            &run_bounded,
            &|| super::lifecycle::await_launchd_healthy(env),
            &|| recovery_allowed(env),
            &|| {
                // Recheck after diagnostic collection: a late daemon must
                // never be killed simply because its first health wait expired.
                if !recovery_allowed(env)? {
                    return Err(AppError::Storage("the daemon or startup evidence changed before launchd recovery; registration was left intact".into()));
                }
                control().replace(&target(env), &super::agent::path(env))
            },
        )?;
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
/// the install transaction's own is: a
/// fixture that ran the real binary would register an agent pointing at the
/// test binary into the developer's own login session, under the one label
/// this project owns.
fn ensure_running(
    env: &Environment,
    label: &str,
    launchctl: &dyn Fn(&[&str], Duration) -> std::io::Result<Output>,
) -> Result<(), AppError> {
    let target = format!("gui/{}/{label}", super::commands::user_id());
    match launchctl(&["kickstart", &target], registration::REGISTRATION_DEADLINE) {
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
    match launchctl(&["kickstart", &target], registration::REGISTRATION_DEADLINE) {
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
    launchctl: &dyn Fn(&[&str], Duration) -> std::io::Result<Output>,
) -> Result<(), AppError> {
    registration::Control {
        run: launchctl,
        now: &Instant::now,
        sleep: &std::thread::sleep,
    }
    .load(
        &target(env),
        &super::agent::path(env),
        Instant::now() + registration::REGISTRATION_DEADLINE,
    )
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

fn start_with_recovery(
    env: &Environment,
    launchctl: &dyn Fn(&[&str], Duration) -> std::io::Result<Output>,
    health: &dyn Fn() -> Result<(), AppError>,
    may_reseat: &dyn Fn() -> Result<bool, AppError>,
    reseat: &dyn Fn() -> Result<(), AppError>,
) -> Result<(), AppError> {
    clear_failure(env)?;
    let label = super::agent::label(env);
    ensure_running(env, &label, launchctl)?;
    let first = match health() {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    let diagnostic = match launchctl(
        &["print", &target(env)],
        registration::REGISTRATION_DEADLINE,
    ) {
        Ok(output) => format!(
            "launchctl print {} ({}): {}",
            target(env),
            output.status,
            describe_output(&output)
        ),
        Err(error) => format!("launchctl print {} failed: {error}", target(env)),
    };
    let first = first.with_context(&diagnostic);
    match may_reseat() {
        Ok(false) => return Err(first),
        Err(error) => {
            return Err(first.with_context(&format!(
                "cannot establish safe registration recovery: {error}"
            )));
        }
        Ok(true) => (),
    }
    let retry = (|| {
        reseat()?;
        // Do not erase a failure published after the eligibility check or by
        // bootstrap's RunAtLoad child. It still belongs to this attempt.
        ensure_running(env, &label, launchctl)?;
        health()
    })();
    retry.map_err(|error| {
        error.with_context(&format!(
            "First launchd attempt failed: {first}\nOne registration recovery also failed:"
        ))
    })
}

fn recovery_allowed(env: &Environment) -> Result<bool, AppError> {
    if super::lifecycle::is_live(env) || env.daemon_failure().try_exists()? {
        return Ok(false);
    }
    validate_registration(env, true)?;
    Ok(true)
}

/// Refuses destructive operations when the existing definition cannot be
/// established as belonging to this store and an executable.
fn validate_registration(env: &Environment, require_executable: bool) -> Result<(), AppError> {
    let path = super::agent::path(env);
    let text = std::fs::read_to_string(&path).map_err(|error| {
        AppError::Storage(format!(
            "read {} before launchd replacement: {error}",
            path.display()
        ))
    })?;
    let expected = format!("<string>{}</string>", super::agent::label(env));
    let label = text
        .split_once("<key>Label</key>")
        .map(|(_, value)| value.trim_start());
    let args = super::agent::registered_args(&text).unwrap_or_default();
    let stores = args.iter().filter(|arg| *arg == "--store-path").count();
    if text.matches("<key>Label</key>").count() != 1
        || !label.is_some_and(|value| value.starts_with(&expected))
        || stores > 1
        || args.last().is_some_and(|arg| arg == "--store-path")
    {
        return Err(AppError::Storage(format!(
            "{} has a malformed or foreign registration; it was left intact",
            path.display()
        )));
    }
    let exe = super::agent::registered_exe(&text).ok_or_else(|| {
        AppError::Storage(format!(
            "{} has no readable executable; registration was left intact",
            path.display()
        ))
    })?;
    let store = super::agent::registered_store(&text)
        .map(|path| crate::env::canonical_ish(&path).unwrap_or(path))
        .unwrap_or_else(|| {
            crate::env::StoreLocation::for_home(env.home())
                .path()
                .to_path_buf()
        });
    if store != env.store_path() || !exe.is_absolute() || (require_executable && !exe.is_file()) {
        return Err(AppError::Storage(format!(
            "{} does not name a valid executable for this store; registration was left intact",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_evidence_and_malformed_or_foreign_registration_prevent_recovery() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        let path = super::super::agent::path(&env);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let execution =
            super::super::agent::ExecutionPath::parse(Some(std::ffi::OsStr::new("/usr/bin:/bin")))
                .unwrap();
        let valid = super::super::agent::plist(&std::env::current_exe().unwrap(), &env, &execution);
        for contents in ["not a plist".into(), valid.replace(&super::super::agent::label(&env), "foreign.label"), valid.replace("<string>daemon</string>", "<string>--store-path</string><string>/foreign/store.db</string><string>daemon</string>")] {
            std::fs::write(&path, contents).unwrap();
            assert!(recovery_allowed(&env).is_err());
        }
        std::fs::write(&path, &valid).unwrap();
        assert!(recovery_allowed(&env).unwrap());
        std::fs::write(
            env.daemon_failure(),
            "an application failure, even if unreadable",
        )
        .unwrap();
        assert!(!recovery_allowed(&env).unwrap());
        clear_failure(&env).unwrap();
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(env.daemon_pidfile())
            .unwrap();
        fs4::FileExt::lock_exclusive(&lock).unwrap();
        assert!(
            !recovery_allowed(&env).unwrap(),
            "a live or late daemon must prevent reseating"
        );
    }

    #[test]
    fn failed_start_is_reseated_once_and_original_failure_survives() {
        use std::cell::Cell;
        for recovers in [false, true] {
            let dir = scratch();
            let env = Environment::at(dir.path());
            let checks = Cell::new(0);
            let reloads = Cell::new(0);
            let health = || {
                checks.set(checks.get() + 1);
                if recovers && checks.get() == 2 {
                    Ok(())
                } else {
                    Err(AppError::Storage(format!(
                        "health failure {}",
                        checks.get()
                    )))
                }
            };
            let fake = |args: &[&str], _: Duration| {
                Ok(if args[0] == "print" {
                    failure(0, "OS_REASON_CODESIGNING")
                } else {
                    success()
                })
            };
            let result = start_with_recovery(&env, &fake, &health, &|| Ok(true), &|| {
                reloads.set(reloads.get() + 1);
                Ok(())
            });
            assert_eq!(reloads.get(), 1);
            assert_eq!(checks.get(), 2);
            if recovers {
                result.unwrap();
            } else {
                let error = result.unwrap_err().to_string();
                for expected in [
                    "health failure 1",
                    "health failure 2",
                    "OS_REASON_CODESIGNING",
                ] {
                    assert!(error.contains(expected), "{error}");
                }
            }
        }
    }

    #[test]
    fn a_late_application_failure_survives_the_recovery_recheck() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        std::fs::create_dir_all(env.daemon_state_dir()).unwrap();
        let result = start_with_recovery(
            &env,
            &|_, _| Ok(success()),
            &|| Err(AppError::Storage("initial health failure".into())),
            &|| {
                // Model the child publishing after the first eligibility read.
                std::fs::write(env.daemon_failure(), "late schema failure")?;
                Ok(true)
            },
            &|| {
                assert!(!recovery_allowed(&env)?);
                Err(AppError::Storage(
                    "late application failure; left intact".into(),
                ))
            },
        );
        let error = result.unwrap_err().to_string();
        assert!(error.contains("initial health failure"), "{error}");
        assert!(error.contains("late application failure"), "{error}");
        assert_eq!(
            std::fs::read_to_string(env.daemon_failure()).unwrap(),
            "late schema failure"
        );
    }

    #[test]
    fn health_or_ineligible_failure_never_reloads() {
        let dir = scratch();
        let env = Environment::at(dir.path());
        for healthy in [true, false] {
            let result = start_with_recovery(
                &env,
                &|_, _| Ok(success()),
                &|| {
                    if healthy {
                        Ok(())
                    } else {
                        Err(AppError::Storage("schema failure".into()))
                    }
                },
                &|| Ok(false),
                &|| panic!("must not reload"),
            );
            assert_eq!(result.is_ok(), healthy);
        }
    }

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
        let fake = |args: &[&str], _: Duration| -> std::io::Result<Output> {
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
        let fake = |args: &[&str], _: Duration| -> std::io::Result<Output> {
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
        let fake = |args: &[&str], _: Duration| -> std::io::Result<Output> {
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
        let fake = |args: &[&str], _: Duration| -> std::io::Result<Output> {
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
