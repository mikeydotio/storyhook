//! Native `story reset`: a front end over the convergent reset engine (SH-886).
//!
//! Every reset entry point shares one contract (council C1): the story's
//! window closes, its worktree and local branch are discarded, its awaiting
//! reason clears, and it returns to todo. What reset cannot prove the story
//! owns is left in place and reported. `--force` is accepted and changes
//! nothing. Remote branches and pull requests are always preserved.

use super::story_reset::StoryResetService;
use super::{Ctx, project_prefix, resolve_open_story};
use crate::domain::StoryCleanupLease;
use crate::error::AppError;
use crate::store::{ReadOps, ResetOrigin, Store, StoryReset};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// How long `story reset` waits for the daemon to finish the reset before it
/// reports the reset still running; below the served deadline (decision D11).
pub const NATIVE_WAIT: Duration = Duration::from_secs(90);

/// Pause between reads of an unfinished receipt.
const NATIVE_POLL: Duration = Duration::from_millis(200);

/// Caller-owned terminal facts; the daemon must never substitute its own terminal.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetCaller {
    /// Exact pane of the CLI client, when called from tmux.
    pub pane: Option<String>,
    /// Tmux socket from that client's environment.
    pub socket: Option<std::path::PathBuf>,
}

impl ResetCaller {
    /// Whether the request came without terminal identity (HTTP or a legacy client).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pane.is_none() && self.socket.is_none()
    }

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

/// A reservation `story reset` recorded before SH-886. The daemon adopts it
/// with no more authority than it records; nothing writes new ones.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResetReservation {
    /// Unique operation identity, retained through retries.
    pub operation: String,
    /// Immutable resource identity, absent for a story with no workspace.
    pub lease: Option<StoryCleanupLease>,
    /// Permission supplied by the most recent invocation, never inferred.
    pub force: bool,
    /// The original blocked reason, which the old contract restored.
    pub previous_awaiting: Option<String>,
    /// Current phase or failure diagnostic.
    pub detail: String,
}

/// Resets an open ordinary story and returns its receipt. Once reserved, the
/// reset never fails: the daemon's reset runtime finishes it, and this call
/// waits up to [`NATIVE_WAIT`] before it returns the receipt still running.
pub fn reset_story<S: Store>(
    ctx: &Ctx<'_, S>,
    id: &str,
    force: bool,
    caller: &ResetCaller,
) -> Result<StoryReset, AppError> {
    // Every reset discards local work (council C1); `--force` changes nothing.
    let _ = force;
    let canonical = ctx.store().read(|tx| {
        let prefix = project_prefix(tx, ctx.project())?;
        let (_, row) = resolve_open_story(tx, ctx.project(), &prefix, id)?;
        Ok(row.snapshot.id)
    })?;
    let origin = ResetOrigin {
        automation_generation: ctx
            .store()
            .read(|tx| Ok(tx.settings(ctx.project())?.automations_after))?,
        caller: caller.clone(),
        cwd: Some(ctx.cwd().to_path_buf()),
        fire_hooks: ctx.hooks_enabled(),
        hook_depth: ctx.depth(),
        legacy_force: None,
    };
    let service = StoryResetService::new(ctx);
    let reset = service.reserve_from(&canonical, &canonical, &origin)?;
    let Some(runtime) = ctx.reset_runtime() else {
        // In-process callers have no runtime: the reset runs here.
        return service.execute(&canonical, &reset.token, || {
            super::engine::await_card_reset_dispatch(ctx, &reset)
        });
    };
    runtime.request(&reset);
    let deadline = Instant::now() + NATIVE_WAIT;
    loop {
        let current = service.get(&canonical, &reset.token)?;
        if current.completed || Instant::now() >= deadline {
            return Ok(current);
        }
        std::thread::sleep(NATIVE_POLL);
    }
}
