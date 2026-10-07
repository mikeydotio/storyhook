//! Git against a repository through a private, disposable object directory.
//!
//! Git reads every object of the repository, whose object directory is an
//! alternate, and every object Git writes lands in a temporary directory that
//! is removed with the [`PrivateObjects`] value. No checkout, index, ref or
//! repository object changes. Gate inspection introduced the pattern (it
//! rebuilds the proposed merge to read committed configuration); verification
//! batching's trial merges (SH-830) share it.

use crate::error::AppError;
use crate::process::{Captured, TerminationPolicy, run_captured_answer, run_captured_query};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Bound on one Git command; its whole process group is killed at the deadline.
const GIT_DEADLINE: Duration = Duration::from_secs(30);

/// Largest Git answer read: far above any real configuration file or conflict
/// list, and bounded so a hostile tree cannot exhaust memory. A longer answer
/// is refused, never read as a prefix (SH-815).
const GIT_ANSWER_LIMIT: u64 = 8 * 1024 * 1024;

/// A repository opened with a private object directory.
pub(crate) struct PrivateObjects {
    checkout: PathBuf,
    objects: tempfile::TempDir,
    source: PathBuf,
    label: &'static str,
}

impl PrivateObjects {
    /// Explicitly settle private objects when cleanup is part of a diagnostic result.
    pub(crate) fn close(self) -> Result<(), AppError> {
        self.objects
            .close()
            .map_err(|e| AppError::Storage(format!("{} private object cleanup: {e}", self.label)))
    }

    /// Computes a merge without borrowing attributes or configuration from
    /// this checkout. New objects retain this value's private lifetime.
    pub(crate) fn merge(
        &self,
        parents: [&str; 2],
        nul: bool,
        deadline: Option<Instant>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Captured, AppError> {
        super::isolated_merge::merge(
            &self.checkout,
            Some((self.objects.path(), &self.source)),
            parents,
            nul,
            super::isolated_merge::MergeControl {
                label: self.label,
                deadline,
                cancelled,
            },
        )
    }

    /// Creates the private object directory for `checkout`'s repository.
    ///
    /// `label` names the caller in every error it reports; `prefix` names the
    /// temporary directory, so a leftover one can be traced to its caller.
    pub(crate) fn open(
        checkout: &Path,
        label: &'static str,
        prefix: &str,
    ) -> Result<Self, AppError> {
        Self::open_controlled(checkout, label, prefix, None, &|| false)
    }

    /// Opens storage under the caller's deadline and cancellation, including setup.
    pub(crate) fn open_controlled(
        checkout: &Path,
        label: &'static str,
        prefix: &str,
        deadline: Option<Instant>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, AppError> {
        if cancelled() || deadline.is_some_and(|value| value <= Instant::now()) {
            return Err(AppError::Storage(format!(
                "{label}: preparation authority expired"
            )));
        }
        let args = ["rev-parse", "--path-format=absolute", "--git-common-dir"];
        let timeout = deadline.map_or(GIT_DEADLINE, |value| {
            GIT_DEADLINE.min(value.saturating_duration_since(Instant::now()))
        });
        let result = run_captured_query(
            git_command(checkout, None, &args),
            timeout,
            cancelled,
            GIT_ANSWER_LIMIT,
            &[],
        )
        .map_err(|error| AppError::Storage(format!("{label} Git setup: {}", error.detail())))?;
        let common = answer(result, label, &args)?;
        let common = String::from_utf8(common).map_err(|error| {
            AppError::Storage(format!("Git common directory is not UTF-8: {error}"))
        })?;
        let common = PathBuf::from(common.strip_suffix('\n').unwrap_or(&common));
        let objects = tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .map_err(|error| {
                AppError::Storage(format!("creating private {label} objects: {error}"))
            })?;
        Ok(Self {
            checkout: checkout.to_path_buf(),
            objects,
            source: common.join("objects"),
            label,
        })
    }

    /// Runs a Git query with the private object directory and `env` added,
    /// and returns its whole answer whatever its exit status. Exit codes in
    /// `answers` are answers, so only another failure is journaled. It stops
    /// at the earlier of the per-command bound and `deadline`, or as soon as
    /// `cancelled` answers true; a cut answer is refused.
    pub(crate) fn query(
        &self,
        args: &[&str],
        env: &[(&str, &str)],
        answers: &'static [i32],
        deadline: Option<Instant>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Captured, AppError> {
        let timeout = deadline.map_or(GIT_DEADLINE, |deadline| {
            GIT_DEADLINE.min(deadline.saturating_duration_since(Instant::now()))
        });
        let mut command = git_command(
            &self.checkout,
            Some((self.objects.path(), &self.source)),
            args,
        );
        command.envs(env.iter().copied());
        let result = run_captured_query(command, timeout, cancelled, GIT_ANSWER_LIMIT, answers)
            .map_err(|error| {
                AppError::Storage(format!("{} Git {args:?}: {}", self.label, error.detail()))
            })?;
        refuse_cut(&result, self.label, args)?;
        Ok(result)
    }

    /// Runs Git with the private object directory; a nonzero exit is an error.
    pub(crate) fn git(&self, args: &[&str]) -> Result<Vec<u8>, AppError> {
        answer(
            capture(
                &self.checkout,
                Some((self.objects.path(), &self.source)),
                self.label,
                args,
            )?,
            self.label,
            args,
        )
    }
}

/// Runs a Git query in `checkout` against the repository's own object store:
/// objects it writes are the repository's. Exit codes in `answers` are
/// answers; it stops at the per-command bound or as soon as `cancelled`
/// answers true, and a cut answer is refused. `label` names the caller in
/// every error.
pub(crate) fn repository_query(
    checkout: &Path,
    label: &str,
    args: &[&str],
    env: &[(&str, &str)],
    answers: &'static [i32],
    cancelled: &dyn Fn() -> bool,
) -> Result<Captured, AppError> {
    let mut command = git_command(checkout, None, args);
    command.envs(env.iter().copied());
    let result = run_captured_query(command, GIT_DEADLINE, cancelled, GIT_ANSWER_LIMIT, answers)
        .map_err(|error| AppError::Storage(format!("{label} Git {args:?}: {}", error.detail())))?;
    refuse_cut(&result, label, args)?;
    Ok(result)
}

/// Runs Git in `checkout` against the repository's own objects only; a
/// nonzero exit is an error. `label` names the caller in every error.
pub(crate) fn git(checkout: &Path, label: &str, args: &[&str]) -> Result<Vec<u8>, AppError> {
    answer(capture(checkout, None, label, args)?, label, args)
}

fn git_command(
    checkout: &Path,
    objects: Option<(&Path, &Path)>,
    args: &[&str],
) -> std::process::Command {
    let mut command = crate::env::git_env::command(checkout);
    if let Some((objects, source)) = objects {
        command
            .env("GIT_OBJECT_DIRECTORY", objects)
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", source);
    }
    command.args(args);
    command
}

fn capture(
    checkout: &Path,
    objects: Option<(&Path, &Path)>,
    label: &str,
    args: &[&str],
) -> Result<Captured, AppError> {
    run_captured_answer(
        git_command(checkout, objects, args),
        GIT_DEADLINE,
        TerminationPolicy::Kill,
        GIT_ANSWER_LIMIT,
    )
    .map_err(|error| AppError::Storage(format!("{label} Git {args:?}: {}", error.detail())))
}

fn answer(result: Captured, label: &str, args: &[&str]) -> Result<Vec<u8>, AppError> {
    if !result.status.success() {
        return Err(AppError::Storage(format!(
            "{label} Git {args:?} failed: {}",
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    refuse_cut(&result, label, args)?;
    Ok(result.stdout)
}

// A committed file is judged whole or not at all (SH-815): a prefix could
// parse where the file does not, and a digest would cover the prefix.
fn refuse_cut(result: &Captured, label: &str, args: &[&str]) -> Result<(), AppError> {
    if result.stdout_truncated {
        return Err(AppError::Storage(format!(
            "{label} Git {args:?} answered more than {} MiB; a cut answer is refused rather than read",
            GIT_ANSWER_LIMIT / (1024 * 1024)
        )));
    }
    Ok(())
}
