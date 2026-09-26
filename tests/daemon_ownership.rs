//! Who started the daemon, and how it reports that about itself (SH-784).
//!
//! Every real launchd path is out of reach here — like every other test in
//! this codebase, running real `launchctl` would register an agent into the
//! developer's own login session. What *is* reachable, and what these tests
//! cover: the fork path a test binary always takes (`choose_launcher`'s
//! `is_test_build()` branch fires before anything else), and a bare manual
//! invocation with no `--owner` flag at all. `src/daemon/lifecycle.rs`'s own
//! unit tests cover `choose_launcher`'s decision table and the port-hint
//! mechanism directly; `src/daemon/launchd.rs`'s cover the kickstart/bootstrap
//! retry logic against an injected fake `launchctl`.

use std::process::Stdio;
use std::time::{Duration, Instant};

use storyhook::daemon::lifecycle::{self, DaemonInfo, DaemonOwner, ForkReason};
use storyhook_test_support::{ChildGuard, TestEnv, scratch_dir};

const STARTUP: Duration = Duration::from_secs(10);

/// Stops whatever daemon `env` is running, even if the test panics first —
/// the same guard shape `tests/daemon_lifecycle.rs` uses, local to this file
/// for the same reason that file's own is local rather than shared.
struct DaemonGuard<'a>(&'a TestEnv);

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        let _ = lifecycle::stop(&self.0.environment(), lifecycle::StopMode::Force);
    }
}

/// Blocks until `env`'s portfile is readable, or fails the test — the same
/// reasoning `tests/daemon_git_env.rs::await_daemon` gives: the portfile,
/// never `daemon_is_live()`, because liveness is the pidfile lock, taken
/// before the bind and the publish.
fn await_daemon(env: &TestEnv) -> DaemonInfo {
    let portfile = env.environment().daemon_file();
    let deadline = Instant::now() + STARTUP;
    while Instant::now() < deadline {
        if let Some(info) = lifecycle::read_info_at(&portfile) {
            return info;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!(
        "the daemon never published {} within {STARTUP:?}",
        portfile.display()
    );
}

/// A `story daemon start` in the test harness always takes the fork path
/// (`is_test_build()` is checked before OS or login-agent health), so this
/// is the one `DaemonOwner` a test binary can ever observe end to end without
/// touching real `launchctl`.
#[test]
fn a_test_daemon_reports_itself_as_forked_for_the_test_build() {
    let env = TestEnv::isolated();
    let _guard = DaemonGuard(&env);
    let dir = scratch_dir();

    env.story(dir.path())
        .args(["daemon", "start"])
        .assert()
        .success();
    let info = env
        .daemon()
        .expect("a started test daemon must publish a portfile");

    assert!(
        matches!(
            info.owner,
            Some(DaemonOwner::Forked {
                reason: ForkReason::TestBuild,
                ..
            })
        ),
        "a test binary must never depend on this machine's own launchd state: {:?}",
        info.owner
    );

    let reported = env
        .story(dir.path())
        .args(["daemon", "status"])
        .output()
        .expect("running daemon status");
    let said = String::from_utf8_lossy(&reported.stdout);
    assert!(said.contains("test build"), "{said}");
}

/// `daemon --serve` with no `--owner` flag at all — a human running the
/// command by hand, never a code path internal to `ensure`/`start`/`restart`
/// — must self-report `Manual`, and must do so from a direct spawn (the way
/// launchd itself execs the daemon), never through `spawn_child`.
#[test]
fn a_bare_serve_invocation_with_no_owner_flag_reports_manual() {
    let env = TestEnv::isolated();
    env.stop_daemon();
    let dir = scratch_dir();

    let mut serve = env.raw_story(dir.path());
    serve
        .args(["daemon", "--serve", "--port", "0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _daemon = ChildGuard::spawn(&mut serve).expect("spawning a daemon directly");
    let info = await_daemon(&env);

    assert!(
        matches!(
            info.owner,
            Some(DaemonOwner::Forked {
                reason: ForkReason::Manual,
                ..
            })
        ),
        "a bare invocation with no --owner flag must self-report as manual: {:?}",
        info.owner
    );
}
