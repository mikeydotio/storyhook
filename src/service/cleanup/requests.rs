//! Closure request retry policy and durable diagnostics.
use super::CleanupSkip;
use crate::domain::StoryEvent;
use crate::error::AppError;
use crate::service::{
    Ctx,
    resources::{ResourceOptions, ResourceService},
    verification::VerificationGeneration,
};
use crate::store::{ClosureCleanup, ExpectedSeq, ReadOps, Store, WriteOps};

/// Recovery wake and persisted backoff use the same minimum interval.
pub(crate) const RETRY_SECONDS: i64 = 30;

/// Selects unfinished requests whose persisted backoff has expired.
pub(crate) fn due(request: &ClosureCleanup, now: &str) -> bool {
    !request.completed && request.retry_at.as_deref().is_none_or(|at| at <= now)
}

/// Proves that no locally discoverable resource needs cleanup authority.
pub(super) fn without_lease(ctx: &Ctx<'_, impl Store>, id: &str) -> Result<(), CleanupSkip> {
    let report = ResourceService::new(ctx)
        .resolve(id, &ResourceOptions::default())
        .map_err(|e| CleanupSkip {
            story_id: id.into(),
            reason: "resource-unverifiable".into(),
            detail: e.to_string(),
        })?;
    if report.status == "absent" && report.pane.is_none() && report.candidates.is_empty() {
        return Ok(());
    }
    Err(CleanupSkip {
        story_id: id.into(),
        reason: "missing-lease".into(),
        detail: format!(
            "resources have no agreed cleanup lease: {}; {}",
            report.status,
            report.diagnostics.join("; ")
        ),
    })
}

/// Requires completion evidence from the current lifecycle for branch deletion.
pub(super) fn completed_work(
    ctx: &Ctx<'_, impl Store>,
    request: &ClosureCleanup,
    generation: Option<&VerificationGeneration>,
) -> Result<bool, AppError> {
    let Some(generation) = generation else {
        return Ok(false);
    };
    // Existing receipts remain evidence, but an old generation's lease cannot
    // authorize deleting the current workspace's branch.
    if generation.lease.is_none() || generation.lease != request.lease {
        return Ok(false);
    }
    let current_lifecycle = ctx.store().read(|tx| {
        let row = tx.story(request.project, request.story)?;
        if row.is_none_or(|r| r.state == "dropped") {
            return Ok(false);
        }
        let states = tx.state_map(request.project)?;
        let events = tx.events_for(request.project, request.story)?;
        // Reopening without resubmission must not borrow the prior verdict.
        Ok(events
            .iter()
            .rev()
            .find_map(|event| match event.known() {
                Some(StoryEvent::StoryStateChanged { state, .. })
                    if states
                        .get(state)
                        .is_none_or(|s| s.super_state != crate::domain::SuperState::Closed) =>
                {
                    Some(state == crate::service::verification::VERIFYING_STATE)
                }
                _ => None,
            })
            .unwrap_or(false))
    })?;
    if !current_lifecycle {
        return Ok(false);
    }
    if generation.landed || generation.reap_marker.is_some() {
        return Ok(true);
    }
    Ok(generation.overridden
        && ctx.store().read(|tx| {
            Ok(tx.pr_links(ctx.project())?.iter().any(|(story, link)| {
                *story == request.story && link.close_on_merge && link.status == "merged"
            }))
        })?)
}

/// Records a token-guarded receipt or retry and emits only changed diagnostics.
pub(crate) fn finish(
    ctx: &Ctx<'_, impl Store>,
    request: &ClosureCleanup,
    issue: Option<&CleanupSkip>,
) -> Result<(), AppError> {
    let detail = issue.map(|i| format!("{}: {}", i.reason, i.detail));
    let retry_at = if issue.is_some() {
        Some(
            (chrono::DateTime::parse_from_rfc3339(&ctx.now())
                .map_err(|e| AppError::Storage(format!("cleanup clock: {e}")))?
                + chrono::Duration::seconds(RETRY_SECONDS))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        )
    } else {
        None
    };
    ctx.write_stories(|tx| {
        let Some(mut current) = tx.closure_cleanup(ctx.project(), request.story)? else {
            return Ok(());
        };
        if current.token != request.token {
            return Ok(());
        }
        let changed_diagnostic = current.detail != detail;
        let completed = current.completed || issue.is_none();
        if !changed_diagnostic && current.completed == completed && current.retry_at == retry_at {
            return Ok(());
        }
        current.completed = completed;
        current.detail = detail.clone();
        current.retry_at = retry_at.clone();
        if !tx.update_closure_cleanup(&current)? {
            return Ok(());
        }
        if changed_diagnostic && let Some(detail) = &detail {
            let row = tx.story(ctx.project(), request.story)?
                .ok_or_else(|| AppError::Storage("cleanup story disappeared".into()))?;
            let prefix = crate::service::project_prefix(tx, ctx.project())?;
            let states = tx.state_map(ctx.project())?;
            let next = if current.completed {
                "Cleanup previously completed. Replacement resources were preserved; this receipt cannot authorize their removal."
            } else {
                "Cleanup is incomplete and will retry. Use story cleanup for an immediate retry after repair."
            };
            let text = format!(
                "STORY RESOURCE CLEANUP REQUIRED\n\n{}\n\n{next}",
                crate::text_lint::quote_evidence(detail)
            );
            crate::service::append_and_fold(
                tx, ctx.project(), request.story, &prefix, &states,
                ExpectedSeq::Exact(row.head_seq),
                &[StoryEvent::StoryCommentAdded { at: ctx.now(), text }],
                ctx.provenance(),
            )?;
        }
        Ok(())
    })?;
    Ok(())
}
