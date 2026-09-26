//! Controller exclusion ends on return; destructive children keep separate workspace ownership.
use fs4::FileExt;
use std::fs::File;
use std::path::Path;

/// Releases controller ownership despite incidental copies between fork and exec.
/// Destructive children retain their separate WorkspaceLock until their effects stop.
pub(crate) struct ExecutorLock<'a> {
    file: &'a File,
    path: &'a Path,
}

impl<'a> ExecutorLock<'a> {
    /// Locks a controller file whose owner keeps it alive through this guard.
    pub(crate) fn acquire(file: &'a File, path: &'a Path) -> std::io::Result<Self> {
        file.try_lock_exclusive()?;
        Ok(Self { file, path })
    }
}

impl Drop for ExecutorLock<'_> {
    fn drop(&mut self) {
        // CLOEXEC does not close copies between another thread's fork and exec.
        // Controller exclusion must end on return; workspace ownership must not.
        if let Err(error) = FileExt::unlock(self.file) {
            crate::daemon::activity::emit(
                "ERROR",
                "reset",
                "event",
                &self.path.display().to_string(),
                &format!("could not release controller lock: {error}"),
            );
        }
    }
}
