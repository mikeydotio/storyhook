//! SH-794: blocking fixture contracts, independent of production supervision.

use super::{
    ChildGuard, Duration, Path, PathBuf, assert_ok, checkout, fs, load_grace, scratch_dir,
};
use std::io::{ErrorKind, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use tempfile::TempDir;

const PATIENCE: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(20);

/// An already-listening, short socket path with one explicit release message.
pub(super) struct ReleaseBarrier {
    listener: UnixListener,
    path: PathBuf,
    _directory: TempDir,
}

impl ReleaseBarrier {
    /// Bind before launching the hook; a long worktree path exceeds sun_path.
    pub(super) fn new() -> Self {
        let directory = scratch_dir();
        let path = directory.path().join("release.sock");
        let listener = UnixListener::bind(&path).expect("bind restoration barrier");
        listener.set_nonblocking(true).unwrap();
        Self {
            listener,
            path,
            _directory: directory,
        }
    }

    /// The socket passed to the blocking leaf.
    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// Wait with the repository's contention policy, never a blocking accept.
    pub(super) fn accept(&self) -> UnixStream {
        load_grace::wait_for(
            load_grace::Patience::new(PATIENCE),
            POLL,
            || format!("no fixture connected to {}", self.path.display()),
            || match self.listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(true).unwrap();
                    Some(stream)
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => None,
                Err(error) => panic!("accept {}: {error}", self.path.display()),
            },
        )
    }
}

fn spawn(args: &[&str]) -> ChildGuard {
    let mut command = Command::new("python3");
    command
        .arg(checkout().join("tests/support/merge_gate_wait.py"))
        .args(args);
    ChildGuard::spawn_with_output(&mut command).expect("spawn blocking fixture")
}

fn ready_pid(path: &Path, child: &mut ChildGuard) -> u32 {
    load_grace::wait_for(
        load_grace::Patience::new(PATIENCE),
        POLL,
        || format!("blocking fixture did not publish {}", path.display()),
        || {
            assert!(
                child.try_wait().is_none(),
                "fixture exited before readiness at {}",
                path.display()
            );
            match fs::read_to_string(path) {
                Ok(text) => {
                    assert!(text.ends_with('\n'), "incomplete PID publication: {text:?}");
                    Some(
                        text.trim()
                            .parse()
                            .expect("fixture readiness must name a PID"),
                    )
                }
                Err(error) if error.kind() == ErrorKind::NotFound => None,
                Err(error) => panic!("read {}: {error}", path.display()),
            }
        },
    )
}

#[test]
fn signal_waiter_is_a_leaf_and_ready_for_hup_and_term() {
    for signal in [libc::SIGHUP, libc::SIGTERM] {
        let directory = scratch_dir();
        let marker = directory.path().join("ready");
        let mut child = spawn(&["signal", marker.to_str().unwrap(), "0"]);
        let pid = ready_pid(&marker, &mut child);
        assert_eq!(pid, child.pid());
        let census = Command::new("ps")
            .args(["-axo", "pid=,ppid="])
            .output()
            .unwrap();
        assert_ok(&census, "census the fixture's direct descendants");
        assert!(
            !String::from_utf8_lossy(&census.stdout).lines().any(|line| {
                line.split_whitespace()
                    .nth(1)
                    .and_then(|value| value.parse::<u32>().ok())
                    == Some(pid)
            }),
            "the signal waiter must not create a sleep grandchild"
        );
        assert_eq!(unsafe { libc::kill(pid.try_into().unwrap(), signal) }, 0);
        assert_eq!(
            child
                .wait_within(load_grace::graced_now(PATIENCE), || {
                    "signal waiter survived".into()
                })
                .signal(),
            Some(signal)
        );
    }
}

#[test]
fn barrier_requires_release_and_refuses_eof_or_invalid_bytes() {
    for message in [Some(b'1'), None, Some(b'x')] {
        let barrier = ReleaseBarrier::new();
        let directory = scratch_dir();
        let marker = directory.path().join("ready");
        let mut child = spawn(&[
            "barrier",
            marker.to_str().unwrap(),
            barrier.path().to_str().unwrap(),
            "123",
        ]);
        assert_eq!(ready_pid(&marker, &mut child), 123);
        let mut connection = barrier.accept();
        assert!(child.try_wait().is_none(), "barrier passed before release");
        if let Some(byte) = message {
            connection.write_all(&[byte]).unwrap();
        }
        drop(connection);
        let out = child.wait_with_output_within(load_grace::graced_now(PATIENCE), || {
            "barrier ignored release or EOF".into()
        });
        if message == Some(b'1') {
            assert_ok(&out, "explicitly released barrier");
        } else {
            assert!(!out.status.success());
            assert!(String::from_utf8_lossy(&out.stderr).contains("release barrier"));
        }
    }
}

#[test]
fn merge_gate_has_no_shell_busy_waits() {
    let source = include_str!("../merge_gate.rs").replace("\\n", "\n");
    let spins = regex::Regex::new(r"while [^;]*;\s*do\s*:\s*;\s*done").unwrap();
    let hits: Vec<_> = spins.find_iter(&source).map(|hit| hit.as_str()).collect();
    assert!(
        hits.is_empty(),
        "blocking fixtures must replace busy waits: {hits:?}"
    );
}
