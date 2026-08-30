//! The process boundary used by the Full Auto engine.
//!
//! [`Dispatcher`] is deliberately synchronous. The engine decides which
//! thread owns an attempt; this module decides what one attempt means. That
//! keeps the store-pool deadlock rule in the caller while giving reconcile
//! tests a seam that never needs a worktree, tmux server, or agent process.

use std::ffi::OsString;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

use regex::Regex;
use wait_timeout::ChildExt;

use crate::domain::is_epic;
use crate::env::Environment;
use crate::env::spawn_env::apply_dispatch_allowlist;
use crate::error::AppError;
use crate::store::{
    EngineAgent, EngineLaneRecord, EngineLaneState, EngineRunRecord, EngineRunState, EngineScope,
    ReadOps, Store, StoreError, WriteOps,
};

use super::{Ctx, project_prefix, resolve_story};

/// How long the worktree/tmux/agent helper may run.
///
/// This is the dashboard dispatch bound moved to the shared seam: the script's
/// readiness handoff is bounded below this, while a networked `git fetch` is
/// the genuinely variable part.
const DISPATCH_TIMEOUT: Duration = Duration::from_secs(180);

/// The design record models lane count as a `u8`. Keep the service boundary
/// faithful even though SQLite stores the value in an INTEGER column.
pub const MAX_ENGINE_LANES: u32 = u8::MAX as u32;

pub const OPERATOR_STOPPED: &str = "operator-stopped";
pub const OPERATOR_STOPPED_NOW: &str = "operator-stopped-now";

/// A tmux client normally answers in milliseconds. The shared machine-probe
/// budget bounds a wedged server without inventing another patience value.
const TMUX_TIMEOUT: Duration = crate::daemon::tailnet::TAILNET_PROBE_TIMEOUT;

/// Bounds diagnostics from a faulty helper or tmux client.
const MAX_CAPTURE_BYTES: u64 = 64 * 1024;

const PROMPT_OVERRIDE_ENV_VARS: [&str; 4] = [
    "STORY_PROMPT",
    "STORY_AUTO_PROMPT",
    "STORY_AUTO_PROMPT_SOLO",
    "STORY_PROMPT_EXTRA",
];

const TEMPLATE_PLACEHOLDERS: [&str; 5] = ["<name>", "<dir>", "<reap>", "<n>", "<done-state>"];
pub(crate) const CHARTER_INERT_BANNED: [char; 8] = ['`', '$', ';', '&', '|', '<', '>', '!'];

/// Everything the shell actuator needs to dispatch one already-selected story.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchRequest {
    pub project: String,
    pub story: String,
    pub agent: EngineAgent,
}

/// Everything the shell helper needs to release one engine-owned claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnclaimRequest {
    pub project: String,
    pub story: String,
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
        Command::new(&self.tmux_program)
    }
}

impl Dispatcher for ShellDispatcher {
    fn dispatch(&self, request: DispatchRequest) -> Result<DispatchOutcome, AppError> {
        run_shell_dispatch(
            &self.story_sh_path,
            &request.project,
            &request.story,
            request.agent,
            true,
            true,
            &self.env,
        )
    }

    fn unclaim(&self, request: UnclaimRequest) -> Result<DispatchOutcome, AppError> {
        run_shell_unclaim(
            &self.story_sh_path,
            &request.project,
            &request.story,
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
            Err(CaptureError::Timeout) => Err(AppError::Storage(format!(
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
}

/// One transactionally consistent run and its ordered lanes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunView {
    pub run: EngineRunRecord,
    pub lanes: Vec<EngineLaneRecord>,
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
                state: EngineRunState::Running,
                consecutive_hard_stops: 0,
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
            Ok(run) => Ok(run),
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
                    Ok(RunView { run, lanes })
                })
                .collect()
        })?)
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

    fn stop_now(&self, run_id: &RunId) -> Result<RunView, AppError> {
        let project = self.ctx.project();
        let transition_at = self.ctx.now();
        let (slug, lanes) = self.ctx.store().write(|tx| {
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
            let lanes = tx.engine_lanes(run_id)?;
            run.state = EngineRunState::Draining;
            if run.stop_reason.as_deref() != Some(OPERATOR_STOPPED_NOW) {
                run.stop_reason = Some(OPERATOR_STOPPED_NOW.to_string());
                run.acknowledged_at = None;
            }
            run.updated_at = transition_at.clone();
            tx.update_engine_run(&run)?;
            Ok((slug, lanes))
        })?;

        for lane in lanes
            .into_iter()
            .filter(|lane| lane.state != EngineLaneState::Idle)
        {
            if lane.state == EngineLaneState::Quarantined {
                self.clear_quarantined_lane(&lane)?;
                continue;
            }
            let story = lane.story_id.clone().expect("filtered occupied lane");
            let outcome = self
                .dispatcher
                .unclaim(UnclaimRequest {
                    project: slug.clone(),
                    story: story.clone(),
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

fn idle_lane(run_id: &str, lane_index: u32, at: &str) -> EngineLaneRecord {
    EngineLaneRecord {
        run_id: run_id.to_string(),
        lane_index,
        state: EngineLaneState::Idle,
        story_id: None,
        window_name: None,
        worktree_path: None,
        dispatched_at: None,
        last_observed_at: at.to_string(),
        outcome: None,
        outcome_detail: None,
    }
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
/// boundary.
pub(crate) fn run_shell_dispatch(
    script: &Path,
    project: &str,
    story: &str,
    agent: EngineAgent,
    auto: bool,
    full_auto: bool,
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
    if auto {
        command.arg("--auto");
    }
    if full_auto {
        debug_assert!(auto, "Full Auto is a modifier of autonomous dispatch");
        command.arg("--full-auto");
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
        CaptureError::Timeout => AppError::Storage(format!(
            "dispatch did not finish within {}s and was terminated",
            DISPATCH_TIMEOUT.as_secs()
        )),
    })?;
    classify_dispatch_bytes(&captured.stdout, &captured.stderr)
}

/// Runs the non-destructive inverse of dispatch through the same helper
/// boundary. `story.sh unclaim` owns prior-state restoration and window
/// closure; the engine deliberately does not reproduce either half.
fn run_shell_unclaim(
    script: &Path,
    project: &str,
    story: &str,
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
    command
        .current_dir(env.home())
        .env("STORY_BIN", exe)
        .env("STORYHOOK_STORE_PATH", env.store_path())
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
        CaptureError::Timeout => AppError::Storage(format!(
            "unclaim did not finish within {}s and was terminated",
            DISPATCH_TIMEOUT.as_secs()
        )),
    })?;
    classify_dispatch_bytes(&captured.stdout, &captured.stderr)
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

struct Captured {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

enum CaptureError {
    Stage(std::io::Error),
    Spawn(std::io::Error),
    Wait(std::io::Error),
    Timeout,
}

impl CaptureError {
    fn detail(&self) -> String {
        match self {
            Self::Stage(error) | Self::Spawn(error) | Self::Wait(error) => error.to_string(),
            Self::Timeout => "the process timed out".to_string(),
        }
    }
}

fn run_captured(mut command: Command, timeout: Duration) -> Result<Captured, CaptureError> {
    let stdout_file = tempfile::tempfile().map_err(CaptureError::Stage)?;
    let stderr_file = tempfile::tempfile().map_err(CaptureError::Stage)?;
    let child_stdout = stdout_file.try_clone().map_err(CaptureError::Stage)?;
    let child_stderr = stderr_file.try_clone().map_err(CaptureError::Stage)?;
    command
        .stdin(Stdio::null())
        .stdout(child_stdout)
        .stderr(child_stderr);
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command.spawn().map_err(CaptureError::Spawn)?;
    let pid = child.id();
    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        Ok(None) => {
            kill_process_group(pid);
            let _ = child.wait();
            return Err(CaptureError::Timeout);
        }
        Err(error) => {
            kill_process_group(pid);
            let _ = child.wait();
            return Err(CaptureError::Wait(error));
        }
    };
    Ok(Captured {
        status,
        stdout: read_capture(stdout_file),
        stderr: read_capture(stderr_file),
    })
}

fn kill_process_group(pid: u32) {
    #[cfg(unix)]
    // SAFETY: this is the process group created for the child immediately
    // above; it has not been reaped and therefore cannot have been recycled.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    #[cfg(not(unix))]
    let _ = pid;
}

fn read_capture(mut file: File) -> Vec<u8> {
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(0)).is_ok() {
        let _ = file.take(MAX_CAPTURE_BYTES).read_to_end(&mut bytes);
    }
    bytes
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
            Err(CaptureError::Timeout)
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
            std::fs::read_to_string(log).unwrap().trim(),
            "kill-window -t @exact"
        );
        assert!(
            error
                .to_string()
                .contains("tmux refused to kill window `@exact`")
        );
    }
}
