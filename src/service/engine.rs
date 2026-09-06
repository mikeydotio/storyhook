//! The process boundary used by the Full Auto engine.
//!
//! [`Dispatcher`] is deliberately synchronous. The engine decides which
//! thread owns an attempt; this module decides what one attempt means. That
//! keeps the store-pool deadlock rule in the caller while giving reconcile
//! tests a seam that never needs a worktree, tmux server, or agent process.

use std::ffi::OsString;
#[cfg(test)]
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use regex::Regex;

use crate::domain::{
    CLEANUP_LEASE_ENV, CLEANUP_LEASE_VERSION, DISPLAY_PROMOTION_STATE, LABEL_NO_AUTO,
    StoryCleanupLease, SuperState, VERIFYING_STATE_SLUG, is_epic, validate_dispatch_option_token,
};
use crate::env::Environment;
use crate::env::spawn_env::apply_dispatch_allowlist;
use crate::error::AppError;
use crate::event_hooks::HookEventType;
#[cfg(test)]
use crate::process::read_capture;
use crate::process::{CaptureError, Captured, run_captured};
use crate::store::ids::GlobalSeq;
use crate::store::{
    EngineAgent, EngineLaneRecord, EngineLaneState, EngineQuarantineRecord, EngineRunRecord,
    EngineRunState, EngineScope, EngineSpeed, ReadOps, Store, StoreError, WriteOps,
};

use super::{Ctx, QueryService, ReadyQueueFilters, project_prefix, resolve_story};

/// How long the worktree/tmux/agent helper may run.
///
/// This is the dashboard dispatch bound moved to the shared seam: the script's
/// readiness handoff is bounded below this, while a networked `git fetch` is
/// the genuinely variable part.
pub(crate) const DISPATCH_TIMEOUT: Duration = Duration::from_secs(180);

/// How often stop-now re-reads a lane whose dispatch helper still owns the
/// exact cleanup lease it is about to publish.
const DISPATCH_SETTLE_POLL: Duration = Duration::from_millis(20);

/// The design record models lane count as a `u8`. Keep the service boundary
/// faithful even though SQLite stores the value in an INTEGER column.
pub const MAX_ENGINE_LANES: u32 = u8::MAX as u32;

pub const OPERATOR_STOPPED: &str = "operator-stopped";
pub const OPERATOR_STOPPED_NOW: &str = "operator-stopped-now";

/// The run-level stop reason the breaker writes (D10).
pub const BREAKER_TRIPPED: &str = "breaker-tripped";
/// The run-level stop reason a drained queue writes.
pub const QUEUE_DRAINED: &str = "queue-drained";

/// The lane-level outcome recorded when a story reached a CLOSED superstate.
pub const COMPLETED: &str = "completed";

/// Consecutive hard stops that halt a run (D10). A completion zeroes the count.
pub const HARD_STOP_BREAKER: u32 = 3;

/// The machine-wide ceiling on lanes filled across every live run (D14).
///
/// **Derived, not picked.** A filled lane is exactly one `story.sh dispatch`
/// subprocess, and this machine already bounds those:
/// [`crate::api::dispatch::MAX_RUNNING`]. Restating that budget as its own
/// literal would be a second opinion about one machine, which this project has
/// paid for repeatedly (SH-136). `engine_lane_budget_matches_dispatch_capacity`
/// fails if the two ever drift.
pub const ENGINE_LANE_BUDGET: usize = crate::api::dispatch::MAX_RUNNING;

/// `make test`'s measured warm median, in seconds, from
/// `docs/rearch/baseline/timings.md`.
///
/// Kept beside the ceiling it feeds so the derivation is legible at the point
/// of use; `stall_ceiling_derives_from_the_measured_suite_median` reads the
/// same figure back out of that document and fails if the measurement moves,
/// in the shape `tests/machine_lock.rs` already uses for `GATE_MEDIAN_SECS`.
pub const GATE_MEDIAN_SECS: u64 = 36;

/// How much slack the ceiling carries over the derived worst case.
///
/// Stated as its own named factor rather than folded into the product, so a
/// reader can see what is measurement and what is judgement (SH-394).
pub const STALL_MARGIN: u64 = 2;

/// How long a lane may show no observable progress before it is a hard stop.
///
/// **Derived from the deadline it disproves, never a bare literal** (SH-394).
/// This ceiling was first derived when a lane's longest *legitimate* silence
/// was its own full `make test` run, queuing on the machine-wide `gate` lock
/// behind up to [`ENGINE_LANE_BUDGET`] other lanes doing the same (SH-457
/// takes that lock inside `scripts/run-tests.sh`).
///
/// **SH-521 moved that run out of the lane.** A lane now runs only its own
/// new and directly impacted tests (D4); the full suite runs once, on one
/// serialized daemon verification worker, for a story that has already
/// reached `verifying` — which [`LaneClassification::Verifying`] holds
/// outside this ceiling entirely, precisely because that queue can
/// legitimately outrun it. What remains behind this ceiling is a lane's own
/// much smaller test leg, so [`GATE_MEDIAN_SECS`] — the full suite's measured
/// warm median — stays a generous, conservative bound on it rather than a
/// tight one: SH-394's rule is to widen a fixture's own cost over tightening
/// an assertion that sits too close to flake, and re-deriving down to a
/// narrower, less-measured figure would be exactly that.
///
/// **If a lane's own test leg is ever folded back into a `make test` run
/// serialized behind other lanes, this ceiling must be re-derived, not
/// merely raised** — that was the original 873s-vs-median distinction this
/// constant was built to make, and SH-521 changed which run is being bounded
/// without changing that underlying rule.
pub const STALL_CEILING_SECS: u64 = ENGINE_LANE_BUDGET as u64 * GATE_MEDIAN_SECS * STALL_MARGIN;

/// How often a live run should be reconciled in the absence of any other wake.
///
/// A quarter of the ceiling, so a stall surfaces well inside it rather than up
/// to a full ceiling late. Derived from [`STALL_CEILING_SECS`]; the timer that
/// *uses* this is `crate::daemon::engine::poll_engine` (SH-466).
pub const RECONCILE_TICK_SECS: u64 = STALL_CEILING_SECS / 4;

// Compile-time, not a test: these are `const`, so a runtime assertion over them
// folds to a constant and proves nothing (clippy says so). Stated here, beside
// the constants, so editing one to an impossible value fails the BUILD rather
// than a suite somebody might not run.
const _: () = assert!(
    STALL_MARGIN >= 1,
    "a margin below 1 puts the stall ceiling under the worst legitimate silence it derives from"
);
const _: () = assert!(
    RECONCILE_TICK_SECS > 0,
    "a tick of zero is a busy loop, which the reconcile design forbids"
);

/// Why a lane stopped in a way that needs a human.
///
/// [`Self::Interrupted`] is declared here but produced only by daemon-start
/// reconciliation (SH-466), so that story adds a *producer* rather than
/// widening a shipped enum every reader already matches on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardStopKind {
    /// The agent blocked the story or set `awaiting` on it.
    AgentBlocked,
    /// The lane's window is gone while its story is still OPEN.
    WindowGone,
    /// Nothing observable changed for longer than [`STALL_CEILING_SECS`].
    Stalled,
    /// The daemon restarted while the lane was mid-story (SH-466).
    Interrupted,
    /// `story next` refused the claim the fill phase attempted (SH-120: the
    /// refusal is relayed verbatim, never composed here).
    DispatchRefused,
}

impl HardStopKind {
    /// The stable machine-readable classification recorded on the lane.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentBlocked => "agent-blocked",
            Self::WindowGone => "window-gone",
            Self::Stalled => "stalled",
            Self::Interrupted => "interrupted",
            Self::DispatchRefused => "dispatch-refused",
        }
    }
}

/// Which reconcile pass produced an observation (SH-466).
///
/// `Steady` is the ordinary tick- or bus-woken pass. `Restart` runs once per
/// live run at daemon start, before any run resumes claiming (D11). The two
/// differ in exactly two places: what a dead window means, and whether the
/// stall clock is trusted across the gap the daemon just crossed — see
/// [`classify`] and [`EngineService::record_progress`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconcilePass {
    /// The ordinary pass: a dead window is [`HardStopKind::WindowGone`], read
    /// as evidence someone watched it close, and the stall clock is read
    /// normally.
    Steady,
    /// The daemon-start pass: a dead window is [`HardStopKind::Interrupted`]
    /// — nobody watched it close, the daemon that would have watched just
    /// restarted — and the stall clock is re-seeded rather than read, because
    /// silence across an outage of unknown length is unobserved, not stalled
    /// (SH-372). Never fills idle lanes, because the dispatch subprocess
    /// calls back into this daemon over `/api/v1/invoke`, which is not yet
    /// answering this early in startup, and never terminates the run: D11
    /// says the run continues with fresh lanes, which happens on the first
    /// steady pass that follows.
    Restart,
}

/// What one reconcile pass decided about one occupied lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaneClassification {
    /// The story is still OPEN and something moved, or the ceiling has not
    /// been reached. Nothing to do but record the observation.
    Progressing,
    /// The story reached the required `verifying` handoff (SH-521): held,
    /// not filled, not quarantined, not counted toward the breaker. The lane
    /// stays occupied because the story still owns a live worktree and
    /// window that only the verifier's own reap reclaims.
    Verifying,
    /// The story left the OPEN superstate. Free the lane, zero the streak.
    Completed,
    /// A hard stop. Quarantine the lane and increment the streak.
    HardStop(HardStopKind),
}

/// One lane's facts as of one pass, gathered before anything is decided.
///
/// Separating observation from classification is what lets the taxonomy be
/// table-tested without a store, a dispatcher, or a clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaneObservation {
    /// Whether the lane's story has left the OPEN superstate.
    pub story_closed: bool,
    /// Whether the lane's story sits in the required `verifying` handoff
    /// (SH-521) — still OPEN, so [`Self::story_closed`] is `false`.
    pub story_verifying: bool,
    /// Whether the agent blocked the story or set `awaiting` on it.
    pub agent_blocked: bool,
    /// Whether the lane's window still runs the agent it was launched with.
    pub window_alive: bool,
    /// The story's current change-feed position, or `None` if it could not be
    /// resolved (a deleted story, say).
    pub head_global_seq: Option<i64>,
    /// The seq recorded the last time this lane was seen to move.
    pub last_progress_seq: Option<i64>,
    /// Seconds since [`Self::last_progress_seq`] last advanced, or `None` when
    /// no progress has been observed yet.
    pub seconds_since_progress: Option<u64>,
    /// The story's own `awaiting` text, when it has one — the reason to
    /// relay verbatim if this lane is quarantined, rather than overwriting it
    /// with a message composed here (SH-120).
    pub awaiting_reason: Option<String>,
}

/// What one reconcile pass did, as data rather than rendered text.
///
/// SH-467's CLI and SH-468's HTTP both render this; neither should have to
/// parse a sentence back apart.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconcileReport {
    /// The run this pass reconciled.
    pub run_id: RunId,
    /// Lane indices freed because their story completed.
    pub completed: Vec<u32>,
    /// Lane indices held this pass because their story reached `verifying`.
    pub verifying: Vec<u32>,
    /// Lane indices quarantined this pass, with why.
    pub quarantined: Vec<(u32, HardStopKind)>,
    /// Lane indices filled this pass, with the story each claimed.
    pub filled: Vec<(u32, String)>,
    /// The run's state after the pass.
    pub run_state: EngineRunState,
    /// The run's stop reason after the pass, when it has one.
    pub stop_reason: Option<String>,
}

/// Decides one lane's fate from what was observed. Pure, so the whole failure
/// taxonomy is table-testable.
///
/// **Order is the design, not an implementation detail.**
///
/// `Completed` is tested first because completion is a *store* fact while a
/// closed window is only evidence about a window (D3, SH-226): an agent that
/// finished its story and let its pane exit must read as `Completed`, never
/// `WindowGone`. Reading the window first would report success as failure and
/// quarantine finished work.
///
/// `AgentBlocked` is tested next, ahead of `Verifying`: a story can sit in
/// `verifying` with `awaiting` also set (centralized verification's own
/// `return_for_repair` falls back to `set_awaiting` when it cannot reach the
/// dispatched pane, SH-521), and that diagnosis must surface rather than be
/// masked by the handoff.
///
/// `Verifying` is tested ahead of `WindowGone` and `Stalled`. The agent's
/// last action for a successful story is `story move <n> verifying`, and the
/// dispatch launch execs the agent process directly rather than typing into a
/// persistent shell (`plugins/story/bin/story.sh`'s own `remain-on-exit`
/// rationale) — so the pane is normally already dead the instant a story
/// reaches this handoff, exactly like an ordinary completion. Reading the
/// window next would report every successful lane as `WindowGone`, and D4's
/// serial, machine-wide verification queue can legitimately outrun any one
/// lane's own [`STALL_CEILING_SECS`] besides, so reading the clock next would
/// eventually report the same success as `Stalled` instead. Neither is a
/// failure; the story is exactly where it is supposed to be.
///
/// `pass` decides the last two rows only (SH-466). Under
/// [`ReconcilePass::Restart`], a dead window is
/// [`HardStopKind::Interrupted`] rather than `WindowGone` — nobody watched it
/// close, the daemon that would have watched just restarted — and the stall
/// check is skipped entirely: a window still alive across a restart proves
/// nothing stalled, since [`EngineService::record_progress`] re-seeds the
/// clock for exactly this pass rather than let it read the outage as
/// silence (SH-372).
#[must_use]
pub fn classify(
    observation: &LaneObservation,
    stall_ceiling_secs: u64,
    pass: ReconcilePass,
) -> LaneClassification {
    if observation.story_closed {
        return LaneClassification::Completed;
    }
    if observation.agent_blocked {
        return LaneClassification::HardStop(HardStopKind::AgentBlocked);
    }
    if observation.story_verifying {
        return LaneClassification::Verifying;
    }
    if !observation.window_alive {
        return LaneClassification::HardStop(match pass {
            ReconcilePass::Steady => HardStopKind::WindowGone,
            ReconcilePass::Restart => HardStopKind::Interrupted,
        });
    }
    if pass == ReconcilePass::Restart {
        return LaneClassification::Progressing;
    }
    // A seq that moved is progress regardless of the clock; only a lane that
    // has BOTH failed to move and outrun the ceiling has stalled. `None` on
    // either side states nothing and is seeded rather than punished (SH-372).
    let unmoved = match (observation.head_global_seq, observation.last_progress_seq) {
        (Some(head), Some(recorded)) => head == recorded,
        _ => false,
    };
    if unmoved
        && observation
            .seconds_since_progress
            .is_some_and(|elapsed| elapsed > stall_ceiling_secs)
    {
        return LaneClassification::HardStop(HardStopKind::Stalled);
    }
    LaneClassification::Progressing
}

/// A tmux client normally answers in milliseconds. The shared machine-probe
/// budget bounds a wedged server without inventing another patience value.
const TMUX_TIMEOUT: Duration = crate::daemon::tailnet::TAILNET_PROBE_TIMEOUT;

const PROMPT_OVERRIDE_ENV_VARS: [&str; 4] = [
    "STORY_PROMPT",
    "STORY_AUTO_PROMPT",
    "STORY_AUTO_PROMPT_SOLO",
    "STORY_PROMPT_EXTRA",
];

const TEMPLATE_PLACEHOLDERS: [&str; 5] = ["<name>", "<dir>", "<reap>", "<n>", "<done-state>"];
pub(crate) const CHARTER_INERT_BANNED: [char; 8] = ['`', '$', ';', '&', '|', '<', '>', '!'];

/// Optional refinements for one dispatch (SH-517, SH-523).
///
/// Each field is already validated by its caller before it reaches here —
/// the charset-gated `OptionToken` at the HTTP boundary
/// (`crate::api::dispatch`), or `story.sh`'s own catalog check for a
/// CLI-driven call. This type carries plain, already-safe strings purely to
/// keep [`run_shell_dispatch`]'s parameter list from growing without bound
/// as SH-517 adds more of them; it is not itself a validation boundary.
///
/// `Default` is "no selection" — [`run_shell_dispatch`] then appends none of
/// `--model`/`--effort`/`--speed` to the helper's argv, preserving the legacy
/// argv byte for byte. Full Auto copies its immutable run configuration here;
/// SH-466 still keeps interrupted-lane recovery separate from attended resume.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DispatchOptions {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub fast: bool,
    pub resume: bool,
}

/// Everything the shell actuator needs to dispatch one already-selected story.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchRequest {
    pub project: String,
    pub story: String,
    pub agent: EngineAgent,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub speed: Option<EngineSpeed>,
}

/// Everything the shell helper needs to release one engine-owned claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnclaimRequest {
    pub project: String,
    pub story: String,
    /// Creation-time identity authorizing exact non-verification cleanup.
    pub cleanup_lease: StoryCleanupLease,
}

/// A parsed answer from `story.sh`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchOutcomeState {
    /// The helper answered `"ok": true`.
    Ok,
    /// The helper returned a well-formed refusal payload.
    Refused,
}

/// The helper's own answer, classified without replacing any of its fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchOutcome {
    pub state: DispatchOutcomeState,
    pub payload: serde_json::Value,
}

impl DispatchOutcome {
    /// Builds an outcome from the helper's complete JSON value.
    #[must_use]
    pub fn from_payload(payload: serde_json::Value) -> Self {
        let state = if payload.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
            DispatchOutcomeState::Ok
        } else {
            DispatchOutcomeState::Refused
        };
        Self { state, payload }
    }
}

/// The testability seam around worktree/tmux/agent side effects.
pub trait Dispatcher: Send + Sync {
    fn dispatch(&self, request: DispatchRequest) -> Result<DispatchOutcome, AppError>;
    fn unclaim(&self, request: UnclaimRequest) -> Result<DispatchOutcome, AppError>;
    fn window_alive(&self, window: &str) -> bool;
    fn kill_window(&self, window: &str) -> Result<(), AppError>;
}

/// Refusing dispatcher for lifecycle operations that are store-only.
///
/// Keeping this explicit rather than faking a helper path means an accidental
/// expansion of `pause`, `resume`, status, or graceful stop into an external
/// side effect fails loudly at the exact call site. Immediate stop and the
/// reconciler receive a real [`ShellDispatcher`] or test fake instead.
pub(crate) struct StoreOnlyDispatcher;

impl Dispatcher for StoreOnlyDispatcher {
    fn dispatch(&self, _request: DispatchRequest) -> Result<DispatchOutcome, AppError> {
        Err(store_only_dispatcher_error())
    }

    fn unclaim(&self, _request: UnclaimRequest) -> Result<DispatchOutcome, AppError> {
        Err(store_only_dispatcher_error())
    }

    fn window_alive(&self, _window: &str) -> bool {
        false
    }

    fn kill_window(&self, _window: &str) -> Result<(), AppError> {
        Err(store_only_dispatcher_error())
    }
}

fn store_only_dispatcher_error() -> AppError {
    AppError::Storage(
        "internal: a store-only engine control attempted an external dispatch operation"
            .to_string(),
    )
}

/// Production dispatcher backed by Storyhook's existing shell helper and tmux.
pub struct ShellDispatcher {
    story_sh_path: PathBuf,
    env: Environment,
    tmux_program: OsString,
}

impl ShellDispatcher {
    #[must_use]
    pub fn new(story_sh_path: impl Into<PathBuf>, env: Environment) -> Self {
        Self {
            story_sh_path: story_sh_path.into(),
            env,
            tmux_program: OsString::from("tmux"),
        }
    }

    fn tmux(&self) -> Command {
        let mut command = Command::new(&self.tmux_program);
        // Monitoring and stop must address the same server as dispatch,
        // independent of the terminal that originally started the daemon.
        apply_dispatch_allowlist(&mut command);
        command
    }
}

impl Dispatcher for ShellDispatcher {
    fn dispatch(&self, request: DispatchRequest) -> Result<DispatchOutcome, AppError> {
        let options = DispatchOptions {
            model: request.model,
            effort: request.effort,
            fast: request.speed == Some(EngineSpeed::Fast),
            resume: false,
        };
        let outcome = run_shell_dispatch(
            &self.story_sh_path,
            &request.project,
            &request.story,
            request.agent,
            true,
            true,
            &options,
            &self.env,
        )?;
        if outcome.state == DispatchOutcomeState::Ok {
            cleanup_lease_from_payload(&outcome.payload, &request.project, &request.story)?;
        }
        Ok(outcome)
    }

    fn unclaim(&self, request: UnclaimRequest) -> Result<DispatchOutcome, AppError> {
        run_shell_unclaim(
            &self.story_sh_path,
            &request.project,
            &request.story,
            &request.cleanup_lease,
            &self.env,
        )
    }

    fn window_alive(&self, window: &str) -> bool {
        let mut command = self.tmux();
        command.args([
            "display-message",
            "-p",
            "-t",
            window,
            "#{pane_pid}\t#{pane_current_command}\t#{pane_dead}",
        ]);
        let captured = match run_captured(command, TMUX_TIMEOUT) {
            Ok(captured) if captured.status.success() => captured,
            _ => return false,
        };
        let answer = String::from_utf8_lossy(&captured.stdout);
        let mut fields = answer.trim_end().splitn(3, '\t');
        let Some(pid) = fields.next().and_then(|raw| raw.parse::<i32>().ok()) else {
            return false;
        };
        let Some(command) = fields.next() else {
            return false;
        };
        if fields.next() != Some("0") || !pid_is_live(pid) {
            return false;
        }
        ProcessIdentity::from_process().matches(command)
    }

    fn kill_window(&self, window: &str) -> Result<(), AppError> {
        let mut command = self.tmux();
        command.args(["kill-window", "-t", window]);
        match run_captured(command, TMUX_TIMEOUT) {
            Ok(captured) if captured.status.success() => Ok(()),
            Ok(captured) => {
                let detail = String::from_utf8_lossy(&captured.stderr).trim().to_string();
                let suffix = if detail.is_empty() {
                    String::new()
                } else {
                    format!(": {detail}")
                };
                Err(AppError::Storage(format!(
                    "tmux refused to kill window `{window}`{suffix}"
                )))
            }
            Err(CaptureError::Timeout(_)) => Err(AppError::Storage(format!(
                "tmux did not answer while killing window `{window}` within {}s",
                TMUX_TIMEOUT.as_secs()
            ))),
            Err(error) => Err(AppError::Storage(format!(
                "could not kill tmux window `{window}`: {}",
                error.detail()
            ))),
        }
    }
}

/// Stable identity for one engine run.
pub type RunId = String;

/// The caller-selected shape of a new run. Project identity comes from the
/// service context, so a request cannot name one project while writing another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartRequest {
    pub scope: EngineScope,
    pub lanes: u32,
    pub agent: EngineAgent,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub speed: Option<EngineSpeed>,
}

/// One transactionally consistent run and its ordered lanes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedNoAutoStory {
    pub id: String,
    pub title: String,
}

/// One transactionally consistent run, its ordered lanes, and work the run is
/// deliberately leaving for a person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunView {
    pub run: EngineRunRecord,
    pub lanes: Vec<EngineLaneRecord>,
    pub skipped_no_auto: Vec<SkippedNoAutoStory>,
}

/// The durable lifecycle of Full Auto runs, excluding reconciliation.
pub struct EngineService<'ctx, S: Store, D: Dispatcher> {
    ctx: &'ctx Ctx<'ctx, S>,
    dispatcher: &'ctx D,
}

impl<'ctx, S: Store, D: Dispatcher> EngineService<'ctx, S, D> {
    #[must_use]
    pub fn new(ctx: &'ctx Ctx<'ctx, S>, dispatcher: &'ctx D) -> Self {
        Self { ctx, dispatcher }
    }

    /// Starts one run and all of its idle lanes in a single transaction.
    pub fn start(&self, request: StartRequest) -> Result<EngineRunRecord, AppError> {
        if !(1..=MAX_ENGINE_LANES).contains(&request.lanes) {
            return Err(AppError::Validation(format!(
                "an engine run needs between 1 and {MAX_ENGINE_LANES} lanes"
            )));
        }
        for (name, value) in [("model", &request.model), ("effort", &request.effort)] {
            if let Some(value) = value {
                validate_dispatch_option_token(value).map_err(|reason| {
                    AppError::Validation(format!("invalid engine {name} `{value}`: {reason}"))
                })?;
            }
        }

        let project = self.ctx.project();
        let now = self.ctx.now();
        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let result = self.ctx.store().write(|tx| {
            let project_record = tx
                .project(project)?
                .ok_or_else(|| StoreError::NotFound(format!("project {project} does not exist")))?;
            if tx.checkout_path(project)?.is_none() {
                return Err(StoreError::from(AppError::Validation(no_checkout_refusal(
                    &project_record.slug,
                ))));
            }

            if let EngineScope::Epic(id) = &request.scope {
                let prefix = project_prefix(&*tx, project)?;
                let (_, row) =
                    resolve_story(&*tx, project, &prefix, id).map_err(StoreError::from)?;
                if !is_epic(&row.snapshot) {
                    return Err(StoreError::from(AppError::Validation(format!(
                        "story `{id}` is not an epic, so it cannot scope an engine run"
                    ))));
                }
            }

            let run = EngineRunRecord {
                id: run_id.clone(),
                project_slug: project_record.slug,
                scope: request.scope.clone(),
                lanes: request.lanes,
                agent: request.agent,
                model: request.model.clone(),
                effort: request.effort.clone(),
                speed: request.speed,
                state: EngineRunState::Running,
                consecutive_hard_stops: 0,
                recent_quarantines: Vec::new(),
                stop_reason: None,
                acknowledged_at: None,
                created_at: now.clone(),
                updated_at: now.clone(),
            };
            tx.create_engine_run(&run)?;
            for lane_index in 0..request.lanes {
                tx.put_engine_lane(&idle_lane(&run.id, lane_index, &now))?;
            }
            Ok(run)
        });

        match result {
            Ok(run) => {
                // After the commit, per every other hook-firing call site's
                // rule: the hook may itself call back into `story`, and firing
                // inside the write above would wait on a lock this call
                // already holds.
                self.ctx.fire_hook(
                    HookEventType::EngineRunStarted,
                    &serde_json::json!({
                        "event_type": "engine_run_started",
                        "run_id": &run.id,
                        "scope": run.scope.kind(),
                        "epic_id": run.scope.story_id(),
                        "lanes": run.lanes,
                        "agent": run.agent.as_str(),
                        "timestamp": self.ctx.now(),
                    }),
                );
                Ok(run)
            }
            Err(StoreError::Invariant(detail)) if is_live_run_collision(&detail) => {
                let slug = self.project_slug()?;
                Err(AppError::Validation(format!(
                    "project `{slug}` already has a live engine run"
                )))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Reads all project runs, or exactly one named run, with ordered lanes.
    pub fn status(&self, run_id: Option<&RunId>) -> Result<Vec<RunView>, AppError> {
        let project = self.ctx.project();
        let now = self.ctx.now();
        Ok(self.ctx.store().read(|tx| {
            let slug = tx
                .project(project)?
                .ok_or_else(|| StoreError::NotFound(format!("project {project} does not exist")))?
                .slug;
            let runs = match run_id {
                Some(id) => vec![run_for_project(tx, &slug, id)?],
                None => tx.engine_runs(&slug)?,
            };
            runs.into_iter()
                .map(|run| {
                    let lanes = tx.engine_lanes(&run.id)?;
                    let skipped_no_auto = if run.state.is_live() {
                        needs_human_stories(tx, project, &now, &run.scope)?
                    } else {
                        Vec::new()
                    };
                    Ok(RunView {
                        run,
                        lanes,
                        skipped_no_auto,
                    })
                })
                .collect()
        })?)
    }

    /// Resolves an optional CLI/HTTP run selector without guessing among
    /// historical runs. The schema permits at most one live run per project;
    /// spelling the multi-live case anyway makes corruption fail closed in
    /// every Store implementation, not only SQLite.
    pub fn resolve_run_id(&self, requested: Option<&RunId>) -> Result<RunId, AppError> {
        if let Some(run_id) = requested {
            return Ok(run_id.clone());
        }
        let project = self.ctx.project();
        let (slug, live) = self.ctx.store().read(|tx| {
            let slug = project_slug(tx, project)?;
            let live = tx
                .engine_runs(&slug)?
                .into_iter()
                .filter(|run| run.state.is_live())
                .map(|run| run.id)
                .collect::<Vec<_>>();
            Ok((slug, live))
        })?;
        match live.as_slice() {
            [run_id] => Ok(run_id.clone()),
            [] => Err(AppError::Validation(format!(
                "project `{slug}` has no live engine run; pass `--run <id>` to name a halted or finished run"
            ))),
            many => Err(AppError::Validation(format!(
                "project `{slug}` has {} live engine runs, so none can be inferred; pass `--run <id>` to name one",
                many.len()
            ))),
        }
    }

    pub fn pause(&self, run_id: &RunId) -> Result<RunView, AppError> {
        self.transition(run_id, |run, _lanes| {
            require_state(run, "pause", &[EngineRunState::Running])?;
            run.state = EngineRunState::Paused;
            Ok(())
        })
    }

    pub fn resume(&self, run_id: &RunId) -> Result<RunView, AppError> {
        self.transition(run_id, |run, _lanes| {
            require_state(run, "resume", &[EngineRunState::Paused])?;
            run.state = EngineRunState::Running;
            Ok(())
        })
    }

    /// Stops a run gracefully or releases every occupied lane immediately.
    pub fn stop(&self, run_id: &RunId, now: bool) -> Result<RunView, AppError> {
        if now {
            self.stop_now(run_id)
        } else {
            self.transition(run_id, |run, lanes| {
                require_state(
                    run,
                    "stop",
                    &[EngineRunState::Running, EngineRunState::Paused],
                )?;
                run.state = if lanes.iter().all(|lane| lane.state == EngineLaneState::Idle) {
                    EngineRunState::Finished
                } else {
                    EngineRunState::Draining
                };
                run.stop_reason = Some(OPERATOR_STOPPED.to_string());
                run.acknowledged_at = None;
                Ok(())
            })
        }
    }

    /// Clears the persistent notification for a run with a recorded reason.
    pub fn acknowledge(&self, run_id: &RunId) -> Result<RunView, AppError> {
        self.set_acknowledged(run_id)
    }

    /// The ordinary, steady-state reconcile pass over one run — see
    /// [`ReconcilePass::Steady`].
    ///
    /// Woken by a project-change bus event, the coarse liveness tick derived
    /// from [`STALL_CEILING_SECS`], or a control command
    /// (`crate::daemon::engine::poll_engine`, SH-466).
    pub fn reconcile(&self, run_id: &RunId) -> Result<ReconcileReport, AppError> {
        self.reconcile_pass(run_id, ReconcilePass::Steady)
    }

    /// The daemon-start reconcile pass (D11, SH-466) — see
    /// [`ReconcilePass::Restart`].
    ///
    /// Run once per live run before any run resumes claiming;
    /// `crate::daemon::engine::poll_engine` is the only caller.
    pub fn reconcile_after_restart(&self, run_id: &RunId) -> Result<ReconcileReport, AppError> {
        self.reconcile_pass(run_id, ReconcilePass::Restart)
    }

    /// One reconcile pass over one run: observe, classify, quarantine, break,
    /// and — [`ReconcilePass::Steady`] only — fill and terminate.
    ///
    /// # Why the phases are separate transactions
    ///
    /// A dispatcher call must never happen inside a store write. `story.sh`
    /// makes its own `story` calls back into this daemon over
    /// `/api/v1/invoke`, so holding a write transaction across one risks the
    /// deadlock `docs/spec/dashboard-dispatch.md` documents. Each phase opens
    /// its own short write and the subprocess work happens between them — the
    /// shape [`Self::stop_now`] already uses.
    fn reconcile_pass(
        &self,
        run_id: &RunId,
        pass: ReconcilePass,
    ) -> Result<ReconcileReport, AppError> {
        let slug = self.project_slug()?;
        let mut report = ReconcileReport {
            run_id: run_id.clone(),
            completed: Vec::new(),
            verifying: Vec::new(),
            quarantined: Vec::new(),
            filled: Vec::new(),
            run_state: EngineRunState::Running,
            stop_reason: None,
        };

        // ---- observe + classify -------------------------------------------
        let observed = self.observe_lanes(&slug, run_id, pass)?;

        // ---- apply: free completions, hold handoffs, quarantine hard stops
        let mut hard_stops = Vec::new();
        for (lane, classification, observation) in observed {
            match classification {
                LaneClassification::Progressing => {
                    self.record_progress(&lane, observation.head_global_seq, pass)?;
                }
                LaneClassification::Verifying => {
                    // Held, not freed: the story still owns a live worktree
                    // and window that only the verifier's own reap reclaims
                    // (SH-521). The story DID move to reach this handoff, so
                    // the stall clock restarts the same way a Progressing
                    // lane's does.
                    report.verifying.push(lane.lane_index);
                    self.record_progress(&lane, observation.head_global_seq, pass)?;
                }
                LaneClassification::Completed => {
                    report.completed.push(lane.lane_index);
                    self.free_completed_lane(&lane)?;
                }
                LaneClassification::HardStop(kind) => {
                    report.quarantined.push((lane.lane_index, kind));
                    hard_stops.push(self.quarantine_lane(
                        run_id,
                        &lane,
                        kind,
                        observation.awaiting_reason.as_deref(),
                    )?);
                }
            }
        }

        // ---- breaker ------------------------------------------------------
        // A completion zeroes the streak; each hard stop increments it.
        // Applied AFTER every lane is classified so one pass that both
        // completes and fails is scored once, in a defined order, rather than
        // depending on lane index.
        let view = self.apply_breaker(run_id, report.completed.len(), &hard_stops)?;
        report.run_state = view.run.state;
        report.stop_reason = view.run.stop_reason.clone();
        if view.run.state != EngineRunState::Running {
            // Halted, paused, draining or finished: no new claims. A draining
            // run whose last lane just freed still needs the terminal check
            // below, so fall through to it rather than returning here.
            if view.run.state == EngineRunState::Draining {
                self.clear_quarantined_lanes(run_id)?;
                let view = self.finish_if_drained(run_id, QUEUE_DRAINED)?;
                report.run_state = view.run.state;
                report.stop_reason = view.run.stop_reason.clone();
            }
            return Ok(report);
        }

        // A restart pass never fills and never terminates (D11): the
        // dispatch subprocess calls back into this daemon over
        // `/api/v1/invoke`, which is not yet answering this early in
        // startup, and "the run continues with fresh lanes" is the *next*
        // (steady) pass's job, not this one's.
        if pass == ReconcilePass::Restart {
            return Ok(report);
        }

        // Below the breaker threshold, the story and run own the durable
        // diagnosis; the lane returns to the dispatch pool (D10).
        self.clear_quarantined_lanes(run_id)?;

        // ---- fill ---------------------------------------------------------
        let dispatch_stops = self.fill_idle_lanes(&slug, run_id, &mut report)?;
        if !dispatch_stops.is_empty() {
            let view = self.apply_breaker(run_id, 0, &dispatch_stops)?;
            report.run_state = view.run.state;
            report.stop_reason = view.run.stop_reason.clone();
            if view.run.state == EngineRunState::Running {
                self.clear_quarantined_lanes(run_id)?;
            }
            // A refused dispatch ended this fill attempt. Even after its lane
            // is released, an empty `filled` list is not evidence that the
            // queue drained; the next wake may claim the next story.
            return Ok(report);
        }

        // ---- terminate ----------------------------------------------------
        // Nothing claimable and every lane idle. Checked after filling, so a
        // run only ends once a claim attempt has actually come back empty.
        if report.filled.is_empty() {
            let view = self.finish_if_drained(run_id, QUEUE_DRAINED)?;
            report.run_state = view.run.state;
            report.stop_reason = view.run.stop_reason.clone();
        }
        Ok(report)
    }

    /// Reads every occupied lane and decides its fate. One read transaction
    /// for the store facts, then the window probes outside it.
    #[allow(clippy::type_complexity)]
    fn observe_lanes(
        &self,
        slug: &str,
        run_id: &RunId,
        pass: ReconcilePass,
    ) -> Result<Vec<(EngineLaneRecord, LaneClassification, LaneObservation)>, AppError> {
        let project = self.ctx.project();
        let now = self.ctx.now();
        let facts = self.ctx.store().read(|tx| {
            let _ = run_for_project(tx, slug, run_id)?;
            let prefix = project_prefix(tx, project)?;
            let mut facts = Vec::new();
            for lane in tx.engine_lanes(run_id)? {
                if lane.state == EngineLaneState::Idle || lane.state == EngineLaneState::Quarantined
                {
                    continue;
                }
                let story = lane
                    .story_id
                    .clone()
                    .expect("a non-idle lane holds a story");
                // A story that will not resolve states nothing about progress:
                // `head_global_seq` stays `None` and `classify` declines to
                // call that a stall (SH-372).
                let row = resolve_story(tx, project, &prefix, &story)
                    .ok()
                    .map(|(_, row)| row);
                facts.push((lane, row));
            }
            Ok(facts)
        })?;

        let mut observed = Vec::with_capacity(facts.len());
        for (lane, row) in facts {
            // The window probe is a subprocess, so it runs outside the read.
            let window_alive = lane
                .pane_id
                .as_deref()
                .filter(|pane| valid_pane_id(pane))
                .map(str::to_string)
                .or_else(|| {
                    lane.window_name
                        .as_deref()
                        .map(|window| exact_window_target(slug, window))
                })
                .is_some_and(|target| self.dispatcher.window_alive(&target));
            let head_global_seq = row.as_ref().map(|row| row.head_global_seq.get());
            let observation = LaneObservation {
                story_closed: row
                    .as_ref()
                    .is_some_and(|row| row.superstate == SuperState::Closed),
                story_verifying: row
                    .as_ref()
                    .is_some_and(|row| row.state == VERIFYING_STATE_SLUG),
                agent_blocked: row.as_ref().is_some_and(|row| {
                    row.awaiting.is_some() || row.state == DISPLAY_PROMOTION_STATE
                }),
                window_alive,
                head_global_seq,
                last_progress_seq: lane.last_progress_seq.map(GlobalSeq::get),
                seconds_since_progress: lane
                    .last_progress_at
                    .as_deref()
                    .and_then(|at| elapsed_secs(at, &now)),
                awaiting_reason: row.as_ref().and_then(|row| row.awaiting.clone()),
            };
            let classification = classify(&observation, STALL_CEILING_SECS, pass);
            observed.push((lane, classification, observation));
        }
        Ok(observed)
    }

    /// Records that a lane's story moved, so the stall clock restarts from the
    /// change rather than from the observation.
    ///
    /// Under [`ReconcilePass::Restart`] the reseed is forced regardless of
    /// whether the seq moved: the daemon has no observation from anywhere
    /// inside the outage it just crossed, so an unmoved seq states nothing
    /// about whether the lane stalled during it (SH-372) — the clock starts
    /// fresh from this pass rather than carrying a pre-outage timestamp that
    /// would make an untouched, perfectly healthy lane read as `Stalled` on
    /// the very next steady pass.
    fn record_progress(
        &self,
        lane: &EngineLaneRecord,
        head_global_seq: Option<i64>,
        pass: ReconcilePass,
    ) -> Result<(), AppError> {
        let observed_at = self.ctx.now();
        let mut updated = lane.clone();
        updated.last_observed_at = observed_at.clone();
        let moved = pass == ReconcilePass::Restart
            || match (head_global_seq, lane.last_progress_seq.map(GlobalSeq::get)) {
                (Some(head), Some(recorded)) => head != recorded,
                (Some(_), None) => true,
                _ => false,
            };
        if moved {
            updated.last_progress_seq = head_global_seq.map(GlobalSeq::new);
            updated.last_progress_at = Some(observed_at);
        }
        if &updated != lane {
            self.ctx.store().write(|tx| tx.put_engine_lane(&updated))?;
        }
        Ok(())
    }

    /// Frees a lane whose story reached a CLOSED superstate.
    fn free_completed_lane(&self, lane: &EngineLaneRecord) -> Result<(), AppError> {
        let observed_at = self.ctx.now();
        let mut idle = idle_lane(&lane.run_id, lane.lane_index, &observed_at);
        idle.outcome = Some(COMPLETED.to_string());
        idle.outcome_detail = lane.story_id.clone();
        self.ctx.store().write(|tx| tx.put_engine_lane(&idle))?;
        Ok(())
    }

    /// Records a hard stop on the story and preserves the lane's evidence.
    ///
    /// The reason is free text on the story's `awaiting`, not a `blocked-by`
    /// edge: SH-398's rule is about blockers that ARE stories, and a dead
    /// window is not one. Worktree, branch, PR and window are all left intact
    /// — the lane keeps its `worktree_path` and `window_name` so a human can
    /// see what the agent left behind (D11).
    ///
    /// `existing_reason` is whatever the story's own `awaiting` already said
    /// at observation time — for [`HardStopKind::AgentBlocked`], the agent's
    /// own diagnosis (its charter tells it to `story block <n> the-reason`
    /// before stopping), or centralized verification's own message when
    /// `return_for_repair` could not reach a dead pane (SH-521). SH-120's
    /// relay rule applies here exactly as it does to a dispatch refusal: the
    /// existing text is appended to, never replaced by, a message composed
    /// here. Every other kind observes `existing_reason` as `None` by
    /// construction of `classify`'s own precedence — a story cannot reach
    /// `WindowGone`, `Stalled`, `Interrupted` or `DispatchRefused` while
    /// `awaiting` is set, because `AgentBlocked` is tested first.
    fn quarantine_lane(
        &self,
        run_id: &RunId,
        lane: &EngineLaneRecord,
        kind: HardStopKind,
        existing_reason: Option<&str>,
    ) -> Result<EngineQuarantineRecord, AppError> {
        let observed_at = self.ctx.now();
        let mut fired_reason = None;
        if let Some(story) = lane.story_id.as_deref() {
            let provenance = format!(
                "Full Auto: {} on lane {} of run {run_id}{}{}. Worktree, branch and window are preserved for inspection; re-dispatch deliberately once you have looked.",
                kind.as_str(),
                lane.lane_index,
                lane.window_name
                    .as_deref()
                    .map(|w| format!(" (window {w})"))
                    .unwrap_or_default(),
                lane.worktree_path
                    .as_deref()
                    .map(|p| format!(" (worktree {p})"))
                    .unwrap_or_default(),
            );
            let reason = match existing_reason {
                Some(existing) if !existing.is_empty() => format!("{existing} ({provenance})"),
                _ => provenance,
            };
            super::StoryService::new(self.ctx).set_awaiting(story, &reason)?;
            fired_reason = Some(reason);
        }
        let mut quarantined = lane.clone();
        quarantined.state = EngineLaneState::Quarantined;
        quarantined.last_observed_at = observed_at.clone();
        quarantined.outcome = Some(kind.as_str().to_string());
        quarantined.outcome_detail = lane.story_id.clone();
        self.ctx
            .store()
            .write(|tx| tx.put_engine_lane(&quarantined))?;
        self.fire_lane_quarantined_hook(
            run_id,
            lane.lane_index,
            lane.story_id.as_deref(),
            kind,
            fired_reason.as_deref(),
            lane.window_name.as_deref(),
            lane.worktree_path.as_deref(),
        );
        Ok(EngineQuarantineRecord {
            lane_index: lane.lane_index,
            story_id: lane.story_id.clone(),
            kind: kind.as_str().to_string(),
            detail: fired_reason,
            pane_id: lane.pane_id.clone(),
            window_name: lane.window_name.clone(),
            worktree_path: lane.worktree_path.clone(),
            observed_at,
        })
    }

    /// Fires the `engine_lane_quarantined` hook for a lane just quarantined
    /// and committed to the store (SH-472).
    ///
    /// Shared by [`Self::quarantine_lane`] (the four taxonomy-driven hard
    /// stops) and [`Self::fill_idle_lanes`]'s `DispatchRefused` branch, which
    /// writes its own quarantine inline rather than calling
    /// `quarantine_lane` — one payload-building call site means the two
    /// existing producers cannot drift on shape, and means SH-466's future
    /// `Interrupted` producer inherits this hook for free if it reuses
    /// `quarantine_lane` the way its own design description suggests.
    #[allow(clippy::too_many_arguments)]
    fn fire_lane_quarantined_hook(
        &self,
        run_id: &RunId,
        lane_index: u32,
        story_id: Option<&str>,
        kind: HardStopKind,
        reason: Option<&str>,
        window_name: Option<&str>,
        worktree_path: Option<&str>,
    ) {
        self.ctx.fire_hook(
            HookEventType::EngineLaneQuarantined,
            &serde_json::json!({
                "event_type": "engine_lane_quarantined",
                "run_id": run_id,
                "lane_index": lane_index,
                "story_id": story_id,
                "kind": kind.as_str(),
                "reason": reason,
                "window_name": window_name,
                "worktree_path": worktree_path,
                "timestamp": self.ctx.now(),
            }),
        );
    }

    /// Applies this pass's completions and hard stops to the breaker.
    fn apply_breaker(
        &self,
        run_id: &RunId,
        completions: usize,
        hard_stops: &[EngineQuarantineRecord],
    ) -> Result<RunView, AppError> {
        // Set only on the pass that actually flips Running -> Halted, never
        // on a later pass over an already-halted run (whose own lanes may
        // still individually hard-stop afterward, incrementing the streak
        // further with the state guard below refusing to re-transition) —
        // the `run.state == Running` guard is the only place the flip can
        // happen, so this cell is exactly "did this call just do that" (SH-472).
        let just_halted = std::cell::Cell::new(false);
        let view = self.transition(run_id, |run, _| {
            if completions > 0 {
                run.consecutive_hard_stops = 0;
                run.recent_quarantines.clear();
            }
            run.consecutive_hard_stops += u32::try_from(hard_stops.len()).unwrap_or(u32::MAX);
            run.recent_quarantines.extend_from_slice(hard_stops);
            if run.recent_quarantines.len() > HARD_STOP_BREAKER as usize {
                let excess = run.recent_quarantines.len() - HARD_STOP_BREAKER as usize;
                run.recent_quarantines.drain(..excess);
            }
            if run.consecutive_hard_stops >= HARD_STOP_BREAKER
                && run.state == EngineRunState::Running
            {
                run.state = EngineRunState::Halted;
                run.stop_reason = Some(BREAKER_TRIPPED.to_string());
                run.acknowledged_at = None;
                just_halted.set(true);
            }
            Ok(())
        })?;
        if just_halted.get() {
            self.fire_run_halted_hook(run_id, &view);
        }
        Ok(view)
    }

    /// Fires the `engine_run_halted` hook once the breaker has actually
    /// tripped and that halt has committed (SH-472).
    ///
    /// The "last three quarantine reasons" come from the run's bounded,
    /// durable consecutive series. A lane may already have returned to work,
    /// so its current row cannot be the authority for earlier failures.
    fn fire_run_halted_hook(&self, run_id: &RunId, view: &RunView) {
        let reasons: Vec<String> = view
            .run
            .recent_quarantines
            .iter()
            .rev()
            .map(quarantine_reason_line)
            .collect();
        self.ctx.fire_hook(
            HookEventType::EngineRunHalted,
            &serde_json::json!({
                "event_type": "engine_run_halted",
                "run_id": run_id,
                "consecutive_hard_stops": view.run.consecutive_hard_stops,
                "last_quarantine_reasons": reasons,
                "timestamp": self.ctx.now(),
            }),
        );
    }

    /// Claims and dispatches into every idle lane the budget allows.
    ///
    /// Serial by construction (A4): `story claim --next` is the arbiter, a
    /// claim is milliseconds, and the store is the only thing that can
    /// adjudicate a race between two lanes wanting the same story.
    fn fill_idle_lanes(
        &self,
        slug: &str,
        run_id: &RunId,
        report: &mut ReconcileReport,
    ) -> Result<Vec<EngineQuarantineRecord>, AppError> {
        let mut hard_stops = Vec::new();
        let view = self.one_view(run_id)?;
        let scope_epic = match &view.run.scope {
            EngineScope::Epic(id) => Some(id.clone()),
            EngineScope::Project => None,
        };
        let occupied = view
            .lanes
            .iter()
            .filter(|lane| lane.state != EngineLaneState::Idle)
            .count();
        let idle: Vec<EngineLaneRecord> = view
            .lanes
            .iter()
            .filter(|lane| lane.state == EngineLaneState::Idle)
            .cloned()
            .collect();

        for (n, lane) in idle.into_iter().enumerate() {
            if occupied + n >= ENGINE_LANE_BUDGET {
                break;
            }
            let dispatched_at = self.ctx.now();
            let mut working = lane.clone();
            let filters = ReadyQueueFilters {
                phase: None,
                epic: scope_epic.as_deref(),
                exclude_label: Some(LABEL_NO_AUTO),
            };
            let Some((before, claimed)) = super::StoryService::new(self.ctx)
                .claim_next_filtered_if(
                    filters,
                    None,
                    |tx| Ok(run_for_project(tx, slug, run_id)?.state == EngineRunState::Running),
                    |tx, before, claimed| {
                        working.state = EngineLaneState::Dispatching;
                        working.story_id = Some(claimed.id.clone());
                        working.dispatched_at = Some(dispatched_at.clone());
                        working.last_observed_at = dispatched_at.clone();
                        working.outcome = Some(before.state.clone());
                        tx.put_engine_lane(&working)
                    },
                )?
            else {
                break;
            };
            let story = claimed.id.clone();
            debug_assert_eq!(working.story_id.as_deref(), Some(story.as_str()));
            debug_assert_eq!(working.outcome.as_deref(), Some(before.state.as_str()));

            let outcome = self.dispatcher.dispatch(DispatchRequest {
                project: slug.to_string(),
                story: story.clone(),
                agent: view.run.agent,
                model: view.run.model.clone(),
                effort: view.run.effort.clone(),
                speed: view.run.speed,
            })?;
            match outcome.state {
                DispatchOutcomeState::Ok => {
                    let mut live = working.clone();
                    live.state = EngineLaneState::Working;
                    live.pane_id = outcome
                        .payload
                        .get("pane")
                        .and_then(|v| v.as_str())
                        .filter(|pane| valid_pane_id(pane))
                        .map(str::to_string);
                    live.window_name = outcome
                        .payload
                        .get("window_name")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    live.worktree_path = outcome
                        .payload
                        .get("worktree_path")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    live.cleanup_lease =
                        cleanup_lease_from_payload(&outcome.payload, slug, &story).ok();
                    live.outcome = None;
                    live.outcome_detail = None;
                    self.ctx.store().write(|tx| tx.put_engine_lane(&live))?;
                    report.filled.push((lane.lane_index, story));
                }
                DispatchOutcomeState::Refused => {
                    // The script's own refusal, relayed verbatim rather than
                    // replaced by a list composed here (SH-120). The lane is
                    // quarantined rather than freed, because the story is
                    // claimed and something has to say why. The report's kind
                    // and the lane's own stored `outcome` must spell the same
                    // event: a refusal is not a dead window.
                    let mut stuck = working.clone();
                    stuck.state = EngineLaneState::Quarantined;
                    stuck.outcome = Some(HardStopKind::DispatchRefused.as_str().to_string());
                    let diagnosis = helper_diagnosis(&outcome.payload);
                    super::StoryService::new(self.ctx).set_awaiting(&story, &diagnosis)?;
                    stuck.outcome_detail = Some(story.clone());
                    self.ctx.store().write(|tx| tx.put_engine_lane(&stuck))?;
                    // This branch quarantines inline rather than calling
                    // `quarantine_lane` (the story is freshly claimed here,
                    // not yet observed by `observe_lanes`), so the
                    // `engine_lane_quarantined` hook needs its own fire call
                    // through the shared helper (SH-472).
                    self.fire_lane_quarantined_hook(
                        run_id,
                        lane.lane_index,
                        Some(story.as_str()),
                        HardStopKind::DispatchRefused,
                        Some(&diagnosis),
                        None,
                        None,
                    );
                    report
                        .quarantined
                        .push((lane.lane_index, HardStopKind::DispatchRefused));
                    hard_stops.push(EngineQuarantineRecord {
                        lane_index: lane.lane_index,
                        story_id: Some(story),
                        kind: HardStopKind::DispatchRefused.as_str().to_string(),
                        detail: Some(diagnosis),
                        pane_id: None,
                        window_name: None,
                        worktree_path: None,
                        observed_at: dispatched_at,
                    });
                    break;
                }
            }
        }
        Ok(hard_stops)
    }

    /// Ends a run whose lanes are all idle.
    ///
    /// **A `Running` run does not auto-finish while a `no-auto` story still
    /// sits in its scope.** "Nothing claimable" and "nothing left, ever" are
    /// different facts: the engine deliberately skips `no-auto` for a human
    /// to act on (D12), and reporting it via [`RunView::skipped_no_auto`]
    /// only means something if the run is still there to look at when a
    /// human checks. Without this guard a project whose entire backlog is
    /// `no-auto` finished the instant `start` created it — often before the
    /// operator's own next command reached the daemon, since a store write
    /// wakes this loop synchronously at the request boundary that made it
    /// (SH-202) — leaving `engine status` reporting "no live engine run"
    /// with no trace of what happened beyond `start`'s own JSON.
    ///
    /// A `Draining` run is unaffected: `stop` (graceful) is an explicit
    /// operator decision to end the run once its existing lanes clear, and
    /// that decision does not wait on a `no-auto` item nobody has claimed.
    ///
    /// Fires `engine_run_drained` (SH-472) only when the queue organically
    /// ran dry — `stop_reason` was `None` going in and `reason` is
    /// [`QUEUE_DRAINED`] specifically. An operator-initiated stop
    /// (`OPERATOR_STOPPED`/`OPERATOR_STOPPED_NOW`) already set its own
    /// `stop_reason` before this ever runs, so the `is_none()` guard below
    /// keeps this call from overwriting it — and from firing a notification
    /// about something the operator just did themselves.
    fn finish_if_drained(&self, run_id: &RunId, reason: &str) -> Result<RunView, AppError> {
        let project = self.ctx.project();
        let now = self.ctx.now();
        let needs_human = !self
            .ctx
            .store()
            .read(|tx| {
                let slug = project_slug(tx, project)?;
                let run = run_for_project(tx, &slug, run_id)?;
                needs_human_stories(tx, project, &now, &run.scope)
            })?
            .is_empty();
        let just_drained = std::cell::Cell::new(false);
        let view = self.transition(run_id, |run, lanes| {
            let all_idle = lanes.iter().all(|lane| lane.state == EngineLaneState::Idle);
            let waiting_on_a_human = run.state == EngineRunState::Running && needs_human;
            if all_idle
                && !waiting_on_a_human
                && matches!(
                    run.state,
                    EngineRunState::Running | EngineRunState::Draining
                )
            {
                run.state = EngineRunState::Finished;
                if run.stop_reason.is_none() {
                    run.stop_reason = Some(reason.to_string());
                    run.acknowledged_at = None;
                    just_drained.set(reason == QUEUE_DRAINED);
                }
            }
            Ok(())
        })?;
        if just_drained.get() {
            self.ctx.fire_hook(
                HookEventType::EngineRunDrained,
                &serde_json::json!({
                    "event_type": "engine_run_drained",
                    "run_id": run_id,
                    "timestamp": self.ctx.now(),
                }),
            );
        }
        Ok(view)
    }

    fn stop_now(&self, run_id: &RunId) -> Result<RunView, AppError> {
        let project = self.ctx.project();
        let transition_at = self.ctx.now();
        let slug = self.ctx.store().write(|tx| {
            let slug = project_slug(tx, project)?;
            let mut run = run_for_project(tx, &slug, run_id)?;
            require_state(
                &run,
                "stop --now",
                &[
                    EngineRunState::Running,
                    EngineRunState::Paused,
                    EngineRunState::Draining,
                ],
            )?;
            run.state = EngineRunState::Draining;
            if run.stop_reason.as_deref() != Some(OPERATOR_STOPPED_NOW) {
                run.stop_reason = Some(OPERATOR_STOPPED_NOW.to_string());
                run.acknowledged_at = None;
            }
            run.updated_at = transition_at.clone();
            tx.update_engine_run(&run)?;
            Ok(slug)
        })?;

        let dispatch_deadline = Instant::now() + DISPATCH_TIMEOUT;
        loop {
            let lanes = self.stop_lanes_after_dispatch(run_id, dispatch_deadline)?;
            for lane in lanes
                .into_iter()
                .filter(|lane| lane.state != EngineLaneState::Idle)
            {
                if lane.state == EngineLaneState::Quarantined {
                    self.clear_quarantined_lane(&lane)?;
                    continue;
                }
                let story = lane.story_id.clone().expect("filtered occupied lane");
                let cleanup_lease = lane.cleanup_lease.clone().ok_or_else(|| {
                    AppError::Storage(format!(
                        "engine run `{run_id}` cannot immediately stop lane {} story `{story}`: no cleanup lease was recorded for this legacy lane",
                        lane.lane_index
                    ))
                })?;
                let outcome = self
                    .dispatcher
                    .unclaim(UnclaimRequest {
                        project: slug.clone(),
                        story: story.clone(),
                        cleanup_lease,
                    })
                    .map_err(|error| {
                        error.with_context(&format!(
                            "engine run `{run_id}` could not immediately stop lane {} story `{story}`",
                            lane.lane_index
                        ))
                    })?;
                if outcome.state == DispatchOutcomeState::Refused {
                    return Err(AppError::Validation(format!(
                        "engine run `{run_id}` could not immediately stop lane {} story `{story}`: {}",
                        lane.lane_index,
                        helper_diagnosis(&outcome.payload)
                    )));
                }
                self.release_lane(&lane, &outcome.payload)?;
            }

            let all_idle = self.ctx.store().read(|tx| {
                Ok(tx
                    .engine_lanes(run_id)?
                    .iter()
                    .all(|lane| lane.state == EngineLaneState::Idle))
            })?;
            if all_idle {
                break;
            }
        }

        let finished_at = self.ctx.now();
        self.ctx.store().write(|tx| {
            let mut run = run_for_project(tx, &slug, run_id)?;
            let lanes = tx.engine_lanes(run_id)?;
            if lanes.iter().any(|lane| lane.state != EngineLaneState::Idle) {
                return Err(StoreError::from(AppError::Validation(format!(
                    "engine run `{run_id}` still has occupied lanes after immediate stop"
                ))));
            }
            run.state = EngineRunState::Finished;
            run.updated_at = finished_at;
            tx.update_engine_run(&run)
        })?;
        self.one_view(run_id)
    }

    fn stop_lanes_after_dispatch(
        &self,
        run_id: &RunId,
        deadline: Instant,
    ) -> Result<Vec<EngineLaneRecord>, AppError> {
        loop {
            let lanes = self.ctx.store().read(|tx| tx.engine_lanes(run_id))?;
            let Some(dispatching) = lanes.iter().find(|lane| {
                lane.state == EngineLaneState::Dispatching && lane.cleanup_lease.is_none()
            }) else {
                return Ok(lanes);
            };
            if Instant::now() >= deadline {
                return Err(AppError::Storage(format!(
                    "engine run `{run_id}` could not immediately stop lane {} story `{}`: dispatch did not publish its cleanup lease within {}s",
                    dispatching.lane_index,
                    dispatching.story_id.as_deref().unwrap_or("unknown"),
                    DISPATCH_TIMEOUT.as_secs()
                )));
            }
            std::thread::sleep(DISPATCH_SETTLE_POLL);
        }
    }

    fn release_lane(
        &self,
        lane: &EngineLaneRecord,
        payload: &serde_json::Value,
    ) -> Result<(), AppError> {
        let observed_at = self.ctx.now();
        let mut idle = idle_lane(&lane.run_id, lane.lane_index, &observed_at);
        idle.outcome = Some(OPERATOR_STOPPED_NOW.to_string());
        idle.outcome_detail = Some(payload.to_string());
        self.ctx.store().write(|tx| tx.put_engine_lane(&idle))?;
        Ok(())
    }

    fn clear_quarantined_lane(&self, lane: &EngineLaneRecord) -> Result<(), AppError> {
        let observed_at = self.ctx.now();
        let mut idle = idle_lane(&lane.run_id, lane.lane_index, &observed_at);
        idle.outcome = lane.outcome.clone();
        idle.outcome_detail = lane.outcome_detail.clone();
        self.ctx.store().write(|tx| tx.put_engine_lane(&idle))?;
        Ok(())
    }

    fn clear_quarantined_lanes(&self, run_id: &RunId) -> Result<(), AppError> {
        let lanes = self.ctx.store().read(|tx| tx.engine_lanes(run_id))?;
        for lane in lanes
            .iter()
            .filter(|lane| lane.state == EngineLaneState::Quarantined)
        {
            self.clear_quarantined_lane(lane)?;
        }
        Ok(())
    }

    fn transition(
        &self,
        run_id: &RunId,
        mutate: impl FnOnce(&mut EngineRunRecord, &[EngineLaneRecord]) -> Result<(), StoreError>,
    ) -> Result<RunView, AppError> {
        let project = self.ctx.project();
        let updated_at = self.ctx.now();
        self.ctx.store().write(|tx| {
            let slug = project_slug(tx, project)?;
            let mut run = run_for_project(tx, &slug, run_id)?;
            let lanes = tx.engine_lanes(run_id)?;
            let before = run.clone();
            mutate(&mut run, &lanes)?;
            if run != before {
                run.updated_at = updated_at;
                tx.update_engine_run(&run)?;
            }
            Ok(())
        })?;
        self.one_view(run_id)
    }

    fn set_acknowledged(&self, run_id: &RunId) -> Result<RunView, AppError> {
        let project = self.ctx.project();
        let at = self.ctx.now();
        self.ctx.store().write(|tx| {
            let slug = project_slug(tx, project)?;
            let mut run = run_for_project(tx, &slug, run_id)?;
            if run.stop_reason.is_none() {
                return Err(StoreError::from(AppError::Validation(format!(
                    "engine run `{run_id}` has no stop notification to acknowledge"
                ))));
            }
            if run.acknowledged_at.is_none() {
                run.acknowledged_at = Some(at.clone());
                run.updated_at = at.clone();
                tx.update_engine_run(&run)?;
            }
            Ok(())
        })?;
        self.one_view(run_id)
    }

    fn one_view(&self, run_id: &RunId) -> Result<RunView, AppError> {
        self.status(Some(run_id))?
            .into_iter()
            .next()
            .ok_or_else(|| AppError::NotFound(format!("engine run `{run_id}` not found")))
    }

    fn project_slug(&self) -> Result<String, AppError> {
        Ok(self
            .ctx
            .store()
            .read(|tx| project_slug(tx, self.ctx.project()))?)
    }
}

/// Whole seconds from `earlier` to `later`, or `None` if either fails to parse
/// or the clock went backwards.
///
/// `None` states nothing rather than zero: `classify` treats an unparseable or
/// inverted interval as "no elapsed time is known", which cannot become a
/// stall (SH-372). A stall must be positively demonstrated, never inferred
/// from a timestamp nobody could read.
pub(crate) fn elapsed_secs(earlier: &str, later: &str) -> Option<u64> {
    let earlier = chrono::DateTime::parse_from_rfc3339(earlier).ok()?;
    let later = chrono::DateTime::parse_from_rfc3339(later).ok()?;
    u64::try_from((later - earlier).num_seconds()).ok()
}

/// An empty lane at `lane_index`, ready to be filled.
///
/// The progress pair is cleared along with the story: an idle lane holds no
/// story to have progressed, and carrying the previous occupant's seq forward
/// would let the next story inherit a stall clock it never started (SH-465).
fn idle_lane(run_id: &str, lane_index: u32, at: &str) -> EngineLaneRecord {
    EngineLaneRecord {
        run_id: run_id.to_string(),
        lane_index,
        state: EngineLaneState::Idle,
        story_id: None,
        pane_id: None,
        window_name: None,
        worktree_path: None,
        cleanup_lease: None,
        dispatched_at: None,
        last_observed_at: at.to_string(),
        last_progress_seq: None,
        last_progress_at: None,
        outcome: None,
        outcome_detail: None,
    }
}

fn valid_pane_id(value: &str) -> bool {
    value.strip_prefix('%').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn exact_window_target(session: &str, window: &str) -> String {
    format!("={session}:={window}")
}

/// One human-readable line for a quarantined lane, for the
/// `engine_run_halted` hook's `last_quarantine_reasons` (SH-472).
///
/// Mirrors the dashboard's own `buildEngineQuarantineItem`
/// (`src/web_dashboard.html`): the detail is appended only when it differs
/// from the story id, which is the ordinary `quarantine_lane` shape (where
/// `outcome_detail` *is* the story id) versus a `DispatchRefused` quarantine
/// (where it is the helper's own diagnosis text instead) — so the two stay
/// in visual agreement rather than drifting on their own formatting rules.
fn quarantine_reason_line(item: &EngineQuarantineRecord) -> String {
    let story = item.story_id.as_deref().unwrap_or("no story");
    let kind = &item.kind;
    match item.detail.as_deref() {
        Some(detail) if detail != story => {
            format!("lane {} ({story}): {kind} — {detail}", item.lane_index)
        }
        _ => format!("lane {} ({story}): {kind}", item.lane_index),
    }
}

/// Every ready story in `scope` still carrying `no-auto` — the engine skips
/// these deliberately (D12), and a human might still relabel or claim one by
/// hand. Shared by [`EngineService::status`]'s reporting and
/// [`EngineService::finish_if_drained`]'s termination guard, so the two can
/// never disagree about what "needs a human" means (SH-136).
fn needs_human_stories(
    tx: &impl ReadOps,
    project: crate::store::ProjectId,
    now: &str,
    scope: &EngineScope,
) -> Result<Vec<SkippedNoAutoStory>, StoreError> {
    Ok(QueryService::new(tx, project, now)
        .next_filtered(
            usize::MAX,
            ReadyQueueFilters {
                phase: None,
                epic: scope.story_id(),
                exclude_label: None,
            },
        )
        .map_err(StoreError::from)?
        .into_iter()
        .filter(|view| view.story.labels.iter().any(|label| label == LABEL_NO_AUTO))
        .map(|view| SkippedNoAutoStory {
            id: view.story.id,
            title: view.story.title,
        })
        .collect())
}

fn project_slug(tx: &impl ReadOps, project: crate::store::ProjectId) -> Result<String, StoreError> {
    Ok(tx
        .project(project)?
        .ok_or_else(|| StoreError::NotFound(format!("project {project} does not exist")))?
        .slug)
}

fn run_for_project(
    tx: &impl ReadOps,
    project_slug: &str,
    run_id: &str,
) -> Result<EngineRunRecord, StoreError> {
    tx.engine_run(run_id)?
        .filter(|run| run.project_slug == project_slug)
        .ok_or_else(|| StoreError::NotFound(format!("engine run `{run_id}` not found")))
}

fn require_state(
    run: &EngineRunRecord,
    action: &str,
    allowed: &[EngineRunState],
) -> Result<(), StoreError> {
    if allowed.contains(&run.state) {
        return Ok(());
    }
    Err(StoreError::from(AppError::Validation(format!(
        "engine run `{}` is `{}` and cannot `{action}`",
        run.id,
        run.state.as_str()
    ))))
}

fn no_checkout_refusal(project_slug: &str) -> String {
    format!(
        "project `{project_slug}` has no checkout on this machine, so there is nowhere to run a git worktree — record one with `story --project {project_slug} project link checkout <path>`. Its stories stay readable meanwhile; only the repo-side verbs need a directory."
    )
}

fn is_live_run_collision(detail: &str) -> bool {
    detail.contains("UNIQUE constraint failed: engine_runs.project_slug")
}

fn helper_diagnosis(payload: &serde_json::Value) -> String {
    payload
        .get("display")
        .or_else(|| payload.get("reason"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("the unclaim helper refused without a diagnosis")
        .to_string()
}

/// Runs one helper invocation. The dashboard uses `auto` from its request and
/// never supplies `full_auto`; [`ShellDispatcher`] supplies both flags for an
/// engine lane so that only the engine receives that identity and isolation
/// boundary. Full Auto lanes copy their run's immutable provider options into
/// `options`; attended dispatch supplies its request-scoped selections.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_shell_dispatch(
    script: &Path,
    project: &str,
    story: &str,
    agent: EngineAgent,
    auto: bool,
    full_auto: bool,
    options: &DispatchOptions,
    env: &Environment,
) -> Result<DispatchOutcome, AppError> {
    let [
        story_prompt,
        story_auto_prompt,
        story_auto_prompt_solo,
        story_prompt_extra,
    ] = PROMPT_OVERRIDE_ENV_VARS.map(|name| std::env::var(name).ok());
    if let Some(name) = prompt_override_violation(
        auto,
        story_prompt.as_deref(),
        story_auto_prompt.as_deref(),
        story_auto_prompt_solo.as_deref(),
        story_prompt_extra.as_deref(),
    ) {
        let display = format!(
            "[story] refused to dispatch {story} — this daemon's own ${name} environment value \
             contains a character CHARTER-INERT bans (one of ` $ ; & | < > ! or a newline) \
             and would be pasted into a live shell-backed pane verbatim. Fix ${name} in the \
             daemon's own environment and restart it, then retry."
        );
        return Ok(DispatchOutcome::from_payload(serde_json::json!({
            "ok": false,
            "reason": "unsafe-prompt-override",
            "display": display,
            "env_var": name,
        })));
    }

    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("story"));
    let mut command = Command::new("bash");
    command
        .arg(script)
        .arg("--project")
        .arg(project)
        .arg("dispatch")
        .arg(story)
        .arg(format!("--agent={}", agent.as_str()));
    if options.resume {
        command.arg("--resume");
    }
    if auto {
        command.arg("--auto");
    }
    if full_auto {
        debug_assert!(auto, "Full Auto is a modifier of autonomous dispatch");
        debug_assert!(
            !options.resume,
            "Full Auto reuses its fresh engine-owned claim and never resumes artifacts"
        );
        // Full Auto dispatch follows the engine's atomic claim immediately. `--force`
        // reuses that sole claim while the helper still rejects worktree, branch, or pane artifacts.
        command.arg("--full-auto").arg("--force");
    }
    if let Some(model) = &options.model {
        command.arg(format!("--model={model}"));
    }
    if let Some(effort) = &options.effort {
        command.arg(format!("--effort={effort}"));
    }
    if options.fast {
        command.arg("--speed=fast");
    }
    apply_dispatch_allowlist(&mut command);
    command
        .current_dir(env.home())
        .env("STORY_BIN", exe)
        .env("STORYHOOK_STORE_PATH", env.store_path())
        .env("STORY_TARGET_SESSION", project)
        .env("STORY_CREATE_SESSION", "1")
        .env("GIT_TERMINAL_PROMPT", "0");

    let captured = run_captured(command, DISPATCH_TIMEOUT).map_err(|error| match error {
        CaptureError::Stage(detail) => {
            AppError::Storage(format!("could not stage dispatch output: {detail}"))
        }
        CaptureError::Spawn(detail) => {
            AppError::Storage(format!("failed to start the dispatch script: {detail}"))
        }
        CaptureError::Wait(detail) => {
            AppError::Storage(format!("could not wait for the dispatch process: {detail}"))
        }
        CaptureError::Timeout(_) => AppError::Storage(format!(
            "dispatch did not finish within {}s and was terminated",
            DISPATCH_TIMEOUT.as_secs()
        )),
    })?;
    classify_dispatch_capture(&captured)
}

/// Runs the non-destructive inverse of dispatch through the same helper
/// boundary. `story.sh unclaim` owns prior-state restoration and window
/// closure; the engine deliberately does not reproduce either half.
fn run_shell_unclaim(
    script: &Path,
    project: &str,
    story: &str,
    cleanup_lease: &StoryCleanupLease,
    env: &Environment,
) -> Result<DispatchOutcome, AppError> {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("story"));
    let mut command = Command::new("bash");
    command
        .arg(script)
        .arg("--project")
        .arg(project)
        .arg("unclaim")
        .arg(story);
    apply_dispatch_allowlist(&mut command);
    let encoded_lease = serde_json::to_string(cleanup_lease).map_err(|error| {
        AppError::Storage(format!("could not encode stop-now cleanup lease: {error}"))
    })?;
    command
        .current_dir(env.home())
        .env("STORY_BIN", exe)
        .env("STORYHOOK_STORE_PATH", env.store_path())
        .env(CLEANUP_LEASE_ENV, encoded_lease)
        .env("STORY_TARGET_SESSION", project)
        .env("GIT_TERMINAL_PROMPT", "0");

    let captured = run_captured(command, DISPATCH_TIMEOUT).map_err(|error| match error {
        CaptureError::Stage(detail) => {
            AppError::Storage(format!("could not stage unclaim output: {detail}"))
        }
        CaptureError::Spawn(detail) => {
            AppError::Storage(format!("failed to start the unclaim helper: {detail}"))
        }
        CaptureError::Wait(detail) => {
            AppError::Storage(format!("could not wait for the unclaim helper: {detail}"))
        }
        CaptureError::Timeout(_) => AppError::Storage(format!(
            "unclaim did not finish within {}s and was terminated",
            DISPATCH_TIMEOUT.as_secs()
        )),
    })?;
    let outcome = classify_dispatch_capture(&captured)?;
    if outcome.state == DispatchOutcomeState::Ok {
        let echoed = outcome
            .payload
            .pointer("/cleanup/lease")
            .cloned()
            .ok_or_else(|| {
                AppError::Storage("leased unclaim success omitted its cleanup receipt".into())
            })?;
        let echoed: StoryCleanupLease = serde_json::from_value(echoed).map_err(|error| {
            AppError::Storage(format!(
                "leased unclaim returned an invalid cleanup lease: {error}"
            ))
        })?;
        let absent = outcome
            .payload
            .pointer("/cleanup/postconditions/tmux_story_windows_absent")
            .and_then(serde_json::Value::as_bool);
        if &echoed != cleanup_lease || absent != Some(true) {
            return Err(AppError::Storage(
                "leased unclaim claimed success without echoing its lease and proving exact story-window absence".into(),
            ));
        }
    }
    Ok(outcome)
}

fn cleanup_lease_from_payload(
    payload: &serde_json::Value,
    project: &str,
    story: &str,
) -> Result<StoryCleanupLease, AppError> {
    let lease = payload
        .get("cleanup_lease")
        .cloned()
        .ok_or_else(|| AppError::Storage("dispatch success omitted cleanup_lease".to_string()))?;
    let lease: StoryCleanupLease = serde_json::from_value(lease).map_err(|error| {
        AppError::Storage(format!(
            "dispatch success carried an invalid cleanup_lease: {error}"
        ))
    })?;
    if lease.version != CLEANUP_LEASE_VERSION
        || lease.project_slug != project
        || lease.story_id != story
    {
        return Err(AppError::Storage(
            "dispatch cleanup lease does not match protocol version, project, and story"
                .to_string(),
        ));
    }
    Ok(lease)
}

/// A much shorter bound than [`DISPATCH_TIMEOUT`]: `story.sh capabilities`
/// spawns no worktree, tmux window, or provider CLI — it only prints a
/// static per-provider catalog (SH-517).
const CAPABILITIES_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs `story.sh capabilities --agent=<agent>` through the same helper
/// boundary [`run_shell_dispatch`] uses, for
/// `crate::api::dispatch`'s `GET /api/dispatch-options` (SH-517). No
/// `--project`: capabilities never looks up a story or project.
pub(crate) fn run_shell_capabilities(
    script: &Path,
    agent: EngineAgent,
    env: &Environment,
) -> Result<DispatchOutcome, AppError> {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("story"));
    let mut command = Command::new("bash");
    command
        .arg(script)
        .arg("capabilities")
        .arg(format!("--agent={}", agent.as_str()));
    apply_dispatch_allowlist(&mut command);
    command
        .current_dir(env.home())
        .env("STORY_BIN", exe)
        .env("GIT_TERMINAL_PROMPT", "0");

    let captured = run_captured(command, CAPABILITIES_TIMEOUT).map_err(|error| match error {
        CaptureError::Stage(detail) => {
            AppError::Storage(format!("could not stage capabilities output: {detail}"))
        }
        CaptureError::Spawn(detail) => {
            AppError::Storage(format!("failed to start the capabilities helper: {detail}"))
        }
        CaptureError::Wait(detail) => AppError::Storage(format!(
            "could not wait for the capabilities helper: {detail}"
        )),
        CaptureError::Timeout(_) => AppError::Storage(format!(
            "capabilities did not finish within {}s and was terminated",
            CAPABILITIES_TIMEOUT.as_secs()
        )),
    })?;
    classify_dispatch_capture(&captured)
}

fn classify_dispatch_bytes(stdout: &[u8], stderr: &[u8]) -> Result<DispatchOutcome, AppError> {
    match serde_json::from_slice::<serde_json::Value>(trim_ascii(stdout)) {
        Ok(payload) => Ok(DispatchOutcome::from_payload(payload)),
        Err(_) => {
            let stderr = String::from_utf8_lossy(stderr).trim().to_string();
            let message = if stderr.is_empty() {
                "the dispatch script exited without printing a result".to_string()
            } else {
                stderr
            };
            Err(AppError::Storage(message))
        }
    }
}

fn classify_dispatch_capture(captured: &Captured) -> Result<DispatchOutcome, AppError> {
    let outcome = classify_dispatch_bytes(&captured.stdout, &captured.stderr)?;
    if outcome.state == DispatchOutcomeState::Ok && !captured.status.success() {
        let stderr = String::from_utf8_lossy(&captured.stderr);
        let stderr = stderr.trim();
        let detail = if stderr.is_empty() {
            String::new()
        } else {
            format!(": {stderr}")
        };
        return Err(AppError::Storage(format!(
            "story helper reported success but exited {}{detail}",
            captured.status
        )));
    }
    Ok(outcome)
}

#[cfg(test)]
pub(crate) fn classify_dispatch_files(
    stdout: File,
    stderr: File,
) -> Result<DispatchOutcome, AppError> {
    classify_dispatch_bytes(&read_capture(stdout), &read_capture(stderr))
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

pub(crate) fn charter_inert_violation(value: &str) -> bool {
    let mut stripped = value.to_string();
    for token in TEMPLATE_PLACEHOLDERS {
        stripped = stripped.replace(token, "");
    }
    stripped.contains(CHARTER_INERT_BANNED) || stripped.contains('\n')
}

pub(crate) fn prompt_override_violation(
    auto: bool,
    story_prompt: Option<&str>,
    story_auto_prompt: Option<&str>,
    story_auto_prompt_solo: Option<&str>,
    story_prompt_extra: Option<&str>,
) -> Option<&'static str> {
    let mut candidates = Vec::with_capacity(3);
    if auto {
        candidates.push(("STORY_AUTO_PROMPT", story_auto_prompt));
        candidates.push(("STORY_AUTO_PROMPT_SOLO", story_auto_prompt_solo));
    } else {
        candidates.push(("STORY_PROMPT", story_prompt));
    }
    candidates.push(("STORY_PROMPT_EXTRA", story_prompt_extra));
    candidates
        .into_iter()
        .find(|(_, value)| value.is_some_and(charter_inert_violation))
        .map(|(name, _)| name)
}

fn pid_is_live(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 does not alter the target; it asks the kernel whether
    // the process exists and whether this caller may signal it.
    let status = unsafe { libc::kill(pid, 0) };
    status == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

struct ProcessIdentity {
    pattern: Option<Regex>,
    launch_binaries: Vec<PathBuf>,
}

impl ProcessIdentity {
    fn from_process() -> Self {
        let pattern = std::env::var("STORY_READY_PROCESS_PATTERN")
            .unwrap_or_else(|_| "^(claude|node|codex)$".to_string());
        let launch_words = match std::env::var("STORY_LAUNCH_CMD") {
            Ok(command) => command
                .split_whitespace()
                .next()
                .map(|word| vec![word.to_string()])
                .unwrap_or_default(),
            Err(_) => vec!["claude".to_string(), "codex".to_string()],
        };
        Self {
            pattern: Regex::new(&pattern).ok(),
            launch_binaries: launch_words
                .iter()
                .filter_map(|word| resolve_executable(word))
                .collect(),
        }
    }

    fn matches(&self, observed: &str) -> bool {
        let observed = Path::new(observed)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(observed);
        if observed.is_empty() {
            return false;
        }
        if self
            .pattern
            .as_ref()
            .is_some_and(|pattern| pattern.is_match(observed))
        {
            return true;
        }
        self.launch_binaries.iter().any(|resolved| {
            let Some(base) = resolved.file_name().and_then(|name| name.to_str()) else {
                return false;
            };
            if observed == base {
                return true;
            }
            if !is_version_name(base) || !is_version_name(observed) {
                return false;
            }
            resolved
                .parent()
                .map(|parent| parent.join(observed))
                .is_some_and(|sibling| is_executable(&sibling))
        })
    }
}

fn resolve_executable(word: &str) -> Option<PathBuf> {
    let candidate = PathBuf::from(word);
    let found = if candidate.components().count() > 1 {
        candidate
    } else {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|directory| directory.join(word))
            .find(|path| is_executable(path))?
    };
    let resolved = std::fs::canonicalize(found).ok()?;
    is_executable(&resolved).then_some(resolved)
}

fn is_version_name(name: &str) -> bool {
    !name.is_empty()
        && name.split('.').all(|component| {
            !component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(path: &Path, body: &str) {
        use std::os::unix::fs::PermissionsExt as _;

        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn dispatcher_with_tmux(root: &Path, tmux_program: &Path) -> ShellDispatcher {
        ShellDispatcher {
            story_sh_path: root.join("story.sh"),
            env: Environment::at(root.join("home")),
            tmux_program: tmux_program.as_os_str().to_owned(),
        }
    }

    #[test]
    fn payload_classification_preserves_the_whole_answer() {
        let payload = serde_json::json!({
            "ok": false,
            "reason": "future-refusal",
            "future": {"nested": true}
        });
        assert_eq!(
            DispatchOutcome::from_payload(payload.clone()),
            DispatchOutcome {
                state: DispatchOutcomeState::Refused,
                payload,
            }
        );
    }

    #[test]
    fn process_identity_accepts_resolved_and_installed_sibling_versions_only() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let root = storyhook_test_support::scratch_dir();
        let versions = root.path().join("versions");
        std::fs::create_dir(&versions).unwrap();
        for version in ["2.1.227", "2.1.228"] {
            let path = versions.join(version);
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        symlink(versions.join("2.1.228"), root.path().join("claude")).unwrap();
        let identity = ProcessIdentity {
            pattern: Regex::new("^(claude|node)$").ok(),
            launch_binaries: vec![std::fs::canonicalize(root.path().join("claude")).unwrap()],
        };
        assert!(identity.matches("2.1.228"));
        assert!(identity.matches("2.1.227"));
        assert!(identity.matches("node"));
        assert!(!identity.matches("9.9.9"));
        assert!(!identity.matches("zsh"));
    }

    #[test]
    fn process_identity_does_not_widen_a_plain_named_install_to_any_version() {
        let identity = ProcessIdentity {
            pattern: None,
            launch_binaries: vec![PathBuf::from("/usr/local/bin/claude")],
        };
        assert!(identity.matches("claude"));
        assert!(!identity.matches("2.1.228"));
    }

    #[test]
    fn charter_inert_check_preserves_placeholders_but_rejects_shell_syntax() {
        assert!(!charter_inert_violation(
            "Work <n> in <name> at <dir>; reap token is omitted"
                .replace(';', ",")
                .as_str()
        ));
        assert!(charter_inert_violation("story <n> > /tmp/exfil"));
        assert!(charter_inert_violation("line one\nline two"));
    }

    #[test]
    fn charter_inert_check_accepts_the_rendered_completion_state_placeholder() {
        assert!(!charter_inert_violation(
            "move story <n> to <done-state>, then run <reap>"
        ));
    }

    #[test]
    fn bounded_capture_terminates_a_process_group() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        assert!(matches!(
            run_captured(command, Duration::from_millis(20)),
            Err(CaptureError::Timeout(_))
        ));
    }

    #[test]
    fn shell_window_probe_requires_a_live_pid_and_agent_identity() {
        let root = storyhook_test_support::scratch_dir();
        let live = root.path().join("tmux-live");
        executable(
            &live,
            &format!("printf '{}\\tcodex\\t0\\n'", std::process::id()),
        );
        assert!(dispatcher_with_tmux(root.path(), &live).window_alive("@7"));

        let shell = root.path().join("tmux-shell");
        executable(
            &shell,
            &format!("printf '{}\\tzsh\\t0\\n'", std::process::id()),
        );
        assert!(!dispatcher_with_tmux(root.path(), &shell).window_alive("@7"));

        let dead = root.path().join("tmux-dead");
        executable(
            &dead,
            &format!("printf '{}\\tcodex\\t1\\n'", std::process::id()),
        );
        assert!(!dispatcher_with_tmux(root.path(), &dead).window_alive("@7"));
    }

    #[test]
    fn shell_kill_targets_the_exact_window_and_carries_tmux_diagnostics() {
        let root = storyhook_test_support::scratch_dir();
        let log = root.path().join("args");
        let tmux = root.path().join("tmux");
        executable(
            &tmux,
            &format!("printf '%s\\n' \"$*\" > '{}'; exit 23", log.display()),
        );
        let error = dispatcher_with_tmux(root.path(), &tmux)
            .kill_window("@exact")
            .unwrap_err();
        assert_eq!(
            std::fs::read_to_string(log)
                .unwrap_or_else(|failure| panic!(
                    "tmux fixture produced no argv: {failure}; {error}"
                ))
                .trim(),
            "kill-window -t @exact"
        );
        assert!(
            error
                .to_string()
                .contains("tmux refused to kill window `@exact`")
        );
    }
}
