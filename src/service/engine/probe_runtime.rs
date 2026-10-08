//! External boundaries for a liveness observation. Production uses the real
//! monotonic clock, ownership resolver and bounded process capture. Tests can
//! advance one logical clock across these same call sites without racing exec.
use crate::env::Environment;
use crate::error::AppError;
use crate::process::{CaptureError, Captured, run_captured};
use crate::service::tmux_target::Target;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

pub(super) trait ProbeRuntime {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn inspect(
        &self,
        env: &Environment,
        socket: Option<&Path>,
        deadline: Instant,
    ) -> Result<Target, AppError> {
        super::super::tmux_target::inspect(env, socket, deadline, &Default::default())
    }

    fn capture(&self, command: Command, timeout: Duration) -> Result<Captured, CaptureError> {
        run_captured(command, timeout)
    }

    fn remaining(&self, deadline: Instant) -> Result<Duration, AppError> {
        let timeout = deadline.saturating_duration_since(self.now());
        if timeout.is_zero() {
            Err(AppError::Validation(
                "tmux ownership operation budget exhausted".into(),
            ))
        } else {
            Ok(timeout)
        }
    }
}

pub(super) struct LiveProbeRuntime;
impl ProbeRuntime for LiveProbeRuntime {}
