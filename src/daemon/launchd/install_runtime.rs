//! Production operations for the registration transaction.

use super::super::lifecycle::{self, DaemonOwner};
use super::{
    install::{self, Incumbent, Runtime},
    registration::REGISTRATION_DEADLINE,
};
use crate::{env::Environment, error::AppError};
use std::{
    cell::RefCell,
    path::Path,
    time::{Duration, Instant},
};

/// Installs an already validated definition under the caller's lifecycle lock.
pub(crate) fn install_agent(
    env: &Environment,
    path: &Path,
    contents: &str,
) -> Result<(), AppError> {
    // Do not overwrite a foreign or unreadable definition even if it happens
    // to occupy the filename this store would normally use.
    if path.try_exists()? {
        super::validate_registration(env, false)?;
    }
    install::apply(
        path,
        contents,
        &Agent {
            env,
            unmanaged_token: RefCell::new(None),
        },
    )
}

struct Agent<'a> {
    env: &'a Environment,
    unmanaged_token: RefCell<Option<String>>,
}

impl Runtime for Agent<'_> {
    fn registered(&self) -> Result<bool, AppError> {
        super::control().registered(&super::target(self.env))
    }

    fn incumbent(&self) -> Result<Option<Incumbent>, AppError> {
        if !lifecycle::is_live(self.env) {
            return Ok(None);
        }
        let info = lifecycle::read_info(self.env).ok_or_else(|| {
            AppError::Storage(
                "a live daemon has no readable portfile; registration was left intact".into(),
            )
        })?;
        if !info.serves(self.env.store_path()) {
            return Err(AppError::Storage(
                "the incumbent names another store; registration was left intact".into(),
            ));
        }
        lifecycle::hello(&info)?;
        match info.owner {
            Some(DaemonOwner::Launchd { label })
                if label == super::super::agent::label(self.env) =>
            {
                Ok(Some(Incumbent::Managed))
            }
            None | Some(DaemonOwner::Forked { .. }) => {
                self.unmanaged_token.borrow_mut().get_or_insert(info.token);
                Ok(Some(Incumbent::Unmanaged))
            }
            _ => Err(AppError::Storage(
                "the incumbent has a different service owner; registration was left intact".into(),
            )),
        }
    }

    fn drain(&self) -> Result<(), AppError> {
        lifecycle::stop(self.env, lifecycle::StopMode::Graceful)?;
        // RunAtLoad may have lost the lifetime lock to this old unmanaged
        // process. Its attempt is over once the incumbent drains.
        if self.unmanaged_token.borrow().is_some() {
            super::clear_failure(self.env)?;
        }
        Ok(())
    }

    fn unload(&self) -> Result<(), AppError> {
        let until = Instant::now() + REGISTRATION_DEADLINE;
        super::control().unload(&super::target(self.env), until)?;
        await_removed(
            self.env,
            self.unmanaged_token.borrow().as_deref(),
            until,
            &Instant::now,
            &std::thread::sleep,
            &lifecycle::hello_with_timeout,
        )
    }

    fn register(&self) -> Result<(), AppError> {
        super::clear_failure(self.env)?;
        super::control().replace(
            &super::target(self.env),
            &super::super::agent::path(self.env),
        )
    }

    fn start(&self, previous_build: bool) -> Result<(), AppError> {
        super::ensure_running(
            self.env,
            &super::super::agent::label(self.env),
            &super::run_bounded,
        )?;
        lifecycle::await_launchd_build(self.env, !previous_build)
    }

    fn restore_unmanaged(&self) -> Result<(), AppError> {
        if self.registered()? {
            return Err(AppError::Storage(
                "refusing unmanaged recovery while launchd registration remains loaded".into(),
            ));
        }
        lifecycle::restore_unmanaged(self.env)
    }
}

// A removed job can still be releasing its lifetime lock. Only the
// authenticated unmanaged incumbent is permitted to survive rollback.
fn await_removed(
    env: &Environment,
    unmanaged_token: Option<&str>,
    until: Instant,
    now: &dyn Fn() -> Instant,
    sleep: &dyn Fn(Duration),
    hello: &dyn Fn(&lifecycle::DaemonInfo, Duration) -> Result<(), AppError>,
) -> Result<(), AppError> {
    let mut last_failure = None;
    while lifecycle::is_live(env) {
        let remaining = until.saturating_duration_since(now());
        if remaining.is_zero() {
            let mut error = AppError::Storage("launchd registration was removed but its daemon still holds the store; recovery cannot safely continue".into());
            if let Some(failure) = last_failure {
                error = error.with_context(&format!(
                    "the recorded unmanaged daemon did not authenticate: {failure}"
                ));
            }
            return Err(error);
        }
        if let Some(info) = lifecycle::read_info(env)
            && unmanaged_token == Some(info.token.as_str())
        {
            match hello(&info, remaining.min(lifecycle::CONTROL_DEADLINE)) {
                Ok(()) => return Ok(()),
                Err(error) => last_failure = Some(error),
            }
        }
        sleep(Duration::from_millis(100).min(until.saturating_duration_since(now())));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs4::FileExt;
    use std::{cell::Cell, fs::File};

    fn locked_fixture() -> (tempfile::TempDir, Environment, File) {
        let dir = tempfile::tempdir_in("/private/tmp").unwrap();
        let env = Environment::at(dir.path());
        std::fs::create_dir_all(env.daemon_state_dir()).unwrap();
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(env.daemon_pidfile())
            .unwrap();
        lock.lock_exclusive().unwrap();
        let bound = crate::daemon::serve::BoundAddress {
            loopback: "127.0.0.1:1".parse().unwrap(),
            tailnet: None,
        };
        let info =
            lifecycle::info_for(&bound, "old-token".into(), &env.now(), env.store_path()).unwrap();
        lifecycle::write_info(&env, &info).unwrap();
        (dir, env, lock)
    }

    #[test]
    fn only_an_authenticated_original_daemon_may_survive_removal() {
        for answers in [true, false] {
            let (_dir, env, _lock) = locked_fixture();
            let start = Instant::now();
            let time = Cell::new(start);
            let checks = Cell::new(0);
            let result = await_removed(
                &env,
                Some("old-token"),
                start + REGISTRATION_DEADLINE,
                &|| time.get(),
                &|delay| time.set(time.get() + delay),
                &|info, budget| {
                    checks.set(checks.get() + 1);
                    assert_eq!(info.token, "old-token");
                    assert!(!budget.is_zero() && budget <= lifecycle::CONTROL_DEADLINE);
                    if answers {
                        Ok(())
                    } else {
                        time.set(time.get() + budget);
                        Err(AppError::Storage("stale portfile did not answer".into()))
                    }
                },
            );
            assert!(checks.get() > 0);
            assert_eq!(result.is_ok(), answers);
            if !answers {
                assert_eq!(time.get() - start, REGISTRATION_DEADLINE);
                assert!(result.unwrap_err().to_string().contains("stale portfile"));
            }
        }
    }

    #[test]
    fn a_late_child_must_release_its_real_lock_before_recovery_continues() {
        let (_dir, env, lock) = locked_fixture();
        let start = Instant::now();
        let time = Cell::new(start);
        await_removed(
            &env,
            None,
            start + REGISTRATION_DEADLINE,
            &|| time.get(),
            &|delay| {
                time.set(time.get() + delay);
                FileExt::unlock(&lock).unwrap();
            },
            &|_, _| panic!("a replacement is not the recorded incumbent"),
        )
        .unwrap();
        assert!(time.get() > start);
        assert!(!lifecycle::is_live(&env));
    }

    #[test]
    fn another_live_child_cannot_inherit_the_unmanaged_exception() {
        let (_dir, env, _lock) = locked_fixture();
        let start = Instant::now();
        let time = Cell::new(start);
        let result = await_removed(
            &env,
            Some("a-different-token"),
            start + REGISTRATION_DEADLINE,
            &|| time.get(),
            &|delay| time.set(time.get() + delay),
            &|_, _| panic!("must not authenticate an unrelated token"),
        );
        assert!(result.is_err());
        assert_eq!(time.get() - start, REGISTRATION_DEADLINE);
    }
}
