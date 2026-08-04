//! The git environment storyhook refuses to inherit, and the one place a `git`
//! it runs is constructed.
//!
//! Moved here verbatim from `service::project`, where it was private and
//! therefore protected only the origin and ownership probes that module owns.
//! Four other `git` call sites — `service::git`'s two, `github::sync_state` and
//! `service::migrate` — had no such protection, and the environment they were
//! exposed to is not `service::project`'s business to own. This module changes
//! no behaviour; it changes who the rule belongs to.

use std::path::Path;
use std::process::Command;

/// The git environment variables every probe here removes.
///
/// **`git config --get remote.origin.url` obeys `$GIT_DIR` over `cwd`, and
/// `git rev-parse --show-toplevel` does not.** With one inherited, the two
/// disagree: the origin is read from whatever repository the variable names
/// while the top level is read from the working directory, so an ownership
/// check comparing them agrees with itself and registers another repository's
/// identity. Measured on git 2.50.1.
///
/// The reachability is not the one it looks like. No git hook sets `GIT_DIR`
/// on that version — `pre-commit`, `post-commit`, `post-checkout` and
/// `pre-push` all run without it. What does reach here is a daemon: it inherits
/// the environment of whichever client started it and keeps it for its whole
/// life, and since SH-114 the daemon is the process that runs these probes.
const GIT_ENV_TO_SCRUB: [&str; 9] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_DISCOVERY_ACROSS_FILESYSTEM",
    "GIT_NAMESPACE",
];

/// A `git` to run in `cwd`, with the inherited git environment removed.
pub fn command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(cwd);
    for name in GIT_ENV_TO_SCRUB {
        command.env_remove(name);
    }
    command
}
