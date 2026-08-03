//! A pseudo-terminal, so the interactive branches can be tested at all.
//!
//! # Why this exists
//!
//! Two of storyhook's code paths only run when stdin is a terminal: the
//! `story project new` questionnaire, and the typed-slug confirmation that
//! guards project destruction. Neither had ever been executed by a test — a
//! piped stdin is not a terminal, so every existing harness reaches the
//! *refusal* and never the prompt. The confirmation in particular has guarded
//! every `deinit` in this repository's history without a single test ever
//! having typed a token into it.
//!
//! [`openpty`](libc::openpty) closes that. The child gets a real terminal on
//! fds 0, 1 and 2, `IsTerminal` answers true, and the test drives the
//! conversation from the master side. It costs no new dependency: `libc` was
//! already here for `crash.rs`'s signal numbers.
//!
//! # The three conditions this harness ships under
//!
//! `make test` is the only gate this repository has, so a harness that can
//! wedge it is worse than the coverage it buys. All three of these are
//! load-bearing:
//!
//! 1. **The child's environment must carry [`daemon_containment`]**, because
//!    killing the child does *not* kill a daemon it started: the daemon is
//!    spawned into its own process group and stays alive on the
//!    `STORYHOOK_PARENT_PID` suicide contract and a kernel-assigned port.
//!    [`Pty::spawn`] refuses a command that does not carry both.
//! 2. **Every blocking operation has a deadline** — each [`Pty::expect`], and
//!    [`Pty::wait`] — *and* a whole test file can arm a [`watchdog`]. A wedge
//!    is then a failing test carrying the transcript that explains it, rather
//!    than a gate that stops for eight hours.
//! 3. **`scripts/check-no-orphan-servers.sh postlude`**, already the last line
//!    of `make test`, is the backstop that says none of this leaked.
//!
//! And one rule with no code behind it: **never widen an `expect` to stabilize
//! a flaky test.** An `expect` that matches anything is a test that asserts
//! nothing, and it will pass forever.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::daemon_containment;

/// How long a single [`Pty::expect`] waits before declaring the child wedged.
///
/// **A wedge detector, not a performance budget**, and the difference decides
/// the number. It is the point at which "slow" stops being a plausible
/// explanation, and the run is better spent failing with a transcript than
/// waiting for one that is never coming.
///
/// It was 10 s, and 10 s was measured to be wrong. A `story` command under a pty
/// prints its first prompt in about 0.9 s, but roughly two runs in ten it takes
/// **seven to ten seconds instead** — reproducibly, in this file and not in a
/// probe binary running the same command, with every instrumentable sub-phase
/// (spawn, `wait`, the store commands around it) still measuring in
/// milliseconds. Neither warming the daemon first nor removing the `git config`
/// call the client makes changed the distribution. Filed rather than papered
/// over; the deadline is set above the observed stall so a *real* wedge is still
/// caught quickly and a known slow start is not reported as one.
pub const EXPECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the pump sleeps between reads when the child has said nothing.
const IDLE_POLL: Duration = Duration::from_millis(10);

/// How long an exited child must stay quiet before the transcript is complete.
///
/// The alternative — waiting for EOF — does not work: a `story` command that
/// starts a daemon leaves a descendant holding the terminal open, so EOF never
/// arrives at all.
const DRAIN_QUIET: Duration = Duration::from_millis(150);

/// A child process attached to a pseudo-terminal.
///
/// Drop kills the child and reaps it, so a failed assertion cannot leave a
/// process behind holding the fixture's store open.
pub struct Pty {
    master: OwnedFd,
    child: Option<Child>,
    transcript: String,
    timeout: Duration,
    label: String,
}

/// What one read from the master side produced.
///
/// **Three states rather than two, and the third one is load-bearing.** "Bytes
/// arrived" and "nothing was ready" read the same to a waiter that only wants
/// to know whether to keep going, but they are opposites to a waiter that wants
/// to know whether the child has *finished* talking. Collapsing them cost this
/// harness a ten-second tail on every [`Pty::wait`]: a descendant that inherits
/// the terminal — a daemon this command started — holds the slave open, so EOF
/// never comes, and a drain loop that cannot see "quiet" runs until its
/// deadline.
enum Pumped {
    /// Bytes arrived and are in the transcript.
    Read,
    /// Nothing was ready. The child may still be thinking.
    Idle,
    /// Every slave descriptor is closed: nothing more will ever arrive.
    Eof,
}

impl Pty {
    /// Runs `command` with a real terminal on its standard streams.
    ///
    /// # Panics
    ///
    /// If `command` does not carry [`daemon_containment`]. That is condition 1
    /// above, enforced rather than documented: a leaked daemon poisons every
    /// later run on the machine, and it once cost 78 of 139 tests.
    #[must_use]
    pub fn spawn(label: &str, mut command: Command) -> Self {
        Self::assert_contained(label, &command);

        let mut master: RawFd = -1;
        let mut slave: RawFd = -1;
        // SAFETY: both out-parameters are valid stack locations, and the three
        // null pointers ask for the system's default terminal name, termios and
        // window size — which is what a plain terminal is.
        let rc = unsafe {
            libc::openpty(
                &raw mut master,
                &raw mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(
            rc,
            0,
            "{label}: openpty failed: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: `openpty` returned 0, so both are freshly opened descriptors
        // this process owns and nothing else holds.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };

        let dup = || slave.try_clone().expect("duplicating the pty slave");
        command
            .stdin(Stdio::from(dup()))
            .stdout(Stdio::from(dup()))
            .stderr(Stdio::from(dup()));
        let child = command
            .spawn()
            .unwrap_or_else(|e| panic!("{label}: spawning the child under a pty: {e}"));
        // The parent must not keep a slave descriptor open, or the master never
        // reports EOF and every `expect` for something the child will not say
        // waits the full timeout instead of failing the moment it exits.
        drop(slave);

        set_nonblocking(&master, label);
        Self {
            master,
            child: Some(child),
            transcript: String::new(),
            timeout: EXPECT_TIMEOUT,
            label: label.to_string(),
        }
    }

    /// Uses `timeout` for subsequent [`expect`](Self::expect)s.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Waits until the child has written `needle`.
    ///
    /// # Panics
    ///
    /// If the timeout elapses, or the child exits without ever saying it. Both
    /// panics carry the whole transcript, because "timed out" on its own tells
    /// you nothing about which of six questions it stopped at.
    pub fn expect(&mut self, needle: &str) {
        let deadline = Instant::now() + self.timeout;
        loop {
            if self.transcript.contains(needle) {
                return;
            }
            if matches!(self.pump(), Pumped::Eof) {
                assert!(
                    self.transcript.contains(needle),
                    "{}: the child exited without ever writing {needle:?}.{}",
                    self.label,
                    self.rendered_transcript(),
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{}: timed out after {:?} waiting for {needle:?}.{}",
                self.label,
                self.timeout,
                self.rendered_transcript(),
            );
        }
    }

    /// Types `line` and a newline at the terminal.
    pub fn send_line(&mut self, line: &str) {
        let mut master = self.master_file();
        writeln!(master, "{line}")
            .unwrap_or_else(|e| panic!("{}: writing `{line}`: {e}", self.label));
        master
            .flush()
            .unwrap_or_else(|e| panic!("{}: flushing `{line}`: {e}", self.label));
        std::mem::forget(master);
    }

    /// Everything the child has written so far.
    #[must_use]
    pub fn transcript(&self) -> &str {
        &self.transcript
    }

    /// Waits for the child to exit and returns its status.
    ///
    /// Drains the terminal on the way, so the transcript is complete by the
    /// time an assertion about the exit code looks at it.
    ///
    /// # Panics
    ///
    /// If the child has not exited within the timeout, having killed it first
    /// so the run does not stall.
    pub fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + self.timeout;
        let mut child = self
            .child
            .take()
            .unwrap_or_else(|| panic!("{}: waited twice", self.label));
        loop {
            match child.try_wait().expect("polling the child") {
                Some(status) => {
                    self.drain();
                    return status;
                }
                None if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "{}: the child was still running after {:?}, and has been killed.{}",
                        self.label,
                        self.timeout,
                        self.rendered_transcript(),
                    );
                }
                None => {
                    self.pump();
                }
            }
        }
    }

    /// Reads whatever the exited child left behind, and stops when it goes
    /// quiet.
    ///
    /// **Bounded by silence, not by the deadline.** A `story` command that
    /// starts a daemon leaves a descendant holding the terminal, so the master
    /// never reports EOF; a drain that waited for one would add the whole
    /// remaining timeout to every such test — which is exactly what it did,
    /// nine seconds at a time, until [`Pumped`] grew its third state.
    fn drain(&mut self) {
        let mut quiet = Duration::ZERO;
        while quiet < DRAIN_QUIET {
            match self.pump() {
                Pumped::Read => quiet = Duration::ZERO,
                Pumped::Idle => quiet += IDLE_POLL,
                Pumped::Eof => return,
            }
        }
    }

    /// One read from the master, appended to the transcript.
    fn pump(&mut self) -> Pumped {
        let mut buffer = [0_u8; 4096];
        let mut master = self.master_file();
        let read = master.read(&mut buffer);
        std::mem::forget(master);
        match read {
            Ok(0) => Pumped::Eof,
            Ok(n) => {
                self.transcript
                    .push_str(&String::from_utf8_lossy(&buffer[..n]));
                Pumped::Read
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(IDLE_POLL);
                Pumped::Idle
            }
            // A master whose last slave has closed reports EIO on Linux and a
            // zero-length read on macOS. Both mean the same thing.
            Err(e) if e.raw_os_error() == Some(libc::EIO) => Pumped::Eof,
            Err(e) => panic!("{}: reading the pty: {e}", self.label),
        }
    }

    /// The master as a `File`, for `Read`/`Write`.
    ///
    /// Every caller `mem::forget`s the result: `File`'s drop closes the
    /// descriptor, and this one belongs to the `OwnedFd` that outlives it.
    fn master_file(&self) -> std::fs::File {
        // SAFETY: the descriptor is owned by `self.master` and outlives every
        // borrow of it; the `File` is forgotten rather than dropped, so it never
        // closes what it does not own.
        unsafe { std::fs::File::from_raw_fd(self.master.as_raw_fd()) }
    }

    fn rendered_transcript(&self) -> String {
        format!(
            "\n--- transcript ({} bytes) ---\n{}\n--- end of transcript ---",
            self.transcript.len(),
            self.transcript
        )
    }

    /// Condition 1: the child must not be able to start a daemon that outlives
    /// it or binds the developer's own dashboard port.
    fn assert_contained(label: &str, command: &Command) {
        let carried: Vec<&std::ffi::OsStr> = command
            .get_envs()
            .filter_map(|(name, value)| value.map(|_| name))
            .collect();
        for (name, _) in daemon_containment() {
            assert!(
                carried.iter().any(|carried| *carried == name),
                "{label}: a pty child must carry {name}. Killing this child does not kill a \
                 daemon it started — the daemon is in its own process group and relies on the \
                 STORYHOOK_PARENT_PID suicide contract. Build the command with \
                 `TestEnv::raw_story`, which applies both."
            );
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn set_nonblocking(fd: &OwnedFd, label: &str) {
    // SAFETY: `fd` is a descriptor this process owns; both calls are the
    // documented get-then-set pair for a file-status flag.
    let rc = unsafe {
        let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL, 0);
        libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK)
    };
    assert!(
        rc >= 0,
        "{label}: making the pty non-blocking: {}",
        std::io::Error::last_os_error()
    );
}

/// Condition 2's outer half: a wall-clock bound on a whole test file.
///
/// Every [`Pty`] operation is individually deadlined, so this covers what they
/// cannot — a loop in the test itself, a `Command` that blocks before the pty
/// is involved, a future addition nobody thought about. When it fires it prints
/// why and aborts the test binary, which cargo reports as a failure. That is
/// deliberately blunt: a stalled gate is the worst failure this suite can have,
/// because nothing ever ends and so nothing is ever reported.
///
/// Hold the returned guard for the region being watched; dropping it disarms.
#[must_use]
pub fn watchdog(label: &'static str, limit: Duration) -> Watchdog {
    let done = Arc::new(AtomicBool::new(false));
    let armed = Arc::clone(&done);
    let handle = thread::spawn(move || {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if armed.load(Ordering::Relaxed) {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
        if armed.load(Ordering::Relaxed) {
            return;
        }
        eprintln!(
            "{label}: still running after {limit:?} — aborting rather than stalling the gate"
        );
        std::process::abort();
    });
    Watchdog {
        done,
        handle: Some(handle),
    }
}

/// Disarms its [`watchdog`] when dropped.
pub struct Watchdog {
    done: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command with the containment the harness insists on, running `sh`
    /// rather than `story` so these tests say nothing about storyhook.
    fn contained(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(script);
        for (name, value) in daemon_containment() {
            command.env(name, value);
        }
        command
    }

    #[test]
    fn the_child_gets_a_real_terminal() {
        // The whole premise. `[ -t 0 ]` is false under every other harness in
        // this repository, which is why the interactive branches were untested.
        let mut pty = Pty::spawn(
            "terminal",
            contained("if [ -t 0 ]; then echo IS_A_TTY; else echo NOT_A_TTY; fi"),
        );
        pty.expect("IS_A_TTY");
        assert!(pty.wait().success());
    }

    #[test]
    fn a_line_typed_at_the_terminal_reaches_the_child() {
        let mut pty = Pty::spawn("echo", contained("read answer; echo \"GOT:$answer\""));
        pty.send_line("hello");
        pty.expect("GOT:hello");
        assert!(pty.wait().success());
    }

    /// **The test that makes this harness safe to put inside the only gate.**
    ///
    /// If the deadline did not fire, a wedged child would hang `make test`
    /// forever — the exact failure mode this file exists to avoid. So the
    /// timeout path is exercised rather than trusted, and the panic is required
    /// to carry the transcript, because a bare "timed out" says nothing about
    /// which prompt it stopped at.
    #[test]
    fn an_expect_for_something_never_written_fails_within_its_deadline() {
        let started = Instant::now();
        let outcome = std::panic::catch_unwind(|| {
            let mut pty = Pty::spawn("deadline", contained("echo HERE; sleep 60"))
                .timeout(Duration::from_millis(400));
            pty.expect("HERE");
            pty.expect("NEVER WRITTEN");
        });

        let panic = outcome.expect_err("the expect was supposed to fail");
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_else(|| "<non-string panic>".to_string());
        assert!(message.contains("timed out"), "{message}");
        assert!(
            message.contains("NEVER WRITTEN") && message.contains("HERE"),
            "the panic must carry what it waited for and what it saw: {message}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the deadline did not bound the wait: {:?}",
            started.elapsed()
        );
    }

    /// A child that exits without saying it fails *immediately*, rather than
    /// waiting out a timeout for something that can never arrive.
    #[test]
    fn an_expect_fails_at_once_when_the_child_exits_first() {
        let started = Instant::now();
        let outcome = std::panic::catch_unwind(|| {
            let mut pty = Pty::spawn("eof", contained("echo BYE")).timeout(Duration::from_secs(30));
            pty.expect("NEVER WRITTEN");
        });

        assert!(outcome.is_err(), "the expect was supposed to fail");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "EOF must fail the expect rather than waiting out the deadline: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_wedged_child_is_killed_rather_than_waited_for() {
        let outcome = std::panic::catch_unwind(|| {
            let mut pty =
                Pty::spawn("wait", contained("sleep 60")).timeout(Duration::from_millis(300));
            pty.wait();
        });
        assert!(outcome.is_err(), "wait was supposed to give up and kill it");
    }

    #[test]
    fn a_child_without_daemon_containment_is_refused() {
        let outcome = std::panic::catch_unwind(|| {
            let mut bare = Command::new("/bin/sh");
            bare.arg("-c").arg("true");
            let _ = Pty::spawn("uncontained", bare);
        });
        let panic = outcome.expect_err("an uncontained child must be refused");
        let message = panic.downcast_ref::<String>().cloned().unwrap_or_default();
        assert!(message.contains("STORYHOOK_PARENT_PID"), "{message}");
    }

    #[test]
    fn a_disarmed_watchdog_does_not_fire() {
        // The other half is unassertable by construction: a watchdog that fires
        // aborts the process, so a test proving it fires would take the binary
        // with it. This proves the disarm, and the abort path is three lines
        // read rather than run.
        let guard = watchdog("disarmed", Duration::from_millis(50));
        drop(guard);
        thread::sleep(Duration::from_millis(200));
    }
}
