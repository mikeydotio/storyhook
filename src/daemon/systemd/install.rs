//! Installation transactions preserve the previous definition and enablement.

use super::super::{agent::ExecutionPath, commands::LoginAgentReport, install_guard, lifecycle};
use super::command::{self, Runner};
use crate::{env::Environment, error::AppError};
use std::{fs, io::Write, path::Path};

/// Validates and installs one durable per-store user service.
pub(crate) fn install(env: &Environment, this_binary: bool) -> Result<LoginAgentReport, AppError> {
    let exe = crate::path_identity::running_exe()
        .ok_or_else(|| AppError::Storage("cannot find running executable".into()))?;
    let inputs = install_guard::gather(super::super::commands::user_id(), this_binary, exe);
    let verdict = install_guard::decide(&inputs).map_err(|e| AppError::Usage(e.to_string()))?;
    let target = super::path(env);
    super::super::commands::refuse_temporary_store_for_durable_agent(env.store_path(), &target)?;
    let execution =
        ExecutionPath::parse(std::env::var_os("PATH").as_deref()).map_err(AppError::Usage)?;
    let contents = super::definition(env, &verdict.enthrone, &execution)?;
    command::manager(&command::run).map_err(|e| {
        AppError::Usage(format!(
            "cannot install a systemd user service: {e}; start a user manager first"
        ))
    })?;
    lifecycle::with_registration_lock(env, || {
        apply(
            env,
            &contents,
            &command::run,
            &|| {
                lifecycle::stop(env, lifecycle::StopMode::Graceful)?;
                Ok(())
            },
            &|| {
                // The manager owns the process; health is still the daemon protocol.
                super::start_managed(env, &command::run)
            },
        )
    })?;
    Ok(LoginAgentReport::new(
        format!(
            "installed systemd user service {}\n  {}\n  PATH captured from this shell; reinstall after changing tool directories.",
            super::unit(env),
            target.display()
        ),
        None,
    ))
}

fn apply(
    env: &Environment,
    contents: &str,
    run: &Runner<'_>,
    stop: &dyn Fn() -> Result<(), AppError>,
    start: &dyn Fn() -> Result<(), AppError>,
) -> Result<(), AppError> {
    let target = super::path(env);
    let previous = match fs::read(&target) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(AppError::Storage(format!(
                "read {} before installation: {e}",
                target.display()
            )));
        }
    };
    let unit = super::unit(env);
    let reply = run(&["show", &unit, "--property=LoadState,UnitFileState"])?;
    let props = command::parse_properties(&reply.text)?;
    let enabled = match (
        props.get("LoadState").map(String::as_str),
        props.get("UnitFileState").map(String::as_str),
    ) {
        (Some("not-found"), _) => false,
        (Some("loaded"), Some("enabled")) if reply.success => true,
        (Some("loaded"), Some("disabled")) if reply.success => false,
        _ => {
            return Err(AppError::Storage(format!(
                "cannot establish prior enablement of {unit}: {} {}",
                reply.text, reply.diagnostic
            )));
        }
    };
    fs::create_dir_all(env.daemon_state_dir())?;
    atomic_write(&target, contents.as_bytes())?;
    let mut started = false;
    let outcome = (|| {
        command::checked(run, &["daemon-reload"])?;
        command::checked(
            run,
            &[
                "enable",
                target
                    .to_str()
                    .ok_or_else(|| AppError::Usage("systemd unit path is not UTF-8".into()))?,
            ],
        )?;
        stop()?;
        started = true;
        start()
    })();
    if let Err(failure) = outcome {
        let mut diagnostics = Vec::new();
        // Cancel a queued start before restoring configuration; stopping the
        // unit also covers a process that publishes after the health timeout.
        if started && let Err(e) = command::checked(run, &["stop", "--no-block", &unit]) {
            diagnostics.push(e.to_string());
        }
        if !enabled
            && let Err(e) = command::checked(
                run,
                &["disable", target.to_str().expect("validated unit path")],
            )
        {
            diagnostics.push(e.to_string());
        }
        let restore = match previous {
            Some(bytes) => atomic_write(&target, &bytes),
            None => fs::remove_file(&target).map_err(AppError::from),
        };
        if let Err(e) = restore {
            diagnostics.push(e.to_string());
        }
        if let Err(e) = command::checked(run, &["daemon-reload"]) {
            diagnostics.push(e.to_string());
        }
        if enabled && let Err(e) = command::checked(run, &["enable", &unit]) {
            diagnostics.push(e.to_string());
        }
        return Err(AppError::Storage(format!(
            "systemd installation failed: {failure}. Previous definition/enablement restoration: {}. Inspect `story daemon status` before restarting.",
            if diagnostics.is_empty() {
                "complete".into()
            } else {
                diagnostics.join("; ")
            }
        )));
    }
    Ok(())
}

/// Disables and removes only this store's verified registration.
pub(crate) fn uninstall(env: &Environment) -> Result<LoginAgentReport, AppError> {
    lifecycle::with_registration_lock(env, || uninstall_locked(env))
}

fn uninstall_locked(env: &Environment) -> Result<LoginAgentReport, AppError> {
    let target = super::path(env);
    if !target.try_exists()? {
        return Ok(LoginAgentReport::new(
            "the storyhook systemd user service is not installed".into(),
            None,
        ));
    }
    // Refuse a mismatched registration rather than stop a foreign store.
    super::registration(env)?;
    remove_registration(env, &command::run, &|| {
        lifecycle::stop(env, lifecycle::StopMode::Graceful)?;
        Ok(())
    })?;
    Ok(LoginAgentReport::new(
        format!(
            "removed systemd user service {}\n  {}",
            super::unit(env),
            target.display()
        ),
        None,
    ))
}

fn remove_registration(
    env: &Environment,
    run: &Runner<'_>,
    stop: &dyn Fn() -> Result<(), AppError>,
) -> Result<(), AppError> {
    let unit = super::unit(env);
    let target = super::path(env);
    // show may return nonzero for not-found; require that exact structured fact.
    let reply = run(&[
        "show",
        &unit,
        "--property=LoadState,FragmentPath,DropInPaths",
    ])?;
    let props = command::parse_properties(&reply.text)?;
    if props.get("LoadState").map(String::as_str) != Some("not-found") {
        if !reply.success {
            return Err(AppError::Storage(format!(
                "cannot inspect {unit}: {}",
                reply.diagnostic
            )));
        }
        super::validate_loaded(env, &props)?;
        stop()?;
        command::checked(run, &["stop", "--no-block", &unit])?;
        command::checked(run, &["disable", &unit])?;
    } else {
        stop()?;
    }
    fs::remove_file(&target)
        .map_err(|e| AppError::Storage(format!("remove {}: {e}", target.display())))?;
    command::checked(run, &["daemon-reload"])?;
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let parent = path.parent().expect("unit has a parent");
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)
        .map_err(|e| AppError::Storage(format!("replace {}: {e}", path.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::command::Reply;
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn failed_start_restores_previous_bytes_and_enabled_state() {
        let dir = storyhook_test_support::scratch_dir();
        let env = Environment::at(dir.path());
        let target = super::super::path(&env);
        atomic_write(&target, b"previous").unwrap();
        let calls = RefCell::new(Vec::new());
        let run = |args: &[&str]| {
            calls.borrow_mut().push(args.join(" "));
            Ok(Reply {
                success: true,
                text: if args[0] == "show" {
                    "LoadState=loaded\nUnitFileState=enabled".into()
                } else {
                    String::new()
                },
                diagnostic: String::new(),
            })
        };
        let result = apply(&env, "replacement", &run, &|| Ok(()), &|| {
            Err(AppError::Storage("start denied".into()))
        });
        assert!(result.unwrap_err().to_string().contains("start denied"));
        assert_eq!(fs::read(target).unwrap(), b"previous");
        assert_eq!(
            calls
                .borrow()
                .iter()
                .filter(|a| a.as_str() == "daemon-reload")
                .count(),
            2
        );
        assert_eq!(
            calls.borrow().last().unwrap(),
            &format!("enable {}", super::super::unit(&env))
        );
    }
    #[test]
    fn failed_new_install_removes_definition_and_keeps_rollback_diagnostics() {
        let dir = storyhook_test_support::scratch_dir();
        let env = Environment::at(dir.path());
        let run = |args: &[&str]| {
            if args[0] == "disable" {
                return Err(AppError::Storage("disable refused".into()));
            }
            Ok(Reply {
                success: true,
                text: if args[0] == "show" {
                    "LoadState=not-found\nUnitFileState=".into()
                } else {
                    String::new()
                },
                diagnostic: String::new(),
            })
        };
        let result = apply(&env, "replacement", &run, &|| Ok(()), &|| {
            Err(AppError::Storage("start refused".into()))
        })
        .unwrap_err()
        .to_string();
        assert!(!super::super::path(&env).exists());
        assert!(
            result.contains("start refused") && result.contains("disable refused"),
            "{result}"
        );
    }

    #[test]
    fn unreadable_or_unconfirmed_prior_state_is_never_overwritten() {
        let dir = storyhook_test_support::scratch_dir();
        let env = Environment::at(dir.path());
        let target = super::super::path(&env);
        atomic_write(&target, b"previous").unwrap();
        let run = |_: &[&str]| Err(AppError::Storage("bus timeout".into()));
        assert!(
            apply(
                &env,
                "replacement",
                &run,
                &|| panic!("must not stop"),
                &|| panic!("must not start")
            )
            .is_err()
        );
        assert_eq!(fs::read(target).unwrap(), b"previous");
    }
    #[test]
    fn uninstall_stops_before_disabling_the_unit_link() {
        let dir = storyhook_test_support::scratch_dir();
        let env = Environment::at(dir.path());
        atomic_write(&super::super::path(&env), b"definition").unwrap();
        let other = super::super::path(&env).with_file_name("another-store.service");
        atomic_write(&other, b"other registration").unwrap();
        let calls = RefCell::new(Vec::new());
        let run = |args: &[&str]| {
            calls.borrow_mut().push(args[0].to_owned());
            Ok(Reply {
                success: true,
                text: if args[0] == "show" {
                    format!(
                        "LoadState=loaded\nFragmentPath={}\nDropInPaths=",
                        super::super::path(&env).display()
                    )
                } else {
                    String::new()
                },
                diagnostic: String::new(),
            })
        };
        remove_registration(&env, &run, &|| {
            calls.borrow_mut().push("drain".into());
            Ok(())
        })
        .unwrap();
        assert_eq!(
            *calls.borrow(),
            ["show", "drain", "stop", "disable", "daemon-reload"]
        );
        assert!(!super::super::path(&env).exists());
        assert_eq!(fs::read(other).unwrap(), b"other registration");
    }
}
