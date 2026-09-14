//! An attempt's irreversible cancellation signal; only its owner reaps children.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// A read-only cancellation handle; only the owning worker can signal it.
#[derive(Clone, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether the owner has irreversibly requested cancellation.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{
        CaptureError, TerminationPolicy, TimeoutTermination, run_captured_cancellable,
    };
    use std::{
        process::Command,
        time::{Duration, Instant},
    };

    #[test]
    fn cancellation_before_spawn_never_starts_the_command() {
        let root = storyhook_test_support::scratch_dir();
        let marker = root.path().join("started");
        let token = Cancellation::default();
        token.cancel();
        let mut command = Command::new("touch");
        command.arg(&marker);
        let result = run_captured_cancellable(
            command,
            Duration::from_secs(5),
            TerminationPolicy::Kill,
            &token,
            |_| -> Result<(), String> { panic!("a cancelled command cannot register") },
        );
        assert!(matches!(result, Err(CaptureError::Cancelled)));
        assert!(!marker.exists());
    }

    #[test]
    fn cancellation_during_registration_terminates_and_reaps_the_owned_group() {
        for ignore_term in [false, true] {
            let root = storyhook_test_support::scratch_dir();
            let ready = root.path().join("ready");
            let terminated = root.path().join("terminated");
            let token = Cancellation::default();
            let mut command = Command::new("sh");
            let handler = if ignore_term {
                "trap '' TERM"
            } else {
                "trap 'printf terminated > \"$2\"; exit 0' TERM"
            };
            // The worker publishes only after ignoring TERM. A foreground
            // wait would defer the parent's trap until the worker is killed.
            let script = format!(
                "{handler}; sh -c 'trap \"\" TERM; printf ready > \"$1\"; exec sleep 30' worker \"$1\" & wait"
            );
            command
                .args(["-c", &script, "cancel-probe"])
                .arg(&ready)
                .arg(&terminated);
            let mut leader = 0;
            let result = run_captured_cancellable(
                command,
                Duration::from_secs(30),
                TerminationPolicy::TerminateThenKill {
                    grace: Duration::from_millis(100),
                },
                &token,
                |pid| {
                    leader = pid;
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while !ready.exists() {
                        assert!(
                            Instant::now() < deadline,
                            "worker never installed its signal handler"
                        );
                        std::thread::yield_now();
                    }
                    token.cancel();
                    Ok(())
                },
            );
            assert!(matches!(result, Err(CaptureError::Cancelled)));
            assert_eq!(terminated.exists(), !ignore_term);
            // A second wait has no child left: the capture owner reaped it.
            let mut status = 0;
            assert_eq!(
                unsafe { libc::waitpid(leader as i32, &mut status, libc::WNOHANG) },
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
        }
    }

    #[test]
    fn cancellation_is_observed_after_registration_while_waiting_for_output() {
        let token = Cancellation::default();
        let (registered, observed) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                let mut command = Command::new("sh");
                command.args(["-c", "sleep 30"]);
                run_captured_cancellable(
                    command,
                    Duration::from_secs(30),
                    TerminationPolicy::Kill,
                    &token,
                    |_| {
                        registered.send(()).unwrap();
                        Ok(())
                    },
                )
            });
            observed.recv_timeout(Duration::from_secs(5)).unwrap();
            token.cancel();
            assert!(matches!(
                worker.join().unwrap(),
                Err(CaptureError::Cancelled)
            ));
        });
    }

    #[test]
    fn a_real_timeout_remains_a_timeout_without_operator_cancellation() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        assert!(matches!(
            run_captured_cancellable(
                command,
                Duration::ZERO,
                TerminationPolicy::Kill,
                &Cancellation::default(),
                |_| Ok(())
            ),
            Err(CaptureError::Timeout(TimeoutTermination::Killed))
        ));
    }
}
