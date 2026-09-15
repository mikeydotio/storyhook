//! Dropping retires execution, not the only copy of committed work.
mod process;
mod safety;

use super::{CleanupRemoval, CleanupSkip};
use crate::domain::{StoryCleanupLease, StoryEvent};
use crate::error::AppError;
use crate::service::{
    Ctx,
    executor_lock::ExecutorLock,
    resources::{ResourceOptions, ResourceService},
    story_reset::identity,
    workspace_lock::{self, WorkspaceLock},
};
use crate::store::{
    DroppedCleanup, DroppedCleanupPhase as Phase, GlobalSeq, ReadOps, Store, StoreError,
    StoredEvent, StoryNo, WriteOps,
};
use std::path::Path;

/// Identifies the latest abandonment event, independent of later discussion.
pub(super) fn generation(events: &[StoredEvent]) -> Option<GlobalSeq> {
    events.iter().rev().find_map(|event| match event.known() {
        Some(StoryEvent::StoryStateChanged { state, .. }) if state == "dropped" => {
            Some(event.global_seq)
        }
        _ => None,
    })
}

fn issue(lease: &StoryCleanupLease, reason: &str, detail: impl ToString) -> CleanupSkip {
    CleanupSkip {
        story_id: lease.story_id.clone(),
        reason: reason.into(),
        detail: detail.to_string(),
    }
}

/// Cleans one exact dropped workspace under durable and process-local ownership.
pub(super) fn run<S: Store>(
    ctx: &Ctx<'_, S>,
    repository: &Path,
    lease: &StoryCleanupLease,
    dry_run: bool,
) -> Result<Option<CleanupRemoval>, CleanupSkip> {
    let refuse = |e| issue(lease, "resource-unverifiable", e);
    if lease.repository_path != repository {
        return Err(issue(
            lease,
            "repository-mismatch",
            "lease does not name the registered checkout",
        ));
    }
    let controller_path = ctx
        .env()
        .store_path()
        .with_extension(format!("drop-{}.lock", lease.story_id));
    let controller = if dry_run {
        None
    } else {
        Some(
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&controller_path)
                .map_err(|e| refuse(AppError::from(e)))?,
        )
    };
    let _executor = controller
        .as_ref()
        .map(|file| ExecutorLock::acquire(file, &controller_path))
        .transpose()
        .map_err(|e| issue(lease, "cleanup-busy", e))?;
    let workspace = if dry_run {
        None
    } else {
        Some(
            WorkspaceLock::acquire(repository, &lease.story_id)
                .map_err(|e| issue(lease, "workspace-busy", e))?,
        )
    };
    let (story, drop_seq, old) = ctx
        .store()
        .read(|tx| {
            let prefix = crate::service::project_prefix(tx, ctx.project())?;
            let story = StoryNo::parse_id(&prefix, &lease.story_id)
                .map_err(|e| StoreError::Invariant(e.to_string()))?;
            let seq = require_dropped(tx, ctx.project(), story)?;
            Ok((story, seq, tx.dropped_cleanup(ctx.project(), story)?))
        })
        .map_err(|e| refuse(e.into()))?;
    let prior = old.filter(|r| r.generation == drop_seq && r.lease == *lease);
    let mut record = match prior {
        Some(record) => record,
        None => {
            let report = ResourceService::new(ctx)
                .resolve(
                    &lease.story_id,
                    &ResourceOptions {
                        lease_json: Some(
                            serde_json::to_string(lease).map_err(|e| refuse(e.into()))?,
                        ),
                        ..Default::default()
                    },
                )
                .map_err(refuse)?;
            safety::validate(ctx, repository, lease, &report, true)?;
            let paths = identity::capture(&report).map_err(refuse)?;
            let process_start = process::capture(&report).map_err(refuse)?;
            DroppedCleanup {
                project: ctx.project(),
                story,
                token: uuid::Uuid::new_v4().simple().to_string(),
                generation: drop_seq,
                lease: lease.clone(),
                resources: report,
                paths,
                process_start,
                phase: Phase::Prepared,
                released: true,
                failure: None,
            }
        }
    };
    let preflight = identity::validate(&record.paths)
        .map_err(refuse)
        .and_then(|()| {
            safety::validate(
                ctx,
                repository,
                lease,
                &record.resources,
                record.phase == Phase::Prepared,
            )
        });
    if let Err(error) = preflight {
        if !dry_run
            && !record.released
            && matches!(record.phase, Phase::Prepared | Phase::Quiescent)
        {
            record.released = true;
            record.failure = Some(format!("{}: {}", error.reason, error.detail));
            ctx.store()
                .write(|tx| tx.put_dropped_cleanup(&record))
                .map_err(|e| {
                    issue(
                        lease,
                        "dropped-cleanup-failed",
                        format!("{}; saving refusal failed: {e}", error.detail),
                    )
                })?;
        }
        return Err(error);
    }
    // A receipt cannot transfer deletion authority to a recreated workspace.
    let has_worktree = safety::worktree_present(lease).map_err(refuse)?;
    let panes = safety::panes(lease).map_err(refuse)?;
    if record.phase == Phase::Removed && (has_worktree || !panes.is_empty()) {
        return Err(issue(
            lease,
            "resource-identity-unsafe",
            "resources reappeared after this drop was cleaned; preserved replacements",
        ));
    }
    if record.released && record.phase == Phase::Removed {
        return Ok(None);
    }
    let removal = CleanupRemoval {
        story_id: lease.story_id.clone(),
        worktree: lease.worktree_path.clone(),
        branch: lease.branch.clone(),
        removed_worktree: has_worktree,
        removed_local_branch: false,
        removed_tmux_window: !panes.is_empty(),
        retained_local_branch: crate::service::resources::git::branch_exists(
            repository,
            &lease.branch,
        )
        .map_err(refuse)?,
        reclaimed_bytes: if has_worktree {
            super::directory_size(&lease.worktree_path)
        } else {
            0
        },
    };
    if dry_run {
        if record.phase == Phase::Stopping {
            return Err(issue(
                lease,
                "cleanup-recovery-required",
                "interrupted process cleanup must reconcile its journal; run story cleanup",
            ));
        }
        safety::validate(ctx, repository, lease, &record.resources, true)?;
        if record.phase == Phase::Prepared {
            safety::same_pane(lease, &record.resources).map_err(refuse)?;
        }
        return Ok((has_worktree || !panes.is_empty()).then_some(removal));
    }
    record.released = false;
    record.failure = None;
    ctx.store()
        .write(|tx| {
            if require_dropped(tx, ctx.project(), story)? != drop_seq {
                return Err(StoreError::Invariant(
                    "drop generation changed during cleanup admission".into(),
                ));
            }
            if tx.block_deliveries(ctx.project())?.iter().any(|delivery| {
                delivery.story == story
                    && delivery.status == crate::store::DeliveryStatus::Attempting
            }) {
                return Err(StoreError::Invariant(
                    "terminal delivery still owns this story; retry cleanup after it settles"
                        .into(),
                ));
            }
            tx.put_dropped_cleanup(&record)
        })
        .map_err(|e| issue(lease, "cleanup-busy", e))?;

    let result = execute(
        ctx,
        &mut record,
        workspace.as_ref().expect("real cleanup owns workspace"),
    );
    if let Err(error) = result {
        record.failure = Some(error.to_string());
        // Neither stage has an outstanding effect: writers stopped before Git starts.
        record.released = matches!(record.phase, Phase::Prepared | Phase::Quiescent);
        ctx.store()
            .write(|tx| tx.put_dropped_cleanup(&record))
            .map_err(|persist| {
                issue(
                    lease,
                    "dropped-cleanup-failed",
                    format!("{error}; recording progress also failed: {persist}"),
                )
            })?;
        return Err(issue(lease, "dropped-cleanup-failed", error));
    }
    Ok((has_worktree || !panes.is_empty()).then_some(removal))
}

fn require_dropped(
    tx: &impl ReadOps,
    project: crate::store::ProjectId,
    story: StoryNo,
) -> Result<GlobalSeq, StoreError> {
    let row = tx
        .story(project, story)?
        .ok_or_else(|| StoreError::NotFound("dropped cleanup story".into()))?;
    if row.state != "dropped" {
        return Err(StoreError::Invariant(format!(
            "cleanup story is {}, not dropped",
            row.state
        )));
    }
    generation(&tx.events_for(project, story)?)
        .ok_or_else(|| StoreError::Invariant("dropped story has no drop event".into()))
}

fn execute<S: Store>(
    ctx: &Ctx<'_, S>,
    record: &mut DroppedCleanup,
    workspace: &WorkspaceLock,
) -> Result<(), AppError> {
    let lease = record.lease.clone();
    if record.phase == Phase::Prepared {
        safety::same_pane(&lease, &record.resources)?;
        record.phase = Phase::Stopping;
        ctx.store().write(|tx| tx.put_dropped_cleanup(record))?;
    }
    if record.phase == Phase::Stopping {
        process::stop(ctx, record, workspace)?;
        if !safety::panes(&lease)?.is_empty() {
            return Err(AppError::Validation(
                "dropped story window remains after termination".into(),
            ));
        }
        record.phase = Phase::Quiescent;
        ctx.store().write(|tx| tx.put_dropped_cleanup(record))?;
    }
    identity::validate(&record.paths)?;
    safety::validate(ctx, &lease.repository_path, &lease, &record.resources, true)
        .map_err(|e| AppError::Validation(format!("{}: {}", e.reason, e.detail)))?;
    if !safety::panes(&lease)?.is_empty() {
        return Err(AppError::Validation(
            "dropped story window reappeared".into(),
        ));
    }
    if safety::worktree_present(&lease)? {
        record.phase = Phase::Removing;
        ctx.store().write(|tx| tx.put_dropped_cleanup(record))?;
        workspace_lock::git(
            &lease.repository_path,
            &[
                "worktree",
                "remove",
                "--",
                lease
                    .worktree_path
                    .to_str()
                    .ok_or_else(|| AppError::Validation("non-UTF8 worktree path".into()))?,
            ],
            Some(workspace),
        )?;
    }
    identity::validate(&record.paths)?;
    if safety::worktree_present(&lease)? || !safety::panes(&lease)?.is_empty() {
        return Err(AppError::Validation(
            "dropped cleanup postconditions failed: exact window or worktree remains".into(),
        ));
    }
    record.phase = Phase::Removed;
    record.released = true;
    ctx.store().write(|tx| tx.put_dropped_cleanup(record))?;
    Ok(())
}
