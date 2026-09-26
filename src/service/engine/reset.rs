//! Explicit cancellation owns resources until cleanup and restoration commit.

use super::*;
use crate::domain::{StoryEvent, active_state};
use crate::service::StoryService;
use crate::service::executor_lock::ExecutorLock;
use crate::service::workspace_lock::WorkspaceLock;
use crate::store::{EngineReset, ExpectedSeq, ProjectId, StoryNo};
use std::os::fd::{AsRawFd, BorrowedFd};

/// What Stop Now does with one occupied lane. Every lane maps to exactly one
/// outcome, and none of them refuses forever, so no lane can keep a stopped
/// run draining indefinitely (SH-774).
enum StopTarget {
    /// The lane is idle, or this attempt released it without cleanup.
    Settled,
    /// A leased reset that the helper must clean up.
    Reset(EngineReset),
    /// Another cleanup operation owns the story and releases the lane when
    /// it finishes; the store refuses this lane's writes until then.
    Deferred(String),
}

/// Rejects mutations that would transfer a story while reset owns its work.
pub(crate) fn refuse_reserved(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
) -> Result<(), AppError> {
    super::super::story_reset::refuse_reserved(tx, project, story)?;
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
                    // A purged story cannot hold this token; it must not
                    // refuse the authorization of every other lane.
                    let Some(row) = optional_lane_story(tx, project, &prefix, id)? else {
                        continue;
                    };
                    if let Some(reset) = tx.engine_reset(project, row.story_no)?
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
        let (lock_path, lock) = run_lock_file(self.ctx.env(), "reset", run_id)?;
        // A busy lock means another Stop Now already owns this run's cleanup.
        // This request still records the same durable intent below, so the
        // owner, or the next steady reconcile, finishes it; a duplicate has
        // nothing to add and nothing to fail (SH-774).
        let executor = match ExecutorLock::acquire(&lock, &lock_path) {
            Ok(guard) => Some(guard),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
            Err(error) => {
                return Err(AppError::Storage(format!(
                    "locking Stop Now controller {} for engine run `{run_id}`: {error}",
                    lock_path.display()
                )));
            }
        };
        // The helper runs from a neutral directory; only this context retains
        // the original caller. Refuse before scheduling restartable cleanup.
        let caller = self
            .ctx
            .cwd()
            .canonicalize()
            .map_err(|e| AppError::Storage(format!("resolving reset caller directory: {e}")))?;
        let now = self.ctx.now();
        let finished = self.ctx.store().write(|tx| {
            let project = self.ctx.project();
            let slug = project_slug(tx, project)?;
            let mut run = run_for_project(tx, &slug, run_id)?;
            // Every path to `finished` requires idle lanes: nothing is left
            // to discard, whatever stopped the run.
            if run.state == EngineRunState::Finished {
                return Ok(true);
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
            let prefix = project_prefix(tx, project)?;
            let active = active_state(&tx.states(project)?)
                .ok_or_else(|| StoreError::Invariant("project has no active state".into()))?.slug.clone();
            for lane in tx.engine_lanes(run_id)? {
                if let (Some(id), Some(lease)) = (&lane.story_id, &lane.cleanup_lease) {
                    let Some(row) = optional_lane_story(tx, project, &prefix, id)? else {
                        continue;
                    };
                    if row.state == active && caller.starts_with(&lease.worktree_path) {
                        return Err(StoreError::Invariant(format!("cannot reset calling worktree for story `{id}`; invoke Stop Now from outside its worktree")));
                    }
                }
            }
            let before = run.clone();
            run.state = EngineRunState::Draining;
            if run.stop_reason.as_deref() != Some(OPERATOR_STOPPED_NOW) {
                run.acknowledged_at = None;
            }
            run.stop_reason = Some(OPERATOR_STOPPED_NOW.into());
            // The change watcher compares whole run records: rewriting only
            // `updated_at` on a retry would wake the next retry at once.
            if run != before {
                run.updated_at = now.clone();
                tx.update_engine_run(&run)?;
            }
            Ok(false)
        })?;
        if finished || executor.is_none() {
            return self.one_view(run_id);
        }
        let lanes = self.stop_lanes_after_dispatch(run_id, Instant::now() + DISPATCH_TIMEOUT)?;
        let mut failures = Vec::new();
        let mut deferred = Vec::new();
        for lane in lanes
            .iter()
            .filter(|lane| lane.state != EngineLaneState::Idle)
        {
            let attempt = self.reserve_reset(lane).and_then(|target| {
                let reset = match target {
                    StopTarget::Settled => return Ok(()),
                    StopTarget::Deferred(owner) => {
                        deferred.push(format!(
                            "lane {} story `{}`: {owner} owns its cleanup",
                            lane.lane_index,
                            lane.story_id.as_deref().unwrap_or("unknown")
                        ));
                        return Ok(());
                    }
                    StopTarget::Reset(reset) => reset,
                };
                let result = (|| {
                    let workspace = WorkspaceLock::acquire(
                        &reset.lease.repository_path,
                        &reset.lease.story_id,
                    )?;
                    self.ctx.store().write(|tx| {
                        let current =
                            tx.engine_reset(reset.project, reset.story)?
                                .ok_or_else(|| {
                                    StoreError::Invariant(
                                        "reset reservation disappeared before workspace admission"
                                            .into(),
                                    )
                                })?;
                        if current.token != reset.token || current.lease != reset.lease {
                            return Err(StoreError::Invariant(
                                "reset ownership changed before workspace admission".into(),
                            ));
                        }
                        crate::service::block_delivery::supersede_pending(
                            tx,
                            reset.project,
                            reset.story,
                            "engine reset owns workspace replacement",
                        )?;
                        Ok(())
                    })?;
                    let outcome = self
                        .dispatcher
                        .reset(reset.clone(), workspace.descriptor())?;
                    validate_receipt(&reset, &outcome)?;
                    self.finish_reset(&reset, &outcome.payload, workspace)
                })();
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
        // Not a failure: the other owner releases these lanes when it
        // finishes, and the durable intent makes the next attempt finish the
        // run then (SH-774).
        if !deferred.is_empty() {
            return self.one_view(run_id);
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

    /// Reserves one occupied lane's leased reset, or releases a lane that
    /// Stop Now can never reset.
    ///
    /// A lane without a cleanup lease, or
    /// whose story no longer exists, is released without cleanup (SH-774):
    /// no retry can produce the missing proof of ownership, so refusing it
    /// kept the run draining forever and blocked the project's next run.
    fn reserve_reset(&self, observed: &EngineLaneRecord) -> Result<StopTarget, AppError> {
        let now = self.ctx.now();
        Ok(self.ctx.write_stories(|tx| {
            let project = self.ctx.project();
            let lane = tx
                .engine_lanes(&observed.run_id)?
                .into_iter()
                .find(|lane| lane.lane_index == observed.lane_index)
                .ok_or_else(|| StoreError::Invariant("reset lane disappeared".into()))?;
            if lane.state == EngineLaneState::Idle {
                return Ok(StopTarget::Settled);
            }
            if lane.story_id != observed.story_id || lane.cleanup_lease != observed.cleanup_lease {
                return Err(StoreError::Invariant("reset lane ownership changed".into()));
            }
            let prefix = project_prefix(tx, project)?;
            let id = lane.story_id.as_deref().expect("occupied lane");
            let Some(row) = optional_lane_story(tx, project, &prefix, id)? else {
                let detail = format!(
                    "Full Auto Stop Now released run `{}` lane {} without cleanup: story `{id}` \
                     no longer exists, so there is no story to restore. Any window, worktree \
                     or branch of that story stays in place.",
                    lane.run_id, lane.lane_index,
                );
                put_or_retire_idle_lane(tx, &released_lane(&lane, &now, detail))?;
                return Ok(StopTarget::Settled);
            };
            let number = row.story_no;
            if let Some(owner) = super::super::story_reset::foreign_owner(tx, project, number)? {
                return Ok(StopTarget::Deferred(owner));
            }
            if let Some(reset) = tx.engine_reset(project, number)? {
                if reset.run_id != lane.run_id
                    || reset.lane_index != lane.lane_index
                    || Some(&reset.lease) != lane.cleanup_lease.as_ref()
                {
                    return Err(StoreError::Invariant(
                        "reset reservation belongs to a different lane identity".into(),
                    ));
                }
                return Ok(StopTarget::Reset(reset));
            }
            let states = tx.state_map(project)?;
            let active = active_state(&tx.states(project)?)
                .ok_or_else(|| StoreError::Invariant("project has no active state".into()))?
                .slug
                .clone();
            if row.state != active || row.snapshot.superstate == SuperState::Closed {
                // Verification, closure and an ordinary external unclaim all
                // transfer authority away from this engine attempt.
                let idle = idle_lane(&lane.run_id, lane.lane_index, &now);
                put_or_retire_idle_lane(tx, &idle)?;
                return Ok(StopTarget::Settled);
            }
            let Some(lease) = lane.cleanup_lease.clone() else {
                // Only the lease proves which window, worktree and branch this
                // run created (SH-706). Without it the work is kept, claimed
                // and explained, and the lane is released.
                let detail = format!(
                    "Full Auto Stop Now released run `{}` lane {} ({}) without cleanup. The lane \
                     has no cleanup lease, so Stop Now cannot prove which window, worktree and \
                     branch the run created. The story stays claimed and its resources stay in \
                     place. Examine them, then use Reset on the story card (`story reset {id}`) \
                     to discard them, or dispatch the story again to continue.",
                    lane.run_id,
                    lane.lane_index,
                    lane.state.as_str(),
                );
                let mut events = Vec::new();
                if row.awaiting.is_none() {
                    events.push(StoryEvent::StoryAwaitingSet {
                        at: now.clone(),
                        awaiting: detail.clone(),
                    });
                }
                events.push(StoryEvent::StoryCommentAdded {
                    at: now.clone(),
                    text: detail.clone(),
                });
                super::super::append_and_fold(
                    tx,
                    project,
                    number,
                    &prefix,
                    &states,
                    ExpectedSeq::Exact(row.head_seq),
                    &events,
                    self.ctx.provenance(),
                )?;
                put_or_retire_idle_lane(tx, &released_lane(&lane, &now, detail))?;
                return Ok(StopTarget::Settled);
            };
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
            Ok(StopTarget::Reset(reset))
        })?)
    }

    fn finish_reset(
        &self,
        reset: &EngineReset,
        receipt: &serde_json::Value,
        workspace: WorkspaceLock,
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
                        "Full Auto Stop Now discarded unfinished work for run `{}` lane {}. \
                         Removed the exact window, worktree, and local branch. Restored state `{}`. Reset {} completed.",
                        reset.run_id, reset.lane_index, target.slug, reset.token,
                    ),
                },
            ];
            // Removing the reservation and releasing the claim share this
            // transaction; no caller can observe an unguarded active story.
            crate::service::block_delivery::supersede_pending(tx, reset.project, reset.story, "engine reset completed; prior session authority retired")?;
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
        drop(workspace);
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

/// Opens one of a run's controller lock files, next to the store.
///
/// The inode remains after unlock: replacing or removing a lock file would
/// let two workers hold different locks for the same logical run, so the
/// file is never deleted.
pub(super) fn run_lock_file(
    env: &Environment,
    kind: &str,
    run_id: &str,
) -> Result<(PathBuf, std::fs::File), AppError> {
    let key: String = run_id.bytes().map(|b| format!("{b:02x}")).collect();
    let path = env
        .store_path()
        .with_extension(format!("{kind}-{key}.lock"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|e| AppError::Storage(format!("opening {kind} lock {}: {e}", path.display())))?;
    Ok((path, file))
}

/// The idle record of a lane Stop Now released without cleanup; the detail
/// says why and what the operator can do next.
fn released_lane(lane: &EngineLaneRecord, now: &str, detail: String) -> EngineLaneRecord {
    let mut idle = idle_lane(&lane.run_id, lane.lane_index, now);
    idle.outcome = Some(OPERATOR_STOPPED_NOW.into());
    idle.outcome_detail = Some(detail);
    idle
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
    workspace: BorrowedFd<'_>,
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
    crate::service::workspace_lock::inherit_descriptor(workspace, &mut command);
    command.env("STORY_WORKSPACE_LOCK_FD", workspace.as_raw_fd().to_string());
    let captured = crate::process::run_captured_quiescent(
        command,
        DISPATCH_TIMEOUT,
        crate::process::TerminationPolicy::Kill,
    )
    .map_err(|e| AppError::Storage(format!("reset helper failed: {}", e.detail())))?;
    classify_dispatch_capture(&captured)
}
