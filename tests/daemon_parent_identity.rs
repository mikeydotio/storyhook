//! A daemon's parent is a process identity, not only a process id.
//!
//! Process ids are reused. If the parent named when a test daemon started has
//! gone but an unrelated process now owns the same pid, `kill(pid, 0)` alone
//! keeps the daemon alive past the test environment that owns its store. This
//! fixture constructs that identity mismatch directly instead of waiting for
//! the kernel to reuse a pid.

use storyhook::daemon::lifecycle::{self, StopMode};
use storyhook_test_support::{ChildGuard, TestEnv, scratch_dir};

/// Stops the daemon even when the assertion below proves the defect.
struct DaemonGuard<'a>(&'a TestEnv);

impl Drop for DaemonGuard<'_> {
    fn drop(&mut self) {
        let _ = lifecycle::stop(&self.0.environment(), StopMode::Force);
    }
}

/// The owner that was named is gone, and the live process holding its pid is a
/// different incarnation. No daemon is started for it at all, and nothing is
/// written: a daemon started here would have served the fixture past its run,
/// first until its parent watch noticed and, with a pid-only contract, for as
/// long as the unrelated process lived.
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
    let refused = env
        .story(cwd.path())
        .env("STORYHOOK_PARENT_PID", unrelated.pid().to_string())
        .env("STORYHOOK_PARENT_START_TIME", "Thu Jan 1 00:00:00 1970")
        .args(["daemon", "start"])
        .output()
        .expect("running `story daemon start`");

    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success(),
        "a daemon was started for parent pid {} although its recorded start token \
         identifies a different process",
        unrelated.pid()
    );
    assert!(
        stderr.contains("STORYHOOK_PARENT_PID"),
        "the refusal must name the owner contract; it said: {stderr}"
    );
    assert!(
        !env.store_path().exists(),
        "the refused start still created the store {}",
        env.store_path().display()
    );
    assert!(
        !env.environment().daemon_state_dir().exists(),
        "the refused start still created daemon state at {}",
        env.environment().daemon_state_dir().display()
    );

    unrelated.kill_and_reap();
}
