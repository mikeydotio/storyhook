//! A card reset holds readiness until exact resource cleanup succeeds.
mod cleanup;
pub(crate) mod identity;

use super::executor_lock::ExecutorLock;
use super::workspace_lock::WorkspaceLock;
use super::{Ctx, StoryService, append_and_fold, project_prefix, resolve_open_story};
use crate::domain::{StoryEvent, SuperState};
use crate::error::AppError;
use crate::store::{
    ExpectedSeq, ProjectId, ReadOps, Store, StoreError, StoryNo, StoryReset, WriteOps,
};

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

/// Transactional card-reset coordinator; external cleanup runs outside store locks.
pub struct StoryResetService<'a, S: Store> {
    ctx: &'a Ctx<'a, S>,
}

impl<'a, S: Store> StoryResetService<'a, S> {
    /// Binds reset operations to the caller's project and environment.
    pub fn new(ctx: &'a Ctx<'a, S>) -> Self {
        Self { ctx }
    }

    /// Reserves one ordinary open story after exact typed-ID confirmation.
    pub fn reserve(&self, id: &str, confirmation: &str) -> Result<StoryReset, AppError> {
        Ok(self.ctx.store().write(|tx| {
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
            if tx.engine_reset(project, story)?.is_some() {
                return Err(AppError::Validation(
                    "Stop Now already owns this story reset; finish that operation first".into(),
                )
                .into());
            }
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
            };
            tx.put_story_reset(&reset)?;
            Ok(reset)
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

    /// Resumes an operation under a cross-process lock, then commits its receipt.
    /// `quiesce` must join dispatch and the selected verifier before cleanup.
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
        let _executor = ExecutorLock::acquire(&lock, &lock_path).map_err(|e| {
            AppError::Validation(format!(
                "reset {} already running or lock unavailable: {e}",
                reset.token
            ))
        })?;
        reset = self.get(id, token)?;
        if reset.completed {
            return Ok(reset);
        }
        reset.failure = None;
        self.ctx.store().write(|tx| tx.put_story_reset(&reset))?;
        let result = (|| {
            quiesce()?;
            let report = match &reset.resources {
                Some(report) => report.clone(),
                None => super::resources::ResourceService::new(self.ctx)
                    .resolve(id, &Default::default())?,
            };
            // Dispatch and verification release the shared workspace before we
            // acquire it; surviving cleanup children retain this same ownership.
            let workspace = report
                .repository
                .as_ref()
                .map(|repository| WorkspaceLock::acquire(repository, &reset.story_id))
                .transpose()?;
            if reset.resources.is_none() {
                cleanup::validate(&report, self.ctx.cwd(), self.ctx.env())?;
                reset.paths = identity::capture(&report)?;
                reset.resources = Some(report);
                self.ctx.store().write(|tx| tx.put_story_reset(&reset))?;
            }
            self.ctx.store().write(|tx| {
                super::block_delivery::supersede_pending(
                    tx,
                    reset.project,
                    reset.story,
                    "card reset owns workspace replacement",
                )?;
                Ok(())
            })?;
            cleanup::remove(
                reset.resources.as_ref().expect("pinned resources"),
                &reset.paths,
                self.ctx.cwd(),
                self.ctx.env(),
                workspace.as_ref(),
            )?;
            self.finish(&reset, workspace)
        })();
        match result {
            Ok(done) => Ok(done),
            Err(error) => {
                reset.failure = Some(error.to_string());
                self.ctx.store().write(|tx| tx.put_story_reset(&reset)).map_err(|persist| AppError::Storage(format!("reset {} failed: {error}; recording diagnostics also failed: {persist}", reset.token)))?;
                Err(error)
            }
        }
    }

    fn finish(
        &self,
        reset: &StoryReset,
        workspace: Option<WorkspaceLock>,
    ) -> Result<StoryReset, AppError> {
        let now = self.ctx.now();
        let (before, snapshot, done) = self.ctx.write_stories(|tx| {
            let current = tx.story_reset(reset.project, reset.story)?.ok_or_else(|| StoreError::Invariant("reset disappeared".into()))?;
            if current.token != reset.token || current.completed { return Err(StoreError::Invariant("reset owner changed".into())); }
            let prefix = project_prefix(tx, reset.project)?;
            let (_, row) = resolve_open_story(tx, reset.project, &prefix, &reset.story_id)?;
            let states = tx.state_map(reset.project)?;
            let mut done = current;
            done.completed = true;
            done.failure = None;
            super::block_delivery::supersede_pending(tx, reset.project, reset.story, "card reset completed; prior session authority retired")?;
            tx.put_story_reset(&done)?;
            if let Some(incident) = tx.verification_incident(reset.project)? && incident.story == reset.story {
                tx.clear_verification_incident(&incident.incident_id)?;
            }

            for owner in &reset.lanes {
                if let Some(lane) = tx.engine_lanes(&owner.run_id)?.into_iter().find(|lane| lane.lane_index == owner.lane_index) {
                    // An already-started dispatch may fail and release its lane while reset waits.
                    if lane.story_id.as_deref() != Some(&reset.story_id) { continue; }
                    let mut idle = super::engine::idle_lane(&owner.run_id, owner.lane_index, &now);
                    idle.outcome = Some("story-reset".into());
                    super::engine::put_or_retire_idle_lane(tx, &idle)?;
                }
            }
            let events = [StoryEvent::StoryStateChanged { at: now.clone(), state: "todo".into() }, StoryEvent::StoryAwaitingCleared { at: now.clone() }, StoryEvent::StoryCommentAdded { at: now.clone(), text: format!("Reset {} completed. Stopped the story worker. Removed its worktree and local branch. Released ownership and returned to todo. Preserved remote branches and pull requests.", reset.token) }];
            let snapshot = append_and_fold(tx, reset.project, reset.story, &prefix, &states, ExpectedSeq::Exact(row.head_seq), &events, self.ctx.provenance())?;
            Ok((row.snapshot, snapshot, done))
        })?;
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
}

#[cfg(test)]
mod executor_tests;
