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

/// The plugin helpers' per-operation probe budget: `BUDGET_SECONDS` in the
/// `probe_budget.py` this binary embeds. Each caller that runs those helpers
/// pins its own bound against it (SH-766).
#[cfg(test)]
pub(crate) fn plugin_probe_budget() -> Duration {
    const SOURCE: &str = include_str!("../plugins/story/lib/probe_budget.py");
    let seconds = SOURCE
        .lines()
        .find_map(|line| line.strip_prefix("BUDGET_SECONDS = "))
        .expect("probe_budget.py must declare `BUDGET_SECONDS = <seconds>`");
    Duration::from_secs(
        seconds
            .trim()
            .parse()
            .expect("BUDGET_SECONDS must be whole seconds"),
    )
}

/// How long a killed orphan may stay visible to tests: it is a zombie until
/// the system reaps it, which happens after this module's own wait returns.
#[cfg(test)]
const ORPHAN_REAP_WAIT: Duration = Duration::from_secs(5);

/// Whether `pid` stops existing within [`ORPHAN_REAP_WAIT`]: the tests' proof
/// that a timeout stopped a process this module's group kill reached.
#[cfg(test)]
pub(crate) fn pid_disappears(pid: libc::pid_t) -> bool {
    let deadline =
        Instant::now() + storyhook_test_support::load_grace::graced_now(ORPHAN_REAP_WAIT);
    loop {
        // SAFETY: signal 0 only asks whether the pid exists.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Bounds diagnostics from a faulty subprocess.
const MAX_CAPTURE_BYTES: u64 = 64 * 1024;

/// The completed subprocess and its bounded captured output.
pub(crate) struct Captured {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    /// Whether the child wrote more stdout than its read bound, so `stdout`
    /// is a prefix. A caller that parses stdout as an answer must refuse a
    /// prefix rather than read it as a whole (SH-815).
    pub(crate) stdout_truncated: bool,
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
    /// Cleanup was requested, but remaining processes still own external effects.
    Unsettled(String),
}

impl CaptureError {
    /// A stable human-readable description for callers adding context.
    pub(crate) fn detail(&self) -> String {
        match self {
            Self::Stage(error) | Self::Spawn(error) | Self::Wait(error) => error.to_string(),
            Self::Track(error) | Self::Unsettled(error) => error.clone(),
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

/// Captures potentially sensitive output without mirroring it to activity logs.
pub(crate) fn run_captured_private(
    command: Command,
    timeout: Duration,
) -> Result<Captured, CaptureError> {
    let deadline = Instant::now() + timeout;
    run_captured_until(
        command,
        TerminationPolicy::Kill,
        None,
        CaptureWait {
            private_output: true,
            ..CaptureWait::default()
        },
        None,
        |_| Ok(()),
        || Ok(deadline.saturating_duration_since(Instant::now())),
    )
    .map_err(|failure| failure.error)
}

/// Credential-private capture under an existing operation deadline and owner
/// cancellation. Sub-queries cannot renew the caller's remaining budget.
pub(crate) fn run_captured_private_until(
    command: Command,
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
) -> Result<Captured, CaptureError> {
    run_captured_until(
        command,
        TerminationPolicy::Kill,
        None,
        CaptureWait {
            private_output: true,
            ..CaptureWait::default()
        },
        Some(cancelled),
        |_| Ok(()),
        || Ok(deadline.saturating_duration_since(Instant::now())),
    )
    .map_err(|failure| failure.error)
}

/// Bounded credential-private capture with a regular-file request, never a pipe
/// that descendants can keep open after the owning helper exits.
pub(crate) fn run_captured_private_input_until(
    command: Command,
    input: Option<std::fs::File>,
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
) -> Result<Captured, CaptureError> {
    if cancelled() {
        return Err(CaptureError::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(CaptureError::Wait(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "private input deadline expired before starting a child",
        )));
    }
    run_captured_until(
        command,
        TerminationPolicy::Kill,
        input,
        CaptureWait {
            private_output: true,
            quiescent: true,
            ..CaptureWait::default()
        },
        Some(cancelled),
        |_| Ok(()),
        || Ok(deadline.saturating_duration_since(Instant::now())),
    )
    .map_err(|failure| failure.error)
}

/// Journals a supervisory child only when it fails (SH-761): no start
/// record, no output mirroring, and a finish record only for a non-zero
/// exit. A periodic reconcile whose success is the steady state would
/// otherwise fill the very window it keeps alive. Timeouts still record an
/// ERROR, and the caller keeps the captured stderr for its own report.
/// For a child with no owner stop to observe, such as the journal hygiene
/// sweep's bounded `git ls-files` (SH-771).
pub(crate) fn run_captured_quiet(
    command: Command,
    timeout: Duration,
) -> Result<Captured, CaptureError> {
    run_captured_quiet_cancellable(command, timeout, || false)
}

/// Captures a supervisor helper while observing its owner's stop signal.
/// Cancellation is quiet and reaps the same process group as deadline expiry.
pub(crate) fn run_captured_quiet_cancellable(
    command: Command,
    timeout: Duration,
    cancelled: impl Fn() -> bool,
) -> Result<Captured, CaptureError> {
    let deadline = Instant::now() + timeout;
    run_captured_until(
        command,
        TerminationPolicy::Kill,
        None,
        CaptureWait {
            private_output: true,
            failures_only: true,
            ..CaptureWait::default()
        },
        Some(&cancelled),
        |_| Ok(()),
        || Ok(deadline.saturating_duration_since(Instant::now())),
    )
    .map_err(|failure| failure.error)
}

/// Runs a command with file-backed capture and caller-selected termination.
pub(crate) fn run_captured_with_termination(
    command: Command,
    timeout: Duration,
    termination: TerminationPolicy,
) -> Result<Captured, CaptureError> {
    run_captured_with_registration(command, timeout, termination, |_| Ok(()))
}

/// Captures a helper only after its complete process group has stopped.
/// A successful leader cannot acknowledge work that surviving children can still change.
pub(crate) fn run_captured_quiescent(
    command: Command,
    timeout: Duration,
    termination: TerminationPolicy,
) -> Result<Captured, CaptureError> {
    let deadline = Instant::now() + timeout;
    run_captured_quiescent_until(command, termination, || {
        Ok(deadline.saturating_duration_since(Instant::now()))
    })
    .map_err(|failure| failure.error)
}

/// Shares group-quiescence routing with a caller-owned deadline. The production
/// wrapper uses one absolute deadline; tests can establish readiness first.
fn run_captured_quiescent_until(
    command: Command,
    termination: TerminationPolicy,
    remaining: impl FnMut() -> std::io::Result<Duration>,
) -> Result<Captured, CaptureFailure> {
    run_captured_until(
        command,
        termination,
        None,
        CaptureWait {
            quiescent: true,
            ..CaptureWait::default()
        },
        None,
        |_| Ok(()),
        remaining,
    )
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
    run_captured_until(
        command,
        termination,
        None,
        CaptureWait::default(),
        None,
        register,
        || Ok(deadline.saturating_duration_since(Instant::now())),
    )
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
        CaptureWait::default(),
        Some(&|| cancellation.is_cancelled()),
        register,
        || Ok(deadline.saturating_duration_since(Instant::now())),
    )
    .map_err(|failure| failure.error)
}

/// A bounded native effect retains its registration through complete owned
/// process-group settlement. Both pre-spawn checks and drain spend the same
/// absolute operation deadline; an authority change cancels rather than renews it.
pub(crate) fn run_captured_owned_quiescent_until<G>(
    command: Command,
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
    register: impl FnOnce(u32) -> Result<G, String>,
) -> Result<Captured, CaptureError> {
    if cancelled() {
        return Err(CaptureError::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(CaptureError::Wait(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "owned effect deadline expired before starting a child",
        )));
    }
    run_captured_until(
        command,
        TerminationPolicy::Kill,
        None,
        CaptureWait {
            quiescent: true,
            private_output: true,
            ..CaptureWait::default()
        },
        Some(&|| Instant::now() >= deadline || cancelled()),
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
        CaptureWait {
            poll: Some(poll),
            ..CaptureWait::default()
        },
        Some(&|| cancellation.is_cancelled()),
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

/// Runs a command whose standard output is its answer rather than a
/// diagnostic (SH-815). It is journaled like [`run_captured_private`] (its
/// start, timeout and finish, never its output), and at `timeout` its whole
/// process group is stopped under `termination`. Its stdout is read up to
/// `answer_limit` bytes instead of the diagnostic bound, and
/// [`Captured::stdout_truncated`] reports a longer answer.
pub(crate) fn run_captured_answer(
    command: Command,
    timeout: Duration,
    termination: TerminationPolicy,
    answer_limit: u64,
) -> Result<Captured, CaptureError> {
    let deadline = Instant::now() + timeout;
    run_captured_until(
        command,
        termination,
        None,
        CaptureWait {
            private_output: true,
            stdout_limit: Some(answer_limit),
            ..CaptureWait::default()
        },
        None,
        |_| Ok(()),
        || Ok(deadline.saturating_duration_since(Instant::now())),
    )
    .map_err(|failure| failure.error)
}

/// Runs a query: a command whose stdout is its answer and whose exit codes in
/// `answers` are answers too (`git merge-tree` exits 1 for a conflict). It is
/// journaled only when it fails otherwise (SH-761), its stdout is read up to
/// `answer_limit` bytes with a cut reported (SH-815), and its whole process
/// group is killed at `timeout` or as soon as `cancelled` answers true.
pub(crate) fn run_captured_query(
    command: Command,
    timeout: Duration,
    cancelled: &dyn Fn() -> bool,
    answer_limit: u64,
    answers: &'static [i32],
) -> Result<Captured, CaptureError> {
    let deadline = Instant::now() + timeout;
    run_captured_until(
        command,
        TerminationPolicy::Kill,
        None,
        CaptureWait {
            private_output: true,
            failures_only: true,
            stdout_limit: Some(answer_limit),
            answers,
            ..CaptureWait::default()
        },
        Some(cancelled),
        |_| Ok(()),
        || Ok(deadline.saturating_duration_since(Instant::now())),
    )
    .map_err(|failure| failure.error)
}

/// A private query whose owned process group must settle before success.
/// The absolute caller deadline includes setup and descendant drain; it is
/// never renewed when the leader exits. This is process-group custody, not
/// proof about a descendant that deliberately escapes into another session.
pub(crate) fn run_captured_query_quiescent(
    command: Command,
    deadline: Instant,
    cancelled: &dyn Fn() -> bool,
    answer_limit: u64,
    answers: &'static [i32],
) -> Result<Captured, CaptureError> {
    if cancelled() {
        return Err(CaptureError::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(CaptureError::Wait(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "query deadline expired before starting a child",
        )));
    }
    run_captured_until(
        command,
        TerminationPolicy::Kill,
        None,
        CaptureWait {
            quiescent: true,
            private_output: true,
            failures_only: true,
            stdout_limit: Some(answer_limit),
            answers,
            ..CaptureWait::default()
        },
        Some(cancelled),
        |_| Ok(()),
        || Ok(deadline.saturating_duration_since(Instant::now())),
    )
    .map_err(|failure| failure.error)
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
        CaptureWait::default(),
        None,
        |_| Ok(()),
        || Ok(deadline.saturating_duration_since(Instant::now())),
    )
    .map_err(|failure| failure.error)
}

#[derive(Default)]
struct CaptureWait {
    poll: Option<Duration>,
    quiescent: bool,
    private_output: bool,
    /// Journal the child's lifecycle only when it fails (SH-761).
    failures_only: bool,
    /// How much stdout to read; `None` is the diagnostic bound.
    stdout_limit: Option<u64>,
    /// Nonzero exit codes that are answers, journaled as success is.
    answers: &'static [i32],
}

fn run_captured_until<G>(
    mut command: Command,
    termination: TerminationPolicy,
    input: Option<File>,
    wait: CaptureWait,
    cancellation: Option<&dyn Fn() -> bool>,
    register: impl FnOnce(u32) -> Result<G, String>,
    mut remaining: impl FnMut() -> std::io::Result<Duration>,
) -> Result<Captured, CaptureFailure> {
    if cancellation.is_some_and(|cancelled| cancelled()) {
        return Err(CaptureError::Cancelled.into());
    }
    let poll = if cancellation.is_some() {
        Some(
            wait.poll
                .unwrap_or(Duration::from_millis(100))
                .min(Duration::from_millis(100)),
        )
    } else {
        wait.poll
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
    if cancellation.is_some_and(|cancelled| cancelled()) {
        return Err(CaptureError::Cancelled.into());
    }
    let mut child = command.spawn().map_err(CaptureError::Spawn)?;
    let pid = child.id();
    let context = format!("child={pid}");
    if !wait.failures_only {
        crate::daemon::activity::emit("INFO", &source, "event", &context, "process started");
    }
    let observer = if wait.private_output {
        None
    } else {
        crate::daemon::activity::OutputWatch::capture(&source, &context, &stdout_file, &stderr_file)
    };
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
        if cancellation.is_some_and(|cancelled| cancelled()) {
            terminate_timed_out(&mut child, pid, termination);
            drop(observer);
            return Err(CaptureFailure::after(
                settled_error(pid, termination, wait.quiescent, CaptureError::Cancelled),
                stdout_file,
            ));
        }
        let budget = match remaining() {
            Ok(budget) => budget,
            Err(error) => {
                terminate_timed_out(&mut child, pid, termination);
                drop(observer);
                return Err(CaptureFailure::after(
                    settled_error(pid, termination, wait.quiescent, CaptureError::Wait(error)),
                    stdout_file,
                ));
            }
        };
        match child.wait_timeout(poll.map_or(budget, |poll| budget.min(poll))) {
            Ok(Some(_)) if wait.quiescent && process_group_is_live(pid) => {
                if budget.is_zero() {
                    let outcome = terminate_timed_out(&mut child, pid, termination);
                    drop(observer);
                    return Err(CaptureFailure::after(
                        settled_error(
                            pid,
                            termination,
                            wait.quiescent,
                            CaptureError::Timeout(outcome),
                        ),
                        stdout_file,
                    ));
                }
                // The leader may be reaped while inherited children still own
                // effects and append output. Keep capture and caller ownership live.
                thread::sleep(budget.min(Duration::from_millis(10)));
            }
            Ok(Some(status)) => break status,
            Ok(None) if !budget.is_zero() => continue,
            Ok(None) => {
                let outcome = terminate_timed_out(&mut child, pid, termination);
                crate::daemon::activity::emit(
                    "ERROR",
                    &source,
                    "event",
                    &context,
                    "process timed out; group cleanup requested",
                );
                drop(observer);
                return Err(CaptureFailure::after(
                    settled_error(
                        pid,
                        termination,
                        wait.quiescent,
                        CaptureError::Timeout(outcome),
                    ),
                    stdout_file,
                ));
            }
            Err(error) => {
                kill_process_group(pid);
                let _ = child.wait();
                drop(observer);
                return Err(CaptureFailure::after(
                    settled_error(pid, termination, wait.quiescent, CaptureError::Wait(error)),
                    stdout_file,
                ));
            }
        }
    };
    drop(observer);
    let answered = status.success()
        || status
            .code()
            .is_some_and(|code| wait.answers.contains(&code));
    if !(wait.failures_only && answered) {
        crate::daemon::activity::emit(
            if answered { "INFO" } else { "ERROR" },
            &source,
            "event",
            &context,
            &format!("process finished: {status}"),
        );
    }
    let (stdout, stdout_truncated) =
        read_capture_up_to(stdout_file, wait.stdout_limit.unwrap_or(MAX_CAPTURE_BYTES));
    Ok(Captured {
        status,
        stdout,
        stderr: read_capture(stderr_file),
        stdout_truncated,
    })
}

fn settled_error(
    pid: u32,
    termination: TerminationPolicy,
    quiescent: bool,
    error: CaptureError,
) -> CaptureError {
    if !quiescent {
        return error;
    }
    let grace = match termination {
        TerminationPolicy::Kill => Duration::ZERO,
        TerminationPolicy::TerminateThenKill { grace } => grace,
    };
    let deadline = Instant::now() + grace;
    while process_group_is_live(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if process_group_is_live(pid) {
        CaptureError::Unsettled(format!(
            "helper process group {pid} remains after termination; retain ownership until recovery proves quiescence; {}",
            error.detail()
        ))
    } else {
        error
    }
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
pub(crate) fn read_capture(file: File) -> Vec<u8> {
    read_capture_up_to(file, MAX_CAPTURE_BYTES).0
}

/// Reads up to `limit` bytes of one capture file, and whether the file held
/// more. The length is the file's own, so a cut is known without reading it.
fn read_capture_up_to(mut file: File, limit: u64) -> (Vec<u8>, bool) {
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(0)).is_ok() {
        let _ = (&mut file).take(limit).read_to_end(&mut bytes);
    }
    let truncated = file.metadata().is_ok_and(|meta| meta.len() > limit);
    (bytes, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sh871_owned_effect_refuses_expired_deadline_before_spawn_or_registration() {
        let command = Command::new("storyhook-sh871-must-not-spawn-expired-host-request");
        let result = run_captured_owned_quiescent_until(
            command,
            Instant::now(),
            &|| false,
            |_| -> Result<(), String> { panic!("expired effect registered a process") },
        );
        assert!(
            matches!(result,Err(CaptureError::Wait(ref error)) if error.kind()==std::io::ErrorKind::TimedOut)
        );
    }
    #[test]
    fn sh871_owned_effect_retains_registration_until_surviving_group_is_settled() {
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        let root = storyhook_test_support::scratch_dir();
        let ready = root.path().join("ready");
        let pid = AtomicU32::new(0);
        let registered = AtomicBool::new(false);
        let observed = AtomicBool::new(false);
        struct Registration<'a>(&'a AtomicBool);
        impl Drop for Registration<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let mut command = Command::new("sh");
        command.args(["-c","(printf ready > \"$1\"; sleep 30) & while [ ! -f \"$1\" ]; do sleep 0.01; done; printf '{\"ok\":true}'","fixture"]).arg(&ready);
        let result = run_captured_owned_quiescent_until(
            command,
            Instant::now()
                + storyhook_test_support::load_grace::graced_now(Duration::from_secs(30)),
            &|| {
                let leader = pid.load(Ordering::SeqCst);
                // A successfully reaped leader must not release registration while
                // the same process group still contains this effect's child.
                if leader != 0 && ready.exists() && unsafe { libc::kill(leader as i32, 0) } == -1 {
                    assert!(registered.load(Ordering::SeqCst));
                    observed.store(true, Ordering::SeqCst);
                    true
                } else {
                    false
                }
            },
            |leader| {
                pid.store(leader, Ordering::SeqCst);
                registered.store(true, Ordering::SeqCst);
                Ok(Registration(&registered))
            },
        );
        assert!(
            observed.load(Ordering::SeqCst),
            "capture returned before observing surviving descendants"
        );
        assert!(
            matches!(result, Err(CaptureError::Cancelled)),
            "owned capture did not report original cancellation"
        );
        assert!(!registered.load(Ordering::SeqCst));
    }

    #[test]
    fn quiescent_capture_rejects_success_while_descendants_survive_the_deadline() {
        // Startup is not the timeout proof. A delayed shell must first establish
        // its resistant descendant; only then does the driven 100 ms begin.
        for delay in ["0", "0.2"] {
            let root = storyhook_test_support::scratch_dir();
            let ready = root.path().join("ready");
            let mut command = Command::new("sh");
            command.args([
                "-c",
                "sleep \"$1\"; trap '' TERM; (printf ready > \"$2\"; while :; do sleep 30; done) & printf '{\"ok\":true}'",
                "fixture",
                delay,
            ]).arg(&ready);
            let mut startup =
                storyhook_test_support::load_grace::Patience::new(Duration::from_secs(10));
            let mut proof_deadline = None;
            let result = run_captured_quiescent_until(
                command,
                TerminationPolicy::TerminateThenKill {
                    grace: Duration::from_millis(100),
                },
                || {
                    if ready.exists() {
                        let deadline = proof_deadline
                            .get_or_insert_with(|| Instant::now() + Duration::from_millis(100));
                        Ok(deadline.saturating_duration_since(Instant::now()))
                    } else if startup.expired() {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!("descendant never became ready: {startup}"),
                        ))
                    } else {
                        Ok(Duration::from_millis(10))
                    }
                },
            );
            let failure = match result {
                Err(failure) => failure,
                Ok(captured) => panic!(
                    "capture accepted leader success with a live descendant: {:?}",
                    captured.status
                ),
            };
            assert!(proof_deadline.is_some(), "{}", failure.error.detail());
            assert!(
                matches!(
                    failure.error,
                    CaptureError::Timeout(TimeoutTermination::KilledAfterTerminate)
                ),
                "{}",
                failure.error.detail()
            );
            assert_eq!(failure.stdout, br#"{"ok":true}"#);
        }
    }

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
            storyhook_test_support::load_grace::graced_now(Duration::from_secs(10)),
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
            storyhook_test_support::load_grace::graced_now(Duration::from_secs(10)),
            TerminationPolicy::Kill,
            |pid| {
                let deadline = Instant::now()
                    + storyhook_test_support::load_grace::graced_now(Duration::from_secs(5));
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

    /// A deadline no answer test reaches: each child below writes and exits.
    const ANSWER_DEADLINE: Duration = Duration::from_secs(30);

    /// A child that writes exactly `bytes` bytes of stdout and exits.
    fn writes(bytes: u64) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", &format!("head -c {bytes} /dev/zero")]);
        command
    }

    fn answer(bytes: u64, limit: u64) -> Captured {
        match run_captured_answer(
            writes(bytes),
            storyhook_test_support::load_grace::graced_now(ANSWER_DEADLINE),
            TerminationPolicy::Kill,
            limit,
        ) {
            Ok(captured) => captured,
            Err(error) => panic!("the writer failed: {}", error.detail()),
        }
    }

    /// SH-815: an answer that fits its limit exactly is whole, and one byte
    /// more is reported as cut rather than handed over as if it were whole.
    #[test]
    fn an_answer_longer_than_its_limit_is_reported_as_cut() {
        let whole = answer(4096, 4096);
        assert_eq!(whole.stdout.len(), 4096);
        assert!(!whole.stdout_truncated, "an answer at its limit is whole");

        let cut = answer(4097, 4096);
        assert_eq!(cut.stdout.len(), 4096, "the read stops at the limit");
        assert!(cut.stdout_truncated, "one byte past the limit is a cut");
    }

    /// The answer limit replaces the diagnostic bound for its caller only:
    /// an answer larger than 64 KiB arrives whole, and the diagnostic path
    /// still stops at 64 KiB and now says that it did.
    #[test]
    fn the_answer_limit_widens_only_its_own_capture() {
        let large = MAX_CAPTURE_BYTES * 2;
        let whole = answer(large, large);
        assert_eq!(whole.stdout.len() as u64, large);
        assert!(!whole.stdout_truncated);

        let diagnostic = match run_captured(
            writes(MAX_CAPTURE_BYTES + 1),
            storyhook_test_support::load_grace::graced_now(ANSWER_DEADLINE),
        ) {
            Ok(captured) => captured,
            Err(error) => panic!("the writer failed: {}", error.detail()),
        };
        assert_eq!(diagnostic.stdout.len() as u64, MAX_CAPTURE_BYTES);
        assert!(diagnostic.stdout_truncated);
    }

    /// An answer that never comes still ends at its deadline, with the whole
    /// process group gone: the grandchild that holds the capture file open
    /// is killed with its parent, so nothing it started outlives the call.
    #[test]
    fn an_answer_that_never_comes_is_stopped_with_its_whole_group() {
        let root = storyhook_test_support::scratch_dir();
        let grandchild = root.path().join("grandchild");
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "sleep 300 & printf %s $! > \"$1\"; wait",
            "silent-answer",
            grandchild.to_str().unwrap(),
        ]);
        let result = run_captured_answer(
            command,
            Duration::from_millis(200),
            TerminationPolicy::Kill,
            4096,
        );
        assert!(
            matches!(
                result,
                Err(CaptureError::Timeout(TimeoutTermination::Killed))
            ),
            "a silent answer must end at its deadline"
        );
        let pid: libc::pid_t = std::fs::read_to_string(&grandchild)
            .expect("the child recorded its grandchild")
            .trim()
            .parse()
            .expect("a pid");
        assert!(
            pid_disappears(pid),
            "the grandchild {pid} outlived its group's deadline"
        );
    }

    #[test]
    fn sh871_private_host_request_refuses_expired_deadline_before_spawn() {
        // A nonexistent executable distinguishes preflight refusal from spawning
        // and terminating a child after discovering an exhausted budget.
        let command = Command::new("storyhook-sh871-must-not-spawn-expired-host-request");
        let failure = run_captured_private_input_until(command, None, Instant::now(), &|| false)
            .err()
            .expect("expired private request must refuse before spawning");
        assert!(
            matches!(failure, CaptureError::Wait(ref error) if error.kind() == std::io::ErrorKind::TimedOut),
            "{}",
            failure.detail()
        );
    }
}
