use std::path::Path;

use crate::cli::Invocation;

use super::sync_state;

/// Check if auto-sync is enabled and trigger a sync for the affected story.
/// This is called after a story-modifying command succeeds.
/// Errors are printed as warnings (never fails the original command).
pub fn maybe_auto_sync(root: &Path, invocation: &Invocation, created_story_id: Option<&str>) {
    // 1. Load sync config. If not configured or mode != Auto, return silently.
    let _config = match sync_state::load_sync_config(root) {
        Ok(Some(config)) if config.sync.mode == sync_state::SyncMode::Auto => config,
        _ => return,
    };

    // 2. Extract the affected story ID from the invocation
    let story_id = match extract_affected_story_id(invocation) {
        Some(id) => id,
        None => match created_story_id {
            Some(id) => id.to_string(),
            None => return,
        },
    };

    // 3. Run sync for this single story, suppressing errors
    match super::run_sync(root, Some(&story_id), false) {
        Ok(_) => {} // silently succeed
        Err(e) => {
            eprintln!("warning: auto-sync failed for {story_id}: {e}");
        }
    }
}

/// Extract the story ID affected by an invocation, if applicable.
/// Only returns Some for invocations that MODIFY stories.
fn extract_affected_story_id(invocation: &Invocation) -> Option<String> {
    match invocation {
        Invocation::Comment { id, .. }
        | Invocation::Assign { id, .. }
        | Invocation::SetState { id, .. }
        | Invocation::SetAwaiting { id, .. }
        | Invocation::ClearAwaiting { id }
        | Invocation::SetPriority { id, .. }
        | Invocation::Reopen { id, .. } => Some(id.clone()),
        Invocation::SetLabels { id, .. } => Some(id.clone()),
        Invocation::Relate { a, .. } => Some(a.clone()),
        _ => None,
    }
}
