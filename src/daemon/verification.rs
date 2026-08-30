//! The daemon-owned centralized verification worker (SH-521).

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::bus::{Change, ChangeBus};
use crate::api::dispatch::{DispatchAgent, resolve_dispatch_script};
use crate::domain::github_remote::parse_github_url;
use crate::env::Environment;
use crate::env::spawn_env::{apply_dispatch_allowlist, apply_verification_allowlist};
use crate::error::AppError;
use crate::service::{
    Ctx, StoryService, VERIFICATION_CLEANUP_COMPLETE_PREFIX, VERIFICATION_GREEN_PREFIX,
    VerificationCandidate, VerificationProblem, VerificationQueue,
};
use crate::store::{PrLink, ReadOps, Store};

/// Infrastructure recovery cadence when no store event arrives.
const RECOVERY_WAKE: Duration = Duration::from_secs(30);

/// One repository-side verification result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerificationOutcome {
    /// The exact merge tree passed and the guarded merge landed.
    Merged { tree: String, detail: String },
    /// The PR does not merge into current main.
    Conflict { detail: String },
    /// The submission cannot safely be acted on from its registered checkout.
    InvalidSubmission { detail: String },
    /// The exact merge tree failed the repository's release gate.
    TestsFailed {
        tree: String,
        log: String,
        detail: String,
    },
    /// GitHub, git, credentials, or the verifier process failed independently
    /// of the submitted code.
    InfrastructureFailure { detail: String },
}

/// Process boundary for repository verification and agent-session control.
pub trait VerificationActuator: Send + Sync {
    /// Verifies and, on green, lands one submitted PR.
    fn verify(
        &self,
        candidate: &VerificationCandidate,
        pull_request: &PrLink,
    ) -> VerificationOutcome;
    /// Delivers remediation to the exact dispatched agent pane.
    fn notify(&self, candidate: &VerificationCandidate, message: &str) -> Result<(), AppError>;
    /// Reclaims a merged story's worktree, branch, and tmux window.
    fn reap(&self, candidate: &VerificationCandidate) -> Result<(), AppError>;
}

/// Production actuator backed by repository and plugin scripts.
pub struct ShellVerificationActuator {
    env: Environment,
}

impl ShellVerificationActuator {
    /// Creates a shell actuator for one daemon environment.
    #[must_use]
    pub fn new(env: Environment) -> Self {
        Self { env }
    }

    fn helper(
        &self,
        candidate: &VerificationCandidate,
        verb: &str,
        extra: Option<&str>,
    ) -> Result<(), AppError> {
        let script = resolve_dispatch_script(DispatchAgent::Codex)
            .or_else(|_| resolve_dispatch_script(DispatchAgent::Claude))
            .map_err(AppError::Storage)?;
        let mut command = Command::new("bash");
        apply_dispatch_allowlist(&mut command);
        command
            .arg(script)
            .arg("--project")
            .arg(&candidate.project_slug)
            .arg(verb)
            .arg(&candidate.story_id)
            .current_dir(&candidate.checkout)
            .env(
                "STORY_BIN",
                std::env::current_exe().unwrap_or_else(|_| "story".into()),
            )
            .env("STORYHOOK_STORE_PATH", self.env.store_path())
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null());
        if let Some(extra) = extra {
            command.arg(extra);
        }
        let output = command.output().map_err(|error| {
            AppError::Storage(format!("could not run story helper `{verb}`: {error}"))
        })?;
        let payload: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|_| {
            AppError::Storage(format!(
                "story helper `{verb}` returned invalid JSON: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        })?;
        if payload.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
            return Ok(());
        }
        Err(AppError::Storage(
            payload
                .get("display")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("story helper refused without diagnostics")
                .to_string(),
        ))
    }
}

impl VerificationActuator for ShellVerificationActuator {
    fn verify(
        &self,
        candidate: &VerificationCandidate,
        pull_request: &PrLink,
    ) -> VerificationOutcome {
        if let Some(detail) = checkout_repository_problem(&candidate.checkout, pull_request) {
            return VerificationOutcome::InvalidSubmission { detail };
        }
        let mut command = Command::new("bash");
        apply_verification_allowlist(&mut command);
        let output = command
            .arg("scripts/verify-pr.sh")
            .arg(&pull_request.url)
            .current_dir(&candidate.checkout)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GH_PROMPT_DISABLED", "1")
            .stdin(Stdio::null())
            .output();
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                return VerificationOutcome::InfrastructureFailure {
                    detail: format!("could not start scripts/verify-pr.sh: {error}"),
                };
            }
        };
        let parsed: WireOutcome = match serde_json::from_slice(&output.stdout) {
            Ok(parsed) => parsed,
            Err(_) => {
                return VerificationOutcome::InfrastructureFailure {
                    detail: format!(
                        "scripts/verify-pr.sh returned invalid JSON: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    ),
                };
            }
        };
        parsed.into()
    }

    fn notify(&self, candidate: &VerificationCandidate, message: &str) -> Result<(), AppError> {
        self.helper(candidate, "notify", Some(message))
    }

    fn reap(&self, candidate: &VerificationCandidate) -> Result<(), AppError> {
        self.helper(candidate, "reap", None)
    }
}

fn checkout_repository_problem(
    checkout: &std::path::Path,
    pull_request: &PrLink,
) -> Option<String> {
    let origin = crate::service::project::origin_of(checkout);
    let checkout_repo = origin
        .as_ref()
        .and_then(|origin| parse_github_url(origin.raw()));
    match checkout_repo {
        Some(repo)
            if repo.owner.eq_ignore_ascii_case(&pull_request.owner)
                && repo.repo.eq_ignore_ascii_case(&pull_request.repo) =>
        {
            None
        }
        Some(repo) => Some(format!(
            "linked pull request {} belongs to {}/{}, but registered checkout `{}` has origin {}/{}; centralized landing is origin-bound",
            pull_request.url,
            pull_request.owner,
            pull_request.repo,
            checkout.display(),
            repo.owner,
            repo.repo
        )),
        None => Some(format!(
            "registered checkout `{}` has no GitHub `remote.origin.url`; centralized landing requires the linked pull request {} to belong to that origin",
            checkout.display(),
            pull_request.url
        )),
    }
}

#[derive(Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
enum WireOutcome {
    Merged {
        tree: String,
        detail: String,
    },
    Conflict {
        detail: String,
    },
    TestsFailed {
        tree: String,
        log: String,
        detail: String,
    },
    InfrastructureFailure {
        detail: String,
    },
}

impl From<WireOutcome> for VerificationOutcome {
    fn from(value: WireOutcome) -> Self {
        match value {
            WireOutcome::Merged { tree, detail } => Self::Merged { tree, detail },
            WireOutcome::Conflict { detail } => Self::Conflict { detail },
            WireOutcome::TestsFailed { tree, log, detail } => {
                Self::TestsFailed { tree, log, detail }
            }
            WireOutcome::InfrastructureFailure { detail } => Self::InfrastructureFailure { detail },
        }
    }
}

/// Whether a tick drained work or must wait for another wake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TickResult {
    /// No submitted story exists.
    Idle,
    /// One story completed; the caller may immediately ask for the next.
    Completed,
    /// One story returned to its agent; the caller may drain the next.
    Returned,
    /// The highest-priority candidate waits on external infrastructure.
    RetryLater,
}

/// Runs one verification attempt. Public for store-backed integration tests.
pub fn tick_with<S: Store, A: VerificationActuator>(
    store: &S,
    env: &Environment,
    actuator: &A,
) -> Result<TickResult, AppError> {
    let queue = VerificationQueue::new(store);
    let Some(candidate) = queue.next()? else {
        let Some(candidate) = queue.next_cleanup()? else {
            return Ok(TickResult::Idle);
        };
        let ctx = Ctx::new(
            store,
            candidate.project,
            candidate.checkout.clone(),
            env.clone(),
        )
        .no_hooks(true);
        return match actuator.reap(&candidate) {
            Ok(()) => {
                record_cleanup_complete(&ctx, &candidate)?;
                Ok(TickResult::Completed)
            }
            Err(error) => {
                record_cleanup_required(&ctx, &candidate, &error)?;
                Ok(TickResult::RetryLater)
            }
        };
    };
    let ctx = Ctx::new(
        store,
        candidate.project,
        candidate.checkout.clone(),
        env.clone(),
    )
    .no_hooks(true);
    let pull_request = match &candidate.pull_request {
        Ok(pull_request) => pull_request.clone(),
        Err(VerificationProblem::MissingCheckout) => {
            comment_once(
                &ctx,
                &candidate,
                &format!(
                    "CENTRAL VERIFICATION CONFIGURATION REQUIRED — {}",
                    VerificationProblem::MissingCheckout.message()
                ),
            )?;
            return Ok(TickResult::RetryLater);
        }
        Err(problem) => {
            return_for_repair(&ctx, actuator, &candidate, &problem.message())?;
            return Ok(TickResult::Returned);
        }
    };

    match actuator.verify(&candidate, &pull_request) {
        VerificationOutcome::Merged { tree, detail } => {
            StoryService::new(&ctx).comment(
                &candidate.story_id,
                &format!(
                    "{VERIFICATION_GREEN_PREFIX} merge tree `{tree}` passed `make test` and pull request {} landed. {detail}",
                    pull_request.url
                ),
            )?;
            queue.record_merged(&ctx, &candidate.story_id, &pull_request.url)?;
            match actuator.reap(&candidate) {
                Ok(()) => record_cleanup_complete(&ctx, &candidate)?,
                Err(error) => record_cleanup_required(&ctx, &candidate, &error)?,
            }
            Ok(TickResult::Completed)
        }
        VerificationOutcome::Conflict { detail } => {
            return_for_repair(
                &ctx,
                actuator,
                &candidate,
                &format!(
                    "CENTRAL VERIFICATION CONFLICT — the submitted PR no longer merges into current origin/main. Reconcile the existing PR without rewriting published history, run new and impacted tests, push, then move {} back to verifying.\n\n{detail}",
                    candidate.story_id
                ),
            )?;
            Ok(TickResult::Returned)
        }
        VerificationOutcome::InvalidSubmission { detail } => {
            return_for_repair(
                &ctx,
                actuator,
                &candidate,
                &format!(
                    "CENTRAL VERIFICATION INVALID SUBMISSION — {detail}. Link the PR for this checkout's origin, push it, then move {} back to verifying.",
                    candidate.story_id
                ),
            )?;
            Ok(TickResult::Returned)
        }
        VerificationOutcome::TestsFailed { tree, log, detail } => {
            return_for_repair(
                &ctx,
                actuator,
                &candidate,
                &format!(
                    "CENTRAL VERIFICATION RED — merge tree `{tree}` failed `make test`. Full log: `{log}`. Fix the existing PR, run new and impacted tests, push, then move {} back to verifying.\n\n{detail}",
                    candidate.story_id
                ),
            )?;
            Ok(TickResult::Returned)
        }
        VerificationOutcome::InfrastructureFailure { detail } => {
            comment_once(
                &ctx,
                &candidate,
                &format!(
                    "CENTRAL VERIFICATION INFRASTRUCTURE RETRY — the story remains verifying; its code was not classified red. {detail}"
                ),
            )?;
            Ok(TickResult::RetryLater)
        }
    }
}

fn record_cleanup_complete(
    ctx: &Ctx<'_, impl Store>,
    candidate: &VerificationCandidate,
) -> Result<(), AppError> {
    comment_once(
        ctx,
        candidate,
        &format!(
            "{VERIFICATION_CLEANUP_COMPLETE_PREFIX} worktree, branch, and agent window were reaped."
        ),
    )
}

fn record_cleanup_required(
    ctx: &Ctx<'_, impl Store>,
    candidate: &VerificationCandidate,
    error: &AppError,
) -> Result<(), AppError> {
    comment_once(
        ctx,
        candidate,
        &format!(
            "CENTRAL VERIFICATION CLEANUP REQUIRED — the PR landed and the story is done, but automatic reap failed: {error}"
        ),
    )
}

fn return_for_repair<S: Store, A: VerificationActuator>(
    ctx: &Ctx<'_, S>,
    actuator: &A,
    candidate: &VerificationCandidate,
    diagnosis: &str,
) -> Result<(), AppError> {
    StoryService::new(ctx).comment(&candidate.story_id, diagnosis)?;
    StoryService::new(ctx).set_state(
        &candidate.story_id,
        "in-progress",
        None,
        Some("verifying"),
        None,
    )?;
    if let Err(error) = actuator.notify(candidate, diagnosis) {
        StoryService::new(ctx).set_awaiting(
            &candidate.story_id,
            &format!("verification remediation could not reach its agent: {error}"),
        )?;
    }
    Ok(())
}

fn comment_once(
    ctx: &Ctx<'_, impl Store>,
    candidate: &VerificationCandidate,
    text: &str,
) -> Result<(), AppError> {
    let already_recorded = ctx.store().read(|tx| {
        let prefix = tx
            .project(candidate.project)?
            .map(|project| project.prefix)
            .unwrap_or_default();
        let number = crate::store::StoryNo::parse_id(&prefix, &candidate.story_id)
            .map_err(crate::store::StoreError::from)?;
        Ok(tx.story(candidate.project, number)?.is_some_and(|row| {
            row.snapshot
                .comments
                .iter()
                .any(|comment| comment.text == text)
        }))
    })?;
    if !already_recorded {
        StoryService::new(ctx).comment(&candidate.story_id, text)?;
    }
    Ok(())
}

/// Runs the event-driven verifier until daemon shutdown.
pub(crate) fn poll_verification(
    store: &impl Store,
    env: &Environment,
    bus: &ChangeBus,
    stop: &AtomicBool,
) {
    let subscription = bus.subscribe();
    let actuator = ShellVerificationActuator::new(env.clone());
    while !stop.load(Ordering::Relaxed) {
        match tick_with(store, env, &actuator) {
            Ok(TickResult::Completed | TickResult::Returned) => continue,
            Ok(TickResult::RetryLater) => {
                let retry_at = Instant::now() + RECOVERY_WAKE;
                while !stop.load(Ordering::Relaxed) {
                    let remaining = retry_at.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    let _ = subscription.recv(remaining);
                }
            }
            Err(error) => {
                eprintln!("storyhook: centralized verification tick failed: {error}");
                while matches!(subscription.recv(RECOVERY_WAKE), Some(Change::Ping))
                    && !stop.load(Ordering::Relaxed)
                {}
            }
            Ok(TickResult::Idle) => {
                while matches!(subscription.recv(RECOVERY_WAKE), Some(Change::Ping))
                    && !stop.load(Ordering::Relaxed)
                {}
            }
        }
    }
}
