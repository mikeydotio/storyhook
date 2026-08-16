//! SH-186: a wedged or absent `tailscale` must cost the dashboard, and every
//! client that autostarts it, nothing.
//!
//! # The defect this fences
//!
//! `bind_listeners` used to shell out to `tailscale status --json` (capped at
//! [`TAILNET_PROBE_TIMEOUT`]) *between* binding the loopback socket and
//! starting to serve it — so for as long as a wedged `tailscale` blocked that
//! call, the socket accepted connections and answered nothing, and the
//! portfile that publishes the daemon to every other client had not been
//! written yet either. Measured before this story's fix, with a `tailscale`
//! shim that sleeps two minutes: `story web start` took 3.28s of its 5s
//! `SPAWN_DEADLINE`, and the accepting-but-silent window on a direct
//! `story web --serve` was 2.96s.
//!
//! The fix takes the probe off that path entirely: `bind_listeners` binds
//! loopback and stops, and every tailnet bind — including the first —
//! happens on `serve::tailnet_reprobe`'s background thread (previously a
//! self-heal path only, SH-146). These tests assert the *mechanism*, not a
//! generous wall clock: every bound below is derived from
//! [`TAILNET_PROBE_TIMEOUT`] itself, so a change to that budget cannot make
//! the assertions vacuous the way the original regression test's 20s bound
//! (roughly 6x the probe's own 3s) did.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use storyhook::daemon::tailnet::TAILNET_PROBE_TIMEOUT;
use storyhook_test_support::{
    ChildGuard, TestEnv, http_status_line, port_of, reserve_port, scratch_dir,
};

/// A directory holding a `tailscale` that never answers — `sleep 120` wedges
/// every probe attempt for the life of the test, the same `tailscaled`
/// pathology [`TAILNET_PROBE_TIMEOUT`]'s doc comment describes.
fn wedged_tailscale_shim() -> tempfile::TempDir {
    let dir = scratch_dir();
    let path = dir.path().join("tailscale");
    std::fs::write(&path, "#!/bin/sh\nsleep 120\n").expect("writing the shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("making the shim executable");
    }
    dir
}

/// `PATH` with `shim` ahead of everything the harness already puts there —
/// the same construction `tailnet_advertise.rs`, `tailnet_rebind.rs` and
/// `tailnet_probe_budget.rs` use.
fn path_with_shim(env: &TestEnv, shim: &Path) -> OsString {
    let mut entries: Vec<PathBuf> = vec![shim.to_path_buf()];
    entries.extend(std::env::split_paths(&env.path_with_binary()));
    std::env::join_paths(entries).expect("joining PATH")
}

/// How long the loopback socket may accept a connection and answer nothing
/// before that is a bug rather than scheduling noise.
///
/// Deliberately far under [`TAILNET_PROBE_TIMEOUT`] rather than close to it:
/// the whole point of moving the probe off this path is that nothing on it
/// waits on the probe's budget at all. A regression that reintroduced a
/// synchronous probe here would still pass a bound merely *less than*
/// `TAILNET_PROBE_TIMEOUT` most of the time (the probe can fail fast on a
/// missing binary); this bound is small enough that only "no probe on this
/// path" can satisfy it.
const ANSWER_AFTER_ACCEPT: Duration = Duration::from_secs(1);

/// A daemon that binds loopback and then blocks for the whole tailnet probe
/// before it can answer anything is exactly the defect this story fixes —
/// `ANSWER_AFTER_ACCEPT` must be small enough that only "no probe on the
/// pre-serve path at all" can satisfy it.
#[test]
fn answer_after_accept_is_well_under_the_probe_budget() {
    assert!(
        ANSWER_AFTER_ACCEPT < TAILNET_PROBE_TIMEOUT,
        "a bound that isn't strictly tighter than the probe's own budget \
         cannot tell \"no probe on this path\" apart from \"a probe that \
         happened to fail fast\""
    );
}

/// The regression test for SH-186 itself: a wedged `tailscale` must not
/// leave the loopback socket accepting connections and answering nothing.
///
/// Unlike the story's original regression test, this measures the silent
/// window directly — from the instant a TCP connect succeeds to the instant
/// a response arrives — rather than tolerating up to 20s for *some* 200 to
/// eventually show up. Red before this story's fix (measured 2.96s of dead
/// air); green after, because nothing on the path from bind to first-request
/// touches `tailscale` at all.
#[test]
fn a_wedged_tailscale_cli_leaves_no_accepting_but_silent_window() {
    let env = TestEnv::isolated();
    let shim = wedged_tailscale_shim();
    let path = path_with_shim(&env, shim.path());
    let port = reserve_port();

    let mut command = env.raw_story(std::env::temp_dir());
    command
        .args(["web", "--serve", "--port", &port.to_string()])
        .env("PATH", &path);
    let child = command.spawn().expect("spawning the dashboard");
    let guard = ChildGuard::new(child);

    let connect_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        assert!(
            Instant::now() < connect_deadline,
            "the dashboard never bound {port} at all, wedged tailscale or not"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    // The socket accepts; from this instant, an answer must arrive well
    // inside the probe's own budget, not merely inside a wall clock generous
    // enough to hide a 3s stall behind it.
    let status = http_status_line(port, ANSWER_AFTER_ACCEPT);
    assert!(
        status.as_deref().is_some_and(|line| line.contains("200")),
        "the dashboard accepted a connection but did not answer within \
         {ANSWER_AFTER_ACCEPT:?} of a wedged `tailscale`; got: {status:?}"
    );

    // The port belongs to the daemon this test armed, not a stranger that
    // happened to win a stale port — the same identity check `port_of`
    // performs for the client-facing tests below.
    let info = env
        .daemon()
        .expect("the dashboard must have published a portfile by now");
    assert_eq!(
        info.pid,
        guard.pid(),
        "the daemon answering on {port} is not the one this test spawned"
    );
}

/// Publication — the portfile every other client discovers this daemon
/// through — must land well inside the probe's own budget too, not just
/// "eventually." Red before this story's fix: `crates/storyhook-test-
/// support::server::PORTFILE_DEADLINE`'s own doc measured this at 3.36s
/// against an identical wedge.
#[test]
fn publication_does_not_wait_on_the_tailnet_probe() {
    let env = TestEnv::isolated();
    let shim = wedged_tailscale_shim();
    let path = path_with_shim(&env, shim.path());

    let started = Instant::now();
    let mut command = env.raw_story(std::env::temp_dir());
    command.args(["web", "--serve"]).env("PATH", &path);
    let child = command.spawn().expect("spawning the dashboard");
    let guard = ChildGuard::new(child);

    let _port = port_of(&env, guard.pid());
    assert!(
        started.elapsed() < TAILNET_PROBE_TIMEOUT,
        "the portfile took {:?} to publish under a wedged tailscale — at or \
         beyond the probe's own {TAILNET_PROBE_TIMEOUT:?} budget means \
         publication is still waiting on the probe",
        started.elapsed()
    );
}

/// Not just the dashboard: *every* command that autostarts a daemon
/// (`lifecycle::ensure`, on every command's path since SH-114) must be
/// unaffected by a wedged `tailscale`. Red before this story's fix — the
/// client-side `await_healthy` cannot return until a portfile lands, which
/// sat behind the same probe.
#[test]
fn an_ordinary_command_autostarts_fast_under_a_wedged_tailscale() {
    let env = TestEnv::isolated();
    let shim = wedged_tailscale_shim();
    let path = path_with_shim(&env, shim.path());
    let dir = scratch_dir();

    env.stop_daemon();

    let started = Instant::now();
    env.story(dir.path())
        .env("PATH", &path)
        .args(["project", "list"])
        .assert()
        .success();
    assert!(
        started.elapsed() < TAILNET_PROBE_TIMEOUT,
        "an ordinary command that had to autostart its daemon took {:?} under \
         a wedged tailscale — every client pays the probe's cost until \
         publication happens off that path",
        started.elapsed()
    );

    env.story(dir.path())
        .args(["web", "stop"])
        .assert()
        .success();
}

/// `story web start` itself, client-facing: it must return fast under a
/// wedged `tailscale`, and it must never print a tailnet URL it has not
/// actually confirmed — a loopback URL plus a note that the tailnet is
/// still resolving is the honest answer at this instant, per SH-186's
/// council decision, which that story carries as a comment.
#[test]
fn web_start_returns_fast_under_a_wedged_tailscale_and_never_overclaims() {
    let env = TestEnv::isolated();
    let shim = wedged_tailscale_shim();
    let path = path_with_shim(&env, shim.path());
    let dir = scratch_dir();

    env.stop_daemon();

    let started = Instant::now();
    let output = env
        .story(dir.path())
        .env("PATH", &path)
        .args(["web", "start"])
        .output()
        .expect("running `story web start`");
    assert!(
        started.elapsed() < TAILNET_PROBE_TIMEOUT,
        "`story web start` took {:?} under a wedged tailscale",
        started.elapsed()
    );
    assert!(output.status.success(), "{output:?}");

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        stdout.contains("http://127.0.0.1:"),
        "must advertise loopback, the only address certain to be answering \
         at this instant; got: {stdout}"
    );
    assert!(
        !stdout.contains(".ts.net"),
        "must never print a tailnet URL it has not confirmed — the daemon \
         cannot have bound one yet under a probe that is still wedged; got: \
         {stdout}"
    );

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        stderr.contains("resolving") && stderr.to_lowercase().contains("tailnet"),
        "the printed loopback URL can lag reality once the probe is off the \
         critical path, so the CLI must say so rather than let a stale URL \
         read as a confirmed answer; got stderr: {stderr}"
    );

    env.story(dir.path())
        .args(["web", "stop"])
        .assert()
        .success();
}
