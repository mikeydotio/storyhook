//! Publish committed repairs without acquiring a verifier slot or agent workspace.
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::verification::ShellVerificationActuator;
use crate::domain::landing::SubmissionOutcome;
use crate::env::Environment;
use crate::error::AppError;
use crate::process::Cancellation;
use crate::service::repair_publication::{self as spool, Request};
use crate::service::{Ctx, StoryService, VerificationCandidate};
use crate::store::{ReadOps, Store, StoryNo};

/// Recover retained requests at startup and process new notifications promptly.
pub(crate) fn poll<S: Store>(store: &S, env: &Environment, stop: &AtomicBool) {
    let cancel = Cancellation::default();
    let actuator = ShellVerificationActuator::new(env.clone());
    std::thread::scope(|scope| {
        let cancellation = &cancel;
        scope.spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(super::serve::SHUTDOWN_CHECK);
            }
            cancellation.cancel();
        });
        while !cancel.is_cancelled() {
            if let Err(error) = tick_with(store, env, &actuator, &cancel) {
                super::activity::emit(
                    "ERROR",
                    "publication",
                    "event",
                    "repair spool",
                    &error.to_string(),
                );
            }
            std::thread::sleep(super::serve::SHUTDOWN_CHECK);
        }
    });
}

/// Runs due publication work through the production transport and receipt validator.
pub fn tick_with<S: Store>(
    store: &S,
    env: &Environment,
    actuator: &ShellVerificationActuator,
    cancel: &Cancellation,
) -> Result<(), AppError> {
    let mut seen = BTreeSet::new();
    let mut failures = Vec::new();
    for (_, request) in spool::pending(env)? {
        if cancel.is_cancelled() {
            break;
        }
        let key = (
            request.lease.project_slug.clone(),
            request.lease.story_id.clone(),
        );
        if request.retry_at.as_ref().is_some_and(|at| at > &env.now()) || !seen.insert(key) {
            continue;
        }
        if let Err(error) = process(store, env, actuator, &request, cancel, false) {
            failures.push(error.to_string());
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(AppError::Storage(failures.join("; ")))
    }
}

/// Flush pending publication before an override. Failure never revokes the override.
pub(crate) fn flush<S: Store>(ctx: &Ctx<'_, S>, id: &str) -> Result<(), AppError> {
    let project = ctx
        .store()
        .read(|tx| tx.project(ctx.project()))?
        .ok_or_else(|| AppError::Storage("publication project disappeared".into()))?;
    let number = StoryNo::parse_id(&project.prefix, id)?;
    let canonical = number.to_id(&project.prefix);
    // Close the gap between Git creating a commit and its hook being served.
    // An old lease cannot enqueue through a replacement worktree marker.
    let lease = ctx.store().read(|tx| {
        Ok(tx
            .events_for(project.id, number)?
            .iter()
            .rev()
            .find_map(|e| match e.known() {
                Some(crate::domain::StoryEvent::StoryCleanupLeaseRecorded { lease, .. }) => {
                    Some(lease.as_ref().clone())
                }
                _ => None,
            }))
    })?;
    if let Some(lease) = lease
        && lease.worktree_path.exists()
    {
        spool::validate_marker(&lease)?;
        let lane = Ctx::new(
            ctx.store(),
            ctx.project(),
            &lease.worktree_path,
            ctx.env().clone(),
        );
        spool::enqueue(&lane)?;
    }
    let pending = spool::pending(ctx.env())?;
    let Some((_, request)) = pending.into_iter().find(|(_, request)| {
        request.lease.project_slug == project.slug && request.lease.story_id == canonical
    }) else {
        return Ok(());
    };
    process(
        ctx.store(),
        ctx.env(),
        &ShellVerificationActuator::new(ctx.env().clone()),
        &request,
        &Cancellation::default(),
        true,
    )
}

fn process<S: Store>(
    store: &S,
    env: &Environment,
    actuator: &ShellVerificationActuator,
    requested: &Request,
    cancel: &Cancellation,
    wait: bool,
) -> Result<(), AppError> {
    let project = store
        .read(|tx| tx.projects())?
        .into_iter()
        .find(|p| p.slug == requested.lease.project_slug)
        .ok_or_else(|| {
            AppError::Storage(format!(
                "publication project {} disappeared",
                requested.lease.project_slug
            ))
        })?;
    let Some(_automation) = crate::service::automations::enter(store, env, project.id)? else {
        return Ok(());
    };
    if store.read(|tx| Ok(tx.settings(project.id)?.automations_after))?
        != requested.automation_generation
    {
        return Ok(());
    }
    // A separate controller lock protects request acknowledgement and retry
    // evidence. Transport also locks against ordinary verifier submission.
    let start = Instant::now();
    let _owner = loop {
        if let Some(owner) = crate::service::workspace_lock::WorkspaceLock::try_at(
            &env.daemon_state_dir().join("publication-controllers"),
            &format!("{}-{}", project.id.get(), requested.lease.story_id),
        )? {
            break owner;
        }
        if !wait {
            return Ok(());
        }
        if cancel.is_cancelled() || start.elapsed() >= Duration::from_secs(180) {
            return Err(AppError::Storage(
                "repair publication is still pending; preserve the branch".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    // Re-read under ownership: a competing flush may already have completed.
    let mut requests: Vec<_> = spool::pending(env)?
        .into_iter()
        .filter(|(_, r)| r.lease == requested.lease && r.pull_request == requested.pull_request)
        .collect();
    if requests.is_empty() {
        return Ok(());
    }
    let ctx = Ctx::new(
        store,
        project.id,
        &requested.lease.worktree_path,
        env.clone(),
    )
    .no_hooks(true);
    let result = publish(&ctx, actuator, &requests, cancel);
    match result {
        Ok(head) => {
            if requests.iter().any(|(_, r)| r.error.is_some()) {
                StoryService::new(&ctx).comment(
                    &requested.lease.story_id,
                    &format!(
                        "REPAIR PUBLICATION RECOVERED — {} at {head} is published to {}.",
                        requested.lease.branch, requested.pull_request
                    ),
                )?;
            }
            for (path, _) in requests {
                std::fs::remove_file(path)?;
            }
            std::fs::File::open(spool::directory(env))?.sync_all()?;
            super::activity::emit(
                "INFO",
                "publication",
                "event",
                &requested.lease.story_id,
                &format!(
                    "published {} at {head} to {}",
                    requested.lease.branch, requested.pull_request
                ),
            );
            Ok(())
        }
        Err(error) => {
            let detail = format!(
                "{} on {} for {}: {error}",
                requested.head, requested.lease.branch, requested.pull_request
            );
            if requests
                .iter()
                .any(|(_, r)| r.error.as_ref() != Some(&detail))
            {
                StoryService::new(&ctx).comment(&requested.lease.story_id,
                    &format!("REPAIR PUBLICATION FAILED\n\n{}\n\nThe local branch is preserved. Publication will retry. Overrides remain allowed.", crate::text_lint::quote_evidence(&detail)))?;
            }
            let retry_at = (chrono::DateTime::parse_from_rfc3339(&env.now())
                .map_err(|e| AppError::Storage(e.to_string()))?
                + chrono::Duration::seconds(30))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            for (path, r) in &mut requests {
                r.error = Some(detail.clone());
                r.retry_at = Some(retry_at.clone());
                spool::retain(path, r)?;
            }
            Err(AppError::Storage(detail))
        }
    }
}

fn publish<S: Store>(
    ctx: &Ctx<'_, S>,
    actuator: &ShellVerificationActuator,
    requests: &[(PathBuf, Request)],
    cancel: &Cancellation,
) -> Result<String, AppError> {
    let request = &requests[0].1;
    if !request.lease.worktree_path.exists() {
        // Cleanup can finish after the push but before its acknowledgement.
        // Missing local resources are not evidence of a successful publication.
        let linked = ctx.store().read(|tx| {
            let prefix = crate::service::project_prefix(tx, ctx.project())?;
            let number = StoryNo::parse_id(&prefix, &request.lease.story_id)?;
            Ok(tx.pr_links(ctx.project())?.iter().any(|(story, p)| {
                *story == number
                    && p.url == request.pull_request
                    && p.status == "merged"
                    && p.close_on_merge
            }))
        })?;
        if linked && let Some(merged) = spool::guard_merged_pr(ctx, &request.lease)? {
            for (_, r) in requests {
                crate::service::resources::git::text(
                    &r.lease.repository_path,
                    &["merge-base", "--is-ancestor", &r.head, &merged],
                )?;
            }
            return Ok(merged);
        }
        return Err(AppError::Storage(
            "publication worktree is missing without merged PR proof; preserve the request".into(),
        ));
    }
    spool::validate_marker(&request.lease)?;
    crate::service::resources::validate_lease(&request.lease)?;
    let (project, row, link) = ctx.store().read(|tx| {
        let project = tx.project(ctx.project())?.ok_or_else(|| {
            crate::store::StoreError::NotFound("publication project disappeared".into())
        })?;
        let number = StoryNo::parse_id(&project.prefix, &request.lease.story_id)?;
        let row = tx.story(project.id, number)?.ok_or_else(|| {
            crate::store::StoreError::NotFound("publication story disappeared".into())
        })?;
        let links = tx.open_pr_links_for_story(project.id, number)?;
        let link = links
            .into_iter()
            .find(|p| p.url == request.pull_request && p.close_on_merge)
            .ok_or_else(|| {
                crate::store::StoreError::Validation(
                    "publication PR is no longer linked and open".into(),
                )
            })?;
        Ok((project, row, link))
    })?;
    let head = crate::service::resources::git::text(ctx.cwd(), &["rev-parse", "HEAD^{commit}"])?
        .trim()
        .to_owned();
    for (_, r) in requests {
        crate::service::resources::git::text(
            ctx.cwd(),
            &["merge-base", "--is-ancestor", &r.head, &head],
        )?;
    }
    let candidate = VerificationCandidate {
        blocked_by: vec![],
        landing_pending: false,
        project: project.id,
        project_slug: project.slug,
        story_id: request.lease.story_id.clone(),
        title: row.snapshot.title,
        priority: row.snapshot.priority,
        created_at: row.snapshot.created_at,
        verifying_since: None,
        verifying_generation: None,
        blocking_revision: None,
        human_only_revision: None,
        checkout: request.lease.repository_path.clone(),
        cleanup_lease: Some(request.lease.clone()),
        pull_request: Ok(link),
    };
    match actuator
        .publish_repair(&candidate, &head, &request.pull_request, cancel)
        .map_err(|e| AppError::Storage(format!("{e:?}")))?
    {
        SubmissionOutcome::PullRequest(p) if p.head_oid == head => Ok(head),
        SubmissionOutcome::AlreadyLanded(e) if e.head_oid == head => Ok(head),
        _ => Err(AppError::Storage(
            "publication receipt names another commit".into(),
        )),
    }
}
