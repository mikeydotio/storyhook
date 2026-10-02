//! A failed installation restores both the definition and the serving process.

use crate::error::AppError;
use std::{fs, io::Write, path::Path};

/// Ownership observed before changing the registration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Incumbent {
    /// This store's launchd daemon.
    Managed,
    /// A directly started daemon, including pre-ownership metadata.
    Unmanaged,
}

/// External service and daemon operations; file changes remain real in tests.
pub(crate) trait Runtime {
    /// Whether launchd currently owns a registration for this store.
    fn registered(&self) -> Result<bool, AppError>;
    /// Authenticated ownership of the current daemon, if one exists.
    fn incumbent(&self) -> Result<Option<Incumbent>, AppError>;
    /// Gracefully drains the current daemon without abandoning requests.
    fn drain(&self) -> Result<(), AppError>;
    /// Removes the registration and proves that no delayed start remains queued.
    fn unload(&self) -> Result<(), AppError>;
    /// Replaces launchd's registration from the current plist on disk.
    fn register(&self) -> Result<(), AppError>;
    /// Starts and authenticates a managed daemon; rollback may use the old build.
    fn start(&self, previous_build: bool) -> Result<(), AppError>;
    /// Recovers the original unmanaged mode only after registration removal.
    fn restore_unmanaged(&self) -> Result<(), AppError>;
}

/// Installs one definition and rolls back runtime state after any failure.
pub(crate) fn apply(path: &Path, contents: &str, runtime: &dyn Runtime) -> Result<(), AppError> {
    let previous = match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(AppError::Storage(format!(
                "read {} before installation: {error}",
                path.display()
            )));
        }
    };
    let registered = runtime.registered()?;
    let incumbent = runtime.incumbent()?;
    if registered && previous.is_none() {
        return Err(AppError::Storage(format!(
            "launchd has a registration but {} is missing; cannot preserve its definition. Nothing was changed.",
            path.display()
        )));
    }
    if !registered && incumbent == Some(Incumbent::Managed) {
        return Err(AppError::Storage("a managed daemon has no stable launchd registration; nothing was changed. Retry after its pending transition finishes.".into()));
    }
    // A refused drain must leave the old definition and registration intact.
    if incumbent == Some(Incumbent::Managed) {
        runtime.drain()?;
    }
    let mut touched_registration = false;
    let result = (|| {
        atomic_write(path, contents.as_bytes())?;
        touched_registration = true;
        runtime.register()?;
        if incumbent == Some(Incumbent::Unmanaged) {
            runtime.drain()?;
        }
        runtime.start(false)
    })();
    if let Err(failure) = result {
        let restored = rollback(
            path,
            previous.as_deref(),
            registered,
            incumbent,
            touched_registration,
            runtime,
        );
        let recovery = match restored {
            Ok(note) => note,
            Err(error) => format!(
                "Recovery failed: {error}. Service health is NOT confirmed; inspect `story daemon status`."
            ),
        };
        return Err(AppError::Storage(format!(
            "daemon installation failed: {failure}\n\n{recovery}"
        )));
    }
    Ok(())
}

fn rollback(
    path: &Path,
    previous: Option<&[u8]>,
    registered: bool,
    incumbent: Option<Incumbent>,
    touched_registration: bool,
    runtime: &dyn Runtime,
) -> Result<String, AppError> {
    // Removal is a barrier against a late RunAtLoad child. Do not fork or
    // restore a definition underneath a replacement that might still start.
    if touched_registration {
        runtime.unload()?;
    }
    match previous {
        Some(bytes) => atomic_write(path, bytes)?,
        None => match fs::remove_file(path) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => {
                return Err(AppError::Storage(format!(
                    "remove new plist {}: {error}",
                    path.display()
                )));
            }
        },
    }
    if registered {
        runtime.register()?;
        runtime.start(true)?;
        return Ok("The previous agent was put back; its daemon is restored and healthy (it may serve the previous build).".into());
    }
    if incumbent.is_some() {
        match runtime.incumbent()? {
            Some(Incumbent::Unmanaged) => (),
            None => runtime.restore_unmanaged()?,
            Some(Incumbent::Managed) => {
                return Err(AppError::Storage(
                    "a managed replacement survived removal; refusing unmanaged recovery".into(),
                ));
            }
        }
        return Ok("The previous unmanaged daemon is restored and healthy.".into());
    }
    Ok("Previous configuration restored. No daemon was running before installation. Nothing was left behind by this attempt.".into())
}

/// Replaces definition bytes without exposing a partial plist to launchd.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let parent = path.parent().ok_or_else(|| {
        AppError::Storage(format!("agent path has no parent: {}", path.display()))
    })?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path)
        .map_err(|error| AppError::Storage(format!("replace {}: {error}", path.display())))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        fs,
        path::PathBuf,
    };

    struct Fake {
        registered: Cell<bool>,
        live: Cell<Option<Incumbent>>,
        fail: &'static str,
        recovery_fails: bool,
        cleanup_fails: bool,
        calls: RefCell<Vec<String>>,
        path: PathBuf,
    }
    impl Fake {
        fn call(&self, name: &str) -> Result<(), AppError> {
            self.calls.borrow_mut().push(name.into());
            if name == self.fail
                || (self.recovery_fails && name == "start previous")
                || (self.cleanup_fails && name == "unload")
            {
                Err(AppError::Storage(format!("fixture {name} refused")))
            } else {
                Ok(())
            }
        }
    }
    impl Runtime for Fake {
        fn registered(&self) -> Result<bool, AppError> {
            self.call("inspect")?;
            Ok(self.registered.get())
        }
        fn incumbent(&self) -> Result<Option<Incumbent>, AppError> {
            Ok(self.live.get())
        }
        fn drain(&self) -> Result<(), AppError> {
            self.call("drain")?;
            self.live.set(None);
            Ok(())
        }
        fn unload(&self) -> Result<(), AppError> {
            self.call("unload")?;
            self.registered.set(false);
            if self.live.get() == Some(Incumbent::Managed) {
                self.live.set(None);
            }
            Ok(())
        }
        fn register(&self) -> Result<(), AppError> {
            let bytes = fs::read_to_string(&self.path).unwrap();
            self.call(if bytes == "new" {
                "register new"
            } else {
                "register previous"
            })?;
            self.registered.set(true);
            Ok(())
        }
        fn start(&self, previous: bool) -> Result<(), AppError> {
            self.call(if previous {
                "start previous"
            } else {
                "start new"
            })?;
            self.live.set(Some(Incumbent::Managed));
            Ok(())
        }
        fn restore_unmanaged(&self) -> Result<(), AppError> {
            assert!(!self.registered.get() && !self.path.exists());
            self.call("restore unmanaged")?;
            self.live.set(Some(Incumbent::Unmanaged));
            Ok(())
        }
    }
    fn fixture(dir: &Path, prior: Option<Incumbent>, fail: &'static str) -> Fake {
        let path = dir.join("agent.plist");
        if prior == Some(Incumbent::Managed) {
            fs::write(&path, "previous").unwrap();
        }
        Fake {
            registered: Cell::new(prior == Some(Incumbent::Managed)),
            live: Cell::new(prior),
            fail,
            recovery_fails: false,
            cleanup_fails: false,
            calls: RefCell::new(vec![]),
            path,
        }
    }
    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir_in("/private/tmp").unwrap()
    }

    #[test]
    fn success_requires_drain_registration_and_health() {
        for prior in [None, Some(Incumbent::Managed), Some(Incumbent::Unmanaged)] {
            let dir = scratch();
            let f = fixture(dir.path(), prior, "");
            apply(&f.path, "new", &f).unwrap();
            assert_eq!(f.live.get(), Some(Incumbent::Managed));
            assert_eq!(fs::read_to_string(&f.path).unwrap(), "new");
            let calls = f.calls.borrow();
            assert_eq!(calls.last().unwrap(), "start new");
            if let Some(owner) = prior {
                let drain = calls.iter().position(|c| c == "drain").unwrap();
                let register = calls.iter().position(|c| c == "register new").unwrap();
                assert_eq!(drain < register, owner == Incumbent::Managed, "{calls:?}");
            }
        }
    }

    #[test]
    fn failed_replacement_restores_a_running_managed_daemon() {
        for fail in ["register new", "start new"] {
            let dir = scratch();
            let f = fixture(dir.path(), Some(Incumbent::Managed), fail);
            let error = apply(&f.path, "new", &f).unwrap_err().to_string();
            assert!(
                error.contains(fail) && error.contains("restored and healthy"),
                "{error}"
            );
            assert_eq!(fs::read_to_string(&f.path).unwrap(), "previous");
            assert_eq!(f.live.get(), Some(Incumbent::Managed));
            assert_eq!(f.calls.borrow().last().unwrap(), "start previous");
        }
    }

    #[test]
    fn fork_survives_registration_refusal_and_recovers_after_health_failure() {
        for fail in ["register new", "start new"] {
            let dir = scratch();
            let f = fixture(dir.path(), Some(Incumbent::Unmanaged), fail);
            apply(&f.path, "new", &f).unwrap_err();
            assert!(!f.path.exists());
            assert!(!f.registered.get());
            assert_eq!(f.live.get(), Some(Incumbent::Unmanaged));
            assert_eq!(
                f.calls.borrow().iter().any(|c| c == "drain"),
                fail == "start new"
            );
        }
    }

    #[test]
    fn failed_fresh_install_removes_the_partial_registration_and_plist() {
        let dir = scratch();
        let f = fixture(dir.path(), None, "start new");
        apply(&f.path, "new", &f).unwrap_err();
        assert!(!f.path.exists() && !f.registered.get());
        assert_eq!(f.live.get(), None);
        assert!(!f.calls.borrow().iter().any(|c| c == "restore unmanaged"));
    }

    #[test]
    fn recovery_failure_preserves_both_causes_and_does_not_claim_health() {
        let dir = scratch();
        let mut f = fixture(dir.path(), Some(Incumbent::Managed), "start new");
        f.recovery_fails = true;
        let error = apply(&f.path, "new", &f).unwrap_err().to_string();
        assert!(
            error.contains("start new refused") && error.contains("start previous refused"),
            "{error}"
        );
        assert!(!error.contains("restored and healthy"), "{error}");
    }

    #[test]
    fn unreadable_previous_definition_has_no_side_effects() {
        let dir = scratch();
        let f = fixture(dir.path(), None, "");
        fs::create_dir(&f.path).unwrap();
        let error = apply(&f.path, "new", &f).unwrap_err().to_string();
        assert!(error.contains("before installation"), "{error}");
        assert!(f.path.is_dir() && f.calls.borrow().is_empty());
    }

    #[test]
    fn refused_cleanup_keeps_both_errors_and_never_starts_a_fallback() {
        let dir = scratch();
        let mut f = fixture(dir.path(), Some(Incumbent::Unmanaged), "start new");
        f.cleanup_fails = true;
        let error = apply(&f.path, "new", &f).unwrap_err().to_string();
        assert!(
            error.contains("start new refused") && error.contains("unload refused"),
            "{error}"
        );
        assert_eq!(fs::read_to_string(&f.path).unwrap(), "new");
        assert!(f.registered.get());
        assert!(!f.calls.borrow().iter().any(|c| c == "restore unmanaged"));
    }

    #[test]
    fn a_loaded_job_without_a_recoverable_definition_is_untouched() {
        let dir = scratch();
        let f = fixture(dir.path(), None, "");
        f.registered.set(true);
        let error = apply(&f.path, "new", &f).unwrap_err().to_string();
        assert!(error.contains("cannot preserve"), "{error}");
        assert!(!f.path.exists());
        assert_eq!(*f.calls.borrow(), ["inspect"]);
    }

    #[test]
    fn registration_inspection_failure_preserves_the_old_service() {
        let dir = scratch();
        let f = fixture(dir.path(), Some(Incumbent::Managed), "inspect");
        apply(&f.path, "new", &f).unwrap_err();
        assert_eq!(fs::read_to_string(&f.path).unwrap(), "previous");
        assert_eq!(f.live.get(), Some(Incumbent::Managed));
        assert_eq!(*f.calls.borrow(), ["inspect"]);
    }

    #[test]
    fn a_drain_refusal_preserves_the_incumbent_and_previous_definition() {
        let dir = scratch();
        let f = fixture(dir.path(), Some(Incumbent::Managed), "drain");
        apply(&f.path, "new", &f).unwrap_err();
        assert_eq!(f.live.get(), Some(Incumbent::Managed));
        assert_eq!(fs::read_to_string(&f.path).unwrap(), "previous");
        assert!(
            !f.calls
                .borrow()
                .iter()
                .any(|c| c.starts_with("register") || c == "unload")
        );
    }
}
