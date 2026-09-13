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
mod cancellation;
mod progress;
pub use cancellation::Cancellation;

/// Bounds diagnostics from a faulty subprocess.
const MAX_CAPTURE_BYTES: u64 = 64 * 1024;

/// The completed subprocess and its bounded captured output.
pub(crate) struct Captured {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

/// A capture error with any bounded answer collected after process cleanup.
pub(crate) struct CaptureFailure {
    /// The process-lifetime failure, retained independently of its answer.
    pub(crate) error: CaptureError,
    /// Output available after termination; callers must validate it as evidence.
    pub(crate) stdout: Vec<u8>,
}

impl From<CaptureError> for CaptureFailure {
    fn from(error: CaptureError) -> Self {
        Self {
            error,
            stdout: Vec::new(),
        }
    }
}

impl CaptureFailure {
    fn after(error: CaptureError, stdout: File) -> Self {
        Self {
            error,
            stdout: read_capture(stdout),
        }
    }
}

/// A failure to stage, start, wait for, or finish a bounded subprocess.
pub(crate) enum CaptureError {
    Stage(std::io::Error),
    Spawn(std::io::Error),
    Wait(std::io::Error),
    Track(String),
    Timeout(TimeoutTermination),
    Cancelled,
}

impl CaptureError {
    /// A stable human-readable description for callers adding context.
    pub(crate) fn detail(&self) -> String {
        match self {
            Self::Stage(error) | Self::Spawn(error) | Self::Wait(error) => error.to_string(),
            Self::Track(error) => error.clone(),
            Self::Cancelled => "the operator cancelled verification".to_string(),
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
    run_captured_until(command, termination, None, None, None, register, || {
        Ok(deadline.saturating_duration_since(Instant::now()))
    })
    .map_err(|failure| failure.error)
}

/// Runs a command with owner-observed cancellation and bounded group cleanup.
pub(crate) fn run_captured_cancellable<G>(
    command: Command,
    timeout: Duration,
    termination: TerminationPolicy,
    cancellation: &Cancellation,
    register: impl FnOnce(u32) -> Result<G, String>,
) -> Result<Captured, CaptureError> {
    let deadline = Instant::now() + timeout;
    run_captured_until(
        command,
        termination,
        None,
        None,
        Some(cancellation),
        register,
        || Ok(deadline.saturating_duration_since(Instant::now())),
    )
    .map_err(|failure| failure.error)
}

/// Runs a command until its append-only journal stops advancing for `timeout`.
/// Output chatter is deliberately not progress. An unreadable or damaged
/// journal fails closed with the caller's ordinary process-group cleanup.
pub(crate) fn run_captured_with_progress_and_registration<G>(
    command: Command,
    timeout: Duration,
    termination: TerminationPolicy,
    journal: &std::path::Path,
    cancellation: &Cancellation,
    register: impl FnOnce(u32) -> Result<G, String>,
) -> Result<Captured, CaptureFailure> {
    let mut deadline =
        progress::IdleDeadline::new(journal, timeout).map_err(CaptureError::Stage)?;
    // Observe at least four times per idle window, capped to keep journal
    // activity responsive even for the production multi-minute budget.
    let poll = (timeout / 4).min(Duration::from_millis(100));
    let captured = run_captured_until(
        command,
        termination,
        None,
        Some(poll),
        Some(cancellation),
        register,
        || deadline.remaining(),
    )?;
    // A child can damage the journal and exit inside one poll interval. Check
    // once more after reaping so a fast successful result cannot hide that.
    if let Err(error) = deadline.remaining() {
        return Err(CaptureFailure {
            error: CaptureError::Wait(error),
            stdout: captured.stdout,
        });
    }
    Ok(captured)
}

/// Runs a bounded subprocess with staged, file-backed standard input.
pub(crate) fn run_captured_with_input(
    command: Command,
    input: File,
    timeout: Duration,
) -> Result<Captured, CaptureError> {
    let deadline = Instant::now() + timeout;
    run_captured_until(
        command,
        TerminationPolicy::Kill,
        Some(input),
        None,
        None,
        |_| Ok(()),
        || Ok(deadline.saturating_duration_since(Instant::now())),
    )
    .map_err(|failure| failure.error)
}

fn run_captured_until<G>(
    mut command: Command,
    termination: TerminationPolicy,
    input: Option<File>,
    poll: Option<Duration>,
    cancellation: Option<&Cancellation>,
    register: impl FnOnce(u32) -> Result<G, String>,
    mut remaining: impl FnMut() -> std::io::Result<Duration>,
) -> Result<Captured, CaptureFailure> {
    if cancellation.is_some_and(Cancellation::is_cancelled) {
        return Err(CaptureError::Cancelled.into());
    }
    let poll = if cancellation.is_some() {
        Some(
            poll.unwrap_or(Duration::from_millis(100))
                .min(Duration::from_millis(100)),
        )
    } else {
        poll
    };
    let source = crate::daemon::activity::command_source(&command);
    crate::daemon::activity::configure(&mut command);
    let stdout_file = tempfile::tempfile().map_err(CaptureError::Stage)?;
    let stderr_file = tempfile::tempfile().map_err(CaptureError::Stage)?;
    let child_stdout = stdout_file.try_clone().map_err(CaptureError::Stage)?;
    let child_stderr = stderr_file.try_clone().map_err(CaptureError::Stage)?;
    command
        .stdin(input.map_or_else(Stdio::null, Stdio::from))
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
    // macOS removes an exiting child from process lookup before publishing
    // its wait status (SH-698). Either exit observation permits the ordinary
    // bounded wait/capture path; a live unregistered child still fails closed.
    let _registration = match register(pid) {
        Ok(registration) => Some(registration),
        Err(error) => {
            // Query before try_wait can reap: this PID still belongs to us.
            let group = child_process_group(pid);
            if registration_failure_is_exit(child.try_wait(), group) {
                None
            } else {
                kill_process_group(pid);
                let _ = child.wait();
                return Err(CaptureError::Track(error).into());
            }
        }
    };
    let status = loop {
        if cancellation.is_some_and(Cancellation::is_cancelled) {
            terminate_timed_out(&mut child, pid, termination);
            drop(observer);
            return Err(CaptureFailure::after(CaptureError::Cancelled, stdout_file));
        }
        let budget = match remaining() {
            Ok(budget) => budget,
            Err(error) => {
                terminate_timed_out(&mut child, pid, termination);
                drop(observer);
                return Err(CaptureFailure::after(
                    CaptureError::Wait(error),
                    stdout_file,
                ));
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
                drop(observer);
                return Err(CaptureFailure::after(
                    CaptureError::Timeout(outcome),
                    stdout_file,
                ));
            }
            Err(error) => {
                kill_process_group(pid);
                let _ = child.wait();
                drop(observer);
                return Err(CaptureFailure::after(
                    CaptureError::Wait(error),
                    stdout_file,
                ));
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

// Separate observations keep the macOS teardown window testable without
// requiring the scheduler to stop between kernel exit milestones.
fn registration_failure_is_exit(
    status: std::io::Result<Option<ExitStatus>>,
    group: std::io::Result<u32>,
) -> bool {
    match status {
        Ok(Some(_)) => true,
        Ok(None) => group.is_err_and(|error| error.raw_os_error() == Some(libc::ESRCH)),
        Err(_) => false,
    }
}

/// Reads the group of a child we still own without discarding lookup errors.
fn child_process_group(pid: u32) -> std::io::Result<u32> {
    #[cfg(unix)]
    {
        let pid = libc::pid_t::try_from(pid)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        // SAFETY: getpgid only observes kernel state and takes no pointers.
        let group = unsafe { libc::getpgid(pid) };
        if group == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(group as u32)
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
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
    fn registration_accepts_only_proven_exit_states() {
        use std::os::unix::process::ExitStatusExt;

        let absent = || Err(std::io::Error::from_raw_os_error(libc::ESRCH));
        assert!(registration_failure_is_exit(Ok(None), absent()));
        assert!(registration_failure_is_exit(
            Ok(Some(ExitStatus::from_raw(0))),
            absent(),
        ));
        assert!(registration_failure_is_exit(
            Ok(Some(ExitStatus::from_raw(1 << 8))),
            Ok(123),
        ));
        assert!(!registration_failure_is_exit(Ok(None), Ok(123)));
        assert!(!registration_failure_is_exit(
            Ok(None),
            Err(std::io::Error::from_raw_os_error(libc::EPERM)),
        ));
        assert!(!registration_failure_is_exit(
            Err(std::io::Error::from_raw_os_error(libc::ECHILD)),
            absent(),
        ));
    }

    #[test]
    fn a_live_child_registration_failure_remains_fatal() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 30"]);
        let result = run_captured_with_registration(
            command,
            Duration::from_secs(10),
            TerminationPolicy::Kill,
            |_| Err::<(), _>("registry write refused".into()),
        );
        match result {
            Err(CaptureError::Track(error)) => assert_eq!(error, "registry write refused"),
            Err(error) => panic!("wrong failure: {}", error.detail()),
            Ok(_) => panic!("a live unregistered child must not be accepted"),
        }
    }

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
            "trap 'printf terminated > \"$1\"; exit 0' TERM; printf ready > \"$2\"; while :; do sleep 30 & wait; done",
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
