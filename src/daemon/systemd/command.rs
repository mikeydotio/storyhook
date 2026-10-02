//! Bounded, injectable access to the user manager.

use crate::{error::AppError, process};
use std::{collections::BTreeMap, process::Command, time::Duration};

/// One manager control operation must return within this budget.
pub(crate) const COMMAND_DEADLINE: Duration = Duration::from_secs(10);

/// Probe, reload, properties, start: the bounded controls in one launch.
pub(crate) const LAUNCH_CONTROL_CALLS: u64 = 4;

/// A complete systemctl response, kept separate from transport failure.
pub(crate) struct Reply {
    /// Whether systemctl completed successfully.
    pub(crate) success: bool,
    /// Complete stdout for structured evidence.
    pub(crate) text: String,
    /// Bounded stderr retained with every refusal.
    pub(crate) diagnostic: String,
}

/// Injected systemctl boundary; fixtures never register host services.
pub(crate) type Runner<'a> = dyn Fn(&[&str]) -> Result<Reply, AppError> + 'a;

/// The one systemctl spawn site. Jobs are submitted with --no-block;
/// lifecycle health/drain, not a command timeout, bounds their completion.
pub(crate) fn run(args: &[&str]) -> Result<Reply, AppError> {
    let mut command = Command::new("systemctl");
    command
        .args(["--user", "--no-pager", "--no-ask-password"])
        .args(args);
    let output = process::run_captured_quiet(command, COMMAND_DEADLINE).map_err(|e| {
        AppError::Storage(format!(
            "systemctl --user {}: {}",
            args.join(" "),
            e.detail()
        ))
    })?;
    if output.stdout_truncated {
        return Err(AppError::Storage(
            "systemctl returned truncated evidence".into(),
        ));
    }
    Ok(Reply {
        success: output.status.success(),
        text: String::from_utf8_lossy(&output.stdout).trim().to_string(),
        diagnostic: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

/// Requires successful completion and retains operation-specific diagnostics.
pub(crate) fn checked(run: &Runner<'_>, args: &[&str]) -> Result<String, AppError> {
    let reply = run(args)?;
    if reply.success {
        return Ok(reply.text);
    }
    Err(AppError::Storage(format!(
        "systemctl --user {} refused: {} {}",
        args.join(" "),
        reply.text,
        reply.diagnostic
    )))
}

/// Reads the unit properties that bind a loaded job to its definition.
pub(crate) fn properties(
    run: &Runner<'_>,
    unit: &str,
) -> Result<BTreeMap<String, String>, AppError> {
    let text = checked(
        run,
        &[
            "show",
            unit,
            "--property=LoadState,FragmentPath,DropInPaths,ActiveState,UnitFileState",
        ],
    )?;
    parse_properties(&text)
}

/// Parses complete structured properties; malformed evidence is an error.
pub(crate) fn parse_properties(text: &str) -> Result<BTreeMap<String, String>, AppError> {
    text.lines()
        .map(|line| {
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| AppError::Storage(format!("invalid systemctl property: {line}")))?;
            Ok((key.to_owned(), value.to_owned()))
        })
        .collect()
}

/// Probe the manager itself, independently of any unit's validity.
pub(crate) fn manager(run: &Runner<'_>) -> Result<(), String> {
    match run(&["show", "--property=Version", "--value"]) {
        Ok(reply) if reply.success && !reply.text.is_empty() => Ok(()),
        Ok(reply) => Err(format!("{} {}", reply.text, reply.diagnostic)
            .trim()
            .to_owned()),
        Err(error) => Err(error.to_string()),
    }
}
