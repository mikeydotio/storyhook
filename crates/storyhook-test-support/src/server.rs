//! Dashboard servers for tests: how to start one, how to know it is ready, and
//! how to guarantee it does not outlive the test that started it.
//!
//! Every rule enforced here was paid for by SH-51, where two orphaned test
//! servers from an earlier run held ~100 ports and answered a later run's
//! requests out of a stale registry — 78 of 139 tests down, with nothing in the
//! output pointing anywhere near the cause.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use storyhook::daemon::serve::BoundAddress;
use storyhook::env::Environment;
use storyhook::store::SqliteStore;

use crate::env::story_binary;

/// Picks a port for the handful of tests that must know one *before* the
/// thing that binds it exists — `story web start --port N` binds in a child
/// process, and reports success as soon as it has spawned that child, so a
/// port it loses is never reported to anyone.
///
/// **Not a reservation a caller can trust past the moment it returns.** The
/// bind-and-release below rules out a *long-lived* squatter — the fixed
/// counter's hazard, a daemon leaked by an earlier run sitting on a low
/// number forever — but proves nothing about the gap between that release and
/// whatever binds this port for real: a genuine TOCTOU window remains, however
/// narrow (SH-195). A caller that only needs *some* free port, and can learn
/// which one it actually got afterwards — every direct-daemon-spawn caller in
/// this crate does, from the daemon's own portfile — should ask for `--port 0`
/// instead and skip this function entirely, the way
/// [`crate::crash::spawn_daemon`] does. Reach for this one only when the port
/// must be known *before* the thing that binds it exists, because nothing
/// downstream of the spawn will report the real one back (`story web start`'s
/// case, above).
///
/// Two properties, both learned from SH-51:
///
/// - **Outside the kernel's ephemeral range.** Every in-process server here
///   binds port 0, so anything drawn from the ephemeral range can be handed
///   to one of those in the window between this reservation being released
///   and the child binding it — after which the test talks to whichever won.
/// - **Not a fixed sequence.** The old counter started every run at 19000 and
///   marched upward, so any run collided with the survivors of the previous
///   one. The band here is entered at a random offset, and each candidate is
///   bind-tested before being handed out.
pub fn reserve_port() -> u16 {
    use std::sync::atomic::{AtomicU32, Ordering};

    // Above the registered-port range, below the ephemeral range macOS and
    // Linux draw from (49152+ / 32768+ — the band avoids both).
    const BAND: std::ops::Range<u16> = 19000..29000;
    const SPAN: u32 = BAND.end as u32 - BAND.start as u32;

    // A `u32` counter, mapped into the band by `% SPAN` on every call — never
    // a stored "current candidate" a caller reads and then conditionally
    // resets (SH-394). That two-step shape was racy across the concurrent
    // callers this crate actually has: `--test-threads=4` runs multiple test
    // functions in one process, and any of them wrapping the band at the same
    // moment could both observe an out-of-band value, both decide to reset to
    // `BAND.start`, and both hand out that exact port to two different
    // callers — reproduced as `reservations must not repeat: left: 7, right:
    // 8` in a real `cargo test --workspace` run. `fetch_add` alone gives every
    // caller a distinct raw value with no read-then-write gap for another
    // thread to land in; the modulus is a pure function of that value, so two
    // distinct raw values can only ever collide after a full `SPAN`-call
    // cycle, the same harmless long-run reuse the band already relies on.
    static NEXT: std::sync::LazyLock<AtomicU32> = std::sync::LazyLock::new(|| {
        let entropy = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
            ^ std::process::id() as u32;
        AtomicU32::new(entropy % SPAN)
    });

    for _ in 0..64 {
        let raw = NEXT.fetch_add(1, Ordering::Relaxed);
        let candidate = BAND.start + (raw % SPAN) as u16;
        // Binding and immediately releasing proves nothing else holds it
        // *right now* — including a daemon leaked by an earlier run, the
        // exact hazard the fixed counter walked straight into — but not that
        // nothing will grab it before the real caller binds (SH-195).
        if TcpListener::bind(("127.0.0.1", candidate)).is_ok() {
            return candidate;
        }
    }
    panic!("no free port in {BAND:?} — is an earlier run's daemon still holding them?");
}

/// Where a test server answers, and the bearer token every `/api/**` route
/// on it requires (SH-187).
///
/// The token is real, minted fresh per server
/// ([`storyhook::daemon::serve::bind_and_serve`]) — not an empty
/// placeholder, since an empty `expected` fails closed
/// (`crate::api::rpc::token_ok`) and could never authenticate a request
/// anyway. It never touches disk: no portfile, no pidfile, exactly like
/// everything else this test seam skips relative to a real daemon.
pub struct TestServer {
    pub bound: BoundAddress,
    pub token: String,
}

impl TestServer {
    /// The loopback port both listeners share.
    pub fn port(&self) -> u16 {
        self.bound.port()
    }
}

/// Starts a dashboard for `store` on an OS-assigned port and returns every
/// address it bound, plus its token, once it is serving. The sanctioned way
/// for a test to get a server: no test picks its own port, and no test
/// waits on anything weaker than the server's own readiness signal.
///
/// The store is an [`Arc`] because the server outlives this call: it runs on a
/// detached thread for the rest of the test binary's life, exactly as the
/// production daemon runs for the life of its process.
pub fn serve(store: Arc<SqliteStore>, env: &Environment) -> TestServer {
    try_serve_on(store, env, 0).unwrap_or_else(|e| panic!("starting a test server: {e}"))
}

/// [`serve`], but on a caller-chosen `port` and returning the server's own
/// start-up failure instead of panicking.
///
/// The failure must never be swallowed. A server that loses the bind leaves
/// whatever *else* holds that port answering the test's requests, and a
/// stranger's store answers `404` to everything the test asks about — the
/// mass-failure mode of SH-51. Readiness comes from the server's `ready`
/// callback rather than a `connect()` probe for the same reason: only the server
/// can attest that the address is one it actually bound.
///
/// That reason is why the whole [`BoundAddress`] comes back and not just the
/// port. A test that wants the tailnet listener must learn whether it exists
/// from the server that bound it — probing `tailscale` in the test process
/// answers a different question ("does this machine have a tailnet?") and the
/// two came apart under load in SH-110.
pub fn try_serve_on(
    store: Arc<SqliteStore>,
    env: &Environment,
    port: u16,
) -> Result<TestServer, String> {
    let (tx, rx) = mpsc::channel::<Result<TestServer, String>>();
    let ready_tx = tx.clone();
    let env = env.clone();
    std::thread::spawn(move || {
        let outcome =
            storyhook::daemon::serve::bind_and_serve(&*store, &env, port, move |bound, token| {
                let _ = ready_tx.send(Ok(TestServer { bound, token }));
            });
        if let Err(e) = outcome {
            let _ = tx.send(Err(e.to_string()));
        }
    });

    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(server)) => {
            // `ready` fires before the accept loops are spawned, so "bound" and
            // "accepting" are genuinely different moments and this wait is not
            // made redundant by the report.
            wait_for_server(server.port());
            Ok(server)
        }
        Ok(Err(e)) => Err(e),
        Err(_) => {
            Err("a test server neither became ready nor reported a failure within 10s".to_string())
        }
    }
}

/// Stops the `story web start` daemon a test launched, even if the test
/// panics first. Test-spawned servers that outlive the test are exactly what
/// poisoned later runs in SH-51; the in-process ones die with the test
/// binary, but a daemon is a detached child process and only an explicit
/// stop reaps it.
pub struct DaemonGuard {
    vars: Vec<(String, PathBuf)>,
    cwd: PathBuf,
}

impl DaemonGuard {
    /// Arms the guard for a daemon started from `cwd` inside `env` — the same
    /// pair must be passed, because `web stop` finds the daemon through the
    /// pid file in that environment's state home.
    pub fn new(env: &crate::env::TestEnv, cwd: impl Into<PathBuf>) -> Self {
        DaemonGuard {
            vars: env
                .vars()
                .iter()
                .map(|(name, value)| ((*name).to_string(), value.to_path_buf()))
                .collect(),
            cwd: cwd.into(),
        }
    }
}

/// How long [`DaemonGuard`] gives `story web stop` to finish.
///
/// **Chosen, not derived or calibrated**, in the sense [`ACCEPT_DEADLINE`]
/// documents that phrase for. A guard-armed daemon has no in-flight work of
/// its own by the time a test drops it, so `web stop`'s graceful wait should
/// clear in milliseconds; production's own interactive notice for that same
/// wait ([`SERVED_PATIENCE`](storyhook::daemon::lifecycle::SERVED_PATIENCE),
/// 10s) is the only other number in play, and this sits above it rather than
/// racing it. What is being distinguished is "slow" from "never," so the
/// margin is generous on purpose.
const STOP_DEADLINE: Duration = Duration::from_secs(15);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let mut command = std::process::Command::new(story_binary());
        command.current_dir(&self.cwd);
        for (name, value) in &self.vars {
            command.env(name, value);
        }
        command.args(["web", "stop"]);
        let _ = run_bounded(command, "web stop", STOP_DEADLINE);
    }
}

/// How often [`ChildGuard::wait_within`] asks whether the child has exited.
///
/// **Chosen, not derived or calibrated**, in the sense [`ACCEPT_DEADLINE`]
/// documents that phrase for — and the same 25ms [`port_of`] already polls the
/// portfile on, for the same reason: short enough that a child which exits
/// promptly (which is every child on the happy path) is noticed at once, long
/// enough that waiting out a multi-second deadline costs a few hundred
/// `waitpid(WNOHANG)` calls rather than a spin.
const REAP_POLL: Duration = Duration::from_millis(25);

/// A harness deadline that deliberately outlives an ordinary `story` client.
///
/// A client may spend [`storyhook::daemon::lifecycle::SPAWN_LOCK_DEADLINE`]
/// acquiring the daemon spawn lock and then
/// [`storyhook::daemon::lifecycle::SERVED_DEADLINE`] waiting for its command.
/// One [`storyhook::daemon::lifecycle::SPAWN_DEADLINE`] is margin for the
/// parent process to start and report that inner failure before the harness
/// replaces it with a less-specific timeout (SH-394, SH-535).
pub const STORY_COMMAND_DEADLINE: Duration = Duration::from_secs(
    storyhook::daemon::lifecycle::SPAWN_LOCK_DEADLINE.as_secs()
        + storyhook::daemon::lifecycle::SERVED_DEADLINE.as_secs()
        + storyhook::daemon::lifecycle::SPAWN_DEADLINE.as_secs(),
);

/// A captured pipe being drained from the instant its child is spawned.
struct OutputDrain {
    stdout: mpsc::Receiver<std::io::Result<Vec<u8>>>,
    stderr: mpsc::Receiver<std::io::Result<Vec<u8>>>,
}

/// Kills and reaps a child process on drop, so a process a test started can
/// never outlive it.
pub struct ChildGuard {
    child: Child,
    output: Option<OutputDrain>,
}

impl ChildGuard {
    /// Takes ownership of `child`.
    pub fn new(child: Child) -> Self {
        Self {
            child,
            output: None,
        }
    }

    /// Spawns `command` and immediately takes ownership of its child.
    ///
    /// Tracked integration tests use this door instead of [`Command::spawn`],
    /// so every spawned process is guarded even when a test panics before it
    /// reaches its intended wait.
    pub fn spawn(command: &mut Command) -> std::io::Result<Self> {
        command.spawn().map(Self::new)
    }

    /// Spawns `command` with stdout and stderr captured and drained at once.
    ///
    /// Draining from spawn, rather than only once a caller starts waiting,
    /// prevents a verbose child from filling a pipe and blocking before a
    /// concurrent fixture has finished spawning its peers.
    pub fn spawn_with_output(command: &mut Command) -> std::io::Result<Self> {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().expect("stdout was configured as piped");
        let stderr = child.stderr.take().expect("stderr was configured as piped");
        Ok(Self {
            child,
            output: Some(OutputDrain {
                stdout: drain(stdout),
                stderr: drain(stderr),
            }),
        })
    }

    /// The child's process id.
    ///
    /// The identity a crash test compares against: "a daemon died" is not the
    /// claim, "the daemon this test armed died" is.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The child's piped stdin, when the caller only needs to write briefly.
    pub fn stdin(&mut self) -> Option<&mut ChildStdin> {
        self.child.stdin.as_mut()
    }

    /// Takes the child's piped stdin for a long-lived interactive session.
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    /// Takes the child's piped stdout for a long-lived interactive session.
    ///
    /// Returns `None` for a child created by [`Self::spawn_with_output`], whose
    /// output is already being drained for [`Self::wait_with_output_within`].
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    /// Waits for the child to exit and reports how it died, giving up at
    /// `deadline` rather than waiting for ever.
    ///
    /// Safe to call once and then let the guard drop: a reaped child's status is
    /// cached, and the `kill` in [`Drop`] fails harmlessly on a process that is
    /// already gone.
    ///
    /// # There is deliberately no unbounded version (SH-528)
    ///
    /// This used to be a bare `Child::wait()`. A crash fixture waiting on a
    /// daemon that never died then wedged `make test` for **ten hours and
    /// twenty-one minutes**, holding this machine's `gate` lock, with every
    /// subsequent verification queued behind it and nothing above the test
    /// bounding it — `run-rust-battery.sh`, `leg.sh`, `run-tests.sh` and
    /// `verify-pr.sh` carry no `timeout` between them. So the unbounded door is
    /// gone rather than deprecated: the compiler is the fence, and a caller has
    /// to say what it is waiting for and how long that may honestly take.
    ///
    /// # Why a poll loop rather than `wait-timeout`
    ///
    /// That crate is already a `storyhook` dependency, and it is the right tool
    /// in production, where the wait is latency-sensitive and inside one
    /// process's own control flow. It is the wrong tool *here*: it installs a
    /// process-global `SIGCHLD` handler through a `Once`, in a harness linked
    /// into test binaries that assert on signals and zombies themselves
    /// (`tests/orphan_check.rs`, `tests/machine_lock.rs`,
    /// `tests/crash_reports.rs`). A `try_wait` poll is what this crate already
    /// does three times over — [`Pty::wait`](crate::Pty), [`port_of`],
    /// [`wait_for_addr`] — and it costs one `waitpid(WNOHANG)` per
    /// [`REAP_POLL`] against deadlines measured in seconds.
    ///
    /// # Panics
    ///
    /// If the child has not exited by `deadline` — **having killed and reaped
    /// it first**, so the failure cannot leave behind the very process the
    /// story is about (SH-493). `what` is called only on that path, so a
    /// caller may gather expensive evidence (a log tail, a liveness probe)
    /// that is only meaningful at the moment the bound fires, and pays nothing
    /// for it on the happy path.
    pub fn wait_within(&mut self, deadline: Duration, what: impl FnOnce() -> String) -> ExitStatus {
        // `Child::wait` closes stdin before blocking. `try_wait` does not, so
        // the bounded equivalent must do it explicitly or a child waiting for
        // EOF can deadlock with its parent.
        drop(self.child.stdin.take());
        let give_up_at = Instant::now() + deadline;
        loop {
            if let Some(status) = self.try_wait() {
                return status;
            }
            if Instant::now() >= give_up_at {
                let pid = self.pid();
                self.kill_and_reap();
                panic!(
                    "{}\n\nThe child (pid {pid}) was still running after {deadline:?}, and has \
                     been killed so this failure does not also leak it.",
                    what()
                );
            }
            std::thread::sleep(REAP_POLL);
        }
    }

    /// Waits for the child and its captured pipes within one deadline.
    ///
    /// The pipe drains begin in [`Self::spawn_with_output`]. A descendant may
    /// inherit a write end and keep it open after the direct child exits, so
    /// collecting each drain is separately bounded by the same absolute
    /// deadline. A blocked reader is deliberately detached on failure; it is
    /// in a syscall this process cannot interrupt and ends with the test binary.
    pub fn wait_with_output_within(
        &mut self,
        deadline: Duration,
        what: impl FnOnce() -> String,
    ) -> Output {
        let give_up_at = Instant::now() + deadline;
        let mut what = Some(what);
        let status = self.wait_until(give_up_at).unwrap_or_else(|| {
            let pid = self.pid();
            self.kill_and_reap();
            panic!(
                "{}\n\nThe child (pid {pid}) was still running after {deadline:?}, and has \
                 been killed so this failure does not also leak it.",
                what.take().expect("diagnostic is used once")()
            )
        });
        let output = self
            .output
            .take()
            .expect("wait_with_output_within requires ChildGuard::spawn_with_output");
        let stdout = receive_drain(output.stdout, give_up_at).unwrap_or_else(|reason| {
            panic!(
                "{}\n\nThe child (pid {}) exited, but its stdout {reason} before \
                 {deadline:?}; a descendant may still hold the pipe open.",
                what.take().expect("diagnostic is used once")(),
                self.pid()
            )
        });
        let stderr = receive_drain(output.stderr, give_up_at).unwrap_or_else(|reason| {
            panic!(
                "{}\n\nThe child (pid {}) exited, but its stderr {reason} before \
                 {deadline:?}; a descendant may still hold the pipe open.",
                what.take().expect("diagnostic is used once")(),
                self.pid()
            )
        });
        Output {
            status,
            stdout,
            stderr,
        }
    }

    /// How the child died, or `None` while it is still running.
    ///
    /// For the tests that have several children and cannot know which will
    /// finish first — where blocking on one in turn would deadlock on whichever
    /// is waiting for something the caller has not done yet.
    pub fn try_wait(&mut self) -> Option<ExitStatus> {
        self.child
            .try_wait()
            .expect("asking after a child this test started")
    }

    /// Sends the uncatchable kill signal and reaps the child.
    ///
    /// This is the only deliberately unbounded public reap: after `kill`, the
    /// wait is on the kernel rather than on the child's cooperation.
    pub fn kill_and_reap(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Polls until `give_up_at`, closing stdin first as [`Child::wait`] does.
    fn wait_until(&mut self, give_up_at: Instant) -> Option<ExitStatus> {
        drop(self.child.stdin.take());
        loop {
            if let Some(status) = self.try_wait() {
                return Some(status);
            }
            if Instant::now() >= give_up_at {
                return None;
            }
            std::thread::sleep(REAP_POLL);
        }
    }
}

/// Drains one child pipe without making its owner wait to begin reading.
fn drain(mut pipe: impl Read + Send + 'static) -> mpsc::Receiver<std::io::Result<Vec<u8>>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = pipe.read_to_end(&mut bytes).map(|_| bytes);
        let _ = tx.send(result);
    });
    rx
}

/// Receives a completed pipe drain before the shared absolute deadline.
fn receive_drain(
    drain: mpsc::Receiver<std::io::Result<Vec<u8>>>,
    give_up_at: Instant,
) -> Result<Vec<u8>, String> {
    let remaining = give_up_at.saturating_duration_since(Instant::now());
    match drain.recv_timeout(remaining) {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(error)) => Err(format!("failed while being read: {error}")),
        Err(mpsc::RecvTimeoutError::Timeout) => Err("did not close".to_string()),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("reader stopped without returning output".to_string())
        }
    }
}

impl Drop for ChildGuard {
    /// **The one wait in this type that is deliberately unbounded**, and the
    /// reason is the line above it rather than an oversight (SH-528).
    ///
    /// `kill` sends `SIGKILL`, which cannot be caught, blocked or handled by
    /// anyone, so what this waits for is not the child's cooperation but the
    /// kernel's — and a reap after an uncatchable signal is bounded by the OS,
    /// not by anything the child gets a say in. It also runs during unwind,
    /// where a panic of its own would replace whatever failure was already
    /// being reported (SH-142). Do not copy this shape to a wait that is *not*
    /// preceded by a kill: use [`ChildGuard::wait_within`], which exists
    /// because that copy is exactly what wedged the gate for ten hours.
    fn drop(&mut self) {
        self.kill_and_reap();
    }
}

/// How long a listener has to begin accepting before it is called absent.
///
/// **Chosen, not derived or calibrated** — in the sense
/// `src/daemon/lifecycle.rs` uses those words for the production deadlines.
/// The wait is over a process binding a loopback socket plus a one-time
/// filesystem-watcher registration, and no legitimate slow case exists: across
/// a full run its ~141 calls land at 0ms all but three times, worst 4ms.
///
/// Deliberately *not* an import of the daemon's `SPAWN_DEADLINE`, despite the
/// equal value. That one bounds a daemon coming up; this waits on a test HTTP
/// server binding a socket. An import would assert a relationship that does not
/// exist, and the two would then be wrong together (SH-140).
///
/// The value is not the interesting part and has never needed to move — both
/// times this fired, it was right. What was wrong was the message, which named
/// the duration instead of the condition, and so reported an FSEvents pathology
/// as a mass of unexplained server failures.
const ACCEPT_DEADLINE: Duration = Duration::from_secs(5);

/// How often the deadline above is retried. Short enough that a ready listener
/// is noticed at once, long enough not to spin.
const ACCEPT_POLL: Duration = Duration::from_millis(50);

/// Blocks until `127.0.0.1:port` accepts connections.
pub fn wait_for_server(port: u16) {
    wait_for_addr(&format!("127.0.0.1:{port}"));
}

/// Like [`wait_for_server`], but against an arbitrary `host:port` — for the
/// tailnet listener, which `start_server` no longer starts *serving* until
/// its filesystem watcher's one-time setup finishes (see `web.rs`'s
/// `watcher_ready_rx` handshake), so a fixed sleep after `wait_for_server`
/// (loopback-only) isn't a reliable proxy for "the tailnet listener is
/// accepting requests too" under load.
pub fn wait_for_addr(addr: &str) {
    let start = Instant::now();
    loop {
        if TcpStream::connect(addr).is_ok() {
            return;
        }
        if start.elapsed() > ACCEPT_DEADLINE {
            panic!(
                "{addr} never began accepting connections. This is a 'never', not a 'slow': \
                 the bound is {ACCEPT_DEADLINE:?} against an observed 0-4ms across ~141 calls \
                 per run, and raising it cannot help, because both things that have ever \
                 caused it are unbounded. It has fired twice and been right twice — once on a \
                 server that had genuinely bound nothing (SH-110), and once on a machine whose \
                 FSEventStreamStart was serialized behind a huge target/debug/deps. Check \
                 `ls target/debug/deps | wc -l` (see Cargo.toml) before suspecting this test."
            );
        }
        std::thread::sleep(ACCEPT_POLL);
    }
}

/// The port the daemon with pid `pid` bound, once it has published one.
///
/// **This is where the pid the client will talk to is checked against the pid
/// the test armed.** The portfile is how a client finds a daemon, so a portfile
/// naming somebody else is a client about to send its work to the wrong process
/// — caught before the command is sent rather than inferred afterwards from a
/// corpse.
///
/// Read from the portfile rather than remembered from the reservation, because
/// the reservation is only a *preference*: [`bind_preferred`] falls back to a
/// kernel-assigned port, and a test that waited on the number it asked for would
/// wait forever on the day that fallback fired.
///
/// For the spawns this crate cannot wait on: `daemon --serve` and `web --serve`
/// run the daemon in the calling process and never return, so there is no exit
/// to synchronise with and the portfile is the only readiness signal there is. A
/// client-side `start` needs none of this — it blocks until the daemon is
/// healthy, so its portfile is already there when it returns.
///
/// [`bind_preferred`]: storyhook::daemon::lifecycle::bind_preferred
pub fn port_of(env: &crate::env::TestEnv, pid: u32) -> u16 {
    let deadline = Instant::now() + PORTFILE_DEADLINE;
    loop {
        match env.daemon() {
            Some(info) if info.pid == pid => return info.port,
            other => assert!(
                Instant::now() < deadline,
                "the daemon this test armed (pid {pid}) never became the one a client would \
                 find. The portfile names {:?} — if that is another daemon, the command below \
                 would go to it and the armed process would never run the work.",
                other.map(|info| info.pid),
            ),
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// How long a hand-spawned daemon has to publish its portfile.
///
/// **Chosen, not derived**, in the sense [`ACCEPT_DEADLINE`] documents that
/// phrase for — but not arbitrary: a daemon binds its listeners before it
/// publishes, and `bind_listeners` probes the tailnet on the way, which is
/// bounded at `TAILNET_PROBE_TIMEOUT` (3s). A `tailscale` wedged for the whole
/// probe therefore delays publication by that much and no more; measured at
/// 3.36s against a `tailscale` shim that sleeps for two minutes. The rest is
/// margin for process start, and what is being told apart is "published
/// somewhere else" from "never published at all".
pub(crate) const PORTFILE_DEADLINE: Duration = Duration::from_secs(10);

/// Sends `GET /` and returns the response's status line, or `None` while the
/// server is not answering. A read timeout is essential: the failure this
/// exists to catch is a listener that accepts the connection and then says
/// nothing.
pub fn http_status_line(port: u16, timeout: Duration) -> Option<String> {
    let stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream.set_read_timeout(Some(timeout)).ok()?;
    let mut writer = stream.try_clone().ok()?;
    write!(
        writer,
        "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    Some(line)
}

/// Runs `cmd`, bounding the whole spawn-wait-collect sequence by `deadline`,
/// and panics naming `what` if it is not done in time.
///
/// [`Command::output`] waits on the child and *then* reads its pipes to
/// end-of-file — two waits, not one, and the second is unbounded: any
/// descendant that inherited the pipe and outlives the child holds it exactly
/// as long as it likes (SH-94, SH-141). Nowhere is that worse than inside a
/// test [`Drop`]: it runs during unwind, so a test that is already failing
/// hangs instead of reporting, and the only thing that ever ends it is a
/// wall-clock timeout several layers up, long after the evidence is gone
/// (SH-142).
///
/// The worker thread is not joined on timeout — it is blocked in a syscall
/// nothing here can interrupt — so leaving it behind is the price of
/// reporting the failure at all. It ends when the test binary's process does.
pub fn run_bounded(mut cmd: Command, what: &str, deadline: Duration) -> Output {
    let (tx, rx) = mpsc::channel();
    let label = what.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).output());
    });

    match rx.recv_timeout(deadline) {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => panic!("spawning `{label}`: {e}"),
        // The last time this fired elsewhere, the child had already exited and
        // the block was here, in the parent, because a detached daemon was
        // holding the write end of this pipe. `lsof -p <pid of this test
        // binary>` names the pipe; searching the other processes for the same
        // address names whoever holds it — see tests/daemon_fd_hygiene.rs.
        Err(_) => panic!(
            "`{label}` did not finish within {deadline:?} — a deadlock rather than \
             slowness, because every wait inside a `story` command is bounded."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::TestEnv;
    use storyhook::store::{ReadOps, Store};

    /// A private environment, its store, and one project in it.
    ///
    /// Isolated rather than shared: these tests assert on what a *whole* server
    /// answers, so a sibling test's project appearing in the catalog would make
    /// the assertions depend on which tests ran.
    fn served_project() -> (TestEnv, Arc<SqliteStore>, String) {
        let env = TestEnv::isolated();
        let project = env.project().seed_story("A story").build();
        let store = Arc::new(env.open_store());
        let id = project
            .try_project_id(&store)
            .expect("the fixture project is in the store");
        let slug = Store::read(&*store, |tx| {
            Ok(tx.project(id)?.expect("the project row").slug)
        })
        .expect("reading the project");
        (env, store, slug)
    }

    #[test]
    fn reserved_ports_are_free_distinct_and_outside_the_ephemeral_range() {
        let ports: Vec<_> = (0..8).map(|_| reserve_port()).collect();
        let mut unique = ports.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), ports.len(), "reservations must not repeat");
        for port in ports {
            assert!(
                (19000..29000).contains(&port),
                "a port drawn from the kernel's ephemeral range can be stolen by an \
                 in-process server binding port 0 (SH-51); got {port}"
            );
        }
    }

    /// SH-394: `reserve_port`'s old wraparound branch read the shared
    /// counter, decided out-of-band, and only *then* wrote a reset — a gap
    /// another thread could land in and walk away with the identical port.
    /// That is specifically a concurrency defect, so the regression pin has
    /// to actually be concurrent: this crate's own tests already run
    /// `--test-threads=4`, and `reserve_port` is a shared static counter
    /// across every one of them, which is what let the sequential 8-call
    /// test above fail from a race that was not inside its own call
    /// sequence at all (`reservations must not repeat: left: 7, right: 8`,
    /// from a real `cargo test --workspace` run).
    ///
    /// # Why `THREADS * PER_THREAD` stays under the band's own width (10,000)
    ///
    /// The first version of this test requested *more* ports than the band
    /// holds (12,000 of 10,000), on the theory that overrunning the band
    /// would force the old boundary-crossing branch to fire at least once.
    /// It does — but it is the wrong test: once total calls exceed the
    /// band's width, `raw % SPAN` cycles back through values it has already
    /// returned even under the fix, because a 10,000-slot band mathematically
    /// cannot hand out 12,000 distinct ports. Mutation-checked directly, and
    /// the numbers say exactly this: reverted to the old branch, 4 of 4 runs
    /// failed at "2000 of 12000 collided"; the *fix restored*, 8 of 8 runs
    /// *also* failed, at the identical "2000 of 12000" — proving the
    /// over-large volume was measuring band exhaustion, not the race, and
    /// would have "caught" a correct implementation just as readily as a
    /// broken one. Below the band's width, exhaustion cannot happen, so any
    /// collision left to observe is a real one.
    ///
    /// # What this test does and does not establish
    ///
    /// It cannot force the exact instant the old counter crossed the band
    /// boundary — that depended on a random per-process starting offset this
    /// test does not control — so it is a concurrent smoke test, not a
    /// guaranteed reproduction. Measured directly at this test's own volume:
    /// against the reverted (old) branch, 4 of 15 runs failed, each a
    /// different random starting offset landing the shared counter near the
    /// boundary at just the right moment — exactly the load-dependent,
    /// non-deterministic shape the real failure had. The actual correctness
    /// argument for the fix is structural, not statistical:
    /// `AtomicU32::fetch_add` is a single hardware-atomic read-modify-write
    /// with no gap another thread can land in, and every value it can ever
    /// return is guaranteed distinct from every other, so `% SPAN` — a pure
    /// function of that one value, touching no other shared state — cannot
    /// map two concurrent calls to the same port unless total calls exceed
    /// `SPAN`, which is exactly the bound kept below.
    #[test]
    fn concurrent_reservations_never_collide() {
        const THREADS: usize = 32;
        const PER_THREAD: usize = 100;

        let ports: Vec<u16> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..THREADS)
                .map(|_| {
                    scope.spawn(|| (0..PER_THREAD).map(|_| reserve_port()).collect::<Vec<_>>())
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().expect("a reserving thread panicked"))
                .collect()
        });

        assert_eq!(
            ports.len(),
            THREADS * PER_THREAD,
            "every thread must have reported"
        );

        let mut unique = ports.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            ports.len(),
            "{} of {} concurrent reservations collided",
            ports.len() - unique.len(),
            ports.len()
        );
    }

    /// A foreign listener on the port the harness is about to use stands in for
    /// the orphaned `web_test` server from an earlier run that caused SH-51:
    /// having lost the bind, the harness must fail loudly rather than hand the
    /// test a port answered by somebody else (which surfaced as a wall of
    /// inexplicable 404s).
    #[test]
    fn serving_an_occupied_port_fails_loudly_instead_of_trusting_the_squatter() {
        let (env, store, _slug) = served_project();

        let squatter = TcpListener::bind("127.0.0.1:0").expect("binding the squatter");
        let squatted = squatter.local_addr().unwrap().port();

        let outcome = try_serve_on(store, &env.environment(), squatted);
        let error = match outcome {
            Err(error) => error,
            Ok(bound) => panic!(
                "the harness reported a ready server on port {}, which is held by a foreign \
                 listener — every request the test makes would go to that stranger (SH-51)",
                bound.port()
            ),
        };
        assert!(
            error.to_lowercase().contains("in use"),
            "the failure must name the real cause (the port is taken), got: {error}"
        );
    }

    /// Two servers started in the same run must never be handed the same port,
    /// and each must answer only for the store it was started with — the
    /// property that the fixed 19000-based counter could not guarantee across
    /// concurrent runs.
    #[test]
    fn concurrent_servers_get_distinct_ports_and_serve_only_their_own_store() {
        let (env_a, store_a, slug_a) = served_project();
        let (env_b, store_b, slug_b) = served_project();

        let server_a = serve(store_a, &env_a.environment());
        let server_b = serve(store_b, &env_b.environment());
        assert_ne!(
            server_a.port(),
            server_b.port(),
            "two servers must not share a port"
        );

        let own = ureq::get(format!(
            "http://127.0.0.1:{}/api/repos/{slug_a}/data",
            server_a.port()
        ))
        .header("X-Storyhook-Token", &server_a.token)
        .call()
        .expect("a server must serve the store it was started with");
        assert_eq!(own.status(), 200);

        // The two fixtures mint the same slug from the same fixture name, so
        // this asks the sharper question: does B answer for A's *project*, or
        // only for the row in its own database?
        let cross = ureq::get(format!(
            "http://127.0.0.1:{}/api/repos/{slug_b}/data",
            server_b.port()
        ))
        .header("X-Storyhook-Token", &server_b.token)
        .call()
        .expect("each server answers for its own store");
        assert_eq!(cross.status(), 200);
    }

    #[test]
    fn a_served_store_answers_on_the_port_it_reports() {
        let (env, store, _slug) = served_project();

        let port = serve(store, &env.environment()).port();
        let line = http_status_line(port, Duration::from_secs(5));
        assert!(
            line.as_deref().is_some_and(|l| l.contains("200")),
            "serve() must not return until the server actually answers; got {line:?}"
        );
    }

    #[test]
    fn run_bounded_returns_the_childs_actual_output() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf ok; exit 3"]);

        let out = run_bounded(cmd, "printf ok", Duration::from_secs(5));
        assert_eq!(out.stdout, b"ok");
        assert_eq!(out.status.code(), Some(3));
    }

    #[test]
    fn child_guard_collects_output_larger_than_a_pipe_without_deadlocking() {
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            "head -c 1048576 /dev/zero; printf problem >&2; exit 3",
        ]);
        let mut child = ChildGuard::spawn_with_output(&mut cmd).expect("spawning noisy child");

        let out = child.wait_with_output_within(Duration::from_secs(5), || {
            "the noisy child did not finish".to_string()
        });

        assert_eq!(out.stdout.len(), 1_048_576);
        assert_eq!(out.stderr, b"problem");
        assert_eq!(out.status.code(), Some(3));
    }

    #[test]
    fn child_guard_closes_stdin_before_its_bounded_wait() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "cat"]).stdin(Stdio::piped());
        let mut child = ChildGuard::spawn_with_output(&mut cmd).expect("spawning cat");
        child
            .stdin()
            .expect("cat stdin was piped")
            .write_all(b"done")
            .expect("writing cat stdin");

        let out = child.wait_with_output_within(Duration::from_secs(5), || {
            "cat did not observe stdin closing".to_string()
        });

        assert_eq!(out.stdout, b"done");
    }

    #[test]
    fn child_guard_kills_a_child_that_exceeds_its_deadline() {
        let mut cmd = Command::new("sleep");
        cmd.arg("5");
        let mut child = ChildGuard::spawn(&mut cmd).expect("spawning sleep");
        let deadline = Duration::from_millis(300);
        let give_up_ceiling = deadline * 7;

        let started = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            child.wait_within(deadline, || "sleep did not finish".to_string())
        }));

        assert!(result.is_err(), "an overlong child must fail its wait");
        assert!(
            started.elapsed() < give_up_ceiling,
            "the guard waited out the child instead of enforcing {deadline:?}"
        );
        assert!(
            child.try_wait().is_some(),
            "the timeout must reap the child before it reports"
        );
    }

    #[test]
    fn child_guard_bounds_a_pipe_held_by_a_descendant_after_the_child_exits() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 5 & printf done"]);
        let mut child = ChildGuard::spawn_with_output(&mut cmd).expect("spawning pipe holder");
        let deadline = Duration::from_millis(300);
        let give_up_ceiling = deadline * 7;

        let started = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            child.wait_with_output_within(deadline, || "the descendant retained a pipe".to_string())
        }));

        assert!(result.is_err(), "a retained pipe must fail its wait");
        assert!(
            started.elapsed() < give_up_ceiling,
            "the guard waited for the descendant instead of enforcing {deadline:?}"
        );
    }

    /// SH-142: `DaemonGuard`'s `Drop` used to call `.output()` directly, which
    /// waits on the child and *then* reads its pipes to end-of-file with no
    /// bound at all. A command that never finishes — standing in for a child
    /// whose exit does not release the pipe, the SH-94/SH-141 hazard — would
    /// have hung this test (and, inside a `Drop`, the whole suite) rather than
    /// failing inside `deadline`.
    #[test]
    fn run_bounded_gives_up_on_its_deadline_rather_than_waiting_out_a_hung_command() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 5"]);
        let deadline = Duration::from_millis(300);
        // Named rather than inline (SH-394): margin for a subprocess spawn
        // atop the 300ms deadline itself, not a claim about speed.
        let give_up_ceiling = deadline * 7;

        let started = Instant::now();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            run_bounded(cmd, "sleep 5", deadline)
        }));
        let elapsed = started.elapsed();

        assert!(
            result.is_err(),
            "a command that outlives its deadline must be reported, not returned as if \
             it had finished"
        );
        assert!(
            elapsed < give_up_ceiling,
            "run_bounded took {elapsed:?} to give up on a {deadline:?} deadline — it waited \
             out the hung command instead of bounding the wait"
        );
    }
}
