//! Killing the process that owns storyhook's write transaction.
//!
//! The CLI reaches the store only through a daemon, so the process that owns a
//! write transaction **is** the daemon. A crash test that killed the client
//! would be killing a process that holds nothing: it has already handed the work
//! over the wire, and the transaction it is waiting on belongs to somebody else.
//!
//! So every case arms a daemon of its own, gives it exactly one command, and
//! asks what the corpse left behind. The arming is why the daemon is spawned by
//! hand rather than auto-started: choosing a child's environment is something
//! only the process that spawns it can do.
//!
//! # The two hazards that make this shape pass while testing nothing
//!
//! Both are asserted here rather than left to each caller, because a caller that
//! forgets one still looks like a test.
//!
//! 1. **A daemon that was already running.** A client starts one whenever none
//!    is live, so a fixture that built its project through the daemon still has
//!    one — and *it*, not the armed process, would take the work. The corpse
//!    would be the wrong process and the fault would never fire.
//!    [`crash_the_daemon`] stands the incumbent down before arming, and asserts
//!    that it did.
//! 2. **A daemon running at inspection.** A question about bytes on disk is
//!    answered out of a live daemon's page cache, and opening the store
//!    checkpoints the write-ahead log a test may be asking about. Asserted once
//!    the corpse has been reaped, so that whatever the caller does next is
//!    looking at the file.
//!
//! Neither hazard is hypothetical: the first is why the hand-armed daemon case
//! failed when the whole suite was first run with no local transport at all.

use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{ExitStatus, Output, Stdio};

use storyhook::store::FaultPoint;

use crate::env::TestEnv;
use crate::server::{ChildGuard, wait_for_server};

/// What one armed daemon left behind, and what its client was told.
pub struct Crash {
    armed_pid: u32,
    daemon: ExitStatus,
    client: Output,
}

impl Crash {
    /// How the armed daemon died.
    pub fn daemon(&self) -> ExitStatus {
        self.daemon
    }

    /// The process id this crash armed — and, because a [`Crash`] only exists
    /// once that process has been reaped, the process id that died.
    pub fn armed_pid(&self) -> u32 {
        self.armed_pid
    }

    /// What the client saw. Unlike the daemon, the client survives, and for
    /// several cases what it *said* is the whole subject.
    pub fn client(&self) -> &Output {
        &self.client
    }

    /// The client's stderr, decoded.
    pub fn client_stderr(&self) -> String {
        String::from_utf8_lossy(&self.client.stderr).into_owned()
    }
}

/// Runs `story <args>` in `cwd` against a daemon armed to die at `point`.
///
/// For the points inside a write transaction. The daemon starts normally, serves
/// the one request this makes, and is killed part-way through it — which is the
/// experiment, so a daemon that exits any other way is a failure naming the most
/// likely cause.
pub fn crash_the_daemon(env: &TestEnv, cwd: &Path, point: FaultPoint, args: &[&str]) -> Crash {
    let mut armed = arm_a_daemon(env, cwd, point);
    wait_for_server(port_of(env, armed.pid()));

    let armed_pid = armed.pid();
    let client = client_command(env, cwd)
        .args(args)
        .output()
        .expect("running the command whose daemon is about to die");
    let daemon = armed.wait();

    assert_eq!(
        daemon.signal(),
        Some(libc::SIGKILL),
        "{}: the daemon running `story {}` was supposed to be killed at {}; it finished with \
         {daemon:?} instead.\nclient stdout: {}\nclient stderr: {}",
        diagnose(daemon.signal()),
        args.join(" "),
        point.as_str(),
        String::from_utf8_lossy(&client.stdout),
        String::from_utf8_lossy(&client.stderr),
    );
    assert_no_daemon(env, "after the crash");

    Crash {
        armed_pid,
        daemon,
        client,
    }
}

/// Kills a daemon at a point it reaches while *opening* the store, before it has
/// bound anything.
///
/// The migration points live here. They fire inside `open_store`, which runs
/// before the daemon claims its pidfile or binds a port, so there is no client
/// to send and nothing to wait for — the process simply dies on the way up.
pub fn crash_a_starting_daemon(env: &TestEnv, cwd: &Path, point: FaultPoint) -> ExitStatus {
    let mut armed = arm_a_daemon(env, cwd, point);
    let status = armed.wait();
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "{}: {} must be reachable while a daemon carries the store forward; the daemon \
         finished with {status:?} instead",
        diagnose(status.signal()),
        point.as_str(),
    );
    assert_no_daemon(env, "after the crash");
    status
}

/// Spawns `story daemon --serve` in `cwd`, optionally armed to die at `point`.
///
/// The building block the two fixtures above are made of, exposed because the
/// migration race needs several at once and needs to decide for itself what
/// "finished" means for each.
///
/// `--port 0`, not a `reserve_port()`-picked number (SH-195): every caller here
/// learns the daemon's *real* port from its portfile (`port_of`, below) rather
/// than trusting the one it asked for, because `bind_preferred` treats a
/// requested port as a preference and falls back to a kernel-assigned one the
/// moment it is taken. A pre-picked port bought nothing these callers needed
/// and cost a genuine bind-then-release TOCTOU window against whatever else on
/// the machine might claim it in the milliseconds before this daemon actually
/// binds; `--port 0` has no such window, because there is no separate
/// reservation step to race.
pub fn spawn_daemon(env: &TestEnv, cwd: &Path, point: Option<FaultPoint>) -> ChildGuard {
    let mut serve = env.raw_story(cwd);
    if let Some(point) = point {
        serve.env("STORYHOOK_FAULT", point.as_str());
    }
    serve
        .args(["daemon", "--serve", "--port", "0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    ChildGuard::new(serve.spawn().expect("spawning a daemon"))
}

/// Fails unless this environment has no daemon holding its store.
///
/// The lock rather than the portfile: a portfile outlives the process that wrote
/// it, and the question here is whether anything is holding the database *now*.
pub fn assert_no_daemon(env: &TestEnv, when: &str) {
    assert!(
        !env.daemon_is_live(),
        "a daemon is holding the store {when}. It answers reads from its own page cache \
         and keeps the write-ahead log alive, so every question this test asks about the \
         file would be asked of memory instead."
    );
}

/// What a corpse that is not a `SIGKILL` most likely means.
///
/// Two answers, and telling them apart is the difference between a five-minute
/// fix and an afternoon:
///
/// * **`SIGABRT`** — the fault *fired*. `kill(getpid(), SIGKILL)` posts a signal
///   rather than stopping the calling thread, so the instruction after it can
///   still run; when that instruction was an `abort`, the process died of the
///   wrong signal and a crash test read "the fault did not fire". Pinned by
///   `tests/fault_injection.rs`.
/// * **anything else, including a clean exit** — the fault did not fire, and the
///   usual reason is a binary built without the `fault-injection` feature.
fn diagnose(signal: Option<i32>) -> &'static str {
    match signal {
        Some(libc::SIGABRT) => {
            "the daemon aborted, which means the fault fired and then lost the race with the \
             instruction after it — see `process_env_fault` in src/store/fault.rs"
        }
        _ => {
            "the fault never fired. The usual cause is a binary built without the \
             `fault-injection` feature — which is every binary `cargo build` produces, and \
             none that `cargo test` does"
        }
    }
}

/// Stands down whatever daemon is holding the store, then spawns one armed at
/// `point`.
fn arm_a_daemon(env: &TestEnv, cwd: &Path, point: FaultPoint) -> ChildGuard {
    env.stop_daemon();
    assert_no_daemon(env, "before the armed one could start");
    spawn_daemon(env, cwd, Some(point))
}

/// The port the daemon with pid `pid` bound, once it has published one.
///
/// **This is where the pid the client will talk to is checked against the pid
/// the test armed.** The portfile is how a client finds a daemon, so a portfile
/// naming somebody else is a client about to send its work to the wrong process
/// — the first of the two hazards, caught before the command is sent rather than
/// inferred afterwards from a corpse.
///
/// Read from the portfile rather than remembered from the reservation, because
/// the reservation is only a *preference*: `bind_preferred` falls back to a
/// kernel-assigned port, and a test that waited on the number it asked for would
/// wait forever on the day that fallback fired.
fn port_of(env: &TestEnv, pid: u32) -> u16 {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match env.daemon() {
            Some(info) if info.pid == pid => return info.port,
            other => assert!(
                std::time::Instant::now() < deadline,
                "the daemon this test armed (pid {pid}) never became the one a client would \
                 find. The portfile names {:?} — if that is another daemon, the command below \
                 would go to it and the armed process would never run the work.",
                other.map(|info| info.pid),
            ),
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// A `story` command, which is a client of whatever daemon is holding the store
/// — and, by the time this is called, of the one this test armed.
fn client_command(env: &TestEnv, cwd: &Path) -> std::process::Command {
    env.raw_story(cwd)
}
