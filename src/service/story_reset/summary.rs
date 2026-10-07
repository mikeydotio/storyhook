//! The completion record a person reads: what reset removed, what it left,
//! and how to recover discarded work.
use crate::output::{ResetRemoved, ResetView};
use crate::store::StoryReset;

/// The resources a finished reset removed, named as its residue names them.
#[derive(Default)]
struct Removed {
    window: Option<String>,
    worktree: Option<String>,
    branch: Option<String>,
}

impl Removed {
    fn names(&self) -> Vec<&str> {
        [&self.window, &self.worktree, &self.branch]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect()
    }
}

/// What `reset` removed: each resource it found and owned that is not residue.
fn removed(reset: &StoryReset) -> Removed {
    let left = |resource: &str| reset.residue.iter().any(|entry| entry.resource == resource);
    let mut removed = Removed::default();
    if let Some(report) = &reset.resources {
        let window = format!("tmux window {}", report.window_name);
        if report.pane.is_some() && !left(&window) {
            removed.window = Some(window);
        }
        if let Some(worktree) = &report.worktree {
            let resource = format!("worktree {}", worktree.display());
            let existed = reset
                .paths
                .iter()
                .any(|pinned| &pinned.path == worktree && pinned.removable);
            if existed && !left(&resource) {
                removed.worktree = Some(resource);
            }
        }
    }
    if let Some(branch) = reset.recovery.as_ref().and_then(|r| r.branch.as_ref()) {
        let resource = format!("local branch {branch}");
        if !left(&resource) {
            removed.branch = Some(resource);
        }
    }
    removed
}

/// The reset as `story reset` and `story show` report it. Removal is
/// reported only once the reset has finished.
pub(crate) fn view(reset: &StoryReset) -> ResetView {
    let (detail, removed) = if reset.completed {
        let removed = removed(reset);
        let mut detail = format!("Reset {} completed.", reset.token);
        if !reset.residue.is_empty() {
            let left: Vec<_> = reset.residue.iter().map(|e| e.resource.as_str()).collect();
            detail.push_str(&format!(" Left in place: {}.", left.join(", ")));
        }
        let removed = ResetRemoved {
            window: removed.window.is_some(),
            worktree: removed.worktree.is_some(),
            branch: removed.branch.is_some(),
        };
        (detail, removed)
    } else {
        let detail = reset
            .failure
            .clone()
            .unwrap_or_else(|| "The daemon is finishing this reset.".into());
        (detail, ResetRemoved::default())
    };
    ResetView {
        operation: reset.token.clone(),
        detail,
        completed: reset.completed,
        removed,
        residue: reset.residue.clone(),
        recovery: reset.recovery.clone(),
    }
}

/// Lists removed resources, recovery commands and residue for the comment.
pub(super) fn completion(reset: &StoryReset) -> String {
    let mut text = format!("Reset {} completed.", reset.token);
    let removed = removed(reset);
    let names = removed.names();
    if names.is_empty() {
        text.push_str(" It removed no workspace resources.");
    } else {
        text.push_str(&format!(" Removed {}.", names.join(", ")));
    }
    let recovery = reset.recovery.clone().unwrap_or_default();
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
