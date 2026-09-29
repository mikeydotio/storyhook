//! Dropping retires execution, not the only copy of committed work.
mod process;
mod safety;

use super::{CleanupRemoval, CleanupSkip};
use crate::domain::{StoryCleanupLease, StoryEvent, SuperState};
use crate::error::AppError;
use crate::service::{
    Ctx,
    executor_lock::ExecutorLock,
    resources::{ResourceOptions, ResourceService},
    story_reset::identity,
    workspace_lock::{self, WorkspaceLock},
};
use crate::store::{
    ClosureCleanup, DroppedCleanup, DroppedCleanupPhase as Phase, GlobalSeq, ReadOps, Store,
    StoreError, StoredEvent, WriteOps,
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
    request: &ClosureCleanup,
    delete_branch: bool,
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
    let story = request.story;
    let (old, legacy_generation) = ctx
        .store()
        .read(|tx| {
            require_closed(tx, request)?;
            let row = tx
                .story(ctx.project(), story)?
                .expect("validated closed story");
            let legacy = if row.state == "dropped" {
                generation(&tx.events_for(ctx.project(), story)?)
            } else {
                None
            };
            Ok((tx.dropped_cleanup(ctx.project(), story)?, legacy))
        })
        .map_err(|e| refuse(e.into()))?;
    let prior = old.filter(|r| {
        r.lease == *lease && (r.token == request.token || Some(r.generation) == legacy_generation)
    });
    if request.completed && prior.is_none() {
        return Err(issue(
            lease,
            "resource-identity-unsafe",
            "completed closure has no reservation for these resources; preserved replacements",
        ));
    }
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
            let process_start = process::capture(ctx.env(), &report).map_err(refuse)?;
            DroppedCleanup {
                project: ctx.project(),
                story,
                token: request.token.clone(),
                generation: request.generation,
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
    let panes = safety::panes(ctx.env(), lease).map_err(refuse)?;
    let branch_reappeared = record.phase == Phase::Removed
        && delete_branch
        && crate::service::resources::git::branch_exists(repository, &lease.branch)
            .map_err(refuse)?;
    if record.phase == Phase::Removed && (has_worktree || !panes.is_empty() || branch_reappeared) {
        return Err(issue(
            lease,
            "resource-identity-unsafe",
            "resources reappeared after this closure was cleaned; preserved replacements",
        ));
    }
    if record.released && record.phase == Phase::Removed {
        return Ok(None);
    }
    let mut removal = CleanupRemoval {
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
            safety::same_pane(ctx.env(), lease, &record.resources).map_err(refuse)?;
        }
        if delete_branch {
            let preview = super::clean_candidate_owned(ctx.env(), repository, lease, true, None)?;
            removal.removed_local_branch = preview.removed_local_branch;
            removal.retained_local_branch = false;
        }
        return Ok(
            (has_worktree || !panes.is_empty() || removal.removed_local_branch).then_some(removal),
        );
    }
    record.released = false;
    record.failure = None;
    ctx.store()
        .write(|tx| {
            require_closed(tx, request)?;
            let mut pinned = tx
                .closure_cleanup(ctx.project(), story)?
                .expect("validated closure");
            pinned.lease = Some(lease.clone());
            if !tx.update_closure_cleanup(&pinned)? {
                return Err(StoreError::Invariant(
                    "closure changed during cleanup admission".into(),
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
        delete_branch,
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
    if delete_branch {
        removal.removed_local_branch = removal.retained_local_branch;
        removal.retained_local_branch = false;
    }
    Ok((has_worktree || !panes.is_empty() || removal.removed_local_branch).then_some(removal))
}

fn require_closed(tx: &impl ReadOps, request: &ClosureCleanup) -> Result<(), StoreError> {
    let row = tx
        .story(request.project, request.story)?
        .ok_or_else(|| StoreError::NotFound("cleanup story".into()))?;
    let current = tx.closure_cleanup(request.project, request.story)?;
    let state = if crate::domain::is_epic(&row.snapshot) {
        let rows = tx.stories(request.project, &crate::store::StoryQuery::all())?;
        crate::store::effective_states(&rows, &tx.states(request.project)?)[&request.story]
            .1
            .clone()
    } else {
        row.superstate
    };
    if state != SuperState::Closed || current.as_ref().is_none_or(|r| r.token != request.token) {
        return Err(StoreError::Invariant(
            "closed cleanup lifecycle changed; preserved resources".into(),
        ));
    }
    Ok(())
}

fn execute<S: Store>(
    ctx: &Ctx<'_, S>,
    record: &mut DroppedCleanup,
    workspace: &WorkspaceLock,
    delete_branch: bool,
) -> Result<(), AppError> {
    let lease = record.lease.clone();
    if record.phase == Phase::Prepared {
        safety::same_pane(ctx.env(), &lease, &record.resources)?;
        record.phase = Phase::Stopping;
        ctx.store().write(|tx| tx.put_dropped_cleanup(record))?;
    }
    if record.phase == Phase::Stopping {
        process::stop(ctx, record, workspace)?;
        if !safety::panes(ctx.env(), &lease)?.is_empty() {
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
    if !safety::panes(ctx.env(), &lease)?.is_empty() {
        return Err(AppError::Validation(
            "dropped story window reappeared".into(),
        ));
    }
    if delete_branch {
        if record.phase == Phase::Quiescent {
            super::clean_candidate_owned(
                ctx.env(),
                &lease.repository_path,
                &lease,
                true,
                Some(workspace),
            )
            .map_err(|e| AppError::Validation(format!("{}: {}", e.reason, e.detail)))?;
        }
        record.phase = Phase::Removing;
        ctx.store().write(|tx| tx.put_dropped_cleanup(record))?;
        super::clean_candidate_owned(
            ctx.env(),
            &lease.repository_path,
            &lease,
            false,
            Some(workspace),
        )
        .map_err(|e| AppError::Validation(format!("{}: {}", e.reason, e.detail)))?;
    } else if safety::worktree_present(&lease)? {
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
    if safety::worktree_present(&lease)? || !safety::panes(ctx.env(), &lease)?.is_empty() {
        return Err(AppError::Validation(
            "dropped cleanup postconditions failed: exact window or worktree remains".into(),
        ));
    }
    record.phase = Phase::Removed;
    record.released = true;
    ctx.store().write(|tx| tx.put_dropped_cleanup(record))?;
    Ok(())
}
