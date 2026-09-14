//! Cross-process exclusion for one story's workspace operations.

use std::fs::{File, OpenOptions};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::error::AppError;
use crate::process::{Captured, TerminationPolicy, run_captured, run_captured_quiescent};
use fs4::FileExt;

/// Closing the last inherited descriptor releases ownership, even after a crash.
/// Never explicitly unlock: an orphaned cleanup child must retain the lock.
pub(crate) struct WorkspaceLock(File);

impl WorkspaceLock {
    /// Acquires the same nonblocking lock that dispatch holds through handoff.
    pub(crate) fn acquire(checkout: &Path, id: &str) -> Result<Self, AppError> {
        Self::try_acquire(checkout, id)?.ok_or_else(|| busy(id))
    }

    /// Returns no owner when another process retains this story's workspace.
    pub(crate) fn try_acquire(checkout: &Path, id: &str) -> Result<Option<Self>, AppError> {
        let common = git(
            checkout,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            None,
        )?;
        Self::try_at(
            &PathBuf::from(common.trim()).join("storyhook/workspace-locks"),
            id,
        )
    }

    #[cfg(test)]
    fn at(directory: &Path, id: &str) -> Result<Self, AppError> {
        Self::try_at(directory, id)?.ok_or_else(|| busy(id))
    }

    fn try_at(directory: &Path, id: &str) -> Result<Option<Self>, AppError> {
        if id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(AppError::Validation(
                "invalid workspace story identity".into(),
            ));
        }
        std::fs::create_dir_all(directory)?;
        let path = directory.join(format!("{id}.lock"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        if let Err(error) = file.try_lock_exclusive() {
            if error.kind() != std::io::ErrorKind::WouldBlock {
                return Err(error.into());
            }
            return Ok(None);
        }
        Ok(Some(Self(file)))
    }

    /// Pass existing verifier ownership to its repair dispatch.
    pub(crate) fn dispatch_command(&self, command: &mut Command) {
        self.command(command);
        command.env("STORY_WORKSPACE_LOCK_FD", self.0.as_raw_fd().to_string());
    }

    /// Borrows the owned descriptor for an external dispatcher handoff.
    pub(crate) fn descriptor(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }

    /// Inherit ownership only into the guarded command, not all daemon children.
    pub(crate) fn command(&self, command: &mut Command) {
        inherit_descriptor(self.descriptor(), command);
    }
}

/// Inherits an existing workspace owner without extending its Rust borrow.
/// The owner must remain alive until the command has been spawned.
pub(crate) fn inherit_descriptor(descriptor: BorrowedFd<'_>, command: &mut Command) {
    let fd = descriptor.as_raw_fd();
    // SAFETY: fcntl is async-signal-safe. Only the forked child's descriptor
    // flag changes; no allocator or parent-process global state is touched.
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

fn busy(id: &str) -> AppError {
    AppError::Validation(format!(
        "{id} workspace is busy with dispatch, verification, or reset; retry after that operation exits"
    ))
}

/// Runs one bounded command while its children retain workspace ownership.
pub(crate) fn capture(
    mut command: Command,
    lock: Option<&WorkspaceLock>,
) -> Result<Captured, AppError> {
    let result = if let Some(lock) = lock {
        lock.command(&mut command);
        run_captured_quiescent(command, Duration::from_secs(30), TerminationPolicy::Kill)
    } else {
        run_captured(command, Duration::from_secs(30))
    };
    result.map_err(|error| {
        AppError::Validation(format!("workspace command failed: {}", error.detail()))
    })
}

/// Runs Git with the repository environment allowlist and a bounded deadline.
pub(crate) fn git(
    checkout: &Path,
    args: &[&str],
    lock: Option<&WorkspaceLock>,
) -> Result<String, AppError> {
    let mut command = crate::env::git_env::command(checkout);
    command
        .current_dir(checkout)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = capture(command, lock).map_err(|error| {
        error.with_context(&format!("git {} in {}", args.join(" "), checkout.display()))
    })?;
    if !output.status.success() {
        return Err(AppError::Validation(format!(
            "git {} in {}: {}",
            args.join(" "),
            checkout.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| AppError::Validation(format!("git output is not UTF-8: {error}")))
}

#[cfg(test)]
mod tests;
