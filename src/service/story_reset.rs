//! A card reset holds readiness until exact resource cleanup succeeds.
mod cleanup;
pub(crate) mod identity;
mod summary;
mod takeover;

use super::executor_lock::ExecutorLock;
use super::workspace_lock::WorkspaceLock;
use super::{
    Ctx, StoryService, append_and_fold, append_reset_and_fold, project_prefix, resolve_open_story,
};
use crate::domain::{StoryEvent, StorySnapshot, SuperState};
use crate::error::AppError;
use crate::store::patience::{Shutdown, patiently};
use crate::store::{
    ExpectedSeq, ProjectId, ReadOps, Store, StoreError, StoryNo, StoryReset, WriteOps,
};
use std::time::{Duration, Instant};

/// How long a reservation waits out store contention before reporting it.
///
/// Shorter than the dashboard's 75-second mutation deadline, so a contended
/// request still answers before its client gives up.
pub(crate) const RESERVE_PATIENCE: Duration = Duration::from_secs(60);

/// Attempts of the finish transaction before the reset degrades (D3).
const FINISH_ATTEMPTS: u32 = 3;

/// Pause between finish attempts.
const FINISH_PAUSE: Duration = Duration::from_secs(1);

/// How long a reset waits for another process to release the story's
/// workspace lock before it proceeds without that exclusion (SH-886).
pub const WORKSPACE_PATIENCE: Duration = Duration::from_secs(60);

/// Pause between workspace lock attempts.
const WORKSPACE_POLL: Duration = Duration::from_millis(100);

/// Attempts to identify a story's resources before reset pins them for good.
const RESOLVE_ATTEMPTS: u32 = 5;

/// Pause between resource identification attempts.
const RESOLVE_PAUSE: Duration = Duration::from_secs(2);

/// Rejects lifecycle changes while an unfinished reset owns a story.
pub(crate) fn refuse_reserved(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
) -> Result<(), StoreError> {
    if let Some(cleanup) = tx.dropped_cleanup(project, story)?
        && !cleanup.released
    {
        return Err(AppError::Validation(format!(
            "dropped cleanup {} owns {}; retry story cleanup first",
            cleanup.token, cleanup.lease.story_id
        ))
        .into());
    }
    if let Some(reset) = tx.story_reset(project, story)?
        && !reset.completed
    {
        return Err(AppError::Validation(format!(
            "story `{}` reset in progress ({}); retry Reset to finish cleanup",
            reset.story_id, reset.token
        ))
        .into());
    }
    Ok(())
}

/// Names the cleanup operation, other than Full Auto Stop Now, that owns this
/// story, if any: an unreleased dropped-story cleanup, an unfinished card
/// reset, or a native `story reset` reservation.
///
/// Each owner leaves the story non-active or closed when it finishes, so a
/// Stop Now that defers to it releases the lane on its next attempt. Until
/// then the store refuses writes to engine lanes that hold the story.
pub(crate) fn foreign_owner(
    tx: &impl ReadOps,
    project: ProjectId,
    story: StoryNo,
) -> Result<Option<String>, StoreError> {
    if let Some(cleanup) = tx.dropped_cleanup(project, story)?
        && !cleanup.released
    {
        return Ok(Some(format!("dropped cleanup {}", cleanup.token)));
    }
    if let Some(reset) = tx.story_reset(project, story)?
        && !reset.completed
    {
        return Ok(Some(format!("card reset {}", reset.token)));
    }
    if tx.story_resets(project)?.contains_key(&story) {
        return Ok(Some("a story reset reservation".into()));
    }
    Ok(None)
}

/// Transactional card-reset coordinator; external cleanup runs outside store locks.
pub struct StoryResetService<'a, S: Store> {
    ctx: &'a Ctx<'a, S>,
    shutdown: Shutdown,
    workspace_patience: Duration,
}

impl<'a, S: Store> StoryResetService<'a, S> {
    /// Binds reset operations to the caller's project and environment.
    pub fn new(ctx: &'a Ctx<'a, S>) -> Self {
        Self {
            ctx,
            shutdown: Shutdown::new(),
            workspace_patience: WORKSPACE_PATIENCE,
        }
    }

    /// Sets how long a reset waits for a held workspace lock before it
    /// proceeds without it. [`WORKSPACE_PATIENCE`] unless changed.
    #[must_use]
    pub fn with_workspace_patience(mut self, patience: Duration) -> Self {
        self.workspace_patience = patience;
        self
    }

    /// Waits for the story's workspace lock, which dispatch, verification and
    /// their surviving children hold. A reset ignores locks in the end: after
    /// the patience it proceeds without exclusion and records that it did.
    fn workspace(
        &self,
        repository: &std::path::Path,
        story_id: &str,
        residue: &mut cleanup::Residue,
    ) -> Option<WorkspaceLock> {
        let common = match super::resources::git::text(
            repository,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ) {
            Ok(common) => std::path::PathBuf::from(common.trim()),
            Err(error) => {
                residue.leave(
                    "workspace lock",
                    format!("cannot locate it ({error}); reset proceeded without exclusion"),
                );
                return None;
            }
        };
        let deadline = Instant::now() + self.workspace_patience;
        loop {
            match WorkspaceLock::try_acquire_proven(&common, story_id) {
                Ok(Some(lock)) => return Some(lock),
                Ok(None) if Instant::now() < deadline && !self.shutdown.requested() => {
                    std::thread::sleep(WORKSPACE_POLL);
                }
                Ok(None) => {
                    residue.leave(
                        "workspace lock",
                        format!(
                            "another process still held it after {} s; reset proceeded \
                             without exclusion",
                            self.workspace_patience.as_secs_f32()
                        ),
                    );
                    return None;
                }
                Err(error) => {
                    residue.leave(
                        "workspace lock",
                        format!("cannot take it ({error}); reset proceeded without exclusion"),
                    );
                    return None;
                }
            }
        }
    }

    /// Identifies the story's resources, retrying while observation fails.
    /// A report that stays unidentifiable is returned as `unavailable`, so
    /// teardown removes nothing and reports why instead of failing.
    fn identify(&self, id: &str) -> super::resources::ResourceReport {
        let mut outcome = Err(AppError::Validation("not attempted".into()));
        for attempt in 0..RESOLVE_ATTEMPTS {
            if attempt > 0 {
                if self.shutdown.requested() {
                    break;
                }
                std::thread::sleep(RESOLVE_PAUSE);
            }
            outcome =
                super::resources::ResourceService::new(self.ctx).resolve(id, &Default::default());
            if outcome
                .as_ref()
                .is_ok_and(|report| report.status != "unavailable")
            {
                break;
            }
        }
        outcome.unwrap_or_else(|error| super::resources::ResourceReport {
            location_only: false,
            project: String::new(),
            story_id: id.into(),
            status: "unavailable".into(),
            repository: None,
            worktree: None,
            branch: None,
            window_name: id.into(),
            socket_path: None,
            pane: None,
            provider: None,
            candidates: Vec::new(),
            observations: Vec::new(),
            diagnostics: vec![format!("identifying resources: {error}")],
        })
    }

    /// Commits one write, waiting out contention: a reserved reset must finish.
    fn write<T>(
        &self,
        mut f: impl FnMut(&mut S::WriteTx<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        patiently(&self.shutdown, None, || self.ctx.store().write(&mut f))
    }

    /// Reserves one ordinary open story after exact typed-ID confirmation.
    pub fn reserve(&self, id: &str, confirmation: &str) -> Result<StoryReset, AppError> {
        let deadline = Instant::now() + RESERVE_PATIENCE;
        Ok(patiently(&self.shutdown, Some(deadline), || {
            self.ctx.store().write(|tx| {
                let project = self.ctx.project();
                let prefix = project_prefix(tx, project)?;
                let (story, row) = resolve_open_story(tx, project, &prefix, id)?;
                if confirmation != row.snapshot.id {
                    return Err(AppError::Validation(
                        "Type the canonical story ID exactly to confirm reset".into(),
                    )
                    .into());
                }
                if row.snapshot.story_type.as_deref() == Some("epic") {
                    return Err(AppError::Validation(
                        "Reset an ordinary child story, not an epic".into(),
                    )
                    .into());
                }
                if let Some(reset) = tx.story_reset(project, story)?
                    && !reset.completed
                {
                    return Ok(reset);
                }
                // The final lever outranks every other owner of the story.
                let superseded = takeover::supersede_owners(tx, project, story)?;
                let states = tx.state_map(project)?;
                if !states
                    .get("todo")
                    .is_some_and(|state| state.super_state == SuperState::Open)
                {
                    return Err(
                        AppError::Validation("Reset requires the OPEN todo state".into()).into(),
                    );
                }
                let slug = tx
                    .project(project)?
                    .ok_or_else(|| StoreError::NotFound("project".into()))?
                    .slug;
                let mut lanes = Vec::new();
                for run in tx.engine_runs(&slug)? {
                    lanes.extend(
                        tx.engine_lanes(&run.id)?
                            .into_iter()
                            .filter(|lane| lane.story_id.as_deref() == Some(&row.snapshot.id))
                            .map(|lane| crate::store::ResetLane {
                                run_id: lane.run_id,
                                lane_index: lane.lane_index,
                            }),
                    );
                }
                let reset = StoryReset {
                    project,
                    story,
                    story_id: row.snapshot.id,
                    token: uuid::Uuid::new_v4().simple().to_string(),
                    original_state: row.state,
                    lanes,
                    resources: None,
                    paths: Vec::new(),
                    completed: false,
                    failure: None,
                    residue: Vec::new(),
                    recovery: None,
                };
                tx.put_story_reset(&reset)?;
                if !superseded.is_empty() {
                    let states = tx.state_map(project)?;
                    append_and_fold(
                        tx,
                        project,
                        story,
                        &prefix,
                        &states,
                        ExpectedSeq::Exact(row.head_seq),
                        &[StoryEvent::StoryCommentAdded {
                            at: self.ctx.now(),
                            text: format!(
                                "Reset {} superseded {}. Those operations no longer own the story.",
                                reset.token,
                                superseded.join("; ")
                            ),
                        }],
                        self.ctx.provenance(),
                    )?;
                }
                Ok(reset)
            })
        })?)
    }

    /// Reads a receipt only within its original project, story and token.
    pub fn get(&self, id: &str, token: &str) -> Result<StoryReset, AppError> {
        Ok(self.ctx.store().read(|tx| {
            let prefix = project_prefix(tx, self.ctx.project())?;
            let no = StoryNo::parse_id(&prefix, id)
                .map_err(|_| StoreError::NotFound(format!("story {id}")))?;
            tx.story_reset(self.ctx.project(), no)?
                .filter(|reset| reset.token == token)
                .ok_or_else(|| StoreError::NotFound(format!("reset {token} for {id}")))
        })?)
    }

    /// Drives a reserved reset to completion and returns its receipt.
    ///
    /// `quiesce` should join dispatch and the selected verifier; its failure
    /// is recorded and never stops the reset. The receipt is unfinished only
    /// when another executor already runs this same reset, which it joins.
    pub fn execute(
        &self,
        id: &str,
        token: &str,
        quiesce: impl FnOnce() -> Result<(), AppError>,
    ) -> Result<StoryReset, AppError> {
        let mut reset = self.get(id, token)?;
        if reset.completed {
            return Ok(reset);
        }
        let lock_path = self
            .ctx
            .env()
            .store_path()
            .with_extension(format!("story-reset-{}.lock", reset.token));
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|e| {
                AppError::Storage(format!("opening reset lock {}: {e}", lock_path.display()))
            })?;
        // Another executor is already finishing this same reset: join it.
        let _executor = match ExecutorLock::acquire(&lock, &lock_path) {
            Ok(guard) => guard,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                return self.get(id, token);
            }
            Err(error) => {
                return Err(AppError::Storage(format!(
                    "locking reset {} at {}: {error}",
                    reset.token,
                    lock_path.display()
                )));
            }
        };
        reset = self.get(id, token)?;
        if reset.completed {
            return Ok(reset);
        }
        reset.failure = None;
        self.write(|tx| tx.put_story_reset(&reset))?;
        let result = (|| {
            let mut residue = cleanup::Residue::default();
            // Waiting for work to stop is courtesy, not a precondition.
            if let Err(error) = quiesce() {
                residue.leave(
                    "running work",
                    format!("it did not stop in time ({error}); reset proceeded"),
                );
            }
            let report = match &reset.resources {
                Some(report) => report.clone(),
                None => self.identify(id),
            };
            // Dispatch and verification release the shared workspace before we
            // acquire it; surviving cleanup children retain this same ownership.
            let workspace = report
                .repository
                .as_ref()
                .and_then(|repository| self.workspace(repository, &reset.story_id, &mut residue));
            if reset.resources.is_none() {
                // Pinned once, even when identification failed: teardown then
                // withholds removal and reports why, and the reset still ends.
                let mut report = report;
                match identity::capture(&report) {
                    Ok(paths) => reset.paths = paths,
                    Err(error) => {
                        report.status = "unavailable".into();
                        report
                            .diagnostics
                            .push(format!("pinning filesystem identity: {error}"));
                    }
                }
                reset.resources = Some(report);
                self.write(|tx| tx.put_story_reset(&reset))?;
            }
            self.write(|tx| {
                super::block_delivery::supersede_pending(
                    tx,
                    reset.project,
                    reset.story,
                    "card reset owns workspace replacement",
                )?;
                Ok(())
            })?;
            let report = reset.resources.clone().expect("pinned resources");
            let authority = cleanup::authorize(
                &report,
                &reset.paths,
                self.ctx.cwd(),
                self.ctx.env(),
                &mut residue,
            );
            // Recorded before anything is removed, and never overwritten by a
            // retry that can no longer see what an earlier attempt discarded.
            if reset.recovery.is_none() {
                reset.recovery = Some(cleanup::recovery(&report, &authority));
                self.write(|tx| tx.put_story_reset(&reset))?;
            }
            cleanup::remove(
                &report,
                &reset.paths,
                &authority,
                self.ctx.env(),
                workspace.as_ref(),
                &mut residue,
            );
            cleanup::dispatch_overlap(&report, &authority, self.ctx.env(), &mut residue);
            reset.residue = residue.into_entries();
            self.finish(&reset, workspace)
        })();
        match result {
            Ok(done) => Ok(done),
            Err(error) => {
                reset.failure = Some(error.to_string());
                self.write(|tx| tx.put_story_reset(&reset)).map_err(|persist| AppError::Storage(format!("reset {} failed: {error}; recording diagnostics also failed: {persist}", reset.token)))?;
                Err(error)
            }
        }
    }

    /// Commits the reset's one mandatory effect: the story returns to todo,
    /// ownership and lanes are released, and the receipt completes. A finish
    /// that keeps failing degrades instead of stranding the story (D3).
    fn finish(
        &self,
        reset: &StoryReset,
        workspace: Option<WorkspaceLock>,
    ) -> Result<StoryReset, AppError> {
        let now = self.ctx.now();
        let mut attempt = 1;
        let released = loop {
            match patiently(&self.shutdown, None, || self.release(reset, &now)) {
                Ok(released) => break released,
                Err(StoreError::Busy(detail)) => return Err(StoreError::Busy(detail).into()),
                Err(_) if attempt < FINISH_ATTEMPTS => {
                    attempt += 1;
                    std::thread::sleep(FINISH_PAUSE);
                }
                Err(error) => return self.finish_degraded(reset, workspace, &error),
            }
        };
        let Some((before, snapshot, done)) = released else {
            // Another executor finished this reset first.
            return self.get(&reset.story_id, &reset.token);
        };
        // Transition hooks may dispatch the now-ready story.
        drop(workspace);
        StoryService::new(self.ctx).fire_transition_hooks(
            &before.id,
            &before.title,
            &before.state,
            &snapshot.state,
            &snapshot,
            &now,
        );
        Ok(done)
    }

    /// The finish transaction; `None` when the reset was already completed.
    #[allow(clippy::type_complexity)]
    fn release(
        &self,
        reset: &StoryReset,
        now: &str,
    ) -> Result<Option<(StorySnapshot, StorySnapshot, StoryReset)>, StoreError> {
        self.ctx.write_stories(|tx| {
            let current = tx
                .story_reset(reset.project, reset.story)?
                .filter(|current| current.token == reset.token)
                .ok_or_else(|| StoreError::Invariant("reset disappeared".into()))?;
            if current.completed {
                return Ok(None);
            }
            let prefix = project_prefix(tx, reset.project)?;
            let (_, row) = resolve_open_story(tx, reset.project, &prefix, &reset.story_id)?;
            let states = tx.state_map(reset.project)?;
            let mut done = current;
            done.completed = true;
            done.failure = None;
            done.residue = reset.residue.clone();
            let mut recovery = reset.recovery.clone().unwrap_or_default();
            recovery.cleared_awaiting = row.snapshot.awaiting.clone();
            done.recovery = Some(recovery);
            let to_todo = states
                .get("todo")
                .is_some_and(|state| state.super_state == SuperState::Open);
            if !to_todo {
                done.residue.push(crate::store::ResetResidue {
                    resource: "story state".into(),
                    reason: format!(
                        "the project has no open todo state, so the story keeps `{}`",
                        row.state
                    ),
                    blocks_dispatch: false,
                });
            }
            super::block_delivery::supersede_pending(
                tx,
                reset.project,
                reset.story,
                "card reset completed; prior session authority retired",
            )?;
            tx.put_story_reset(&done)?;
            if let Some(incident) = tx.verification_incident(reset.project)?
                && incident.story == reset.story
            {
                tx.clear_verification_incident(&incident.incident_id)?;
            }
            idle_lanes(tx, reset, now)?;
            let mut events = Vec::new();
            if to_todo {
                events.push(StoryEvent::StoryStateChanged {
                    at: now.into(),
                    state: "todo".into(),
                });
            }
            events.push(StoryEvent::StoryAwaitingCleared { at: now.into() });
            if let Some(hold) = summary::dispatch_hold(&done) {
                events.push(StoryEvent::StoryAwaitingSet {
                    at: now.into(),
                    awaiting: hold,
                });
            }
            events.push(StoryEvent::StoryCommentAdded {
                at: now.into(),
                text: summary::completion(&done),
            });
            let snapshot = append_reset_and_fold(
                tx,
                reset.project,
                reset.story,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &events,
                self.ctx.provenance(),
            )?;
            Ok(Some((row.snapshot, snapshot, done)))
        })
    }

    /// Completes the receipt and idles its lanes when the full finish keeps
    /// failing, so the reservation never strands the story. The story keeps
    /// its state; the comment names the error when the story still takes one.
    fn finish_degraded(
        &self,
        reset: &StoryReset,
        workspace: Option<WorkspaceLock>,
        error: &StoreError,
    ) -> Result<StoryReset, AppError> {
        let now = self.ctx.now();
        let note = format!("finished without returning the story to todo: {error}");
        let done = self.write(|tx| {
            let mut done = tx
                .story_reset(reset.project, reset.story)?
                .filter(|current| current.token == reset.token)
                .ok_or_else(|| StoreError::Invariant("reset disappeared".into()))?;
            if done.completed {
                return Ok(done);
            }
            done.completed = true;
            done.failure = Some(note.clone());
            done.residue = reset.residue.clone();
            done.recovery = reset.recovery.clone();
            super::block_delivery::supersede_pending(
                tx,
                reset.project,
                reset.story,
                "card reset completed; prior session authority retired",
            )?;
            tx.put_story_reset(&done)?;
            idle_lanes(tx, reset, &now)?;
            Ok(done)
        })?;
        let comment = patiently(&self.shutdown, None, || {
            self.ctx.write_stories(|tx| {
                let prefix = project_prefix(tx, reset.project)?;
                let (_, row) = resolve_open_story(tx, reset.project, &prefix, &reset.story_id)?;
                let states = tx.state_map(reset.project)?;
                append_reset_and_fold(
                    tx,
                    reset.project,
                    reset.story,
                    &prefix,
                    &states,
                    ExpectedSeq::Exact(row.head_seq),
                    &[StoryEvent::StoryCommentAdded {
                        at: now.clone(),
                        text: format!("Reset {} {note}.", reset.token),
                    }],
                    self.ctx.provenance(),
                )?;
                Ok(())
            })
        });
        // The receipt already carries the note; a refused comment is reported.
        if let Err(comment_error) = comment {
            crate::daemon::activity::emit(
                "ERROR",
                "reset",
                "event",
                &reset.story_id,
                &format!(
                    "reset {} {note}; its comment failed: {comment_error}",
                    reset.token
                ),
            );
        }
        drop(workspace);
        Ok(done)
    }
}

/// Idles each engine lane the reset reserved that still holds its story.
fn idle_lanes(tx: &mut impl WriteOps, reset: &StoryReset, now: &str) -> Result<(), StoreError> {
    for owner in &reset.lanes {
        if let Some(lane) = tx
            .engine_lanes(&owner.run_id)?
            .into_iter()
            .find(|lane| lane.lane_index == owner.lane_index)
        {
            // An already-started dispatch may fail and release its lane while reset waits.
            if lane.story_id.as_deref() != Some(&reset.story_id) {
                continue;
            }
            let mut idle = super::engine::idle_lane(&owner.run_id, owner.lane_index, now);
            idle.outcome = Some("story-reset".into());
            super::engine::put_or_retire_idle_lane(tx, &idle)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod executor_tests;
