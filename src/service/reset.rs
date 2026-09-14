//! Recoverable native reset preserves branches and renews force authorization.

use super::workspace_lock::WorkspaceLock;
use super::{Ctx, append_and_fold, project_prefix, resolve_open_story};
use crate::domain::{StoryCleanupLease, StoryEvent, StorySnapshot, is_epic};
use crate::error::AppError;
use crate::store::{
    EventSeq, ExpectedSeq, ProjectId, ReadOps, Store, StoreError, StoryNo, WriteOps,
};
use serde::{Deserialize, Serialize};
mod resources;
#[cfg(test)]
mod tests;

fn preflight_authority_unchanged(
    tx: &impl ReadOps,
    project: ProjectId,
    number: StoryNo,
    expected: EventSeq,
    current: EventSeq,
) -> Result<bool, StoreError> {
    if current == expected {
        return Ok(true);
    }
    if current < expected {
        return Ok(false);
    }
    // Delivery diagnostics and commit observations do not alter cleanup authority.
    // State, awaiting, lease, and unknown events must still invalidate preflight.
    let events = tx.events_for(project, number)?;
    let mut suffix = events
        .iter()
        .filter(|event| event.seq > expected)
        .peekable();
    Ok(suffix.peek().is_some()
        && suffix.all(|event| {
            matches!(
                event.known(),
                Some(StoryEvent::StoryCommentAdded { .. } | StoryEvent::StoryCommitLinked { .. })
            )
        }))
}

fn preflight_project_unchanged(
    tx: &impl ReadOps,
    expected: &crate::store::ProjectRecord,
    checkout: &std::path::Path,
) -> Result<bool, StoreError> {
    let Some(current) = tx.project(expected.id)? else {
        return Ok(false);
    };
    Ok(current.uuid == expected.uuid
        && current.slug == expected.slug
        && current.prefix == expected.prefix
        && tx.checkout_path(expected.id)?.as_deref() == Some(checkout))
}

/// Caller-owned terminal facts; the daemon must never substitute its own terminal.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetCaller {
    /// Exact pane of the CLI client, when called from tmux.
    pub pane: Option<String>,
    /// Tmux socket from that client's environment.
    pub socket: Option<std::path::PathBuf>,
}

impl ResetCaller {
    /// Captures terminal identity on the CLI side, before RPC serialization.
    pub fn capture() -> Self {
        Self {
            pane: std::env::var("TMUX_PANE").ok(),
            socket: std::env::var("TMUX")
                .ok()
                .and_then(|value| value.split(',').next().map(Into::into)),
        }
    }
}

/// Durable cleanup authority, visible in `story show` while recovery is needed.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResetReservation {
    /// Unique operation identity, retained through retries.
    pub operation: String,
    /// Immutable resource identity, absent for a story with no workspace.
    pub lease: Option<StoryCleanupLease>,
    /// Permission supplied by the most recent invocation, never inferred.
    pub force: bool,
    /// The original blocked reason, restored after reset.
    pub previous_awaiting: Option<String>,
    /// Current phase or failure diagnostic.
    pub detail: String,
}

/// Resets an open ordinary story after all owned resources are absent.
/// Failed cleanup retains its reservation and exposes a safe retry path.
pub fn reset_story<S: Store>(
    ctx: &Ctx<'_, S>,
    id: &str,
    force: bool,
    caller: &ResetCaller,
) -> Result<(), AppError> {
    let (project, checkout, number, before, head, existing) = ctx.store().read(|tx| {
        let project = tx
            .project(ctx.project())?
            .ok_or_else(|| AppError::NotFound("project no longer exists".into()))?;
        let (number, row) = resolve_open_story(tx, project.id, &project.prefix, id)?;
        if is_epic(&row.snapshot) {
            return Err(AppError::Validation("an epic has no resettable workspace".into()).into());
        }
        let existing = tx
            .story_resets(project.id)?
            .remove(&number)
            .map(|encoded| serde_json::from_str::<ResetReservation>(&encoded))
            .transpose()?;
        let checkout = tx.checkout_path(project.id)?;
        Ok((
            project,
            checkout,
            number,
            row.snapshot,
            row.head_seq,
            existing,
        ))
    })?;
    let checkout = checkout
        .ok_or_else(|| AppError::Validation("reset requires a linked project checkout".into()))?;
    let lock = WorkspaceLock::acquire(&checkout, id)?;
    let repository = resources::repository(&checkout, &lock)?;
    let mut reservation = match existing {
        Some(value) => value,
        None => ResetReservation {
            operation: uuid::Uuid::new_v4().to_string(),
            lease: resources::discover(ctx, &project.slug, number, id, &repository, &lock)?,
            force,
            previous_awaiting: before.awaiting.clone(),
            detail: "Reset reserved; cleanup has not completed.".into(),
        },
    };
    reservation.force = force;
    resources::preflight(
        &reservation,
        id,
        &project.slug,
        &repository,
        ctx.cwd(),
        caller,
        &lock,
    )?;
    ctx.write_stories(|tx| {
        if !preflight_project_unchanged(tx, &project, &checkout)? {
            return Err(AppError::Validation(
                "project identity or checkout changed during reset preflight; retry".into(),
            )
            .into());
        }
        let prefix = project_prefix(tx, ctx.project())?;
        let (_, row) = resolve_open_story(tx, ctx.project(), &prefix, id)?;
        let stored = tx.story_resets(ctx.project())?.remove(&number);
        if let Some(stored) = stored {
            let current: ResetReservation = serde_json::from_str(&stored)?;
            if current.operation != reservation.operation {
                return Err(AppError::Validation("reset ownership changed; retry".into()).into());
            }
        } else {
            if !preflight_authority_unchanged(tx, ctx.project(), number, head, row.head_seq)? {
                return Err(AppError::Validation(
                    "story changed during reset preflight; retry".into(),
                )
                .into());
            }
            let states = tx.state_map(ctx.project())?;
            append_and_fold(
                tx,
                ctx.project(),
                number,
                &prefix,
                &states,
                ExpectedSeq::Exact(row.head_seq),
                &[StoryEvent::StoryAwaitingSet {
                    at: ctx.now(),
                    awaiting: format!(
                        "Reset pending for {id}. Run story reset {id} to finish cleanup."
                    ),
                }],
                ctx.provenance(),
            )?;
        }
        super::block_delivery::supersede_pending(
            tx,
            ctx.project(),
            number,
            "native reset owns workspace replacement",
        )?;
        tx.put_legacy_story_reset(
            ctx.project(),
            number,
            Some(&serde_json::to_string(&reservation)?),
        )?;
        Ok(())
    })?;
    let cleanup = resources::remove(
        &reservation,
        id,
        &project.slug,
        &repository,
        ctx.cwd(),
        caller,
        &lock,
    );
    if let Err(error) = cleanup {
        reservation.detail = format!(
            "Reset incomplete: {error}. Retry story reset {id}; add --force only to discard worktree changes."
        );
        ctx.store()
            .write(|tx| {
                tx.put_legacy_story_reset(
                    ctx.project(),
                    number,
                    Some(&serde_json::to_string(&reservation)?),
                )
            })
            .map_err(|journal| {
                AppError::Storage(format!(
                    "{error}; also failed to record reset diagnostics: {journal}"
                ))
            })?;
        return Err(AppError::Validation(reservation.detail));
    }
    let snapshot = finish(ctx, id, number, &reservation)?;
    // A hook may dispatch the now-reset story; publish completion and release
    // external exclusion before invoking user code, as other transitions do.
    drop(lock);
    super::StoryService::new(ctx).fire_transition_hooks(
        id,
        &before.title,
        &before.state,
        "todo",
        &snapshot,
        &ctx.now(),
    );
    Ok(())
}

fn finish(
    ctx: &Ctx<'_, impl Store>,
    id: &str,
    number: StoryNo,
    reservation: &ResetReservation,
) -> Result<StorySnapshot, AppError> {
    Ok(ctx.write_stories(|tx| {
        let prefix = project_prefix(tx, ctx.project())?;
        let (_, row) = resolve_open_story(tx, ctx.project(), &prefix, id)?;
        let stored = tx.story_resets(ctx.project())?.remove(&number)
            .ok_or_else(|| AppError::Validation("reset reservation disappeared".into()))?;
        let stored: ResetReservation = serde_json::from_str(&stored)?;
        if stored.operation != reservation.operation {
            return Err(AppError::Validation("reset ownership changed".into()).into());
        }
        let states = tx.state_map(ctx.project())?;
        let mut events = vec![
            StoryEvent::StoryStateChanged { at: ctx.now(), state: "todo".into() },
            StoryEvent::StoryAwaitingCleared { at: ctx.now() },
        ];
        if let Some(reason) = &reservation.previous_awaiting {
            events.push(StoryEvent::StoryAwaitingSet { at: ctx.now(), awaiting: reason.clone() });
        }
        events.push(StoryEvent::StoryCommentAdded {
            at: ctx.now(),
            text: format!("Reset to Todo. Removed owned workspace resources. Preserved branches and commits. Force: {}.", reservation.force),
        });
        let project = tx.project(ctx.project())?
            .ok_or_else(|| AppError::NotFound("project disappeared".into()))?;
        super::engine::release_reset_lanes(tx, &project.slug, id, &ctx.now())?;
        super::block_delivery::supersede_pending(tx, ctx.project(), number, "native reset completed; prior session authority retired")?;
        tx.put_legacy_story_reset(ctx.project(), number, None)?;
        Ok(append_and_fold(
            tx, ctx.project(), number, &prefix, &states,
            ExpectedSeq::Exact(row.head_seq), &events, ctx.provenance(),
        )?)
    })?)
}
