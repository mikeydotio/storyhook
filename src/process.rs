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

#[cfg(test)]
mod activity_tests;
mod progress;

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
    Track(String),
    Timeout(TimeoutTermination),
}

impl CaptureError {
    /// A stable human-readable description for callers adding context.
    pub(crate) fn detail(&self) -> String {
        match self {
            Self::Stage(error) | Self::Spawn(error) | Self::Wait(error) => error.to_string(),
            Self::Track(error) => error.clone(),
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
    command: Command,
    timeout: Duration,
    termination: TerminationPolicy,
) -> Result<Captured, CaptureError> {
    run_captured_with_registration(command, timeout, termination, |_| Ok(()))
}

/// Runs a command with capture while retaining a caller-owned registration
/// guard for the child's complete lifetime.
pub(crate) fn run_captured_with_registration<G>(
    command: Command,
    timeout: Duration,
    termination: TerminationPolicy,
    register: impl FnOnce(u32) -> Result<G, String>,
) -> Result<Captured, CaptureError> {
    let deadline = Instant::now() + timeout;
    run_captured_until(command, termination, None, register, || {
        Ok(deadline.saturating_duration_since(Instant::now()))
    })
}

/// Runs a command until its append-only journal stops advancing for `timeout`.
/// Output chatter is deliberately not progress. An unreadable or damaged
/// journal fails closed with the caller's ordinary process-group cleanup.
pub(crate) fn run_captured_with_progress_and_registration<G>(
    command: Command,
    timeout: Duration,
    termination: TerminationPolicy,
    journal: &std::path::Path,
    register: impl FnOnce(u32) -> Result<G, String>,
) -> Result<Captured, CaptureError> {
    let mut deadline =
        progress::IdleDeadline::new(journal, timeout).map_err(CaptureError::Stage)?;
    // Observe at least four times per idle window, capped to keep journal
    // activity responsive even for the production multi-minute budget.
    let poll = (timeout / 4).min(Duration::from_millis(100));
    let captured = run_captured_until(command, termination, Some(poll), register, || {
        deadline.remaining()
    })?;
    // A child can damage the journal and exit inside one poll interval. Check
    // once more after reaping so a fast successful result cannot hide that.
    deadline.remaining().map_err(CaptureError::Wait)?;
    Ok(captured)
}

fn run_captured_until<G>(
    mut command: Command,
    termination: TerminationPolicy,
    poll: Option<Duration>,
    register: impl FnOnce(u32) -> Result<G, String>,
    mut remaining: impl FnMut() -> std::io::Result<Duration>,
) -> Result<Captured, CaptureError> {
    let source = crate::daemon::activity::command_source(&command);
    crate::daemon::activity::configure(&mut command);
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
    let context = format!("child={pid}");
    crate::daemon::activity::emit("INFO", &source, "event", &context, "process started");
    let observer = crate::daemon::activity::OutputWatch::capture(
        &source,
        &context,
        &stdout_file,
        &stderr_file,
    );
    // Registration owns the child's process group for the daemon's shutdown
    // and identity checks, and it reads that group from the kernel AFTER the
    // spawn. A child that has already exited by then is a zombie, and macOS
    // answers `getpgid` on a zombie with ESRCH (measured, SH-650) — so a
    // helper that refused within a few milliseconds used to be reported as
    // "could not run" and its answer thrown away. A leader that is gone
    // before it could be owned has nothing long-lived to register: the
    // failure is explained by the exit, and the capture proceeds exactly as
    // it would had the registration guard dropped one instant after the
    // child's own exit. Any other registration failure is still fatal.
    let _registration = match register(pid) {
        Ok(registration) => Some(registration),
        Err(error) => match child.try_wait() {
            Ok(Some(_)) => None,
            _ => {
                kill_process_group(pid);
                let _ = child.wait();
                return Err(CaptureError::Track(error));
            }
        },
    };
    let status = loop {
        let budget = match remaining() {
            Ok(budget) => budget,
            Err(error) => {
                terminate_timed_out(&mut child, pid, termination);
                return Err(CaptureError::Wait(error));
            }
        };
        match child.wait_timeout(poll.map_or(budget, |poll| budget.min(poll))) {
            Ok(Some(status)) => break status,
            Ok(None) if !budget.is_zero() => continue,
            Ok(None) => {
                let outcome = terminate_timed_out(&mut child, pid, termination);
                crate::daemon::activity::emit(
                    "ERROR",
                    &source,
                    "event",
                    &context,
                    "process timed out; group terminated",
                );
                return Err(CaptureError::Timeout(outcome));
            }
            Err(error) => {
                kill_process_group(pid);
                let _ = child.wait();
                return Err(CaptureError::Wait(error));
            }
        }
    };
    drop(observer);
    crate::daemon::activity::emit(
        if status.success() { "INFO" } else { "ERROR" },
        &source,
        "event",
        &context,
        &format!("process finished: {status}"),
    );
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

    /// SH-650: a child that exits before its process group can be read is
    /// captured, not reported as untrackable. The race is CONSTRUCTED (SH-420's
    /// posture): the registration closure waits until the kernel no longer
    /// answers for the leader, then asks the real registry, which must fail
    /// exactly the way it failed in the wild; the capture must still carry
    /// the child's answer.
    #[test]
    fn a_child_that_exits_before_registration_is_still_captured() {
        let root = storyhook_test_support::scratch_dir();
        let env = crate::env::Environment::at(root.path());
        std::fs::create_dir_all(env.daemon_state_dir()).unwrap();
        let owned = crate::daemon::lifecycle::OwnedProcesses::new(env);
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("printf '{\"ok\":false,\"reason\":\"pane-dead\"}'");
        let registration_error = std::sync::Mutex::new(None);

        let captured = run_captured_with_registration(
            command,
            Duration::from_secs(10),
            TerminationPolicy::Kill,
            |pid| {
                let deadline = Instant::now() + Duration::from_secs(5);
                // A zombie no longer answers `getpgid`; that is the condition
                // the real registration trips over.
                while unsafe { libc::getpgid(libc::pid_t::try_from(pid).unwrap()) } != -1 {
                    assert!(Instant::now() < deadline, "the child never exited");
                    thread::sleep(Duration::from_millis(2));
                }
                let result = owned
                    .register("verifier-notify", pid, Some("verify:fixture:SH-1:1"))
                    .map_err(|error| error.to_string());
                *registration_error.lock().unwrap() = result.as_ref().err().cloned();
                result
            },
        );
        let captured = match captured {
            Ok(captured) => captured,
            Err(error) => panic!(
                "an exited child is captured, never reported as untrackable: {}",
                error.detail()
            ),
        };

        assert!(captured.status.success());
        assert_eq!(
            String::from_utf8_lossy(&captured.stdout),
            "{\"ok\":false,\"reason\":\"pane-dead\"}"
        );
        let error = registration_error.lock().unwrap().clone();
        assert!(
            error
                .as_deref()
                .is_some_and(|error| error.contains("could not read process group")),
            "positive control: the registry must have refused the zombie the way it did in the wild, got {error:?}"
        );
    }

    #[test]
    fn graceful_timeout_allows_the_process_group_to_exit_on_term() {
        let root = storyhook_test_support::scratch_dir();
        let marker = root.path().join("terminated");
        let ready = root.path().join("ready");
        let grace = Duration::from_secs(1);
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "trap 'printf terminated > \"$1\"; exit 0' TERM; printf ready > \"$2\"; while :; do sleep 30; done",
            "graceful-timeout-probe",
            marker.to_str().unwrap(),
            ready.to_str().unwrap(),
        ]);

        let result = run_captured_with_registration(
            command,
            Duration::ZERO,
            TerminationPolicy::TerminateThenKill { grace },
            |_| {
                // A timeout may include spawn/staging time. Only ask whether
                // TERM permits cleanup after the child has installed its trap.
                let deadline = Instant::now() + grace;
                while !ready.exists() {
                    if Instant::now() >= deadline {
                        return Err("timeout probe did not install its TERM handler".into());
                    }
                    thread::sleep(grace / 100);
                }
                Ok(())
            },
        );

        match result {
            Err(CaptureError::Timeout(TimeoutTermination::ExitedAfterTerminate)) => {}
            Err(error) => panic!("graceful timeout failed: {}", error.detail()),
            Ok(_) => panic!("the probe exited without reaching its timeout"),
        }
        assert_eq!(std::fs::read_to_string(marker).unwrap(), "terminated");
    }
}
