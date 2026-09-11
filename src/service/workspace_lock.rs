//! Cross-process exclusion for one story's workspace operations.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::error::AppError;
use crate::process::{Captured, run_captured};
use fs4::FileExt;

/// Closing the last inherited descriptor releases ownership, even after a crash.
/// Never explicitly unlock: an orphaned cleanup child must retain the lock.
pub(crate) struct WorkspaceLock(File);

impl WorkspaceLock {
    /// Acquires the same nonblocking lock that dispatch holds through handoff.
    pub(crate) fn acquire(checkout: &Path, id: &str) -> Result<Self, AppError> {
        let common = git(
            checkout,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            None,
        )?;
        Self::at(
            &PathBuf::from(common.trim()).join("storyhook/workspace-locks"),
            id,
        )
    }

    fn at(directory: &Path, id: &str) -> Result<Self, AppError> {
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
            return Err(AppError::Validation(format!(
                "{id} workspace is busy with dispatch, verification, or reset; retry after that operation exits"
            )));
        }
        Ok(Self(file))
    }

    /// Pass existing verifier ownership to its repair dispatch.
    pub(crate) fn dispatch_command(&self, command: &mut Command) {
        self.command(command);
        command.env("STORY_WORKSPACE_LOCK_FD", self.0.as_raw_fd().to_string());
    }

    /// Inherit ownership only into the guarded command, not all daemon children.
    pub(crate) fn command(&self, command: &mut Command) {
        let fd = self.0.as_raw_fd();
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
}

/// Runs one bounded command while its children retain workspace ownership.
pub(crate) fn capture(
    mut command: Command,
    lock: Option<&WorkspaceLock>,
) -> Result<Captured, AppError> {
    if let Some(lock) = lock {
        lock.command(&mut command);
    }
    run_captured(command, Duration::from_secs(30)).map_err(|error| {
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
    let output = capture(command, lock)?;
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
