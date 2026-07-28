//! The seam between *deciding what to do* and *doing it*.
//!
//! Everything that can run a storyhook command — the CLI, the web dashboard,
//! and later the TUI — goes through [`Invoker`]. Today there is exactly one
//! implementation, [`LegacyInvoker`], which forwards to [`crate::app::run`]
//! in the same process. The point of introducing the trait before there is
//! anything to choose between is that adopting it is provably behavior-
//! preserving *now*, when the only implementation is the existing call, and
//! therefore cheap to verify; a later implementation that talks to a store or
//! a daemon becomes a constructor swap rather than a rewrite of every call
//! site.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::app;
use crate::cli::{CliOptions, Invocation};
use crate::error::AppError;
use crate::output::Response;

/// One unit of work for an [`Invoker`]: the command, plus the execution
/// context that has to travel with it.
///
/// Only settings that change *what happens* belong here. `--json` and
/// `--quiet` do not: they are rendering decisions, applied by
/// [`crate::output::render_response`] once the work is done and the caller
/// has the answer back. Keeping them out is what lets one process do the
/// work and another do the rendering.
///
/// `#[non_exhaustive]`: this struct is expected to grow — the working
/// directory and project selector once root resolution moves off the
/// caller, and a hook-recursion depth once hooks can re-enter through a
/// daemon. Construct it with [`InvokeRequest::new`] so that growth is not a
/// breaking change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct InvokeRequest {
    /// What to do.
    pub invocation: Invocation,
    /// Suppress the project's event hooks for this invocation, as
    /// `--no-hooks` does.
    pub no_hooks: bool,
}

impl InvokeRequest {
    /// A request to run `invocation` with hooks enabled.
    pub fn new(invocation: Invocation) -> Self {
        Self {
            invocation,
            no_hooks: false,
        }
    }

    /// Sets whether event hooks are suppressed.
    #[must_use]
    pub fn no_hooks(mut self, no_hooks: bool) -> Self {
        self.no_hooks = no_hooks;
        self
    }
}

/// Executes storyhook commands.
///
/// Implementations differ in *where* the work happens, never in what the
/// answer means: every one of them returns the same
/// [`Response`]/[`AppError`] envelope, which the caller renders itself.
pub trait Invoker {
    /// Runs `request`, returning the unrendered result.
    fn invoke(&self, request: InvokeRequest) -> Result<Response, AppError>;
}

/// Runs commands in this process against a project directory, by calling
/// [`crate::app::run`].
///
/// This is the pre-rearchitecture path, wrapped rather than reimplemented:
/// it forwards verbatim, so it behaves identically to a direct call by
/// construction. `app::run` reads only `no_hooks` and `invocation` off
/// [`CliOptions`], so the `json`/`quiet` fields filled in here are inert —
/// they exist because the struct still carries them for the CLI's own use.
pub struct LegacyInvoker<'a> {
    root: &'a Path,
}

impl<'a> LegacyInvoker<'a> {
    /// An invoker for the project rooted at `root`.
    pub fn new(root: &'a Path) -> Self {
        Self { root }
    }
}

impl Invoker for LegacyInvoker<'_> {
    fn invoke(&self, request: InvokeRequest) -> Result<Response, AppError> {
        app::run(
            self.root,
            CliOptions {
                json: false,
                quiet: false,
                no_hooks: request.no_hooks,
                invocation: request.invocation,
            },
        )
    }
}
