//! The commands that act on the machine rather than on the tracker.
//!
//! Scaffolding an instruction file, installing git hooks, installing the
//! editor plugin: none of these read or write a story, and none of them can
//! be expressed as a store transaction. What they need from the store is the
//! project's *identity* — its prefix and its closed state — so that the
//! content they produce describes the project the user is standing in rather
//! than a generic one.
//!
//! Grouping them is not filing convenience. Each one writes somewhere the rest
//! of storyhook never touches: the repository's working tree, its `.git/hooks`
//! directory, the user's editor configuration. Keeping them behind one service
//! is what makes "which commands can write outside the store?" a question with
//! a short, checkable answer.
//!
//! # The linked-worktree caveat
//!
//! `story hooks install` writes into `<root>/.git/hooks`. In a *linked*
//! worktree `.git` is a file pointing at the real git directory, not a
//! directory, so the install fails there — and it fails with the filesystem's
//! own error rather than a diagnosis. The behaviour is replicated as-is:
//! fixing it means resolving `--git-common-dir`, which is the same resolution
//! the wave that ends worktree divergence has to build anyway, and doing it
//! twice would leave two answers to one question.

use crate::error::AppError;
use crate::store::Store;
use crate::{event_hooks, hooks, plugin};

use super::project::closed_state;
use super::{Ctx, project_prefix, templates};

/// The scaffolding, git-hook and plugin commands.
pub struct SystemService<'ctx, S: Store> {
    ctx: &'ctx Ctx<'ctx, S>,
}

impl<'ctx, S: Store> SystemService<'ctx, S> {
    /// A system service bound to `ctx`.
    pub fn new(ctx: &'ctx Ctx<'ctx, S>) -> Self {
        Self { ctx }
    }

    /// Renders one of the instruction templates for this project.
    ///
    /// Returns the content rather than writing it: `story scaffold` prints to
    /// stdout so the caller can redirect it, review it, or pipe it somewhere
    /// storyhook has no business knowing about.
    pub fn scaffold(&self, kind: &str) -> Result<String, AppError> {
        match kind {
            "agents-md" => {
                let project = self.ctx.project();
                let (prefix, done) = self
                    .ctx
                    .store()
                    .read(|tx| Ok((project_prefix(tx, project)?, closed_state(tx, project)?)))?;
                Ok(templates::agents_md(&prefix, &done))
            }
            "claude-md" => Ok(templates::claude_md()),
            "cursor-rules" => Ok(templates::cursor_rules()),
            _ => Err(AppError::Usage(
                "usage: story scaffold agents-md|claude-md|cursor-rules".to_string(),
            )),
        }
    }

    /// Installs storyhook's git hooks into this checkout.
    ///
    /// A hook file that exists and was not written by storyhook is left alone
    /// and reported as skipped — the user's own `pre-commit` is not ours to
    /// overwrite.
    pub fn install_git_hooks(&self) -> Result<String, AppError> {
        install_git_hooks(self.ctx.cwd())
    }

    /// Removes storyhook's git hooks from this checkout, leaving anyone
    /// else's alone.
    pub fn uninstall_git_hooks(&self) -> Result<String, AppError> {
        uninstall_git_hooks(self.ctx.cwd())
    }

    /// Describes the project's configured event hooks.
    ///
    /// Infallible: "no hooks configured" is an answer, not an error.
    pub fn list_event_hooks(&self) -> String {
        list_event_hooks(self.ctx.cwd())
    }

    /// Fires one event hook with a synthetic payload, so a user can see what
    /// their hook does before a real event does it to them.
    pub fn test_event_hook(&self, event_type: &str) -> Result<String, AppError> {
        event_hooks::test_hook(self.ctx.cwd(), event_type)
    }

    /// Installs the editor/agent plugin named by `target`.
    pub fn install_plugin(&self, target: &str) -> Result<String, AppError> {
        install_plugin(target, self.ctx.cwd())
    }

    /// Removes the editor/agent plugin named by `target`.
    pub fn uninstall_plugin(&self, target: &str) -> Result<String, AppError> {
        uninstall_plugin(target, self.ctx.cwd())
    }
}

// --- the project-less half --------------------------------------------------
//
// These five need a *directory*, not a project: they write `.git/hooks`, read
// `hooks.toml`, or install an editor plugin, and the legacy path answered every
// one of them in a directory storyhook had never heard of. Root resolution
// routes them before it looks for a project, which is why they exist as free
// functions rather than only as methods. `hooks test` is deliberately not among
// them — it fires a real hook against a real project, and the legacy path calls
// `ensure_project` first.

/// Installs storyhook's git hooks into the checkout at `cwd`.
pub fn install_git_hooks(cwd: &std::path::Path) -> Result<String, AppError> {
    hooks::install_hooks(cwd)
}

/// Removes storyhook's git hooks from the checkout at `cwd`.
pub fn uninstall_git_hooks(cwd: &std::path::Path) -> Result<String, AppError> {
    hooks::uninstall_hooks(cwd)
}

/// Describes the event hooks configured at `cwd`.
#[must_use]
pub fn list_event_hooks(cwd: &std::path::Path) -> String {
    event_hooks::list_hooks(cwd)
}

/// Installs the plugin named by `target` for the checkout at `cwd`.
pub fn install_plugin(target: &str, cwd: &std::path::Path) -> Result<String, AppError> {
    plugin::install(target, cwd)
}

/// Removes the plugin named by `target` from the checkout at `cwd`.
pub fn uninstall_plugin(target: &str, cwd: &std::path::Path) -> Result<String, AppError> {
    plugin::uninstall(target, cwd)
}
