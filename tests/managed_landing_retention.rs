//! SH-871: private managed landing retains its branch after normal certification.
//! This local transport fixture does not certify GitHub's service behavior.

use std::process::{ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use storyhook_test_support::{ChildGuard, load_grace};

/// EOF requests Python's exact-session cleanup before direct-child fallback.
/// ChildGuard alone does not own descendants. Keep the pipe outside it so
/// waiting for output does not close the fixture's lifetime lease too early.
struct FixtureOwner {
    child: ChildGuard,
    lifetime: Option<ChildStdin>,
}

impl FixtureOwner {
    fn cancel(&mut self) {
        drop(self.lifetime.take());
    }
}

impl Drop for FixtureOwner {
    fn drop(&mut self) {
        self.cancel();
        let deadline = Instant::now() + load_grace::graced_now(Duration::from_secs(15));
        while self.child.try_wait().is_none() && Instant::now() < deadline {
            // Cancellation observation cadence, not a workload deadline.
            std::thread::sleep(Duration::from_millis(10));
        }
        // ChildGuard remains a direct-child fallback only. Python preserves
        // uncertain scratch roots and refuses further work if its native
        // session census cannot prove settlement; no broad signal is issued.
    }
}

#[test]
fn managed_landing_retains_owned_branch_and_preserves_ordinary_protected_flow() {
    // The resolver execs the pinned supported interpreter, preserving this
    // child's owner pipe. PATH may otherwise select Apple's older Python.
    let mut child = ChildGuard::spawn_with_output(
        Command::new("/bin/bash")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/scripts/python-runtime.sh"
            ))
            .args(["--", "python3", "-B"])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/scripts/tests/test_managed_landing_retention.py"
            ))
            .arg("--watch-owner")
            .stdin(Stdio::piped()),
    )
    .expect("run managed landing retention regressions");
    let lifetime = Some(child.take_stdin().expect("fixture owner pipe"));
    let mut owner = FixtureOwner { child, lifetime };
    let deadline = Instant::now() + load_grace::graced_now(Duration::from_secs(180));
    while owner.child.try_wait().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let expired = owner.child.try_wait().is_none();
    owner.cancel();
    let output = owner
        .child
        .wait_with_output_within(load_grace::graced_now(Duration::from_secs(15)), || {
            "managed landing retention regressions did not finish".into()
        });
    assert!(
        !expired && output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
