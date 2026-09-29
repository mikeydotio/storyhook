//! Verification merges have no attribute or configuration inputs from the host.
//!
//! Object storage is separate from Git administration: an alternate exposes
//! source objects, never source config, info/attributes, refs or an index.

use crate::error::AppError;
use crate::process::{Captured, run_captured_query};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// Bound shared by setup and merge commands; an outer deadline can shorten it.
const COMMAND_BOUND: Duration = Duration::from_secs(30);
/// A partial conflict description must never be mistaken for a complete answer.
const ANSWER_LIMIT: u64 = 8 * 1024 * 1024;

/// One merge's capture policy, including cancellation during initialization.
pub(crate) struct MergeControl<'a> {
    /// Caller named in every diagnostic.
    pub label: &'a str,
    /// Optional deadline for the complete operation.
    pub deadline: Option<Instant>,
    /// Cancellation check shared with the owning verifier.
    pub cancelled: &'a dyn Fn() -> bool,
}

impl MergeControl<'_> {
    fn capture(&self, command: Command, answers: &'static [i32]) -> Result<Captured, AppError> {
        let timeout = self.deadline.map_or(COMMAND_BOUND, |deadline| {
            COMMAND_BOUND.min(deadline.saturating_duration_since(Instant::now()))
        });
        let result = run_captured_query(command, timeout, self.cancelled, ANSWER_LIMIT, answers)
            .map_err(|error| {
                AppError::Storage(format!("{} isolated merge: {}", self.label, error.detail()))
            })?;
        if result.stdout_truncated {
            return Err(AppError::Storage(format!(
                "{} isolated merge: truncated Git answer refused",
                self.label
            )));
        }
        if !result.status.success()
            && !result
                .status
                .code()
                .is_some_and(|code| answers.contains(&code))
        {
            return Err(AppError::Storage(format!(
                "{} isolated merge: Git failed ({}): {}",
                self.label,
                result.status,
                String::from_utf8_lossy(&result.stderr)
            )));
        }
        Ok(result)
    }
}

/// Computes a merge under the no-attributes policy. `objects`, when present,
/// gives the caller-owned primary and source alternate; otherwise objects
/// are written to the source repository (batch assembly).
pub(crate) fn merge(
    checkout: &Path,
    objects: Option<(&Path, &Path)>,
    parents: [&str; 2],
    nul: bool,
    control: MergeControl<'_>,
) -> Result<Captured, AppError> {
    for parent in parents {
        super::trial_merge::require_pinned(parent, control.label)?;
    }
    let mut format_command = crate::env::git_env::command(checkout);
    format_command.args(["rev-parse", "--show-object-format"]);
    let format = control.capture(format_command, &[])?;
    let format = String::from_utf8_lossy(&format.stdout);
    let format = format.trim();
    if !matches!(format, "sha1" | "sha256") {
        return Err(AppError::Storage(format!(
            "{} isolated merge: unsupported object format {format:?}",
            control.label
        )));
    }
    let source;
    let (primary, alternate) = if let Some(pair) = objects {
        pair
    } else {
        let mut command = crate::env::git_env::command(checkout);
        command.args(["rev-parse", "--path-format=absolute", "--git-common-dir"]);
        let common = control.capture(command, &[])?;
        let common =
            String::from_utf8(common.stdout).map_err(|e| AppError::Storage(e.to_string()))?;
        source = Path::new(common.trim_end_matches('\n')).join("objects");
        (source.as_path(), source.as_path())
    };
    let directory = tempfile::Builder::new()
        .prefix("storyhook-merge-admin-")
        .tempdir()
        .map_err(|e| {
            AppError::Storage(format!(
                "{} isolated merge administration: {e}",
                control.label
            ))
        })?;
    let mut init = clean_command(directory.path());
    init.args([
        "init",
        "--bare",
        "--quiet",
        "--template=",
        &format!("--object-format={format}"),
        ".",
    ]);
    control.capture(init, &[])?;
    let command = || {
        let mut command = clean_command(directory.path());
        command
            .env("GIT_DIR", directory.path())
            .env("GIT_OBJECT_DIRECTORY", primary);
        if primary != alternate {
            command.env(
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                quote_alternate(alternate),
            );
        }
        command
    };
    // Capture supplies null stdin, so this writes the empty tree in the same
    // object format and database the merge uses, without a hard-coded SHA.
    let mut empty = command();
    empty.args(["hash-object", "-t", "tree", "-w", "--stdin"]);
    let empty = control.capture(empty, &[])?;
    let empty =
        super::trial_merge::answer_oid(&empty.stdout, control.label, "empty attribute tree")?;
    let mut merge = command();
    // Use the CLI option, not just GIT_ATTR_SOURCE: old Git must fail loudly
    // instead of silently ignoring an unknown environment variable.
    merge.args([
        &format!("--attr-source={empty}"),
        "-c",
        "merge.conflictStyle=diff3",
        "merge-tree",
        "--write-tree",
    ]);
    if nul {
        merge.arg("-z");
    }
    merge.args(parents);
    control.capture(merge, &[1])
}

fn clean_command(directory: &Path) -> Command {
    let mut command = crate::env::git_env::command(directory);
    command
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .args([
            "-c",
            "core.attributesFile=/dev/null",
            "-c",
            "merge.default=text",
        ]);
    command
}

/// Git parses alternates as a colon-separated, C-quoted list, not one path.
fn quote_alternate(path: &Path) -> String {
    let mut result = String::from("\"");
    for byte in path.as_os_str().as_encoded_bytes() {
        match byte {
            b'"' => result.push_str("\\\""),
            b'\\' => result.push_str("\\\\"),
            32..=126 => result.push(char::from(*byte)),
            _ => result.push_str(&format!("\\{byte:03o}")),
        }
    }
    result.push('"');
    result
}
