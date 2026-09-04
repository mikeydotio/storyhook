//! Shared bounded subprocess capture.
//!
//! Callers own timeout policy and result classification. This module owns the
//! descriptor and process-lifetime invariants: output goes to regular
//! temporary files, every child gets its own process group, and a timeout
//! reaps that whole group before captured bytes are read.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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
    Timeout(TimeoutTermination),
}

impl CaptureError {
    /// A stable human-readable description for callers adding context.
    pub(crate) fn detail(&self) -> String {
        match self {
            Self::Stage(error) | Self::Spawn(error) | Self::Wait(error) => error.to_string(),
            Self::Timeout(_) => "the process timed out".to_string(),
        }
    }
}

/// How a timed-out process group should be stopped.
#[derive(Clone, Copy)]
pub(crate) enum TerminationPolicy {
    /// Kill the group immediately.
    Kill,
    /// Ask the group to terminate, then kill survivors after `grace`.
    TerminateThenKill { grace: Duration },
}

/// What happened after a subprocess reached its deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TimeoutTermination {
    /// The caller requested immediate process-group termination.
    Killed,
    /// Every process exited after the group received `SIGTERM`.
    ExitedAfterTerminate,
    /// At least one process survived the grace period and received `SIGKILL`.
    KilledAfterTerminate,
}

/// Runs a command with file-backed capture and an absolute deadline.
pub(crate) fn run_captured(command: Command, timeout: Duration) -> Result<Captured, CaptureError> {
    run_captured_with_termination(command, timeout, TerminationPolicy::Kill)
}

/// Runs a command with file-backed capture and caller-selected termination.
pub(crate) fn run_captured_with_termination(
    mut command: Command,
    timeout: Duration,
    termination: TerminationPolicy,
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
            let outcome = terminate_timed_out(&mut child, pid, termination);
            return Err(CaptureError::Timeout(outcome));
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

fn terminate_timed_out(
    child: &mut std::process::Child,
    pid: u32,
    policy: TerminationPolicy,
) -> TimeoutTermination {
    match policy {
        TerminationPolicy::Kill => {
            kill_process_group(pid);
            let _ = child.wait();
            TimeoutTermination::Killed
        }
        TerminationPolicy::TerminateThenKill { grace } => {
            terminate_process_group(pid);
            let deadline = Instant::now() + grace;
            let mut leader_reaped = false;
            loop {
                if !leader_reaped {
                    match child.try_wait() {
                        Ok(Some(_)) => leader_reaped = true,
                        Ok(None) => {}
                        Err(_) => break,
                    }
                }
                if !process_group_is_live(pid) {
                    return TimeoutTermination::ExitedAfterTerminate;
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                thread::sleep(remaining.min(Duration::from_millis(10)));
            }
            kill_process_group(pid);
            if !leader_reaped {
                let _ = child.wait();
            }
            TimeoutTermination::KilledAfterTerminate
        }
    }
}

fn terminate_process_group(pid: u32) {
    #[cfg(unix)]
    // SAFETY: the group id belongs to the child created by this module and
    // remains live, so it cannot have been recycled onto an unrelated group.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGTERM);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

fn process_group_is_live(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // SAFETY: signal 0 changes no process state; it only asks whether the
        // group still has a member this process can address.
        let result = unsafe { libc::kill(-(pid as i32), 0) };
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graceful_timeout_allows_the_process_group_to_exit_on_term() {
        let marker = storyhook_test_support::scratch_dir();
        let marker = marker.path().join("terminated");
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "trap 'printf terminated > \"$1\"; exit 0' TERM; while :; do sleep 30; done",
            "graceful-timeout-probe",
            marker.to_str().unwrap(),
        ]);

        let result = run_captured_with_termination(
            command,
            Duration::from_millis(20),
            TerminationPolicy::TerminateThenKill {
                grace: Duration::from_secs(1),
            },
        );

        assert!(matches!(
            result,
            Err(CaptureError::Timeout(
                TimeoutTermination::ExitedAfterTerminate
            ))
        ));
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "terminated");
    }
}
