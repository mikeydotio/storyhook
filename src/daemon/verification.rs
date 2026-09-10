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
use crate::domain::pr_url::parse_pr_url;
use crate::domain::{CLEANUP_LEASE_ENV, CLEANUP_LEASE_VERSION, CleanupReceipt};
use crate::env::Environment;
use crate::env::spawn_env::{apply_dispatch_allowlist, apply_verification_allowlist};
use crate::error::AppError;
use crate::process::{
    CaptureError, Captured, TerminationPolicy, TimeoutTermination,
    run_captured_with_progress_and_registration, run_captured_with_registration,
};
use crate::service::engine::{
    DISPATCH_TIMEOUT, DispatchOptions, DispatchOutcomeState, run_shell_dispatch,
};
use crate::service::verification::GenerationWrite;
use crate::service::{
    Ctx, StoryService, VERIFICATION_CLEANUP_COMPLETE_PREFIX, VERIFICATION_GREEN_PREFIX,
    VerificationCandidate, VerificationQueue,
};
use crate::store::{
    EngineAgent, EngineLaneState, EngineSpeed, GlobalSeq, PrLink, ProjectId, ReadOps, Store,
    VerificationFailureDisposition, VerificationIncident, WriteOps,
};

/// Infrastructure recovery cadence when no store event arrives.
const RECOVERY_WAKE: Duration = Duration::from_secs(30);

/// Attempts admitted inside one progress-freshness window: now, +30s, +60s.
pub const INFRASTRUCTURE_RETRY_ATTEMPTS: u32 = super::verification_progress::PUBLISH_INTERVAL
    .as_secs() as u32
    / RECOVERY_WAKE.as_secs() as u32
    + 1;

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

impl VerificationGuard {
    /// Transfers this worker's existing reservation to a newer verification
    /// generation of the same story.
    ///
    /// Reconciliation temporarily removes the story from the queue, then
    /// creates a new generation when the agent resubmits it. The serialized
    /// worker still owns the story throughout, so replacing the generation is
    /// one guarded mutation rather than a release followed by a new acquire.
    fn replace(&mut self, candidate: &VerificationCandidate, started_at: String) {
        assert_eq!(self.active.project, candidate.project);
        assert_eq!(self.active.story_id, candidate.story_id);
        let replacement = ActiveVerification {
            project: candidate.project,
            story_id: candidate.story_id.clone(),
            generation: candidate.verifying_generation,
            started_at,
        };
        let mut slot = self
            .registry
            .active
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        assert_eq!(slot.as_ref(), Some(&self.active));
        *slot = Some(replacement.clone());
        self.active = replacement;
    }
}

/// Largest observed runtime of the default gate (`make test`) under this
/// machine's ordinary concurrent workload, recorded by the Full Auto design
/// investigation. A project's own `[verify] gate` (SH-649) runs under the
/// same silence cap; one that emits no progress journal has only this.
const MEASURED_CONTENDED_GATE_SECS: u64 = 873;

/// Multiplicative slack above the measured contended gate.
const VERIFICATION_IDLE_TIMEOUT_MARGIN: u64 = 2;

/// Maximum silence during centralized verification (SH-592).
///
/// Twice the largest measured contended gate covers healthy silence. One
/// recovery window beyond that gives the inner gate watchdog time to publish
/// its last journal record, descendant tree, and bounded cleanup first.
/// Journal appends renew this deadline, so progressing tests and
/// identity-checked lock waits have no total runtime cap.
pub const VERIFICATION_IDLE_TIMEOUT: Duration = Duration::from_secs(
    MEASURED_CONTENDED_GATE_SECS * VERIFICATION_IDLE_TIMEOUT_MARGIN + RECOVERY_WAKE.as_secs(),
);

/// One repository-side verification result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VerificationOutcome {
    /// The exact merge tree passed and the guarded merge landed.
    Merged {
        tree: String,
        detail: String,
        /// The gate command that certified the tree, as one line (SH-649) —
        /// what the GREEN comment names, never a literal.
        gate: String,
    },
    /// The PR does not merge into current main.
    Conflict { detail: String },
    /// The submission cannot safely be acted on from its registered checkout.
    InvalidSubmission { detail: String },
    /// The exact merge tree failed the repository's release gate.
    TestsFailed {
        tree: String,
        log: String,
        detail: String,
        /// The gate command that failed, as one line (SH-649).
        gate: String,
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

/// How `story.sh notify` answered a remediation delivery (SH-650).
///
/// SH-626's shape, one verb over: "no agent is there" is the helper's own
/// well-formed answer, not a failure of the helper, and the two are told
/// apart because the verifier acts on them differently — an absent agent is
/// re-dispatched in place; anything else parks the story.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotifyDelivery {
    /// The diagnosis was pasted and submitted into the dispatched pane.
    Delivered,
    /// The helper refused, by a name classified [`AgentPresence::Absent`]:
    /// the story's window holds no live dispatched agent to type into.
    AgentAbsent {
        /// The helper's refusal slug.
        reason: String,
        /// The helper's own sentence about it.
        detail: String,
    },
}

/// Whether a `story.sh notify` refusal means the dispatched agent is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentPresence {
    /// No live dispatched agent occupies the window; a resume re-dispatch
    /// (`dispatch --resume`, which respawns a dead pane in place or recreates
    /// a missing window under the same name) cannot kill work in progress.
    Absent,
    /// Either the agent may be live, or tmux could not say — neither is
    /// evidence of absence, and a respawn over a live agent would kill it.
    NotAbsent,
}

/// Every refusal `cmd_notify` (`plugins/story/bin/story.sh`) can emit,
/// classified by name.
///
/// The table is exhaustive on purpose and `tests/notify_reasons.rs` derives
/// the helper's slugs from its own `refuse "…"` literals to prove it: a slug
/// the helper grows that is missing here fails the build rather than falling
/// through to whichever branch happened to be safe. A refusal whose `reason`
/// is absent or unknown is treated as [`AgentPresence::NotAbsent`] by the
/// classifier, never as a licence to respawn.
///
/// `pane-provider-unknown` is deliberately **not** absent: the window exists
/// and carries no Storyhook provider tag, so the verifier cannot prove it is
/// its own — the SH-226 rule against typing into an unverified pane applies
/// with more force to respawning over one. `pane-query-failed` is tmux not
/// answering (SH-626: a probe that could not run has not answered no), and
/// `delivery-failed` is a paste refused by a pane that passed every liveness
/// gate, so the agent is presumed live.
pub const NOTIFY_REFUSALS: [(&str, AgentPresence); 6] = [
    ("pane-query-failed", AgentPresence::NotAbsent),
    ("pane-unavailable", AgentPresence::Absent),
    ("pane-provider-unknown", AgentPresence::NotAbsent),
    ("pane-dead", AgentPresence::Absent),
    ("pane-changed", AgentPresence::Absent),
    ("delivery-failed", AgentPresence::NotAbsent),
];

/// Classifies a notify refusal slug against [`NOTIFY_REFUSALS`]; an unknown
/// or missing slug is never absence.
#[must_use]
pub fn agent_presence(reason: Option<&str>) -> AgentPresence {
    reason
        .and_then(|slug| {
            NOTIFY_REFUSALS
                .iter()
                .find(|(known, _)| *known == slug)
                .map(|(_, presence)| *presence)
        })
        .unwrap_or(AgentPresence::NotAbsent)
}

/// Everything the helper needs to re-dispatch a returned story in place
/// (SH-650): `dispatch <id> --resume --auto`, plus the provider identity of the
/// lane it belongs to when the Full Auto engine holds it.
///
/// `agent: None` leaves the provider to the helper, which reads the one the
/// abandoned dispatch recorded (its window's `@storyhook-agent` option, or
/// the worktree container). A live engine lane is the exception: its run
/// carries the provider, model, effort and speed the lane was launched with,
/// and `full_auto` keeps the lane's identity (`STORYHOOK_FULL_AUTO`, the Full
/// Auto launch template) rather than downgrading the respawn to an ordinary
/// autonomous session inside an engine lane.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResumePlan {
    pub agent: Option<EngineAgent>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub fast: bool,
    pub full_auto: bool,
}

/// The prefix of the story comment written before a resume re-dispatch.
pub const VERIFICATION_RESUME_PREFIX: &str = "CENTRAL VERIFICATION RESUME —";

/// Process boundary for repository verification and agent-session control.
pub trait VerificationActuator: Send + Sync {
    /// Verifies and, on green, lands one submitted PR.
    fn verify(
        &self,
        candidate: &VerificationCandidate,
        pull_request: &PrLink,
    ) -> VerificationOutcome;
    /// Delivers remediation to the exact dispatched agent pane, or answers
    /// that no live agent is there to receive it.
    fn notify(
        &self,
        candidate: &VerificationCandidate,
        message: &str,
    ) -> Result<NotifyDelivery, AppError>;
    /// Re-dispatches a returned story into its own window and worktree with
    /// the resume clause (SH-650). `Err` is the helper's refusal, verbatim.
    fn redispatch(
        &self,
        candidate: &VerificationCandidate,
        plan: &ResumePlan,
    ) -> Result<(), AppError>;
    /// Reclaims a merged story's worktree, branch, and tmux window.
    fn reap(&self, candidate: &VerificationCandidate) -> Result<(), AppError>;
}

/// A control verb's well-formed answer, as the helper's JSON contract states
/// it: `ok:true`, or `ok:false` with a `reason` slug and a `display` sentence.
enum HelperAnswer {
    Ok,
    Refused {
        reason: Option<String>,
        display: String,
    },
}

/// Production actuator backed by repository and plugin scripts.
pub struct ShellVerificationActuator {
    env: Environment,
    owned_processes: super::lifecycle::OwnedProcesses,
    helper_path: Option<PathBuf>,
    story_binary: Option<PathBuf>,
    verification_idle_timeout: Duration,
    control_timeout: Duration,
    termination_grace: Duration,
}

impl ShellVerificationActuator {
    /// Creates a shell actuator for one daemon environment.
    #[must_use]
    pub fn new(env: Environment) -> Self {
        Self {
            owned_processes: super::lifecycle::OwnedProcesses::new(env.clone()),
            env,
            helper_path: None,
            story_binary: None,
            verification_idle_timeout: VERIFICATION_IDLE_TIMEOUT,
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
            owned_processes: super::lifecycle::OwnedProcesses::new(env.clone()),
            env,
            helper_path: Some(helper_path),
            story_binary: Some(story_binary),
            verification_idle_timeout: VERIFICATION_IDLE_TIMEOUT,
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
        verification_idle_timeout: Duration,
        control_timeout: Duration,
        termination_grace: Duration,
    ) -> Self {
        Self {
            owned_processes: super::lifecycle::OwnedProcesses::new(env.clone()),
            env,
            helper_path: Some(helper_path),
            story_binary: Some(story_binary),
            verification_idle_timeout,
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

    fn run_control_command(
        &self,
        command: Command,
        role: &str,
        request_id: &str,
        operation: &str,
    ) -> Result<Captured, AppError> {
        run_captured_with_registration(
            command,
            self.control_timeout,
            TerminationPolicy::Kill,
            |pid| {
                self.owned_processes
                    .register(role, pid, Some(request_id))
                    .map_err(|error| error.to_string())
            },
        )
        .map_err(|error| match error {
            CaptureError::Timeout(_) => AppError::Storage(format!(
                "{operation} did not finish within {:?}; its process group was terminated",
                self.control_timeout
            )),
            other => AppError::Storage(format!("could not run {operation}: {}", other.detail())),
        })
    }

    /// Runs one control verb and returns the helper's own answer. `Err` is a
    /// process or protocol failure (no JSON, a timeout, success JSON from a
    /// failed process); a well-formed refusal is an `Ok` answer with
    /// `ok == false`, its `reason` slug preserved for the caller to classify.
    fn helper(
        &self,
        candidate: &VerificationCandidate,
        verb: &str,
        extra: Option<&str>,
    ) -> Result<HelperAnswer, AppError> {
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
            .envs(self.env.child_vars())
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null());
        if let Some(extra) = extra {
            command.arg(extra);
        }
        let output = self.run_control_command(
            command,
            &format!("verifier-{verb}"),
            &verification_request_id(candidate),
            &format!("story helper `{verb}`"),
        )?;
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
            return Ok(HelperAnswer::Ok);
        }
        Ok(HelperAnswer::Refused {
            reason: payload
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            display: payload
                .get("display")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("story helper refused without diagnostics")
                .to_string(),
        })
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
            .envs(self.env.child_vars())
            .env(CLEANUP_LEASE_ENV, encoded)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null());
        let output = self.run_control_command(
            command,
            "verifier-reap",
            &verification_request_id(candidate),
            "leased story helper `reap`",
        )?;

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
        // The project's own merge gate (SH-649), read from the registered
        // checkout's committed pointer — the same checkout this actuator
        // already trusts for `scripts/verify-pr.sh` itself. A value that
        // cannot be run is local configuration needing a person, so it is
        // refused here, before any journal or process exists, and never
        // handed back to the implementor as a red.
        let gate = match crate::service::gate_command::gate_command_for(&candidate.checkout) {
            Ok(gate) => gate,
            Err(error) => {
                return VerificationOutcome::InfrastructureFailure {
                    detail: format!(
                        "the merge gate for registered checkout `{}` cannot be run: {error}",
                        candidate.checkout.display()
                    ),
                    disposition: VerificationFailureDisposition::Permanent,
                };
            }
        };
        let journal = journal_path(&self.env, candidate);
        // Supervision now depends on the journal (SH-592). Refuse before
        // starting work if it cannot be prepared, rather than silently
        // reverting to the aggregate deadline that killed healthy gates.
        if let Some(parent) = journal.parent()
            && let Err(error) = std::fs::create_dir_all(parent)
        {
            return VerificationOutcome::InfrastructureFailure {
                detail: format!(
                    "could not prepare progress journal {}: {error}",
                    journal.display()
                ),
                disposition: VerificationFailureDisposition::Permanent,
            };
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
            return VerificationOutcome::InfrastructureFailure {
                detail: format!(
                    "could not initialize progress journal {}: {error}",
                    journal.display()
                ),
                disposition: VerificationFailureDisposition::Permanent,
            };
        }
        let mut command = Command::new("bash");
        apply_verification_allowlist(&mut command);
        command
            .arg("scripts/verify-pr.sh")
            .arg(&pull_request.url)
            .arg("--")
            .args(gate.argv())
            .current_dir(&candidate.checkout)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GH_PROMPT_DISABLED", "1")
            .env(
                "STORYHOOK_ACTIVITY_CONTEXT",
                format!("project={} {}", candidate.project_slug, candidate.story_id),
            )
            .env("STORYHOOK_GATE_PROGRESS", &journal);
        let request_id = verification_request_id(candidate);
        let captured = match run_captured_with_progress_and_registration(
            command,
            self.verification_idle_timeout,
            TerminationPolicy::TerminateThenKill {
                grace: self.termination_grace,
            },
            &journal,
            |pid| {
                self.owned_processes
                    .register("verifier", pid, Some(&request_id))
                    .map_err(|error| error.to_string())
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
            Err(CaptureError::Track(error)) => {
                return VerificationOutcome::InfrastructureFailure {
                    detail: format!("could not track scripts/verify-pr.sh: {error}"),
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
                        "scripts/verify-pr.sh made no progress for {:?}; {termination}",
                        self.verification_idle_timeout
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
        parsed.into_outcome(&gate)
    }

    fn notify(
        &self,
        candidate: &VerificationCandidate,
        message: &str,
    ) -> Result<NotifyDelivery, AppError> {
        match self.helper(candidate, "notify", Some(message))? {
            HelperAnswer::Ok => Ok(NotifyDelivery::Delivered),
            HelperAnswer::Refused { reason, display } => match agent_presence(reason.as_deref()) {
                AgentPresence::Absent => Ok(NotifyDelivery::AgentAbsent {
                    reason: reason.unwrap_or_default(),
                    detail: display,
                }),
                AgentPresence::NotAbsent => Err(AppError::Storage(display)),
            },
        }
    }

    fn redispatch(
        &self,
        candidate: &VerificationCandidate,
        plan: &ResumePlan,
    ) -> Result<(), AppError> {
        let script = self.helper_path()?;
        let options = DispatchOptions {
            model: plan.model.clone(),
            effort: plan.effort.clone(),
            fast: plan.fast,
            resume: true,
        };
        // The same argv composer the dashboard and the engine use (SH-136),
        // so a resume typed by the verifier is byte-for-byte the resume a
        // person would have typed — including the target session, without
        // which the helper refuses "story requires tmux" from a daemon.
        let outcome = run_shell_dispatch(
            &script,
            &candidate.project_slug,
            &candidate.story_id,
            plan.agent,
            true,
            plan.full_auto,
            &options,
            &self.env,
        )?;
        match outcome.state {
            DispatchOutcomeState::Ok => Ok(()),
            DispatchOutcomeState::Refused => Err(AppError::Storage(
                outcome
                    .payload
                    .get("display")
                    .or_else(|| outcome.payload.get("reason"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("story helper `dispatch --resume` refused without diagnostics")
                    .to_string(),
            )),
        }
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
    let linked_repo = parse_pr_url(&pull_request.url).ok();
    match (checkout_repo, linked_repo) {
        (Some(checkout_repo), Some(linked_repo))
            if checkout_repo.host.eq_ignore_ascii_case(&linked_repo.host)
                && checkout_repo.owner.eq_ignore_ascii_case(&linked_repo.owner)
                && checkout_repo.repo.eq_ignore_ascii_case(&linked_repo.repo) =>
        {
            None
        }
        (Some(checkout_repo), Some(linked_repo)) => Some(format!(
            "linked pull request {} belongs to {}/{}/{}, but registered checkout `{}` has origin {}/{}/{}; centralized landing is origin-bound",
            pull_request.url,
            linked_repo.host,
            linked_repo.owner,
            linked_repo.repo,
            checkout.display(),
            checkout_repo.host,
            checkout_repo.owner,
            checkout_repo.repo
        )),
        (Some(_), None) => Some(format!(
            "linked pull request URL {} is not a valid GitHub pull request URL; centralized landing requires its host and repository identity",
            pull_request.url
        )),
        (None, _) => Some(format!(
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

impl WireOutcome {
    /// The daemon-side outcome, carrying the gate this run was given. The
    /// wire shape does not repeat the command: the parsed pointer is its one
    /// source, and the script only ever ran what it was handed.
    fn into_outcome(self, gate: &crate::service::gate_command::GateCommand) -> VerificationOutcome {
        match self {
            WireOutcome::Merged { tree, detail } => VerificationOutcome::Merged {
                tree,
                detail,
                gate: gate.display(),
            },
            WireOutcome::Conflict { detail } => VerificationOutcome::Conflict { detail },
            WireOutcome::TestsFailed { tree, log, detail } => VerificationOutcome::TestsFailed {
                tree,
                log,
                detail,
                gate: gate.display(),
            },
            WireOutcome::InfrastructureFailure {
                detail,
                disposition,
            } => VerificationOutcome::InfrastructureFailure {
                detail,
                disposition,
            },
            WireOutcome::InvalidSubmission { detail } => {
                VerificationOutcome::InvalidSubmission { detail }
            }
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
/// This single-attempt test seam supplies no reconciliation waiter, so a
/// conflict returns after notification. Production uses
/// [`tick_with_reconciliation`] and retains both guards.
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
    tick_with_reconciliation(store, env, actuator, activity, inflight, |_| Ok(None))
}

/// Runs one verification cycle while allowing a conflicted story to retain
/// the serialized verifier until its next submission.
///
/// `wait_for_resubmission` owns only the wait mechanism. The verifier validates
/// that the returned candidate is a newer generation of the reserved story,
/// preventing a queue reorder or faulty observer from transferring ownership.
/// Public for store-backed integration tests; production supplies the daemon's
/// change-bus waiter.
pub fn tick_with_reconciliation<S, A, W>(
    store: &S,
    env: &Environment,
    actuator: &A,
    activity: &VerificationActivity,
    inflight: &InFlight,
    mut wait_for_resubmission: W,
) -> Result<TickResult, AppError>
where
    S: Store,
    A: VerificationActuator,
    W: FnMut(&VerificationCandidate) -> Result<Option<VerificationCandidate>, AppError>,
{
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
    let Some(mut candidate) = incident_candidate.or_else(|| ordered.first().cloned()) else {
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
    let started_at = env.now();
    let lifecycle_entry = inflight.enter();
    name_verification(&lifecycle_entry, &candidate, &started_at);
    let mut active = activity.acquire(&candidate, started_at);

    loop {
        match refresh_authority(&queue, &mut active, &lifecycle_entry, env, &mut candidate)? {
            AuthorityRefresh::Current => {}
            AuthorityRefresh::Replaced => continue,
            AuthorityRefresh::Released => return Ok(TickResult::Returned),
        }
        let ctx = Ctx::new(
            store,
            candidate.project,
            candidate.checkout.clone(),
            env.clone(),
        )
        .no_hooks(true);
        let pull_request = match &candidate.pull_request {
            Ok(pull_request) => pull_request.clone(),
            Err(problem) => {
                match return_for_repair(&queue, &ctx, actuator, &candidate, &problem.message())? {
                    GenerationWrite::Applied(_) => return Ok(TickResult::Returned),
                    GenerationWrite::Superseded => match refresh_authority(
                        &queue,
                        &mut active,
                        &lifecycle_entry,
                        env,
                        &mut candidate,
                    )? {
                        AuthorityRefresh::Current | AuthorityRefresh::Replaced => continue,
                        AuthorityRefresh::Released => return Ok(TickResult::Returned),
                    },
                }
            }
        };
        let activity_context = format!("project={} {}", candidate.project_slug, candidate.story_id);
        super::activity::emit(
            "INFO",
            "verifier",
            "event",
            &activity_context,
            "verification started",
        );
        let outcome = actuator.verify(&candidate, &pull_request);
        super::activity::emit(
            if matches!(outcome, VerificationOutcome::Merged { .. }) {
                "INFO"
            } else {
                "ERROR"
            },
            "verifier",
            "event",
            &activity_context,
            &format!("verification outcome: {outcome:?}"),
        );

        match refresh_authority(&queue, &mut active, &lifecycle_entry, env, &mut candidate)? {
            AuthorityRefresh::Current => {}
            AuthorityRefresh::Replaced => continue,
            AuthorityRefresh::Released => return Ok(TickResult::Returned),
        }

        match outcome {
            VerificationOutcome::Merged { tree, detail, gate } => {
                let green_comment = format!(
                    "{VERIFICATION_GREEN_PREFIX} merge tree `{tree}` passed `{gate}` and pull request {} landed. {detail}",
                    pull_request.url
                );
                if matches!(
                    queue.record_generation_merged(
                        &ctx,
                        &candidate,
                        &pull_request.url,
                        &green_comment,
                    )?,
                    GenerationWrite::Superseded
                ) {
                    match refresh_authority(
                        &queue,
                        &mut active,
                        &lifecycle_entry,
                        env,
                        &mut candidate,
                    )? {
                        AuthorityRefresh::Current | AuthorityRefresh::Replaced => continue,
                        AuthorityRefresh::Released => return Ok(TickResult::Returned),
                    }
                }
                // Reaping is cleanup for work whose durable outcome is already
                // recorded; it must not keep graceful shutdown waiting on the
                // completed verification transaction.
                drop(active);
                drop(lifecycle_entry);
                match actuator.reap(&candidate) {
                    Ok(()) => record_cleanup_complete(&ctx, &candidate)?,
                    Err(error) => record_cleanup_required(&ctx, &candidate, &error)?,
                }
                return Ok(TickResult::Completed);
            }
            VerificationOutcome::Conflict { detail } => {
                let remediation_started = return_for_repair(
                    &queue,
                    &ctx,
                    actuator,
                    &candidate,
                    &format!(
                        "CENTRAL VERIFICATION CONFLICT — the submitted PR no longer merges into its current base branch. Reconcile the existing PR without rewriting published history, run new and impacted tests, push, then move {} back to verifying.\n\n{detail}",
                        candidate.story_id
                    ),
                )?;
                let remediation_started = match remediation_started {
                    GenerationWrite::Applied(started) => started,
                    GenerationWrite::Superseded => match refresh_authority(
                        &queue,
                        &mut active,
                        &lifecycle_entry,
                        env,
                        &mut candidate,
                    )? {
                        AuthorityRefresh::Current | AuthorityRefresh::Replaced => continue,
                        AuthorityRefresh::Released => return Ok(TickResult::Returned),
                    },
                };
                if !remediation_started {
                    return Ok(TickResult::Returned);
                }
                let Some(resubmitted) = wait_for_resubmission(&candidate)? else {
                    return Ok(TickResult::Returned);
                };
                if resubmitted.project != candidate.project
                    || resubmitted.story_id != candidate.story_id
                    || resubmitted.verifying_generation.is_none()
                    || resubmitted.verifying_generation == candidate.verifying_generation
                {
                    return Err(AppError::Storage(format!(
                        "reconciliation reservation for {} received the wrong verification candidate",
                        candidate.story_id
                    )));
                }
                transfer_verifier(&mut active, &lifecycle_entry, env, &resubmitted);
                candidate = resubmitted;
                continue;
            }
            VerificationOutcome::InvalidSubmission { detail } => {
                let result = return_for_repair(
                    &queue,
                    &ctx,
                    actuator,
                    &candidate,
                    &format!(
                        "CENTRAL VERIFICATION INVALID SUBMISSION — {detail}. Link the PR for this checkout's origin, push it, then move {} back to verifying.",
                        candidate.story_id
                    ),
                )?;
                if matches!(result, GenerationWrite::Applied(_)) {
                    return Ok(TickResult::Returned);
                }
            }
            VerificationOutcome::TestsFailed {
                tree,
                log,
                detail,
                gate,
            } => {
                let result = return_for_repair(
                    &queue,
                    &ctx,
                    actuator,
                    &candidate,
                    &format!(
                        "CENTRAL VERIFICATION RED — merge tree `{tree}` failed `{gate}`. Full log: `{log}`. Fix the existing PR, run new and impacted tests, push, then move {} back to verifying.\n\n{detail}",
                        candidate.story_id
                    ),
                )?;
                if matches!(result, GenerationWrite::Applied(_)) {
                    return Ok(TickResult::Returned);
                }
            }
            VerificationOutcome::InfrastructureFailure {
                detail,
                disposition,
            } => {
                match record_infrastructure_failure(&queue, &ctx, &candidate, disposition, &detail)?
                {
                    GenerationWrite::Applied(result) => return Ok(result),
                    GenerationWrite::Superseded => {}
                }
            }
        }

        match refresh_authority(&queue, &mut active, &lifecycle_entry, env, &mut candidate)? {
            AuthorityRefresh::Current | AuthorityRefresh::Replaced => continue,
            AuthorityRefresh::Released => return Ok(TickResult::Returned),
        }
    }
}

fn name_verification(
    lifecycle_entry: &crate::daemon::lifecycle::Entry<'_>,
    candidate: &VerificationCandidate,
    started_at: &str,
) {
    lifecycle_entry.name(CurrentRequest {
        request_id: verification_request_id(candidate),
        command: "verify".to_string(),
        project: Some(candidate.project_slug.clone()),
        pid: std::process::id(),
        started_at: started_at.to_string(),
        served_deadline_secs: VERIFICATION_IDLE_TIMEOUT.as_secs(),
        cwd: candidate.checkout.clone(),
    });
}

fn verification_request_id(candidate: &VerificationCandidate) -> String {
    let generation = candidate.verifying_generation.map_or_else(
        || "legacy".to_string(),
        |generation| generation.get().to_string(),
    );
    format!(
        "verify:{}:{}:{generation}",
        candidate.project_slug, candidate.story_id
    )
}

fn incident_matches(incident: &VerificationIncident, candidate: &VerificationCandidate) -> bool {
    candidate.project == incident.project
        && candidate.verifying_generation == Some(incident.generation)
}

enum CandidateAuthority {
    Current,
    Superseded(Option<Box<VerificationCandidate>>),
}

enum AuthorityRefresh {
    Current,
    Replaced,
    Released,
}

fn candidate_authority(
    queue: &VerificationQueue<'_, impl Store>,
    candidate: &VerificationCandidate,
) -> Result<CandidateAuthority, AppError> {
    let current = queue.current_for(candidate)?;
    if current
        .as_ref()
        .is_some_and(|current| current.verifying_generation == candidate.verifying_generation)
    {
        Ok(CandidateAuthority::Current)
    } else {
        Ok(CandidateAuthority::Superseded(current.map(Box::new)))
    }
}

fn refresh_authority<S: Store>(
    queue: &VerificationQueue<'_, S>,
    active: &mut VerificationGuard,
    lifecycle_entry: &crate::daemon::lifecycle::Entry<'_>,
    env: &Environment,
    candidate: &mut VerificationCandidate,
) -> Result<AuthorityRefresh, AppError> {
    match candidate_authority(queue, candidate)? {
        CandidateAuthority::Current => Ok(AuthorityRefresh::Current),
        CandidateAuthority::Superseded(Some(resubmitted)) => {
            super::activity::emit(
                "INFO",
                "verifier",
                "event",
                &format!("project={} {}", candidate.project_slug, candidate.story_id),
                &format!(
                    "verification generation {:?} is superseded and has no outcome authority; transferring to {:?}",
                    candidate.verifying_generation, resubmitted.verifying_generation
                ),
            );
            transfer_verifier(active, lifecycle_entry, env, &resubmitted);
            *candidate = *resubmitted;
            Ok(AuthorityRefresh::Replaced)
        }
        CandidateAuthority::Superseded(None) => Ok(AuthorityRefresh::Released),
    }
}

fn transfer_verifier(
    active: &mut VerificationGuard,
    lifecycle_entry: &crate::daemon::lifecycle::Entry<'_>,
    env: &Environment,
    resubmitted: &VerificationCandidate,
) {
    let resumed_at = env.now();
    active.replace(resubmitted, resumed_at.clone());
    name_verification(lifecycle_entry, resubmitted, &resumed_at);
}

fn record_infrastructure_failure<S: Store>(
    queue: &VerificationQueue<'_, S>,
    ctx: &Ctx<'_, S>,
    candidate: &VerificationCandidate,
    disposition: VerificationFailureDisposition,
    detail: &str,
) -> Result<GenerationWrite<TickResult>, AppError> {
    let incident = match queue.record_generation_incident(
        ctx,
        candidate,
        disposition,
        detail,
        INFRASTRUCTURE_RETRY_ATTEMPTS,
    )? {
        GenerationWrite::Applied(incident) => incident,
        GenerationWrite::Superseded => return Ok(GenerationWrite::Superseded),
    };
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
        Ok(GenerationWrite::Applied(TickResult::Halted))
    } else {
        Ok(GenerationWrite::Applied(TickResult::RetryLater))
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

/// Hands a returned story back to its agent: the transition and the
/// diagnosis comment, then delivery into the dispatched pane.
///
/// `Applied(true)` means remediation is under way in the story's own window —
/// the paste landed, or the agent was absent and a resume re-dispatch of the
/// same story into the same window and worktree succeeded (SH-650, decision
/// D-E of `docs/spec/verification-workflow.md`). `Applied(false)` means the
/// story is parked with `awaiting`: only when the re-dispatch itself was
/// refused, or the refusal was not evidence of absence. The Conflict arm holds
/// the queue on `true` and releases it on `false`, so the hold applies whether
/// or not the FIRST paste landed, and never waits for a resubmission nobody
/// will make.
fn return_for_repair<S: Store, A: VerificationActuator>(
    queue: &VerificationQueue<'_, S>,
    ctx: &Ctx<'_, S>,
    actuator: &A,
    candidate: &VerificationCandidate,
    diagnosis: &str,
) -> Result<GenerationWrite<bool>, AppError> {
    if matches!(
        queue.record_generation_returned(ctx, candidate, diagnosis)?,
        GenerationWrite::Superseded
    ) {
        return Ok(GenerationWrite::Superseded);
    }
    let activity_context = format!("project={} {}", candidate.project_slug, candidate.story_id);
    let absent = match actuator.notify(candidate, diagnosis) {
        Ok(NotifyDelivery::Delivered) => return Ok(GenerationWrite::Applied(true)),
        Ok(NotifyDelivery::AgentAbsent { reason, detail }) => format!("{detail} ({reason})"),
        Err(error) => {
            let reason = format!("verification remediation could not reach its agent: {error}");
            return park(queue, ctx, candidate, &reason);
        }
    };
    // The trail is written BEFORE the respawn so a daemon that dies inside it
    // leaves a story that says what was attempted, not one that merely sits
    // in-progress with a dead pane (SH-306).
    comment_once(
        ctx,
        candidate,
        &format!(
            "{VERIFICATION_RESUME_PREFIX} {absent}. Re-dispatching {} into its own window and worktree with the resume clause.",
            candidate.story_id
        ),
    )?;
    let plan = resume_plan(ctx.store(), candidate)?;
    super::activity::emit(
        "INFO",
        "verifier",
        "event",
        &activity_context,
        &format!("agent absent ({absent}); re-dispatching with {plan:?}"),
    );
    if let Err(error) = actuator.redispatch(candidate, &plan) {
        super::activity::emit(
            "ERROR",
            "verifier",
            "event",
            &activity_context,
            &format!("resume re-dispatch refused: {error}"),
        );
        let reason = format!(
            "verification remediation could not re-dispatch its agent: {error} (after: {absent})"
        );
        return park(queue, ctx, candidate, &reason);
    }
    // The agent is live under the resume charter, which begins by reading this
    // story's comments — where the diagnosis already is. A paste that fails
    // here is recorded, never a hard stop (D-E: `awaiting` only when the
    // re-dispatch itself is refused).
    match actuator.notify(candidate, diagnosis) {
        Ok(NotifyDelivery::Delivered) => {}
        Ok(NotifyDelivery::AgentAbsent { reason, detail }) => comment_once(
            ctx,
            candidate,
            &format!(
                "{VERIFICATION_RESUME_PREFIX} re-dispatched, but the diagnosis could not be pasted afterwards: {detail} ({reason}). It stands as the comment above this one."
            ),
        )?,
        Err(error) => comment_once(
            ctx,
            candidate,
            &format!(
                "{VERIFICATION_RESUME_PREFIX} re-dispatched, but the diagnosis could not be pasted afterwards: {error}. It stands as the comment above this one."
            ),
        )?,
    }
    Ok(GenerationWrite::Applied(true))
}

fn park<S: Store>(
    queue: &VerificationQueue<'_, S>,
    ctx: &Ctx<'_, S>,
    candidate: &VerificationCandidate,
    reason: &str,
) -> Result<GenerationWrite<bool>, AppError> {
    if matches!(
        queue.set_generation_awaiting(ctx, candidate, reason)?,
        GenerationWrite::Superseded
    ) {
        return Ok(GenerationWrite::Superseded);
    }
    Ok(GenerationWrite::Applied(false))
}

/// Derives the resume plan for `candidate` from the store (SH-650).
///
/// A story held by a live lane — `Dispatching` or `Working`, never `Idle` or
/// `Quarantined` — of a live engine run in the candidate's project is
/// re-dispatched as that lane: the run's provider options and `--full-auto`.
/// A quarantined lane is one the engine has already given up on, so passing
/// its identity would be a lie about who observes the window. Everything
/// else (an attended dispatch, a finished run) is an ordinary autonomous
/// resume whose provider the helper reads from the dispatch's own record.
/// Public for store-backed integration tests.
pub fn resume_plan(
    store: &impl Store,
    candidate: &VerificationCandidate,
) -> Result<ResumePlan, AppError> {
    let plan = store.read(|tx| {
        for run in tx.live_engine_runs()? {
            if run.project_slug != candidate.project_slug {
                continue;
            }
            let held = tx.engine_lanes(&run.id)?.into_iter().any(|lane| {
                matches!(
                    lane.state,
                    EngineLaneState::Dispatching | EngineLaneState::Working
                ) && lane.story_id.as_deref() == Some(candidate.story_id.as_str())
            });
            if held {
                return Ok(ResumePlan {
                    agent: Some(run.agent),
                    model: run.model.clone(),
                    effort: run.effort.clone(),
                    fast: run.speed == Some(EngineSpeed::Fast),
                    full_auto: true,
                });
            }
        }
        Ok(ResumePlan::default())
    })?;
    Ok(plan)
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

/// Waits until the reserved story creates a newer verification generation.
///
/// Other queue arrivals and coarse bus wakes only cause a fresh observation;
/// they cannot transfer the reservation. A daemon stop ends the wait without
/// manufacturing a candidate. Public for shutdown and event-order integration
/// tests.
pub fn wait_for_reconciled_candidate(
    store: &impl Store,
    subscription: &crate::daemon::bus::Subscription,
    stop: &AtomicBool,
    reserved: &VerificationCandidate,
) -> Result<Option<VerificationCandidate>, AppError> {
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        if let Some(candidate) =
            VerificationQueue::new(store)
                .ordered()?
                .into_iter()
                .find(|candidate| {
                    candidate.project == reserved.project
                        && candidate.story_id == reserved.story_id
                        && candidate.verifying_generation.is_some()
                        && candidate.verifying_generation != reserved.verifying_generation
                })
        {
            return Ok(Some(candidate));
        }
        let _ = subscription.recv(RECOVERY_WAKE);
    }
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
        match tick_with_reconciliation(store, env, &actuator, activity, inflight, |reserved| {
            wait_for_reconciled_candidate(store, &subscription, stop, reserved)
        }) {
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
