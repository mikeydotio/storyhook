//! A durable dispatch state is pending work, not proof of a live dispatcher.
use super::reset::run_lock_file;
use crate::error::AppError;
use crate::service::Ctx;
use crate::service::executor_lock::ExecutorLock;
use crate::store::{EngineLaneState, ReadOps, Store, StoryReset};

/// Reports live engine dispatches that must settle before a card reset.
///
/// The caller holds the unfinished card-reset reservation. It excludes new
/// claims of this story, so a successful lock probe stays valid after unlock.
/// Resource cleanup and lane release still belong to the reset executor.
pub(crate) fn card_reset_dispatching<S: Store>(
    ctx: &Ctx<'_, S>,
    reset: &StoryReset,
) -> Result<bool, AppError> {
    for owner in &reset.lanes {
        let dispatching = || {
            ctx.store().read(|tx| {
                Ok(tx.engine_lanes(&owner.run_id)?.iter().any(|lane| {
                    lane.lane_index == owner.lane_index
                        && lane.story_id.as_deref() == Some(&reset.story_id)
                        && lane.state == EngineLaneState::Dispatching
                }))
            })
        };
        if !dispatching()? {
            continue;
        }
        let (path, file) = run_lock_file(ctx.env(), "dispatch", &owner.run_id)?;
        let guard = match ExecutorLock::acquire(&file, &path) {
            Ok(guard) => Some(guard),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => None,
            Err(error) => {
                return Err(AppError::Storage(format!(
                    "locking dispatch controller {} for engine run `{}`: {error}",
                    path.display(),
                    owner.run_id,
                )));
            }
        };
        // Refresh after probing: a live dispatcher may have just published
        // its result. Under the lock, any remaining Dispatching row is an
        // orphan; without the lock, only this story's pending lane matters.
        if dispatching()? && guard.is_none() {
            return Ok(true);
        }
    }
    Ok(false)
}
