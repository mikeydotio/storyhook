//! Every run of a provider CLI — `claude` or `codex` — with a deadline
//! (SH-815).
//!
//! On 2026-09-26 a `codex plugin list --json` that never got past its own
//! launch held the central verifier for as long as the daemon lived. It ran
//! through a bare `Command::output()`, so nothing ended the wait. A provider
//! CLI is a third-party binary that can wait on a code-signing check, a login
//! prompt or the network, and every caller in this module's parent can run on
//! a daemon thread: helper resolution for the verifier, block delivery,
//! dispatch and the engine, and the `story plugin` verbs the daemon serves.
//!
//! So every run goes through [`crate::process`]: files rather than pipes, its
//! own process group, a deadline that stops the whole group, and a journal
//! entry for its start, its timeout and its finish. That entry is how a stuck
//! provider shows in `story daemon logs`. The journal also names its own
//! destination to the provider, as it does to every child it runs.

use std::ffi::OsStr;
use std::io::ErrorKind;
use std::process::Command;
use std::time::{Duration, Instant};

use super::{PluginTarget, missing_message};
use crate::env::spawn_env::apply_plugin_cli_allowlist;
use crate::error::AppError;
use crate::process::{
    CaptureError, Captured, TerminationPolicy, TimeoutTermination, run_captured_answer,
};

/// Codex's own timeout for the remote requests `codex plugin list` makes
/// (openai/codex#47286), in seconds.
const CODEX_REMOTE_TIMEOUT_SECS: u64 = 30;

/// How long one provider CLI run may take before its process group is
/// stopped: twice Codex's own remote timeout, so a provider that answers
/// inside its own timeout is never cut short, and one that never answers
/// costs this rather than the daemon's life.
pub(crate) const PROVIDER_CLI_TIMEOUT: Duration =
    Duration::from_secs(2 * CODEX_REMOTE_TIMEOUT_SECS);

/// How long a provider past its deadline has, after SIGTERM to its group,
/// before SIGKILL: time for a verb that changes provider state to release
/// what it holds.
pub(crate) const PROVIDER_TERM_GRACE: Duration = Duration::from_secs(5);

/// The longest provider answer read. `codex plugin list --json` lists every
/// plugin of every configured marketplace, which can pass the 64 KiB
/// diagnostic bound; a longer answer is refused, never parsed as a prefix.
const ANSWER_LIMIT: u64 = 8 * 1024 * 1024;

/// Why a provider CLI run gave no answer.
#[derive(Debug)]
pub(crate) enum ProviderError {
    /// The provider's executable is not on `PATH`.
    Missing(PluginTarget),
    /// The provider started but gave no usable answer: it did not finish in
    /// time, its answer was too long, or it could not be started or waited for.
    Failed(AppError),
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing(target) => f.write_str(&missing_message(*target)),
            Self::Failed(error) => write!(f, "{error}"),
        }
    }
}

impl From<ProviderError> for AppError {
    fn from(error: ProviderError) -> Self {
        match error {
            ProviderError::Missing(target) => AppError::Storage(missing_message(target)),
            ProviderError::Failed(error) => error,
        }
    }
}

/// Runs `<provider> <args>` until it exits or [`PROVIDER_CLI_TIMEOUT`] ends it.
pub(super) fn run(target: PluginTarget, args: &[&str]) -> Result<Captured, ProviderError> {
    super::operation::provider(target, args, || {
        run_at(
            OsStr::new(target.executable()),
            target,
            args,
            PROVIDER_CLI_TIMEOUT,
        )
    })
}

/// Whether the provider CLI is there to use. Claude keeps its historical
/// rule that a `claude` which starts is enough; Codex must also answer
/// `--version` with success. A provider that does not answer in time is an
/// error, never "available": the install that follows would only wait again.
pub(super) fn available(target: PluginTarget) -> Result<bool, ProviderError> {
    available_at(
        OsStr::new(target.executable()),
        target,
        PROVIDER_CLI_TIMEOUT,
    )
}

fn available_at(
    program: &OsStr,
    target: PluginTarget,
    deadline: Duration,
) -> Result<bool, ProviderError> {
    match super::operation::provider(target, &["--version"], || {
        run_at(program, target, &["--version"], deadline)
    }) {
        Ok(out) => Ok(target == PluginTarget::ClaudeCode || out.status.success()),
        Err(ProviderError::Missing(_)) => Ok(false),
        Err(error) => Err(error),
    }
}

/// [`run`] with the executable and the deadline given, so a test can run a
/// provider that never answers without waiting a production minute.
fn run_at(
    program: &OsStr,
    target: PluginTarget,
    args: &[&str],
    deadline: Duration,
) -> Result<Captured, ProviderError> {
    let shown = format!("{} {}", target.executable(), args.join(" "));
    let mut command = Command::new(program);
    apply_plugin_cli_allowlist(&mut command);
    super::operation::inherit_lock(&mut command);
    command.args(args);
    let started = Instant::now();
    let captured = run_captured_answer(
        command,
        deadline,
        TerminationPolicy::TerminateThenKill {
            grace: PROVIDER_TERM_GRACE,
        },
        ANSWER_LIMIT,
    )
    .map_err(|error| match error {
        CaptureError::Spawn(error) if error.kind() == ErrorKind::NotFound => {
            ProviderError::Missing(target)
        }
        CaptureError::Timeout(ended) => ProviderError::Failed(AppError::Storage(format!(
            "`{shown}` did not answer within {deadline:?}; its process group was stopped \
             after {:.1}s ({})",
            started.elapsed().as_secs_f64(),
            ending(ended)
        ))),
        other => ProviderError::Failed(AppError::Storage(format!(
            "failed to run `{shown}`: {}",
            other.detail()
        ))),
    })?;
    if captured.stdout_truncated {
        return Err(ProviderError::Failed(AppError::Storage(format!(
            "`{shown}` answered more than {} MiB; a cut answer is refused rather than read",
            ANSWER_LIMIT / (1024 * 1024)
        ))));
    }
    Ok(captured)
}

/// How a timed-out provider's group ended, as an operator reads it.
fn ending(ended: TimeoutTermination) -> String {
    match ended {
        TimeoutTermination::Killed => "killed".to_string(),
        TimeoutTermination::ExitedAfterTerminate => "it exited on SIGTERM".to_string(),
        TimeoutTermination::KilledAfterTerminate => {
            format!("SIGKILL after a {PROVIDER_TERM_GRACE:?} SIGTERM grace")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    /// Injected timeout policy; child readiness is not required within it.
    const HANG_DEADLINE: Duration = Duration::from_secs(2);

    /// What a loaded gate may add to a timed-out run, on top of its deadline
    /// and its SIGTERM grace: two process spawns and one reap, which SH-643
    /// measured at hundreds of times their idle cost. Generous on purpose: the
    /// claim under test is that the run ends, not that it ends fast.
    const LOAD_MARGIN: Duration = Duration::from_secs(10);

    /// A deadline no answering test provider reaches.
    const ANSWER_DEADLINE: Duration = Duration::from_secs(30);

    fn provider(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("provider");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write the provider");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make the provider executable");
        path
    }

    /// SH-815's defect: a provider that never answers, with a grandchild
    /// that holds its output open. The run must end at its deadline, name
    /// what it ran and for how long, and leave no process of that group.
    #[test]
    fn a_provider_that_never_answers_is_stopped_at_its_deadline() {
        assert_hanging_provider("");
    }

    #[test]
    fn a_provider_timeout_before_readiness_reaps_its_group() {
        assert_hanging_provider("sleep 300 & wait\n");
    }

    fn assert_hanging_provider(startup: &str) {
        let dir = storyhook_test_support::scratch_dir();
        let journal = dir.path().join("journal");
        let _scope = crate::daemon::activity::context::enter(Some(
            crate::daemon::activity::context::LogContext {
                directory: journal.clone(),
                label: "provider-timeout-regression".into(),
            },
        ));
        let grandchild = dir.path().join("grandchild");
        let program = provider(
            dir.path(),
            &format!(
                "{startup}sleep 300 &\nprintf %s $! > '{}'\nwait",
                grandchild.display()
            ),
        );
        let started = Instant::now();
        let result = run_at(
            program.as_os_str(),
            PluginTarget::Codex,
            &["plugin", "list", "--json"],
            HANG_DEADLINE,
        );
        let elapsed = started.elapsed();

        let message = match result {
            Err(ProviderError::Failed(error)) => error.to_string(),
            Err(ProviderError::Missing(_)) => panic!("the provider exists"),
            Ok(_) => panic!("a provider that never answers must not succeed"),
        };
        assert!(
            message.starts_with("`codex plugin list --json` did not answer within 2s"),
            "{message}"
        );
        assert!(
            message.contains("process group was stopped after"),
            "{message}"
        );
        assert!(
            elapsed < HANG_DEADLINE + PROVIDER_TERM_GRACE + LOAD_MARGIN,
            "the run took {elapsed:?}"
        );
        // The parent owns this receipt even if timeout precedes all child code.
        let mut pids = Vec::new();
        for file in crate::daemon::activity::day_files(&journal).unwrap() {
            for line in std::fs::read_to_string(file).unwrap().lines() {
                let row: serde_json::Value = serde_json::from_str(line).unwrap();
                if row["message"] == "process started" {
                    let child = row["context"]
                        .as_str()
                        .unwrap()
                        .split_whitespace()
                        .find_map(|field| field.strip_prefix("child="))
                        .expect("spawn receipt names its child");
                    pids.push(child.parse::<libc::pid_t>().unwrap());
                }
            }
        }
        assert_eq!(pids.len(), 1, "one real provider must have started");
        let pid = pids[0];
        assert!(pid > 0);
        assert!(
            crate::process::pid_disappears(pid),
            "provider {pid} outlived the deadline"
        );
        assert!(
            crate::process::pid_disappears(-pid),
            "provider process group {pid} outlived the deadline"
        );
        if !startup.is_empty() {
            assert!(
                !grandchild.exists(),
                "the stimulus must stall before readiness"
            );
        }
    }

    #[test]
    fn a_provider_that_answers_is_passed_through() {
        let dir = storyhook_test_support::scratch_dir();
        let program = provider(dir.path(), "printf '{\"installed\":[]}'; echo note >&2");
        let out = run_at(
            program.as_os_str(),
            PluginTarget::Codex,
            &["plugin", "list", "--json"],
            ANSWER_DEADLINE,
        )
        .expect("an answering provider");
        assert!(out.status.success());
        assert_eq!(out.stdout, b"{\"installed\":[]}");
        assert_eq!(out.stderr, b"note\n");
    }

    /// An answer past the limit is refused: parsing its prefix would read a
    /// long plugin list as invalid JSON, which looks like "no plugin".
    #[test]
    fn an_answer_longer_than_the_limit_is_refused() {
        let dir = storyhook_test_support::scratch_dir();
        let program = provider(
            dir.path(),
            &format!("head -c {} /dev/zero", ANSWER_LIMIT + 1),
        );
        let message = match run_at(
            program.as_os_str(),
            PluginTarget::Codex,
            &["plugin", "list", "--json"],
            ANSWER_DEADLINE,
        ) {
            Err(ProviderError::Failed(error)) => error.to_string(),
            Err(ProviderError::Missing(_)) => panic!("the provider exists"),
            Ok(_) => panic!("a cut answer must be refused"),
        };
        assert!(message.contains("answered more than 8 MiB"), "{message}");
    }

    #[test]
    fn a_provider_that_is_not_installed_is_missing() {
        let dir = storyhook_test_support::scratch_dir();
        let program = dir.path().join("no-such-provider");
        let error = run_at(
            program.as_os_str(),
            PluginTarget::Codex,
            &["plugin", "list", "--json"],
            ANSWER_DEADLINE,
        )
        .err()
        .expect("a missing provider cannot answer");
        assert!(matches!(error, ProviderError::Missing(PluginTarget::Codex)));
        assert_eq!(
            AppError::from(error).to_string(),
            missing_message(PluginTarget::Codex)
        );
        assert!(!available_at(program.as_os_str(), PluginTarget::Codex, ANSWER_DEADLINE).unwrap());
    }

    /// A `--version` that never answers is an error for both providers,
    /// including Claude, whose rule is otherwise that starting is enough.
    #[test]
    fn a_silent_version_is_an_error_and_never_available() {
        let dir = storyhook_test_support::scratch_dir();
        let program = provider(dir.path(), "exec sleep 300");
        for target in [PluginTarget::ClaudeCode, PluginTarget::Codex] {
            let result = available_at(program.as_os_str(), target, HANG_DEADLINE);
            assert!(
                matches!(result, Err(ProviderError::Failed(_))),
                "{target:?}: {result:?}"
            );
        }
    }

    #[test]
    fn a_failing_version_is_available_for_claude_only() {
        let dir = storyhook_test_support::scratch_dir();
        let program = provider(dir.path(), "exit 3");
        assert!(
            available_at(
                program.as_os_str(),
                PluginTarget::ClaudeCode,
                ANSWER_DEADLINE
            )
            .unwrap()
        );
        assert!(!available_at(program.as_os_str(), PluginTarget::Codex, ANSWER_DEADLINE).unwrap());
    }

    /// `story plugin install` runs in the daemon, and its client waits
    /// `SERVED_DEADLINE` for the answer. One provider call that stalls must
    /// end early enough for that answer to say so.
    #[test]
    fn one_stalled_call_leaves_the_served_exchange_time_to_report_it() {
        assert!(
            PROVIDER_CLI_TIMEOUT + PROVIDER_TERM_GRACE < crate::daemon::lifecycle::SERVED_DEADLINE
        );
    }
}
