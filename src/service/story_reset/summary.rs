//! The completion record a person reads: what reset removed, what it left,
//! and how to recover discarded work.
use crate::store::StoryReset;

/// Lists removed resources, recovery commands and residue for the comment.
pub(super) fn completion(reset: &StoryReset) -> String {
    let mut text = format!("Reset {} completed.", reset.token);
    let left = |resource: &str| reset.residue.iter().any(|entry| entry.resource == resource);
    let mut removed = Vec::new();
    if let Some(report) = &reset.resources {
        let window = format!("tmux window {}", report.window_name);
        if report.pane.is_some() && !left(&window) {
            removed.push(window);
        }
        if let Some(worktree) = &report.worktree {
            let resource = format!("worktree {}", worktree.display());
            let existed = reset
                .paths
                .iter()
                .any(|pinned| &pinned.path == worktree && pinned.removable);
            if existed && !left(&resource) {
                removed.push(resource);
            }
        }
    }
    let recovery = reset.recovery.clone().unwrap_or_default();
    if let Some(branch) = &recovery.branch
        && !left(&format!("local branch {branch}"))
    {
        removed.push(format!("local branch {branch}"));
    }
    if removed.is_empty() {
        text.push_str(" It removed no workspace resources.");
    } else {
        text.push_str(&format!(" Removed {}.", removed.join(", ")));
    }
    text.push_str(
        " Released ownership and returned the story to todo. Preserved remote branches and pull requests.",
    );
    if let (Some(branch), Some(tip)) = (&recovery.branch, &recovery.tip) {
        let unpushed = recovery
            .unpushed
            .map_or_else(|| "an unknown number of".into(), |count| count.to_string());
        text.push_str(&format!(
            " Recovery: local branch {branch} was at {tip}, with {unpushed} commits on no other \
             branch, tag or remote; restore it with `git branch {branch} {tip}`."
        ));
    }
    match (recovery.dirty, recovery.untracked) {
        (Some(0), Some(0)) | (None, None) => {}
        (dirty, untracked) => text.push_str(&format!(
            " Discarded {} changed and {} untracked paths.",
            dirty.unwrap_or(0),
            untracked.unwrap_or(0)
        )),
    }
    if let Some(awaiting) = &recovery.cleared_awaiting {
        text.push_str(&format!(" Cleared the awaiting reason: {awaiting}"));
    }
    for entry in &reset.residue {
        text.push_str(&format!(
            " Left in place: {}: {}.",
            entry.resource, entry.reason
        ));
    }
    text
}

/// The awaiting reason that keeps the story out of dispatch while residue
/// would collide with it, or `None` when nothing would.
pub(super) fn dispatch_hold(reset: &StoryReset) -> Option<String> {
    let blocking: Vec<_> = reset
        .residue
        .iter()
        .filter(|entry| entry.blocks_dispatch)
        .map(|entry| entry.resource.as_str())
        .collect();
    (!blocking.is_empty()).then(|| {
        format!(
            "Reset {} left resources that the next dispatch would collide with: {}. Remove \
             them, or dispatch with --resume to reuse them, then clear this reason.",
            reset.token,
            blocking.join(", ")
        )
    })
}
