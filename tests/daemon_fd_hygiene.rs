//! What a detached daemon is allowed to inherit.
//!
//! # Why this file exists
//!
//! A daemon outlives the command that started it, so every file descriptor it
//! inherits is held *forever*. One of those descriptors being the write end of
//! somebody else's pipe is not a leak that shows up as memory: it is a
//! **deadlock**. The reader of that pipe waits for end-of-file, end-of-file
//! arrives only when the last writer closes, and the daemon is a writer that
//! never closes.
//!
//! That is not a thought experiment. It was captured live during SH-94, in this
//! repository's own gate:
//!
//! ```text
//! store_iso 21517  fd 5  PIPE 0x12fc…432e ->0x69f2…bcad     (blocked in read)
//! story     21548  fd 5  PIPE 0x12fc…432e ->0x69f2…bcad
//! story     21548  fd 7  PIPE 0x69f2…bcad ->0x12fc…432e     (the write end)
//! ```
//!
//! PID 21548 is a `story … daemon --serve`. Its own stdio was redirected
//! correctly — `0` and `1` to `/dev/null`, `2` to its log — so fds 5 and 7 are
//! descriptors nobody meant it to have. The test binary sat in `read(2)` for
//! four minutes; killing the daemon released it in under three seconds, and the
//! test then passed.
//!
//! The deadlock is total rather than slow, because a daemon stops when its
//! parent does (`STORYHOOK_PARENT_PID`) and its parent is the process it is
//! blocking. Neither can move until the other does.
//!
//! # Why a descriptor arrives by accident at all
//!
//! `std` creates the pipes behind `Stdio::piped()` with `pipe2(O_CLOEXEC)`
//! where that call exists. macOS has no `pipe2`, so `std` falls back to
//! `pipe(2)` followed by a *separate* syscall that sets close-on-exec — and
//! between those two syscalls the descriptors are inheritable. A `fork` in
//! another thread of the same process during that window carries both ends into
//! the child. The window is two instructions wide when the machine is idle and
//! as wide as a scheduling quantum when it is not, which is exactly why the
//! symptom is load-dependent.
//!
//! [`inheritable_pipe`] does not race for that window. It creates the same
//! descriptors in the same state deliberately, so the property under test —
//! *the daemon closes what it was not given* — is asserted rather than
//! gambled on.

use std::io::Read;
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::mpsc;
use std::time::Duration;

use storyhook::daemon::lifecycle;
use storyhook_test_support::{TestEnv, scratch_dir};

/// How long end-of-file may take to arrive once nothing should be holding the
/// write end.
///
/// This is not a performance budget and must not be read as one. Every writer
/// has closed by the time it starts, so the correct answer is available
/// immediately and the only thing being distinguished is "arrived" from "never
/// arrives". Ten seconds is a wedge, not a slow machine.
const EOF_DEADLINE: Duration = Duration::from_secs(10);

/// Stops whatever daemon `env` is running, however the test ends.
///
/// It also releases a blocked reader: a daemon holding a stolen write end
/// closes it by dying, so a failing assertion below does not leave the test
/// binary unable to exit.
struct DaemonGuard<'a>(&'a TestEnv);

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        let _ = lifecycle::stop(&self.0.environment(), lifecycle::StopMode::Force);
    }
}

/// A pipe whose two descriptors are **not** close-on-exec.
///
/// The state `std`'s own pipes pass through on every platform without `pipe2`.
/// Created on purpose here so the assertion does not depend on winning a race.
fn inheritable_pipe() -> (OwnedFd, OwnedFd) {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` is a two-element array of the right type, which is the
    // whole contract of `pipe(2)`. Both descriptors are handed to `OwnedFd`
    // immediately, so each is closed exactly once.
    let created = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(
        created,
        0,
        "creating a pipe: {}",
        std::io::Error::last_os_error()
    );
    // The premise, checked rather than assumed. If a platform ever returned
    // close-on-exec descriptors from `pipe(2)`, nothing would be inheritable,
    // the test below would pass without the fix present, and it would go on
    // reading as coverage. A test whose premise has quietly evaporated is worse
    // than no test.
    for fd in fds {
        // SAFETY: `fd` was just returned by `pipe(2)` and is still open.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert_eq!(
            flags & libc::FD_CLOEXEC,
            0,
            "this test needs an inheritable descriptor to have something to \
             inherit; fd {fd} came back from pipe(2) already close-on-exec"
        );
    }
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

/// Reads to end-of-file with a deadline, and says which of the two things
/// happened.
///
/// The reader thread is not joined on timeout — it is blocked in `read(2)` and
/// nothing here can interrupt it. [`DaemonGuard`] is what releases it.
fn read_to_eof(reader: OwnedFd) -> Result<usize, ()> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut file = std::fs::File::from(reader);
        let mut buffer = Vec::new();
        let _ = tx.send(file.read_to_end(&mut buffer));
    });
    match rx.recv_timeout(EOF_DEADLINE) {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(e)) => panic!("reading the pipe: {e}"),
        Err(_) => Err(()),
    }
}

/// Starting a daemon must not hand it descriptors nobody gave it.
///
/// The pipe here stands in for any descriptor that happens to be open when a
/// client decides a daemon is needed — in the captured failure it was another
/// test's `Stdio::piped()` stdout, and on a developer's machine it is whatever
/// the shell, the editor or the git hook that ran `story` had open.
///
/// The shape of the assertion matters: it does not read the daemon's fd table,
/// it asks for the *consequence*. A descriptor the daemon does not hold is one
/// whose end-of-file arrives; a descriptor it does hold is a reader that waits
/// for a process with no reason to exit. Reading the table would pin one
/// implementation of the fix, and this pins the promise.
///
/// Three ways this test could go green for the wrong reason, two of them closed
/// here and one that cannot be:
///
/// * **The daemon died.** A dead daemon closes everything, so end-of-file
///   arrives and the assertion passes with the fix reverted. `DaemonInfo` comes
///   from `read_info`, which parses a portfile and asks nothing about liveness,
///   so the daemon is checked to be *answering* after the read rather than to
///   have *started* before it.
/// * **Nothing was inheritable.** [`inheritable_pipe`] checks its own premise.
/// * **The descriptor was dropped one hop earlier.** If Rust ever adopts
///   `POSIX_SPAWN_CLOEXEC_DEFAULT` on macOS, the intermediate `story` client
///   stops inheriting the pipe, the premise check still passes because it runs
///   in *this* process, and this test greens forever without exercising the
///   fix. Nothing here can detect that. If the test has not failed in a very
///   long time and this note is still present, disarm
///   `disinherit_descriptors` once and confirm it still goes red.
#[test]
fn a_daemon_inherits_nothing_but_the_stdio_it_was_given() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let cwd = scratch_dir();

    let (reader, writer) = inheritable_pipe();

    // The client is an ordinary `story` process, so the daemon arrives the way
    // it always does: as this process's grandchild, through whatever the client
    // inherited.
    env.story(cwd.path())
        .args(["daemon", "start"])
        .assert()
        .success();
    let info = env.daemon().expect(
        "the test needs a daemon to have started; without one it would pass for the wrong reason",
    );

    // Now nothing that should exist holds the write end: this process drops its
    // copy here, and the client that carried it into the daemon has exited.
    drop(writer);

    match read_to_eof(reader) {
        Ok(0) => {}
        Ok(bytes) => panic!("nothing wrote to this pipe, yet {bytes} bytes came out of it"),
        Err(()) => panic!(
            "the daemon is holding a descriptor it was never given: no writer is \
             left, yet end-of-file has not arrived in {EOF_DEADLINE:?}. A detached \
             process holds an inherited descriptor for its whole life, so the \
             reader waits for a close that cannot come — and the daemon will not \
             exit before its parent, which is the process now blocked. See this \
             file's header for the captured instance."
        ),
    }

    // End-of-file also arrives when the daemon has *died*, which would pass the
    // assertion above with the fix reverted. So the daemon is required to be
    // answering now, after the read — not merely to have published a portfile
    // before it.
    lifecycle::hello(&info).expect(
        "the daemon must still be answering after end-of-file arrives. A dead \
         daemon closes every descriptor it holds, including a stolen one, so \
         this test would otherwise pass for the one reason that proves nothing",
    );

    // And it must still have the stdio it *was* given. Nothing above would fail
    // if `disinherit_descriptors` started at fd 0 instead of fd 3: the daemon
    // would exec with no stdio at all, the stolen pipe would still be closed,
    // and the assertion would still pass. Its log is where it writes, so a log
    // with something in it is fd 2 surviving.
    let log = env.environment().daemon_log();
    let written = std::fs::metadata(&log).map(|meta| meta.len()).unwrap_or(0);
    assert!(
        written > 0,
        "the daemon wrote nothing to {}: it should keep the three descriptors it \
         was given, and disinheriting must start above them",
        log.display()
    );
}
