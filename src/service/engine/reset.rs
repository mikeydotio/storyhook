//! Explicit cancellation owns resources until cleanup and restoration commit.

use super::*;
use crate::domain::{StoryEvent, active_state};
use crate::service::StoryService;
use crate::store::{EngineReset, ExpectedSeq, ProjectId, StoryNo};
use fs4::FileExt;

/// Rejects mutations that would transfer a story while reset owns its work.
pub(crate) fn refuse_reserved(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
) -> Result<(), AppError> {
    if let Some(reset) = tx.engine_reset(project, story)? {
        return Err(AppError::Validation(format!(
            "story `{}` reset in progress: run `{}` lane {} owns operation {}; retry Stop Now to finish cleanup",
            reset.lease.story_id, reset.run_id, reset.lane_index, reset.token,
        )));
    }
    Ok(())
}

/// Whether this run has accepted an explicit immediate-stop request.
pub(crate) fn stopping(tx: &impl ReadOps, run: &str) -> Result<bool, StoreError> {
    Ok(tx.engine_run(run)?.is_some_and(|run| {
        run.state == EngineRunState::Draining
            && run.stop_reason.as_deref() == Some(OPERATOR_STOPPED_NOW)
    }))
}

impl<'ctx, S: Store, D: Dispatcher> EngineService<'ctx, S, D> {
    /// Read-only helper authorization, scoped to the selected project and token.
    pub fn reset_target(&self, run_id: &str, token: &str) -> Result<EngineReset, AppError> {
        Ok(self.ctx.store().read(|tx| {
            let project = self.ctx.project();
            let slug = project_slug(tx, project)?;
            run_for_project(tx, &slug, run_id)?;
            if !stopping(tx, run_id)? {
                return Err(StoreError::Invariant(
                    "run no longer owns an immediate stop".into(),
                ));
            }
            let prefix = project_prefix(tx, project)?;
            for lane in tx.engine_lanes(run_id)? {
                if let Some(id) = &lane.story_id {
                    let (number, row) = resolve_story(tx, project, &prefix, id)?;
                    if let Some(reset) = tx.engine_reset(project, number)?
                        && reset.token == token
                        && reset.run_id == run_id
                        && reset.lane_index == lane.lane_index
                        && lane.cleanup_lease.as_ref() == Some(&reset.lease)
                        && active_state(&tx.states(project)?)
                            .is_some_and(|active| row.state == active.slug)
                    {
                        return Ok(reset);
                    }
                }
            }
            Err(StoreError::NotFound(format!(
                "no current reset `{token}` in run `{run_id}`"
            )))
        })?)
    }

    pub(super) fn reset_now(&self, run_id: &RunId) -> Result<RunView, AppError> {
        // The inode remains after unlock: replacing/removing a lock file would
        // let two workers hold different locks for the same logical run.
        let key: String = run_id.bytes().map(|b| format!("{b:02x}")).collect();
        let lock_path = self
            .ctx
            .env()
            .store_path()
            .with_extension(format!("reset-{key}.lock"));
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|e| {
                AppError::Storage(format!("opening reset lock {}: {e}", lock_path.display()))
            })?;
        lock.try_lock_exclusive().map_err(|e| {
            AppError::Validation(format!(
                "engine run `{run_id}` reset already in progress or lock unavailable: {e}"
            ))
        })?;
        // The helper runs from a neutral directory; only this context retains
        // the original caller. Refuse before scheduling restartable cleanup.
        let caller = self
            .ctx
            .cwd()
            .canonicalize()
            .map_err(|e| AppError::Storage(format!("resolving reset caller directory: {e}")))?;
        self.ctx.store().read(|tx| {
            let project = self.ctx.project();
            let slug = project_slug(tx, project)?;
            run_for_project(tx, &slug, run_id)?;
            let prefix = project_prefix(tx, project)?;
            let active = active_state(&tx.states(project)?)
                .ok_or_else(|| StoreError::Invariant("project has no active state".into()))?.slug.clone();
            for lane in tx.engine_lanes(run_id)? {
                if let (Some(id), Some(lease)) = (&lane.story_id, &lane.cleanup_lease) {
                    let (_, row) = resolve_story(tx, project, &prefix, id)?;
                    if row.state == active && caller.starts_with(&lease.worktree_path) {
                        return Err(StoreError::Invariant(format!("cannot reset calling worktree for story `{id}`; invoke Stop Now from outside its worktree")));
                    }
                }
            }
            Ok(())
        })?;
        let now = self.ctx.now();
        self.ctx.store().write(|tx| {
            let slug = project_slug(tx, self.ctx.project())?;
            let mut run = run_for_project(tx, &slug, run_id)?;
            if run.state == EngineRunState::Finished
                && run.stop_reason.as_deref() == Some(OPERATOR_STOPPED_NOW)
            {
                return Ok(());
            }
            require_state(
                &run,
                "stop --now",
                &[
                    EngineRunState::Running,
                    EngineRunState::Paused,
                    EngineRunState::Draining,
                    EngineRunState::Halted,
                ],
            )?;
            run.state = EngineRunState::Draining;
            if run.stop_reason.as_deref() != Some(OPERATOR_STOPPED_NOW) {
                run.acknowledged_at = None;
            }
            run.stop_reason = Some(OPERATOR_STOPPED_NOW.into());
            run.updated_at = now.clone();
            tx.update_engine_run(&run)
        })?;
        if self.one_view(run_id)?.run.state == EngineRunState::Finished {
            return self.one_view(run_id);
        }
        let lanes = self.stop_lanes_after_dispatch(run_id, Instant::now() + DISPATCH_TIMEOUT)?;
        let mut failures = Vec::new();
        for lane in lanes
            .iter()
            .filter(|lane| lane.state != EngineLaneState::Idle)
        {
            let attempt = self.reserve_reset(lane).and_then(|reset| {
                let Some(reset) = reset else {
                    return Ok(());
                };
                let result = self.dispatcher.reset(reset.clone()).and_then(|outcome| {
                    validate_receipt(&reset, &outcome)?;
                    self.finish_reset(&reset, &outcome.payload)
                });
                if let Err(error) = &result {
                    let mut failed = reset;
                    failed.failure = Some(error.to_string());
                    self.ctx.store().write(|tx| tx.put_engine_reset(&failed))?;
                }
                result
            });
            if let Err(error) = attempt {
                let detail = format!(
                    "run `{run_id}` lane {} story `{}`: {error}",
                    lane.lane_index,
                    lane.story_id.as_deref().unwrap_or("unknown")
                );
                self.ctx.store().write(|tx| {
                    if let Some(mut current) = tx
                        .engine_lanes(run_id)?
                        .into_iter()
                        .find(|l| l.lane_index == lane.lane_index && l.story_id == lane.story_id)
                    {
                        current.outcome_detail = Some(detail.clone());
                        tx.put_engine_lane(&current)?;
                    }
                    Ok(())
                })?;
                failures.push(detail);
            }
        }
        if !failures.is_empty() {
            return Err(AppError::Storage(failures.join("; ")));
        }
        self.ctx.store().write(|tx| {
            let slug = project_slug(tx, self.ctx.project())?;
            let mut run = run_for_project(tx, &slug, run_id)?;
            if tx
                .engine_lanes(run_id)?
                .iter()
                .any(|lane| lane.state != EngineLaneState::Idle)
            {
                return Err(StoreError::Invariant(format!(
                    "run `{run_id}` retains occupied lanes after reset"
                )));
            }
            run.state = EngineRunState::Finished;
            run.updated_at = self.ctx.now();
            tx.update_engine_run(&run)
        })?;
        self.one_view(run_id)
    }

    fn reserve_reset(&self, observed: &EngineLaneRecord) -> Result<Option<EngineReset>, AppError> {
        Ok(self.ctx.store().write(|tx| {
            let project = self.ctx.project();
            let lane = tx
                .engine_lanes(&observed.run_id)?
                .into_iter()
                .find(|lane| lane.lane_index == observed.lane_index)
                .ok_or_else(|| StoreError::Invariant("reset lane disappeared".into()))?;
            if lane.state == EngineLaneState::Idle {
                return Ok(None);
            }
            if lane.story_id != observed.story_id || lane.cleanup_lease != observed.cleanup_lease {
                return Err(StoreError::Invariant("reset lane ownership changed".into()));
            }
            let prefix = project_prefix(tx, project)?;
            let id = lane.story_id.as_deref().expect("occupied lane");
            let (number, row) = resolve_story(tx, project, &prefix, id)?;
            if let Some(reset) = tx.engine_reset(project, number)? {
                if reset.run_id != lane.run_id
                    || reset.lane_index != lane.lane_index
                    || Some(&reset.lease) != lane.cleanup_lease.as_ref()
                {
                    return Err(StoreError::Invariant(
                        "reset reservation belongs to a different lane identity".into(),
                    ));
                }
                return Ok(Some(reset));
            }
            let states = tx.state_map(project)?;
            let active = active_state(&tx.states(project)?)
                .ok_or_else(|| StoreError::Invariant("project has no active state".into()))?
                .slug
                .clone();
            if row.state != active || row.snapshot.superstate == SuperState::Closed {
                // Verification, closure and an ordinary external unclaim all
                // transfer authority away from this engine attempt.
                let idle = idle_lane(&lane.run_id, lane.lane_index, &self.ctx.now());
                put_or_retire_idle_lane(tx, &idle)?;
                return Ok(None);
            }
            let lease = lane.cleanup_lease.clone().ok_or_else(|| {
                StoreError::Invariant(format!(
                    "story `{id}` has no cleanup lease; cannot reset legacy lane"
                ))
            })?;
            let events: Vec<_> = tx
                .events_for(project, number)?
                .iter()
                .filter_map(|event| event.known().cloned())
                .collect();
            let destination =
                super::super::story::resolve_unclaim_destination(id, &events, &active, &states);
            let restore_to = if destination.restored_to == VERIFYING_STATE_SLUG
                || destination.restored_to == active
            {
                "todo".into()
            } else {
                destination.restored_to
            };
            let reset = EngineReset {
                project,
                story: number,
                run_id: lane.run_id.clone(),
                lane_index: lane.lane_index,
                token: uuid::Uuid::new_v4().simple().to_string(),
                lease,
                restore_to,
                failure: None,
            };
            tx.put_engine_reset(&reset)?;
            Ok(Some(reset))
        })?)
    }

    fn finish_reset(
        &self,
        reset: &EngineReset,
        receipt: &serde_json::Value,
    ) -> Result<(), AppError> {
        let now = self.ctx.now();
        let (before, snapshot) = self.ctx.write_stories(|tx| {
            let current = tx.engine_reset(reset.project, reset.story)?
                .ok_or_else(|| StoreError::Invariant("reset reservation disappeared".into()))?;
            if current.token != reset.token || current.lease != reset.lease {
                return Err(StoreError::Invariant("reset reservation changed".into()));
            }
            let prefix = project_prefix(tx, reset.project)?;
            let (_, row) = resolve_story(tx, reset.project, &prefix, &reset.lease.story_id)?;
            let states = tx.state_map(reset.project)?;
            let active = active_state(&tx.states(reset.project)?)
                .ok_or_else(|| StoreError::Invariant("project has no active state".into()))?
                .slug.clone();
            if row.state != active {
                return Err(StoreError::Invariant("reserved reset story left its active state".into()));
            }
            let target = states.get(&reset.restore_to)
                .filter(|state| state.super_state == SuperState::Open
                    && state.slug != active && state.slug != VERIFYING_STATE_SLUG)
                .or_else(|| states.get("todo"))
                .ok_or_else(|| StoreError::Invariant("reset fallback state todo missing".into()))?;
            let events = vec![
                StoryEvent::StoryStateChanged { at: now.clone(), state: target.slug.clone() },
                StoryEvent::StoryAwaitingCleared { at: now.clone() },
                StoryEvent::StoryCommentAdded {
                    at: now.clone(),
                    text: format!(
                        "Full Auto Stop Now: discarded unfinished work for run `{}` lane {}; \
                         restored to `{}` after exact window, worktree and local branch cleanup (reset {}).",
                        reset.run_id, reset.lane_index, target.slug, reset.token,
                    ),
                },
            ];
            // Removing the reservation and releasing the claim share this
            // transaction; no caller can observe an unguarded active story.
            tx.remove_engine_reset(reset)?;
            let snapshot = super::super::append_and_fold(
                tx, reset.project, reset.story, &prefix, &states,
                ExpectedSeq::Exact(row.head_seq), &events, self.ctx.provenance(),
            )?;
            let mut idle = idle_lane(&reset.run_id, reset.lane_index, &now);
            idle.outcome = Some(OPERATOR_STOPPED_NOW.into());
            idle.outcome_detail = Some(receipt.to_string());
            put_or_retire_idle_lane(tx, &idle)?;
            Ok((row.snapshot, snapshot))
        })?;
        StoryService::new(self.ctx).fire_transition_hooks(
            &before.id,
            &before.title,
            &before.state,
            &snapshot.state,
            &snapshot,
            &now,
        );
        Ok(())
    }
}

/// A helper's success is evidence only when every required postcondition holds.
pub fn validate_receipt(reset: &EngineReset, outcome: &DispatchOutcome) -> Result<(), AppError> {
    if outcome.state != DispatchOutcomeState::Ok {
        return Err(AppError::Validation(helper_diagnosis(&outcome.payload)));
    }
    let payload = &outcome.payload;
    if payload.get("token").and_then(|v| v.as_str()) != Some(reset.token.as_str())
        || payload.get("lease")
            != Some(
                &serde_json::to_value(&reset.lease)
                    .map_err(|e| AppError::Storage(e.to_string()))?,
            )
        || [
            "tmux_story_windows_absent",
            "worktree_registration_absent",
            "worktree_path_absent",
            "branch_absent",
        ]
        .iter()
        .any(|key| {
            payload
                .get("postconditions")
                .and_then(|v| v.get(key))
                .and_then(|v| v.as_bool())
                != Some(true)
        })
    {
        return Err(AppError::Storage("reset helper success did not prove the exact token, lease and every resource absence postcondition".into()));
    }
    Ok(())
}

pub(super) fn run_shell_reset(
    script: &Path,
    reset: &EngineReset,
    env: &Environment,
) -> Result<DispatchOutcome, AppError> {
    let encoded = serde_json::to_string(reset)
        .map_err(|e| AppError::Storage(format!("encoding reset request: {e}")))?;
    let mut command = Command::new("bash");
    command.arg(script).args([
        "--project",
        &reset.lease.project_slug,
        "reset",
        &reset.lease.story_id,
        "--force",
    ]);
    apply_dispatch_allowlist(&mut command);
    command
        .current_dir(env.home())
        .envs(env.child_vars())
        .env(
            "STORY_BIN",
            std::env::current_exe().map_err(|e| AppError::Storage(e.to_string()))?,
        )
        .env("STORYHOOK_ENGINE_RESET_V1", encoded)
        .env("GIT_TERMINAL_PROMPT", "0");
    let captured = run_captured(command, DISPATCH_TIMEOUT)
        .map_err(|e| AppError::Storage(format!("reset helper failed: {}", e.detail())))?;
    classify_dispatch_capture(&captured)
}
