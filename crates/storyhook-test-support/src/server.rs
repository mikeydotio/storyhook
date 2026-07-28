//! Dashboard servers for tests: how to start one, how to know it is ready, and
//! how to guarantee it does not outlive the test that started it.
//!
//! Every rule enforced here was paid for by SH-51, where two orphaned test
//! servers from an earlier run held ~100 ports and answered a later run's
//! requests out of a stale registry — 78 of 139 tests down, with nothing in the
//! output pointing anywhere near the cause.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::env::story_binary;

/// Picks a port for the handful of tests that must know one *before* the
/// thing that binds it exists — `story web start --port N` binds in a child
/// process, and reports success as soon as it has spawned that child, so a
/// port it loses is never reported to anyone.
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
    use std::sync::atomic::{AtomicU16, Ordering};

    // Above the registered-port range, below the ephemeral range macOS and
    // Linux draw from (49152+ / 32768+ — the band avoids both).
    const BAND: std::ops::Range<u16> = 19000..29000;

    static NEXT: std::sync::LazyLock<AtomicU16> = std::sync::LazyLock::new(|| {
        let entropy = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
            ^ std::process::id();
        let span = (BAND.end - BAND.start) as u32;
        AtomicU16::new(BAND.start + (entropy % span) as u16)
    });

    for _ in 0..64 {
        let candidate = NEXT.fetch_add(1, Ordering::Relaxed);
        let candidate = if BAND.contains(&candidate) {
            candidate
        } else {
            NEXT.store(BAND.start, Ordering::Relaxed);
            BAND.start
        };
        // Binding and immediately releasing proves nothing else holds it —
        // including a daemon leaked by an earlier run, the exact hazard the
        // fixed counter walked straight into.
        if TcpListener::bind(("127.0.0.1", candidate)).is_ok() {
            return candidate;
        }
    }
    panic!("no free port in {BAND:?} — is an earlier run's daemon still holding them?");
}

/// Starts a dashboard server for `registry_path` on an OS-assigned port and
/// returns that port once the server is serving. The sanctioned way for a test
/// to get a server: no test picks its own port, and no test waits on anything
/// weaker than the server's own readiness signal.
pub fn serve(registry_path: &Path) -> u16 {
    try_serve_on(registry_path, 0).unwrap_or_else(|e| panic!("starting a test server: {e}"))
}

/// [`serve`], but on a caller-chosen `port` and returning the server's own
/// start-up failure instead of panicking.
///
/// The failure must never be swallowed. A server that loses the bind leaves
/// whatever *else* holds that port answering the test's requests, and a
/// stranger's registry answers `404` to everything the test asks about — the
/// mass-failure mode of SH-51. Readiness comes from the server's `ready`
/// callback rather than a `connect()` probe for the same reason: only the
/// server can attest that the address is one it actually bound, and that it
/// has finished loading state (see `web::start_server_with_ready`).
pub fn try_serve_on(registry_path: &Path, port: u16) -> Result<u16, String> {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel::<Result<u16, String>>();
    let ready_tx = tx.clone();
    let path = registry_path.to_path_buf();
    std::thread::spawn(move || {
        let outcome = storyhook::web::start_server_with_ready(&path, port, move |addr| {
            let _ = ready_tx.send(Ok(addr.port()));
        });
        if let Err(e) = outcome {
            let _ = tx.send(Err(e.to_string()));
        }
    });

    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(bound)) => {
            wait_for_server(bound);
            Ok(bound)
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err(format!(
            "a server for {} neither became ready nor reported a failure within 10s",
            registry_path.display()
        )),
    }
}

/// Stops the `story web start` daemon a test launched, even if the test
/// panics first. Test-spawned servers that outlive the test are exactly what
/// poisoned later runs in SH-51; the in-process ones die with the test
/// binary, but a daemon is a detached child process and only an explicit
/// stop reaps it.
pub struct DaemonGuard {
    home: PathBuf,
    cwd: PathBuf,
}

impl DaemonGuard {
    /// Arms the guard for a daemon started from `cwd` with `HOME` set to
    /// `home` — the same pair must be passed, because `web stop` finds the
    /// daemon through `$HOME/.storyhook/web.pid`.
    pub fn new(home: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        DaemonGuard {
            home: home.into(),
            cwd: cwd.into(),
        }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new(story_binary())
            .current_dir(&self.cwd)
            .env("HOME", &self.home)
            .args(["web", "stop"])
            .output();
    }
}

/// Kills a child process on drop, so a server a test started can never
/// outlive it.
pub struct ChildGuard(std::process::Child);

impl ChildGuard {
    /// Takes ownership of `child`.
    pub fn new(child: std::process::Child) -> Self {
        ChildGuard(child)
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

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
        if start.elapsed() > Duration::from_secs(5) {
            panic!("{addr} did not start accepting connections within 5 seconds");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::TestEnv;
    use crate::scratch::scratch_dir;

    /// Registers `dir` in a fresh registry and returns the registry's guard,
    /// path, and the id minted for `dir`.
    fn register(dir: &Path) -> (tempfile::TempDir, PathBuf, String) {
        let registry_dir = scratch_dir();
        let registry_path = registry_dir.path().join("registry.toml");
        let repo = storyhook::registry::with_lock_at(&registry_path, |r| r.register(dir, None))
            .expect("registering the test repo must succeed");
        (registry_dir, registry_path, repo.id)
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

    /// A foreign listener on the port the harness is about to use stands in for
    /// the orphaned `web_test` server from an earlier run that caused SH-51:
    /// having lost the bind, the harness must fail loudly rather than hand the
    /// test a port answered by somebody else's registry (which surfaced as a
    /// wall of inexplicable 404s).
    #[test]
    fn serving_an_occupied_port_fails_loudly_instead_of_trusting_the_squatter() {
        let env = TestEnv::shared();
        let project = env.project().build();
        let (_registry_dir, registry_path, _repo_id) = register(project.path());

        let squatter = TcpListener::bind("127.0.0.1:0").expect("binding the squatter");
        let squatted = squatter.local_addr().unwrap().port();

        let outcome = try_serve_on(&registry_path, squatted);
        let error = match outcome {
            Err(error) => error,
            Ok(port) => panic!(
                "the harness reported a ready server on port {port}, which is held by a foreign \
                 listener — every request the test makes would go to that stranger (SH-51)"
            ),
        };
        assert!(
            error.to_lowercase().contains("in use"),
            "the failure must name the real cause (the port is taken), got: {error}"
        );
    }

    /// Two servers started in the same run must never be handed the same port,
    /// and each must answer only for the registry it was started with — the
    /// property that the fixed 19000-based counter could not guarantee across
    /// concurrent runs.
    #[test]
    fn concurrent_servers_get_distinct_ports_and_serve_only_their_own_registry() {
        let env = TestEnv::shared();
        let project_a = env.project().build();
        let (_registry_dir_a, registry_a, id_a) = register(project_a.path());

        let project_b = env.project().build();
        let (_registry_dir_b, registry_b, id_b) = register(project_b.path());

        assert_ne!(id_a, id_b, "the two fixtures must be distinguishable");

        let port_a = serve(&registry_a);
        let port_b = serve(&registry_b);
        assert_ne!(port_a, port_b, "two servers must not share a port");

        let own = ureq::get(format!("http://127.0.0.1:{port_a}/api/repos/{id_a}/data"))
            .call()
            .expect("a server must serve the registry it was started with");
        assert_eq!(own.status(), 200);

        let cross = ureq::get(format!("http://127.0.0.1:{port_b}/api/repos/{id_a}/data")).call();
        let status = match cross.expect_err("a server must not answer for another server's repo") {
            ureq::Error::StatusCode(code) => code,
            other => panic!("expected an HTTP status error, got: {other}"),
        };
        assert_eq!(status, 404);
    }

    #[test]
    fn a_served_registry_answers_on_the_port_it_reports() {
        let env = TestEnv::shared();
        let project = env.project().build();
        let (_registry_dir, registry_path, _id) = register(project.path());

        let port = serve(&registry_path);
        let line = http_status_line(port, Duration::from_secs(5));
        assert!(
            line.as_deref().is_some_and(|l| l.contains("200")),
            "serve() must not return until the server actually answers; got {line:?}"
        );
    }
}
