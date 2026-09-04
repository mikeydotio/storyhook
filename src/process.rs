//! Shared bounded subprocess capture.
//!
//! Callers own timeout policy and result classification. This module owns the
//! descriptor and process-lifetime invariants: output goes to regular
//! temporary files, every child gets its own process group, and a timeout
//! reaps that whole group before captured bytes are read.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

/// Bounds diagnostics from a faulty subprocess.
const MAX_CAPTURE_BYTES: u64 = 64 * 1024;

/// The completed subprocess and its bounded captured output.
pub(crate) struct Captured {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

/// A failure to stage, start, wait for, or finish a bounded subprocess.
pub(crate) enum CaptureError {
    Stage(std::io::Error),
    Spawn(std::io::Error),
    Wait(std::io::Error),
    Timeout,
}

impl CaptureError {
    /// A stable human-readable description for callers adding context.
    pub(crate) fn detail(&self) -> String {
        match self {
            Self::Stage(error) | Self::Spawn(error) | Self::Wait(error) => error.to_string(),
            Self::Timeout => "the process timed out".to_string(),
        }
    }
}

/// Runs a command with file-backed capture and an absolute deadline.
pub(crate) fn run_captured(
    mut command: Command,
    timeout: Duration,
) -> Result<Captured, CaptureError> {
    let stdout_file = tempfile::tempfile().map_err(CaptureError::Stage)?;
    let stderr_file = tempfile::tempfile().map_err(CaptureError::Stage)?;
    let child_stdout = stdout_file.try_clone().map_err(CaptureError::Stage)?;
    let child_stderr = stderr_file.try_clone().map_err(CaptureError::Stage)?;
    command
        .stdin(Stdio::null())
        .stdout(child_stdout)
        .stderr(child_stderr);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn().map_err(CaptureError::Spawn)?;
    let pid = child.id();
    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        Ok(None) => {
            kill_process_group(pid);
            let _ = child.wait();
            return Err(CaptureError::Timeout);
        }
        Err(error) => {
            kill_process_group(pid);
            let _ = child.wait();
            return Err(CaptureError::Wait(error));
        }
    };
    Ok(Captured {
        status,
        stdout: read_capture(stdout_file),
        stderr: read_capture(stderr_file),
    })
}

fn kill_process_group(pid: u32) {
    #[cfg(unix)]
    // SAFETY: this is the process group created for the child immediately
    // above; it has not been reaped and therefore cannot have been recycled.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

/// Reads one capture file from its beginning, bounded for diagnostics.
pub(crate) fn read_capture(mut file: File) -> Vec<u8> {
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(0)).is_ok() {
        let _ = file.take(MAX_CAPTURE_BYTES).read_to_end(&mut bytes);
    }
    bytes
}
