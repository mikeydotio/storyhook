//! A daemon's parent is a process identity, not only a process id.
//!
//! Process ids are reused. If the parent named when a test daemon started has
//! gone but an unrelated process now owns the same pid, `kill(pid, 0)` alone
//! keeps the daemon alive past the test environment that owns its store. This
//! fixture constructs that identity mismatch directly instead of waiting for
//! the kernel to reuse a pid.

use std::time::{Duration, Instant};

use storyhook::daemon::lifecycle::{self, StopMode};
use storyhook_test_support::{ChildGuard, TestEnv, scratch_dir};

/// Stops the daemon even when the assertion below proves the defect.
struct DaemonGuard<'a>(&'a TestEnv);

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        let _ = lifecycle::stop(&self.0.environment(), StopMode::Force);
    }
}

#[test]
fn a_reused_parent_pid_does_not_keep_a_test_daemon_alive() {
    let env = TestEnv::isolated();
    let _daemon_guard = DaemonGuard(&env);
    let cwd = scratch_dir();

    // This live process stands in for an unrelated process that inherited the
    // original parent's recycled pid. The deliberately mismatched start token
    // proves it is not the parent identity the daemon was given.
    let mut unrelated = ChildGuard::spawn(std::process::Command::new("sleep").arg("30"))
        .expect("spawning the live process that holds a reused pid");
    env.story(cwd.path())
        .env("STORYHOOK_PARENT_PID", unrelated.pid().to_string())
        .env("STORYHOOK_PARENT_START_TIME", "Thu Jan 1 00:00:00 1970")
        .args(["daemon", "start"])
        .assert()
        .success();

    let daemon = env
        .daemon()
        .expect("the daemon must publish its identity before parent monitoring starts");
    let deadline = Instant::now() + Duration::from_secs(2);
    while lifecycle::is_live(&env.environment()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }

    assert!(
        env.store_path().is_file(),
        "the daemon start must have opened its fixture store"
    );
    std::fs::remove_file(env.store_path())
        .expect("removing the temporary store after its recorded parent is gone");
    assert!(
        !lifecycle::is_live(&env.environment()),
        "daemon pid {} is still serving the vanished store {} because pid {} exists, even though its recorded parent-start token identifies a different process",
        daemon.pid,
        env.store_path().display(),
        unrelated.pid()
    );

    unrelated.kill_and_reap();
}
