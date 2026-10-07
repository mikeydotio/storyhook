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
        let news =
            current.detail.as_deref().map(fingerprint) != detail.as_deref().map(fingerprint);
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
        if news && let Some(detail) = &detail {
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

/// What a retry must change for its diagnostic to be news: the detail with
/// each run of digits collapsed to `#`. A retry measures elapsed time, load
/// and process ids afresh, and posting each new reading of the same failure
/// added a comment at every retry (SH-881). The exact latest detail is still
/// stored; only the comment depends on this.
fn fingerprint(detail: &str) -> String {
    let mut out = String::with_capacity(detail.len());
    let mut digits = false;
    for c in detail.chars() {
        if c.is_ascii_digit() {
            if !digits {
                out.push('#');
            }
            digits = true;
        } else {
            out.push(c);
            digits = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::Environment;
    use crate::service::{NewStoryInput, StoryService};
    use crate::store::{SqliteStore, StoryNo};

    #[test]
    fn a_fingerprint_drops_each_reading_and_keeps_the_message() {
        assert_eq!(
            fingerprint("probe exceeded 30s after 31.2s (load 4.51); pid 812 alive"),
            fingerprint("probe exceeded 30s after 33.07s (load 12.5); pid 9 alive"),
        );
        assert_eq!(fingerprint("a1b22c333"), "a#b#c#");
        assert_eq!(fingerprint(""), "");
        assert_ne!(
            fingerprint("cleanup pane identity changed"),
            fingerprint("story window remains after cleanup"),
        );
    }

    /// A retry measures elapsed time, load and process ids afresh. Taken as
    /// news, each reading posted another comment on the story (SH-881).
    #[test]
    fn a_retry_that_only_measures_again_posts_no_comment() {
        let fixture = storyhook_test_support::ServiceFixture::new();
        let store = SqliteStore::open(fixture.env().store_path()).unwrap();
        let project = store
            .read(|tx| Ok(tx.project_by_slug("fixture")?.unwrap().id))
            .unwrap();
        let ctx = Ctx::new(
            &store,
            project,
            fixture.cwd(),
            Environment::at(fixture.env().home()),
        )
        .no_hooks(true);
        let stories = StoryService::new(&ctx);
        let id = stories
            .create(&NewStoryInput {
                title: "Retried cleanup".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        stories.set_state(&id, "done", None, None, None).unwrap();
        let story = StoryNo::parse_id("SH", &id).unwrap();
        let fail = |detail: &str| {
            let request = store
                .read(|tx| tx.closure_cleanup(project, story))
                .unwrap()
                .unwrap();
            let issue = CleanupSkip {
                story_id: id.clone(),
                reason: "dropped-cleanup-failed".into(),
                detail: detail.into(),
            };
            finish(&ctx, &request, Some(&issue)).unwrap();
            let comments = store
                .read(|tx| tx.events_for(project, story))
                .unwrap()
                .into_iter()
                .filter(|event| {
                    matches!(event.known(), Some(StoryEvent::StoryCommentAdded { text, .. })
                        if text.starts_with("STORY RESOURCE CLEANUP REQUIRED"))
                })
                .count();
            let stored = store
                .read(|tx| tx.closure_cleanup(project, story))
                .unwrap()
                .unwrap()
                .detail;
            (comments, stored)
        };
        assert_eq!(fail("probe timed out after 31.2s (load 4.51)").0, 1);
        let (comments, stored) = fail("probe timed out after 33.0s (load 12.07)");
        assert_eq!(comments, 1);
        // The comment is not repeated, but the latest reading is kept.
        assert_eq!(
            stored.as_deref(),
            Some("dropped-cleanup-failed: probe timed out after 33.0s (load 12.07)")
        );
        assert_eq!(fail("story window remains after cleanup").0, 2);
    }
}
