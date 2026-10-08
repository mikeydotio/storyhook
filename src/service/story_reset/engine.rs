//! Stop Now uses the shared teardown with its own narrower, exact lane owner.
use super::{cleanup, identity, summary};
use crate::error::AppError;
use crate::service::Ctx;
use crate::service::engine::DispatchOutcome;
use crate::service::resources::{ResourceOptions, ResourceService};
use crate::service::workspace_lock::WorkspaceLock;
use crate::store::patience::{Shutdown, patiently};
use crate::store::{
    EngineReset, EngineResetCleanup, ReadOps, Store, StoreError, StoryReset, WriteOps,
};

fn current(tx: &impl ReadOps, requested: &EngineReset) -> Result<EngineReset, StoreError> {
    if !tx
        .engine_run(&requested.run_id)?
        .is_some_and(|run| run.is_stopping())
        || !tx.engine_lanes(&requested.run_id)?.iter().any(|lane| {
            lane.lane_index == requested.lane_index
                && lane.story_id.as_deref() == Some(requested.lease.story_id.as_str())
                && lane.cleanup_lease.as_ref() == Some(&requested.lease)
        })
    {
        return Err(StoreError::Invariant(
            "Stop Now no longer owns its run and lane".into(),
        ));
    }
    tx.engine_reset(requested.project, requested.story)?
        .filter(|owner| {
            owner.token == requested.token
                && owner.run_id == requested.run_id
                && owner.lane_index == requested.lane_index
                && owner.lease == requested.lease
        })
        .ok_or_else(|| StoreError::Invariant("Stop Now reservation was superseded".into()))
}

/// A guarded replacement within one transaction. A deleted owner is never
/// re-created; old JSON rows need no schema rewrite to acquire optional progress.
fn save<S: Store>(
    ctx: &Ctx<'_, S>,
    expected: &EngineReset,
    progress: &EngineResetCleanup,
) -> Result<EngineReset, AppError> {
    Ok(patiently(&Shutdown::new(), None, || {
        ctx.store().write(|tx| {
            let mut owner = current(tx, expected)?;
            if owner.cleanup != expected.cleanup {
                return Err(StoreError::Invariant("Stop Now progress changed".into()));
            }
            // The existing insertion path enforces immutable ownership. Replace
            // under the same transaction instead of relaxing that public contract.
            tx.remove_engine_reset(&owner)?;
            owner.cleanup = Some(progress.clone());
            tx.put_engine_reset(&owner)?;
            Ok(owner)
        })
    })?)
}

/// Stop Now retains its original marker requirement, narrower than a card reset.
fn lease_guard(reset: &EngineReset, pinned: bool) -> Result<(), AppError> {
    use crate::service::resources::git;
    let lease = &reset.lease;
    let records = git::inventory(&lease.repository_path)?;
    if let Some(record) = records.iter().find(|row| row.path == lease.worktree_path) {
        if record.branch.as_ref() != Some(&lease.branch) {
            return Err(AppError::Validation(
                "Stop Now worktree branch changed".into(),
            ));
        }
        let private = git::text(&lease.worktree_path, &["rev-parse", "--absolute-git-dir"])?;
        let marker = std::path::Path::new(private.trim()).join(crate::domain::CLEANUP_LEASE_MARKER);
        let metadata = std::fs::symlink_metadata(&marker).map_err(|e| {
            AppError::Validation(format!("Stop Now cleanup marker unavailable: {e}"))
        })?;
        if !metadata.file_type().is_file() {
            return Err(AppError::Validation(
                "Stop Now cleanup marker is not a regular file".into(),
            ));
        }
        let bytes = std::fs::read(&marker)
            .map_err(|e| AppError::Validation(format!("reading Stop Now marker: {e}")))?;
        let observed: crate::domain::StoryCleanupLease = serde_json::from_slice(&bytes)
            .map_err(|e| AppError::Validation(format!("decoding Stop Now marker: {e}")))?;
        if observed != *lease {
            return Err(AppError::Validation(
                "Stop Now cleanup marker changed".into(),
            ));
        }
    } else if !pinned && std::fs::symlink_metadata(&lease.worktree_path).is_ok() {
        return Err(AppError::Validation(
            "Stop Now cannot adopt an unregistered directory without previously pinned identity"
                .into(),
        ));
    }
    Ok(())
}

pub(crate) fn execute<S: Store>(
    ctx: &Ctx<'_, S>,
    request: &EngineReset,
    workspace: &WorkspaceLock,
) -> Result<DispatchOutcome, AppError> {
    let mut owner = ctx.store().read(|tx| current(tx, request))?;
    if owner.cleanup.is_none() {
        let origin = ctx.store().read(|tx| {
            Ok(tx
                .engine_run(&owner.run_id)?
                .and_then(|run| run.stop_origin))
        })?;
        let mut resources = ResourceService::new(ctx)
            .resolve(
                &owner.lease.story_id,
                &ResourceOptions {
                    lease_json: Some(
                        serde_json::to_string(&owner.lease)
                            .map_err(|e| AppError::Storage(e.to_string()))?,
                    ),
                    ..Default::default()
                },
            )
            .unwrap_or_else(|error| crate::service::resources::ResourceReport {
                location_only: false,
                project: owner.lease.project_slug.clone(),
                story_id: owner.lease.story_id.clone(),
                status: "unavailable".into(),
                repository: Some(owner.lease.repository_path.clone()),
                worktree: Some(owner.lease.worktree_path.clone()),
                branch: Some(owner.lease.branch.clone()),
                window_name: owner.lease.story_id.clone(),
                socket_path: Some(owner.lease.tmux.socket_path.clone()),
                pane: None,
                provider: None,
                candidates: Vec::new(),
                observations: Vec::new(),
                diagnostics: vec![format!("observing exact Stop Now lease: {error}")],
            });
        if let Err(error) = lease_guard(&owner, false) {
            resources.status = "invalid".into();
            resources.diagnostics.push(error.to_string());
        }
        let paths = match identity::capture(&resources) {
            Ok(paths) => paths,
            Err(error) => {
                resources.status = "unavailable".into();
                resources
                    .diagnostics
                    .push(format!("pinning Stop Now resources: {error}"));
                Vec::new()
            }
        };
        let fallback = crate::store::ResetOrigin::default();
        let from = origin.as_ref().unwrap_or(&fallback);
        let caller = from.cwd.as_deref().unwrap_or(&owner.lease.repository_path);
        let mut residue = cleanup::Residue::default();
        let authority =
            cleanup::authorize(&resources, &paths, from, caller, ctx.env(), &mut residue);
        let mut recovery = cleanup::recovery(&resources, &authority);
        recovery.cleared_awaiting = ctx.store().read(|tx| {
            Ok(tx
                .story(owner.project, owner.story)?
                .and_then(|story| story.awaiting))
        })?;
        let progress = EngineResetCleanup {
            origin,
            resources,
            paths,
            recovery,
            residue: Vec::new(),
            completed: false,
        };
        owner = save(ctx, &owner, &progress)?;
    }
    let mut progress = owner.cleanup.clone().expect("pinned above");
    if !progress.completed {
        let check = || -> Result<(), AppError> {
            ctx.store().read(|tx| current(tx, &owner))?;
            Ok(())
        };
        check()?;
        let fallback = crate::store::ResetOrigin::default();
        let origin = progress.origin.as_ref().unwrap_or(&fallback);
        let caller = origin
            .cwd
            .as_deref()
            .unwrap_or(&owner.lease.repository_path);
        let mut residue = cleanup::Residue::default();
        let mut observed = progress.resources.clone();
        if let Err(error) = lease_guard(&owner, true) {
            observed.status = "invalid".into();
            observed.diagnostics.push(error.to_string());
        }
        if let (Some(repository), Some(branch), Some(expected)) = (
            observed.repository.as_ref(),
            progress.recovery.branch.as_ref(),
            progress.recovery.tip.as_ref(),
        ) {
            match crate::service::resources::git::branch_exists(repository, branch) {
                Ok(false) => {}
                Ok(true) => {
                    let actual = crate::service::resources::git::text(
                        repository,
                        &[
                            "rev-parse",
                            "--verify",
                            &format!("refs/heads/{branch}^{{commit}}"),
                        ],
                    );
                    if !actual.is_ok_and(|tip| tip.trim() == expected) {
                        observed.status = "invalid".into();
                        observed.diagnostics.push(
                            "local branch changed after Stop Now pinned its recovery tip".into(),
                        );
                    }
                }
                Err(error) => {
                    observed.status = "unavailable".into();
                    observed
                        .diagnostics
                        .push(format!("rechecking pinned branch: {error}"));
                }
            }
        }
        let authority = cleanup::authorize(
            &observed,
            &progress.paths,
            origin,
            caller,
            ctx.env(),
            &mut residue,
        );
        let remove_check = || -> Result<(), AppError> {
            check()?;
            if matches!(observed.status.as_str(), "resolved" | "absent") {
                lease_guard(&owner, true)?;
                if let (Some(repository), Some(branch), Some(expected)) = (
                    observed.repository.as_ref(),
                    progress.recovery.branch.as_ref(),
                    progress.recovery.tip.as_ref(),
                ) && crate::service::resources::git::branch_exists(repository, branch)?
                {
                    let actual = crate::service::resources::git::text(
                        repository,
                        &[
                            "rev-parse",
                            "--verify",
                            &format!("refs/heads/{branch}^{{commit}}"),
                        ],
                    )?;
                    if actual.trim() != expected {
                        return Err(AppError::Validation(
                            "Stop Now branch changed before removal".into(),
                        ));
                    }
                }
            }
            Ok(())
        };
        if progress.origin.is_some() {
            cleanup::remove_checked(
                &observed,
                &progress.paths,
                &authority,
                &origin.caller,
                ctx.env(),
                Some(workspace),
                &mut residue,
                Some(&remove_check),
            )?;
        } else {
            // Pre-upgrade intent cannot supply the original terminal or cwd.
            // Leave its resources for a fresh explicit story reset, never use
            // the background worker's identity as permission to remove them.
            residue.leave("legacy Stop Now resources", "the accepted request predates durable caller identity; use an explicit story reset to discard remaining resources");
        }
        cleanup::dispatch_overlap(&observed, &authority, ctx.env(), &mut residue);
        progress.residue = residue.into_entries();
        // Engine policy is narrower than the final-lever story reset: any
        // uncertainty keeps automatic redispatch away from the retained work.
        for entry in &mut progress.residue {
            entry.blocks_dispatch = true;
        }
        progress.completed = true;
        check()?;
        owner = save(ctx, &owner, &progress)?;
    } else {
        // A crash can separate removal from finalization. Observe new residue
        // before releasing readiness, but never repeat destructive work or
        // replace the original recovery record with post-removal observations.
        let fallback = crate::store::ResetOrigin::default();
        let origin = progress.origin.as_ref().unwrap_or(&fallback);
        let caller = origin
            .cwd
            .as_deref()
            .unwrap_or(&owner.lease.repository_path);
        let mut residue = cleanup::Residue::default();
        for entry in &progress.residue {
            residue.leave(&entry.resource, &entry.reason);
        }
        let authority = cleanup::authorize(
            &progress.resources,
            &progress.paths,
            origin,
            caller,
            ctx.env(),
            &mut residue,
        );
        cleanup::dispatch_overlap(&progress.resources, &authority, ctx.env(), &mut residue);
        let mut entries = residue.into_entries();
        for entry in &mut entries {
            entry.blocks_dispatch = true;
        }
        if entries != progress.residue {
            progress.residue = entries;
            owner = save(ctx, &owner, &progress)?;
        }
    }
    Ok(DispatchOutcome::from_payload(serde_json::json!({
        "ok":true, "native_reset":1, "token":owner.token, "lease":owner.lease, "cleanup":progress
    })))
}

/// The shared summary needs a value, not another durable lifecycle owner.
fn summary_value(reset: &EngineReset, cleanup: &EngineResetCleanup) -> StoryReset {
    StoryReset {
        project: reset.project,
        story: reset.story,
        story_id: reset.lease.story_id.clone(),
        token: reset.token.clone(),
        original_state: String::new(),
        lanes: Vec::new(),
        resources: Some(cleanup.resources.clone()),
        paths: cleanup.paths.clone(),
        completed: cleanup.completed,
        failure: None,
        residue: cleanup.residue.clone(),
        recovery: Some(cleanup.recovery.clone()),
        origin: cleanup.origin.clone().unwrap_or_default(),
    }
}

pub(crate) fn completion(
    reset: &EngineReset,
    cleanup: &EngineResetCleanup,
    target: &str,
) -> String {
    format!(
        "Full Auto Stop Now run `{}` lane {}. {}",
        reset.run_id,
        reset.lane_index,
        summary::completion_to(&summary_value(reset, cleanup), target)
    )
}

pub(crate) fn hold(reset: &EngineReset, cleanup: &EngineResetCleanup) -> Option<String> {
    summary::dispatch_hold(&summary_value(reset, cleanup))
}
