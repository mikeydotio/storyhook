//! The daemon-owned centralized verification worker (SH-521).

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::bus::{Change, ChangeBus};
use super::lifecycle::{CurrentRequest, InFlight};
use crate::api::dispatch::{DispatchAgent, resolve_dispatch_script};
use crate::domain::github_remote::parse_github_url;
use crate::domain::{CLEANUP_LEASE_ENV, CLEANUP_LEASE_VERSION, CleanupReceipt};
use crate::env::Environment;
use crate::env::spawn_env::{apply_dispatch_allowlist, apply_verification_allowlist};
use crate::error::AppError;
use crate::process::{
    CaptureError, Captured, TerminationPolicy, TimeoutTermination, run_captured,
    run_captured_with_termination,
};
use crate::service::engine::DISPATCH_TIMEOUT;
use crate::service::{
    Ctx, StoryService, VERIFICATION_CLEANUP_COMPLETE_PREFIX, VERIFICATION_GREEN_PREFIX,
    VerificationCandidate, VerificationProblem, VerificationQueue,
};
use crate::store::{
    GlobalSeq, PrLink, ProjectId, ReadOps, Store, VerificationFailureDisposition,
    VerificationIncident, WriteOps,
};

/// Infrastructure recovery cadence when no store event arrives.
const RECOVERY_WAKE: Duration = Duration::from_secs(30);

/// Attempts admitted inside one progress-freshness window: now, +30s, +60s.
pub const INFRASTRUCTURE_RETRY_ATTEMPTS: u32 = super::verification_progress::PUBLISH_INTERVAL
    .as_secs() as u32
    / RECOVERY_WAKE.as_secs() as u32
    + 1;

/// Marker for the one edited infrastructure evidence comment.
pub const INFRASTRUCTURE_COMMENT_PREFIX: &str = "CENTRAL VERIFICATION INFRASTRUCTURE —";

/// One verification generation currently owned by this daemon's serialized
/// verifier. Queue rank is deliberately absent: priority may change while an
/// attempt is running, but ownership cannot (SH-549).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveVerification {
    /// Store identity of the story's project.
    pub project: ProjectId,
    /// Display id of the story being verified.
    pub story_id: String,
    /// Exact latest transition into `verifying` that this attempt owns.
    pub generation: Option<GlobalSeq>,
    /// When the verifier acquired this generation.
    pub started_at: String,
}

/// Process-local source of truth for verifier ownership.
///
/// Ownership cannot survive the daemon process that owns the synchronous
/// verification subprocess, so persisting it would create stale leases after
/// crashes. Clones share one slot across the verifier, progress publisher and
/// HTTP dispatcher.
#[derive(Clone, Default)]
pub struct VerificationActivity {
    active: Arc<Mutex<Option<ActiveVerification>>>,
}

impl VerificationActivity {
    /// Creates an empty registry. After daemon restart every surviving
    /// `verifying` story is queued until the new worker acquires it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the generation owned at this instant, if any.
    #[must_use]
    pub fn active(&self) -> Option<ActiveVerification> {
        self.active
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Marks `candidate` active until the returned guard is dropped.
    ///
    /// The daemon has one serialized worker; a second simultaneous acquire is
    /// therefore an invariant violation rather than another queue slot.
    #[must_use]
    pub fn acquire(
        &self,
        candidate: &VerificationCandidate,
        started_at: String,
    ) -> VerificationGuard {
        let active = ActiveVerification {
            project: candidate.project,
            story_id: candidate.story_id.clone(),
            generation: candidate.verifying_generation,
            started_at,
        };
        let mut slot = self.active.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(slot.is_none(), "the serialized verifier acquired twice");
        *slot = Some(active.clone());
        VerificationGuard {
            registry: self.clone(),
            active,
        }
    }
}

/// Clears exactly the acquisition it represents. The identity check prevents
/// a delayed drop from clearing a later attempt if verifier concurrency ever
/// changes accidentally.
pub struct VerificationGuard {
    registry: VerificationActivity,
    active: ActiveVerification,
}

impl Drop for VerificationGuard {
    fn drop(&mut self) {
        let mut slot = self
            .registry
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if slot.as_ref() == Some(&self.active) {
            *slot = None;
        }
    }
}

/// Largest observed `make test` runtime under this machine's ordinary
/// concurrent workload, recorded by the Full Auto design investigation.
const MEASURED_CONTENDED_GATE_SECS: u64 = 873;

/// Slack above the measured contended gate for GitHub, fetch, and landing.
const VERIFICATION_TIMEOUT_MARGIN: u64 = 2;

/// Maximum wall-clock duration of one centralized verification attempt.
///
/// Derived from the largest measured contended gate rather than a bare
/// deadline. Twice that measurement leaves one additional full gate window
/// for lock waiting and the networked phases surrounding `make test`.
pub const VERIFICATION_TIMEOUT: Duration =
    Duration::from_secs(MEASURED_CONTENDED_GATE_SECS * VERIFICATION_TIMEOUT_MARGIN);

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
    InfrastructureFailure {
        /// Latest diagnosis.
        detail: String,
        /// Whether retrying unchanged can recover.
        disposition: VerificationFailureDisposition,
    },
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
    helper_path: Option<PathBuf>,
    story_binary: Option<PathBuf>,
    verification_timeout: Duration,
    control_timeout: Duration,
    termination_grace: Duration,
}

impl ShellVerificationActuator {
    /// Creates a shell actuator for one daemon environment.
    #[must_use]
    pub fn new(env: Environment) -> Self {
        Self {
            env,
            helper_path: None,
            story_binary: None,
            verification_timeout: VERIFICATION_TIMEOUT,
            control_timeout: DISPATCH_TIMEOUT,
            termination_grace: RECOVERY_WAKE,
        }
    }

    /// Creates an actuator with explicit subprocess paths.
    ///
    /// This narrow injection seam lets integration tests execute the real
    /// process/receipt boundary without mutating process-global environment or
    /// depending on a machine's installed plugin cache.
    #[must_use]
    pub fn with_paths(env: Environment, helper_path: PathBuf, story_binary: PathBuf) -> Self {
        Self {
            env,
            helper_path: Some(helper_path),
            story_binary: Some(story_binary),
            verification_timeout: VERIFICATION_TIMEOUT,
            control_timeout: DISPATCH_TIMEOUT,
            termination_grace: RECOVERY_WAKE,
        }
    }

    /// Creates an actuator with explicit subprocess paths and timing policy.
    ///
    /// Production uses [`Self::new`]. This injection seam lets integration
    /// tests provoke the full timeout and process-group boundary without
    /// waiting for the production deadline.
    #[must_use]
    pub fn with_paths_and_timing(
        env: Environment,
        helper_path: PathBuf,
        story_binary: PathBuf,
        verification_timeout: Duration,
        control_timeout: Duration,
        termination_grace: Duration,
    ) -> Self {
        Self {
            env,
            helper_path: Some(helper_path),
            story_binary: Some(story_binary),
            verification_timeout,
            control_timeout,
            termination_grace,
        }
    }

    fn helper_path(&self) -> Result<PathBuf, AppError> {
        if let Some(path) = &self.helper_path {
            return Ok(path.clone());
        }
        resolve_dispatch_script(DispatchAgent::Codex)
            .or_else(|_| resolve_dispatch_script(DispatchAgent::Claude))
            .map_err(AppError::Storage)
    }

    fn story_binary(&self) -> PathBuf {
        self.story_binary
            .clone()
            .unwrap_or_else(|| std::env::current_exe().unwrap_or_else(|_| "story".into()))
    }

    fn run_control_command(&self, command: Command, operation: &str) -> Result<Captured, AppError> {
        run_captured(command, self.control_timeout).map_err(|error| match error {
            CaptureError::Timeout(_) => AppError::Storage(format!(
                "{operation} did not finish within {:?}; its process group was terminated",
                self.control_timeout
            )),
            other => AppError::Storage(format!("could not run {operation}: {}", other.detail())),
        })
    }

    fn helper(
        &self,
        candidate: &VerificationCandidate,
        verb: &str,
        extra: Option<&str>,
    ) -> Result<(), AppError> {
        let script = self.helper_path()?;
        let mut command = Command::new("bash");
        apply_dispatch_allowlist(&mut command);
        command
            .arg(script)
            .arg("--project")
            .arg(&candidate.project_slug)
            .arg(verb)
            .arg(&candidate.story_id)
            .current_dir(&candidate.checkout)
            // Provider selection is a dispatch concern. Notify/reap must not
            // inherit a daemon starter's stale provider convention.
            .env_remove("STORY_AGENT")
            .env("STORY_BIN", self.story_binary())
            .env("STORYHOOK_STORE_PATH", self.env.store_path())
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null());
        if let Some(extra) = extra {
            command.arg(extra);
        }
        let output = self.run_control_command(command, &format!("story helper `{verb}`"))?;
        let payload: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|_| {
            AppError::Storage(format!(
                "story helper `{verb}` returned invalid JSON: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        })?;
        if payload.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stderr = stderr.trim();
                let detail = if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                };
                return Err(AppError::Storage(format!(
                    "story helper `{verb}` reported success but exited {}{detail}",
                    output.status
                )));
            }
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

    fn reap_leased(&self, candidate: &VerificationCandidate) -> Result<(), AppError> {
        let lease = candidate.cleanup_lease.as_ref().ok_or_else(|| {
            AppError::Storage(format!(
                "story {} has no cleanup lease for its latest verification generation",
                candidate.story_id
            ))
        })?;
        let encoded = serde_json::to_string(lease).map_err(|error| {
            AppError::Storage(format!("could not encode cleanup lease: {error}"))
        })?;
        let mut command = Command::new("bash");
        apply_dispatch_allowlist(&mut command);
        command
            .arg(self.helper_path()?)
            .arg("--project")
            .arg(&candidate.project_slug)
            .arg("reap")
            .arg(&candidate.story_id)
            .current_dir(&lease.repository_path)
            .env_remove("STORY_AGENT")
            .env("STORY_BIN", self.story_binary())
            .env("STORYHOOK_STORE_PATH", self.env.store_path())
            .env(CLEANUP_LEASE_ENV, encoded)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null());
        let output = self.run_control_command(command, "leased story helper `reap`")?;

        let receipt: CleanupReceipt = serde_json::from_slice(&output.stdout).map_err(|_| {
            AppError::Storage(format!(
                "leased story helper `reap` returned invalid receipt: {}{}",
                String::from_utf8_lossy(&output.stderr).trim(),
                if output.stdout.is_empty() {
                    String::new()
                } else {
                    format!(
                        "; stdout: {}",
                        String::from_utf8_lossy(&output.stdout).trim()
                    )
                }
            ))
        })?;
        if !output.status.success() {
            return Err(AppError::Storage(format!(
                "leased story helper `reap` exited {}: {}",
                output.status, receipt.display
            )));
        }
        if receipt.receipt_version != CLEANUP_LEASE_VERSION {
            return Err(AppError::Storage(format!(
                "leased reap receipt uses unsupported version {}",
                receipt.receipt_version
            )));
        }
        if !receipt.ok {
            return Err(AppError::Storage(receipt.display));
        }
        if receipt.story_id != candidate.story_id || receipt.lease != *lease {
            return Err(AppError::Storage(
                "leased reap receipt does not echo the requested story and lease".to_string(),
            ));
        }
        let post = &receipt.postconditions;
        if !post.worktree_registration_absent
            || !post.worktree_path_absent
            || !post.branch_absent
            || !post.tmux_story_windows_absent
        {
            return Err(AppError::Storage(format!(
                "leased reap receipt claims success without every exact postcondition: {}",
                receipt.display
            )));
        }
        Ok(())
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
        let journal = journal_path(&self.env, candidate);
        // Best-effort and non-fatal (SH-524): the progress journal is an
        // observability feature, not part of the release gate's own
        // correctness, so a failure to prepare it is reported — never
        // silently swallowed (CLAUDE.md's fail-loud rule) — but must not
        // abort the actual verification over it. Truncated at run start so
        // a rerun after an infrastructure retry does not replay stale
        // progress from the attempt before it.
        if let Some(parent) = journal.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            eprintln!(
                "storyhook: could not create verification progress directory {}: {error}",
                parent.display()
            );
        }
        let initial = candidate
            .verifying_generation
            .map_or_else(String::new, |generation| {
                format!(
                    "{}\n",
                    serde_json::json!({
                        "kind": "run",
                        "generation": generation.get(),
                        "at": self.env.now(),
                    })
                )
            });
        if let Err(error) = std::fs::write(&journal, initial) {
            eprintln!(
                "storyhook: could not truncate verification progress journal {}: {error}",
                journal.display()
            );
        }
        let mut command = Command::new("bash");
        apply_verification_allowlist(&mut command);
        command
            .arg("scripts/verify-pr.sh")
            .arg(&pull_request.url)
            .current_dir(&candidate.checkout)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GH_PROMPT_DISABLED", "1")
            .env("STORYHOOK_GATE_PROGRESS", &journal);
        let captured = match run_captured_with_termination(
            command,
            self.verification_timeout,
            TerminationPolicy::TerminateThenKill {
                grace: self.termination_grace,
            },
        ) {
            Ok(captured) => captured,
            Err(CaptureError::Stage(error)) => {
                return VerificationOutcome::InfrastructureFailure {
                    detail: format!("could not stage scripts/verify-pr.sh output: {error}"),
                    disposition: VerificationFailureDisposition::Permanent,
                };
            }
            Err(CaptureError::Spawn(error)) => {
                return VerificationOutcome::InfrastructureFailure {
                    detail: format!("could not start scripts/verify-pr.sh: {error}"),
                    disposition: VerificationFailureDisposition::Permanent,
                };
            }
            Err(CaptureError::Wait(error)) => {
                return VerificationOutcome::InfrastructureFailure {
                    detail: format!("could not wait for scripts/verify-pr.sh: {error}"),
                    disposition: VerificationFailureDisposition::Permanent,
                };
            }
            Err(CaptureError::Timeout(termination)) => {
                let termination = match termination {
                    TimeoutTermination::ExitedAfterTerminate => format!(
                        "sent SIGTERM to its process group, which exited within the {:?} cleanup grace",
                        self.termination_grace
                    ),
                    TimeoutTermination::KilledAfterTerminate => format!(
                        "sent SIGTERM to its process group, then SIGKILL to survivors after the {:?} cleanup grace",
                        self.termination_grace
                    ),
                    TimeoutTermination::Killed => "sent SIGKILL to its process group".to_string(),
                };
                return VerificationOutcome::InfrastructureFailure {
                    detail: format!(
                        "scripts/verify-pr.sh did not finish within {:?}; {termination}",
                        self.verification_timeout
                    ),
                    disposition: VerificationFailureDisposition::Permanent,
                };
            }
        };
        let parsed: WireOutcome = match serde_json::from_slice(&captured.stdout) {
            Ok(parsed) => parsed,
            Err(_) => {
                return VerificationOutcome::InfrastructureFailure {
                    detail: format!(
                        "scripts/verify-pr.sh returned invalid JSON: {}",
                        String::from_utf8_lossy(&captured.stderr).trim()
                    ),
                    disposition: VerificationFailureDisposition::Permanent,
                };
            }
        };
        parsed.into()
    }

    fn notify(&self, candidate: &VerificationCandidate, message: &str) -> Result<(), AppError> {
        self.helper(candidate, "notify", Some(message))
    }

    fn reap(&self, candidate: &VerificationCandidate) -> Result<(), AppError> {
        self.reap_leased(candidate)
    }
}

/// Where the SH-524 progress journal for `candidate` lives.
///
/// Derived rather than passed twice: both the actuator (which sets
/// `$STORYHOOK_GATE_PROGRESS` on the gate's own subprocess) and the
/// publisher (`crate::daemon::verification_progress`, which reads the same
/// file back) call this one function, so the two can never disagree about
/// the path. Scoped to the daemon's own store — `Environment::
/// daemon_state_dir` is store-keyed (SH-113) — and named by project and
/// story so two candidates never collide. Public for store-backed
/// integration tests, the same reason `tick_with` is.
pub fn journal_path(env: &Environment, candidate: &VerificationCandidate) -> PathBuf {
    env.daemon_state_dir()
        .join("verification-progress")
        .join(format!(
            "{}-{}.ndjson",
            candidate.project_slug, candidate.story_id
        ))
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
        disposition: VerificationFailureDisposition,
    },
    InvalidSubmission {
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
            WireOutcome::InfrastructureFailure {
                detail,
                disposition,
            } => Self::InfrastructureFailure {
                detail,
                disposition,
            },
            WireOutcome::InvalidSubmission { detail } => Self::InvalidSubmission { detail },
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
    /// Durable infrastructure evidence has stopped the queue pending acknowledgement.
    Halted,
}

/// Runs one verification attempt. Public for store-backed integration tests.
pub fn tick_with<S: Store, A: VerificationActuator>(
    store: &S,
    env: &Environment,
    actuator: &A,
) -> Result<TickResult, AppError> {
    let inflight = InFlight::new(env.clone());
    tick_with_activity(
        store,
        env,
        actuator,
        &VerificationActivity::new(),
        &inflight,
    )
}

/// Runs one verification attempt while publishing process-local ownership
/// through `activity` and shutdown ownership through `inflight`.
///
/// Public for the blocking-actuator integration tests that prove queue
/// reordering cannot steal either ownership signal (SH-549, SH-556).
pub fn tick_with_activity<S: Store, A: VerificationActuator>(
    store: &S,
    env: &Environment,
    actuator: &A,
    activity: &VerificationActivity,
    inflight: &InFlight,
) -> Result<TickResult, AppError> {
    let queue = VerificationQueue::new(store);
    let ordered = queue.ordered()?;
    let incident = store.read(|tx| tx.verification_incident())?;
    let incident_candidate = incident.as_ref().and_then(|incident| {
        ordered
            .iter()
            .find(|candidate| incident_matches(incident, candidate))
            .cloned()
    });
    if let Some(incident) = incident.as_ref() {
        if incident_candidate.is_none() {
            store.write(|tx| {
                tx.clear_verification_incident(&incident.incident_id)?;
                Ok(())
            })?;
        } else if incident.halted {
            return Ok(TickResult::Halted);
        }
    }
    let Some(candidate) = incident_candidate.or_else(|| ordered.first().cloned()) else {
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
            return_for_repair(
                &ctx,
                actuator,
                &candidate,
                &VerificationProblem::MissingCheckout.message(),
            )?;
            return Ok(TickResult::Returned);
        }
        Err(problem) => {
            return_for_repair(&ctx, actuator, &candidate, &problem.message())?;
            return Ok(TickResult::Returned);
        }
    };

    let started_at = env.now();
    let generation = candidate.verifying_generation.map_or_else(
        || "legacy".to_string(),
        |generation| generation.get().to_string(),
    );
    let lifecycle_entry = inflight.enter();
    lifecycle_entry.name(CurrentRequest {
        request_id: format!(
            "verify:{}:{}:{generation}",
            candidate.project_slug, candidate.story_id
        ),
        command: "verify".to_string(),
        project: Some(candidate.project_slug.clone()),
        pid: std::process::id(),
        started_at: started_at.clone(),
        served_deadline_secs: VERIFICATION_TIMEOUT.as_secs(),
        cwd: candidate.checkout.clone(),
    });
    let active = activity.acquire(&candidate, started_at);
    let outcome = actuator.verify(&candidate, &pull_request);

    if !matches!(outcome, VerificationOutcome::InfrastructureFailure { .. }) {
        clear_matching_incident(store, &candidate)?;
    }

    match outcome {
        VerificationOutcome::Merged { tree, detail } => {
            StoryService::new(&ctx).comment(
                &candidate.story_id,
                &format!(
                    "{VERIFICATION_GREEN_PREFIX} merge tree `{tree}` passed `make test` and pull request {} landed. {detail}",
                    pull_request.url
                ),
            )?;
            queue.record_merged(&ctx, &candidate.story_id, &pull_request.url)?;
            // Reaping is cleanup for work whose durable outcome is already
            // recorded; it must not keep graceful shutdown waiting on the
            // completed verification transaction.
            drop(active);
            drop(lifecycle_entry);
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
        VerificationOutcome::InfrastructureFailure {
            detail,
            disposition,
        } => record_infrastructure_failure(&ctx, &candidate, disposition, &detail),
    }
}

fn incident_matches(incident: &VerificationIncident, candidate: &VerificationCandidate) -> bool {
    candidate.project == incident.project
        && candidate.verifying_generation == Some(incident.generation)
}

fn clear_matching_incident(
    store: &impl Store,
    candidate: &VerificationCandidate,
) -> Result<(), AppError> {
    let incident = store.read(|tx| tx.verification_incident())?;
    if let Some(incident) = incident.filter(|incident| incident_matches(incident, candidate)) {
        store.write(|tx| {
            tx.clear_verification_incident(&incident.incident_id)?;
            Ok(())
        })?;
    }
    Ok(())
}

fn record_infrastructure_failure(
    ctx: &Ctx<'_, impl Store>,
    candidate: &VerificationCandidate,
    disposition: VerificationFailureDisposition,
    detail: &str,
) -> Result<TickResult, AppError> {
    let generation = candidate.verifying_generation.ok_or_else(|| {
        AppError::Storage(format!(
            "{} has no verification generation",
            candidate.story_id
        ))
    })?;
    let project = ctx
        .store()
        .read(|tx| tx.project(candidate.project))?
        .ok_or_else(|| AppError::NotFound(format!("project {}", candidate.project.get())))?;
    let story = crate::store::StoryNo::parse_id(&project.prefix, &candidate.story_id)?;
    let incident_id = format!("{}:{}", candidate.project.get(), generation.get());
    let now = ctx.now();
    let mut incident = ctx
        .store()
        .read(|tx| tx.verification_incident())?
        .filter(|current| current.incident_id == incident_id)
        .unwrap_or(VerificationIncident {
            incident_id,
            project: candidate.project,
            story,
            generation,
            disposition,
            halted: false,
            attempts: 0,
            detail: String::new(),
            first_failed_at: now.clone(),
            last_failed_at: now.clone(),
        });
    incident.attempts = incident.attempts.saturating_add(1);
    incident.disposition = disposition;
    incident.detail = detail.to_string();
    incident.last_failed_at = now;
    incident.halted = disposition == VerificationFailureDisposition::Permanent
        || incident.attempts >= INFRASTRUCTURE_RETRY_ATTEMPTS;
    ctx.store()
        .write(|tx| tx.put_verification_incident(&incident))?;

    let state = if incident.halted {
        "HALTED"
    } else {
        "RETRYING"
    };
    let body = format!(
        "{INFRASTRUCTURE_COMMENT_PREFIX} {state}\n\nAttempt {} of {}. First failure: {}. Latest attempt: {}.\nThe story remains verifying; its code was not classified red.\n\n{}",
        incident.attempts,
        INFRASTRUCTURE_RETRY_ATTEMPTS,
        incident.first_failed_at,
        incident.last_failed_at,
        incident.detail
    );
    StoryService::new(ctx).upsert_marked_comment(
        &candidate.story_id,
        INFRASTRUCTURE_COMMENT_PREFIX,
        &body,
    )?;
    if incident.halted {
        Ctx::new(
            ctx.store(),
            ctx.project(),
            ctx.cwd().to_path_buf(),
            ctx.env().clone(),
        )
        .fire_hook(
            crate::event_hooks::HookEventType::VerificationHalted,
            &serde_json::json!({
                "event_type": "verification_halted",
                "incident_id": incident.incident_id,
                "project": candidate.project_slug,
                "story_id": candidate.story_id,
                "attempts": incident.attempts,
                "first_failed_at": incident.first_failed_at,
                "last_failed_at": incident.last_failed_at,
                "detail": incident.detail,
            }),
        );
        Ok(TickResult::Halted)
    } else {
        Ok(TickResult::RetryLater)
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
            "{VERIFICATION_CLEANUP_COMPLETE_PREFIX} exact leased worktree, branch, and agent window were verified absent."
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
        let number = crate::store::StoryNo::parse_id(&prefix, &candidate.story_id)?;
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
    activity: &VerificationActivity,
    inflight: &InFlight,
) {
    let subscription = bus.subscribe();
    let actuator = ShellVerificationActuator::new(env.clone());
    while !stop.load(Ordering::Relaxed) {
        match tick_with_activity(store, env, &actuator, activity, inflight) {
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
            Ok(TickResult::Halted) => {
                while matches!(subscription.recv(RECOVERY_WAKE), Some(Change::Ping))
                    && !stop.load(Ordering::Relaxed)
                {}
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
