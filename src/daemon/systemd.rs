//! Per-store Linux user services, with no shell or ambient test registration.

use super::{
    agent::ExecutionPath,
    lifecycle::{self, DaemonLauncher, DaemonOwner},
};
use crate::{env::Environment, error::AppError};
use std::{
    fs,
    path::{Path, PathBuf},
};
pub(crate) mod command;
mod definition;
mod install;
#[cfg(test)]
mod selection_tests;
pub(crate) use install::{install, uninstall};

/// This store's systemd unit identity.
pub(crate) fn unit(env: &Environment) -> String {
    format!("{}.service", super::agent::label(env))
}

/// The user unit definition for this environment.
pub(crate) fn path(env: &Environment) -> PathBuf {
    env.config_home().join("systemd/user").join(unit(env))
}

fn definition(env: &Environment, exe: &Path, path: &ExecutionPath) -> Result<String, AppError> {
    definition::Registration::new(env, exe, path).render()
}

/// Availability of a manager and the exact store's generated registration.
pub(crate) enum Selection {
    /// A validated local definition exists.
    Installed,
    /// The user manager is reachable, but this store has no unit.
    NotInstalled,
    /// Manager transport is unavailable; the diagnostic accompanies a fork.
    Unavailable(String),
}

/// Distinguishes a missing manager from an invalid installed service.
pub(crate) fn select(env: &Environment, run: &command::Runner<'_>) -> Result<Selection, AppError> {
    if let Err(reason) = command::manager(run) {
        return Ok(Selection::Unavailable(reason));
    }
    match registration(env)? {
        Some(_) => Ok(Selection::Installed),
        None => Ok(Selection::NotInstalled),
    }
}

fn registration(env: &Environment) -> Result<Option<definition::Registration>, AppError> {
    let file = path(env);
    let text = match fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(AppError::Storage(format!("read {}: {e}", file.display()))),
    };
    let registered = definition::parse(&text)?;
    if registered.store != env.store_path()
        || registered.state.join("storyhook") != env.state_home()
    {
        return Err(AppError::Storage(format!(
            "{} names a different store or runtime directory; run `story daemon install`",
            file.display()
        )));
    }
    Ok(Some(registered))
}

/// Starts through the manager; the client never forks a managed daemon.
pub(crate) struct SystemdUserLauncher;
impl DaemonLauncher for SystemdUserLauncher {
    fn launch(&self, env: &Environment) -> Result<DaemonOwner, AppError> {
        start_managed(env, &command::run)?;
        Ok(DaemonOwner::Systemd { unit: unit(env) })
    }
}

fn request_start(env: &Environment, run: &command::Runner<'_>) -> Result<(), AppError> {
    let unit = unit(env);
    command::checked(run, &["daemon-reload"])?;
    let props = command::properties(run, &unit)?;
    validate_loaded(env, &props)?;
    command::checked(run, &["start", "--no-block", &unit])?;
    Ok(())
}

fn validate_loaded(
    env: &Environment,
    props: &std::collections::BTreeMap<String, String>,
) -> Result<(), AppError> {
    let unit = unit(env);
    if props.get("LoadState").map(String::as_str) != Some("loaded")
        || !props.get("FragmentPath").is_some_and(|fragment| {
            crate::env::canonical_ish(Path::new(fragment)).is_ok_and(|actual| {
                crate::env::canonical_ish(&path(env)).is_ok_and(|expected| actual == expected)
            })
        })
        || props.get("DropInPaths").map(String::as_str) != Some("")
    {
        return Err(AppError::Storage(format!(
            "systemd unit {unit} is masked, overridden or not loaded from {}; inspect `systemctl --user cat {unit}` and reinstall. Properties: {props:?}",
            path(env).display()
        )));
    }
    Ok(())
}

fn start_managed(env: &Environment, run: &command::Runner<'_>) -> Result<(), AppError> {
    match fs::remove_file(env.daemon_failure()) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(AppError::Storage(format!("clear old startup failure: {e}"))),
    }
    request_start(env, run)?;
    lifecycle::await_managed_healthy(
        env,
        "systemd",
        &format!("journalctl --user -u {}", unit(env)),
        Some(&DaemonOwner::Systemd { unit: unit(env) }),
    )
}

/// Saved PATH, verified against the entire generated unit.
pub(crate) fn registered_path(env: &Environment) -> Result<Option<ExecutionPath>, AppError> {
    registration(env)?
        .map(|r| {
            ExecutionPath::parse(Some(std::ffi::OsStr::new(&r.path))).map_err(AppError::Storage)
        })
        .transpose()
}

/// Definition health remains visible even when no daemon is running.
pub(crate) fn report(env: &Environment) -> String {
    match registration(env) {
        Ok(None) => format!(
            "systemd user service not installed ({})",
            path(env).display()
        ),
        Ok(Some(r)) => format!(
            "systemd user service {}\n  {}\n  executable {}{}",
            unit(env),
            path(env).display(),
            r.exe.display(),
            if r.exe.is_file() { "" } else { " (missing)" }
        ),
        Err(error) => format!("systemd user service unhealthy: {error}"),
    }
}

/// A registration keeps runtime state from being garbage-collected, even if
/// its executable is currently missing. Invalid files conservatively retain
/// the runtime directory selected by their store-keyed filename.
pub(crate) fn agent_serving(env: &Environment, store: &Path) -> Option<PathBuf> {
    let store = crate::env::canonical_ish(store).unwrap_or_else(|_| store.to_owned());
    let key = crate::env::StoreLocation::key_for_path(&store);
    [env.config_home().to_owned(), env.home().join(".config")]
        .into_iter()
        .map(|root| {
            root.join("systemd/user").join(format!(
                "{}.{}.service",
                super::agent::LAUNCHD_LABEL,
                key
            ))
        })
        .find(|candidate| candidate.symlink_metadata().is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_owns_scheduling_and_carries_exact_store_and_path() {
        let env = Environment::at("/home/example");
        let execution =
            ExecutionPath::parse(Some(std::ffi::OsStr::new("/opt/tools:/usr/bin"))).unwrap();
        let text = definition(&env, Path::new("/opt/story"), &execution).unwrap();
        for setting in [
            "Nice=0",
            "CPUSchedulingPolicy=other",
            "IOSchedulingClass=best-effort",
            "IOSchedulingPriority=4",
            "CPUWeight=100",
            "IOWeight=100",
            "Restart=no",
            "--owner systemd",
            "PATH=/opt/tools:/usr/bin",
            "--store-path",
            "TimeoutStopSec=infinity",
        ] {
            assert!(text.contains(setting), "missing {setting}: {text}");
        }
        assert!(text.contains(env.store_path().to_str().unwrap()));
    }

    #[test]
    fn stores_have_separate_services() {
        let env = Environment::at("/home/example");
        let other = env.clone().with_store(
            crate::env::StoreLocation::resolve(
                Some(Path::new("/data/other.db")),
                &crate::env::StoreVars::default(),
                env.home(),
            )
            .unwrap(),
        );
        assert_ne!(unit(&env), unit(&other));
        assert_ne!(path(&env), path(&other));
    }
}
