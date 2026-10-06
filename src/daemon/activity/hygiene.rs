//! Journal hygiene across registered checkouts (SH-771).
//!
//! Every journal writer makes its directory ignore itself
//! ([`super::ignore::prepare`]), but only when it writes. This sweep covers
//! the rest. At daemon start, and every [`SWEEP_INTERVAL`] after, it
//! prepares the journal directory of every registered checkout that has
//! one. So a checkout journaled before SH-771, or one whose ignore file was
//! deleted while nothing journaled there, is fixed with no user action.
//!
//! An ignore file cannot hide files that are already in the index. The
//! sweep asks git which journal files each checkout tracks and publishes
//! the answer in [`Environment::journal_hygiene_file`]. Both `story daemon
//! status` (which never opens the store or contacts the daemon) and the
//! verifier status snapshot read it there. Storyhook never changes an
//! index, commits or pushes: the finding names the command the user runs.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::context::{LogContext, enter};
use crate::env::Environment;
use crate::error::AppError;
use crate::store::{ProjectId, ReadOps, Store};

/// How often the sweep runs after the one at daemon start. A deleted ignore
/// file in a checkout nothing journals to is back within this long.
pub(crate) const SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// How long one `git ls-files` may take. It reads one local index; the
/// bound covers a loaded machine without holding daemon shutdown long.
pub(crate) const TRACKED_CHECK_DEADLINE: Duration = Duration::from_secs(10);

/// A registered checkout whose index tracks journal files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct TrackedJournal {
    /// The project's local row id, which the verifier status matches on.
    pub(crate) project_id: ProjectId,
    /// The project slug the message names.
    pub(crate) project: String,
    /// The registered checkout whose index tracks the files.
    pub(crate) checkout: PathBuf,
    /// How many journal paths the index tracks.
    pub(crate) files: usize,
    /// Whether git listed more paths than the capture kept, which makes
    /// `files` a lower bound.
    pub(crate) more: bool,
}

impl TrackedJournal {
    /// The one actionable sentence every status surface shows.
    pub(crate) fn warning(&self) -> String {
        format!(
            "{}: git tracks {}{} activity journal file{} in {}; an ignore file cannot hide \
             them. Run `git rm -r --cached {}` in that checkout, then commit. Storyhook never \
             changes the index.",
            self.project,
            if self.more { "at least " } else { "" },
            self.files,
            if self.files == 1 { "" } else { "s" },
            super::project_journal(&self.checkout).display(),
            super::PROJECT_JOURNAL,
        )
    }
}

/// What one sweep found, as published for readers.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Findings {
    tracked: Vec<TrackedJournal>,
}

/// Removes the previous daemon's findings.
///
/// The daemon calls this after it holds its lifetime lock and before it
/// publishes its portfile. So any findings that a reader of a running
/// daemon sees were written by that daemon, and no identity stamp is
/// needed.
///
/// # Errors
///
/// Any failure to remove the file, other than its absence.
pub(crate) fn reset(env: &Environment) -> std::io::Result<()> {
    match std::fs::remove_file(env.journal_hygiene_file()) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Sweeps at once (daemon start), then every [`SWEEP_INTERVAL`] until
/// `stop`. Independent of `STORYHOOK_VERIFIER_MIRROR`: hygiene is not a
/// terminal view.
pub(crate) fn poll(store: &impl Store, env: &Environment, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        if let Err(error) = sweep(store, env) {
            super::emit(
                "WARN",
                "hygiene",
                "event",
                "",
                &format!("journal hygiene sweep failed: {error}"),
            );
        }
        let until = Instant::now() + SWEEP_INTERVAL;
        while !stop.load(Ordering::Relaxed) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// One pass over every registered checkout: prepare each existing journal
/// directory, find the journal files each git index tracks, and publish
/// the findings.
///
/// A checkout without a journal directory is skipped. The daemon has not
/// journaled there, and a sweep must not create one. Per-checkout failures
/// are journaled under that project's scope and do not stop the pass.
///
/// # Errors
///
/// The registered checkouts cannot be read, or the findings cannot be
/// published.
pub(crate) fn sweep(
    store: &impl Store,
    env: &Environment,
) -> Result<Vec<TrackedJournal>, AppError> {
    let checkouts = store.read(|tx| {
        let mut checkouts = Vec::new();
        for project in tx.projects()? {
            if let Some(checkout) = tx.checkout_path(project.id)? {
                checkouts.push((project.id, project.slug, checkout));
            }
        }
        Ok(checkouts)
    })?;
    let mut tracked = Vec::new();
    for (project_id, project, checkout) in checkouts {
        let Some(_automation) = crate::service::automations::enter(store, env, project_id)? else {
            continue;
        };
        let directory = super::project_journal(&checkout);
        if !checkout.is_absolute() || !directory.is_dir() {
            continue;
        }
        let _scope = enter(Some(LogContext {
            directory: directory.clone(),
            label: format!("project={project} hygiene"),
        }));
        if let Err(error) = super::ignore::prepare(&directory) {
            super::emit(
                "WARN",
                "hygiene",
                "event",
                "",
                &format!(
                    "journal {} cannot be made to ignore itself: {error}",
                    directory.display()
                ),
            );
        }
        if !in_git_work_tree(&checkout) {
            continue;
        }
        match tracked_files(env, &checkout) {
            Ok(None) => {}
            Ok(Some((files, more))) => tracked.push(TrackedJournal {
                project_id,
                project,
                checkout,
                files,
                more,
            }),
            Err(error) => super::emit(
                "WARN",
                "hygiene",
                "event",
                "",
                &format!(
                    "cannot tell whether git tracks {}: {error}",
                    directory.display()
                ),
            ),
        }
    }
    publish(
        env,
        &Findings {
            tracked: tracked.clone(),
        },
    )?;
    Ok(tracked)
}

/// Whether git can find a repository for `checkout`. The answer comes from
/// the filesystem, so a checkout that is not a repository costs no process
/// and journals no failure on each sweep.
fn in_git_work_tree(checkout: &Path) -> bool {
    checkout.ancestors().any(|dir| dir.join(".git").exists())
}

/// How many journal paths `checkout`'s index tracks, with whether the list
/// was cut short; `None` when it tracks none. `ls-files` reads only the
/// index, so no ignore rule changes its answer.
fn tracked_files(env: &Environment, checkout: &Path) -> Result<Option<(usize, bool)>, String> {
    let mut command = crate::env::git_env::command(checkout);
    command.args(["ls-files", "-z", "--", super::PROJECT_JOURNAL]);
    // Journals only a failure: this runs every minute for every checkout,
    // and success is the steady state (the SH-761 rule).
    let captured =
        crate::process::run_captured_quiet(command, env.subprocess_bound(TRACKED_CHECK_DEADLINE))
            .map_err(|error| error.detail())?;
    if !captured.status.success() {
        return Err(format!(
            "git ls-files {}: {}",
            captured.status,
            String::from_utf8_lossy(&captured.stderr).trim()
        ));
    }
    let files = captured
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .count();
    Ok((files > 0).then_some((files, captured.stdout_truncated)))
}

/// Replaces the findings file with a complete new copy, so a reader never
/// sees a partial one.
fn publish(env: &Environment, findings: &Findings) -> Result<(), AppError> {
    let path = env.journal_hygiene_file();
    let directory = path
        .parent()
        .expect("the findings file lives in the daemon state directory");
    std::fs::create_dir_all(directory)?;
    let mut staged = tempfile::NamedTempFile::new_in(directory)?;
    staged.write_all(&serde_json::to_vec(findings)?)?;
    staged
        .persist(&path)
        .map_err(|error| AppError::from(error.error))?;
    Ok(())
}

/// The running daemon's latest findings. No file means no sweep has
/// finished since the daemon started: there is nothing to report.
fn read(env: &Environment) -> Result<Vec<TrackedJournal>, String> {
    let path = env.journal_hygiene_file();
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(unreadable(&path, &error)),
    };
    serde_json::from_slice::<Findings>(&bytes)
        .map(|findings| findings.tracked)
        .map_err(|error| unreadable(&path, &error))
}

fn unreadable(path: &Path, error: &dyn std::fmt::Display) -> String {
    format!(
        "journal hygiene findings at {} are unreadable: {error}",
        path.display()
    )
}

/// One warning per checkout whose index tracks journal files, for `story
/// daemon status`. An unreadable findings file is itself a warning.
pub(crate) fn warnings(env: &Environment) -> Vec<String> {
    match read(env) {
        Ok(tracked) => tracked.iter().map(TrackedJournal::warning).collect(),
        Err(error) => vec![error],
    }
}

/// The warning for `project`, if its checkout's index tracks journal files,
/// for the verifier status. An unreadable findings file is reported on
/// every project.
pub(crate) fn warning_for(env: &Environment, project: ProjectId) -> Option<String> {
    match read(env) {
        Ok(tracked) => tracked
            .iter()
            .find(|finding| finding.project_id == project)
            .map(TrackedJournal::warning),
        Err(error) => Some(error),
    }
}

#[cfg(test)]
#[path = "hygiene_tests.rs"]
mod tests;
