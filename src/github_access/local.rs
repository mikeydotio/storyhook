//! Local helper protocol; no daemon or store is opened.

use super::Repository;
use crate::error::AppError;
use std::path::Path;

/// Executes the local GitHub helper protocol and returns its machine output.
pub fn run_local(arguments: &[String]) -> Result<Vec<u8>, AppError> {
    if arguments.first().is_some_and(|mode| mode == "observe") {
        if arguments.len() < 5 || arguments[1] != "--checkout" || arguments[3] != "--" {
            return Err(AppError::Usage(
                "usage: story github observe --checkout PATH -- ls-remote|fetch ARGUMENTS".into(),
            ));
        }
        return super::OriginObservation::resolve(Path::new(&arguments[2]))?.git(&arguments[4..]);
    }
    run_attempt(arguments, true)
}

fn run_attempt(arguments: &[String], may_refresh: bool) -> Result<Vec<u8>, AppError> {
    let usage = || {
        AppError::Usage(
            "usage: story github resolve|exec|git --checkout PATH [--authority PATH] [--expected HOST/OWNER/REPO] [-- ARGUMENTS]".into(),
        )
    };
    if arguments.len() < 3 || arguments[1] != "--checkout" {
        return Err(usage());
    }
    let repository = Repository::resolve(Path::new(&arguments[2]))?;
    let mut remaining = &arguments[3..];
    let mut seen_authority = false;
    let mut seen_expected = false;
    while let Some(option) = remaining.first() {
        match option.as_str() {
            "--authority" if !seen_authority => {
                let path = remaining.get(1).ok_or_else(usage)?;
                let authority = Repository::resolve(Path::new(path))?;
                if authority.identity() != repository.identity() {
                    return Err(AppError::Validation(format!(
                        "checkout {} differs from source authority {}; refusing GitHub operation",
                        repository.qualified(),
                        authority.qualified()
                    )));
                }
                seen_authority = true;
            }
            "--expected" if !seen_expected => {
                let expected = remaining.get(1).ok_or_else(usage)?;
                if expected != &repository.qualified() {
                    return Err(AppError::Validation(format!(
                        "GitHub origin changed from {expected} to {}; refusing to continue the operation",
                        repository.qualified()
                    )));
                }
                seen_expected = true;
            }
            "--" => break,
            _ => return Err(usage()),
        }
        remaining = &remaining[2..];
    }
    let result = match arguments[0].as_str() {
        "resolve" if remaining.is_empty() => serde_json::to_vec(&repository).map_err(|error| {
            AppError::GithubApi(format!("encoding resolved GitHub origin: {error}"))
        }),
        "exec" if remaining.len() > 1 && remaining[0] == "--" => repository.gh(&remaining[1..]),
        "git" if remaining.len() > 1 && remaining[0] == "--" => repository.git(&remaining[1..]),
        _ => Err(usage()),
    };
    let Err(error) = result else {
        return result;
    };
    if !may_refresh || seen_expected || !is_discovery_read(&arguments[0], remaining) {
        return Err(error);
    }
    let refreshed = Repository::resolve(Path::new(&arguments[2])).map_err(|refresh| {
        refresh.with_context(&format!("origin refresh after failed read: {error}"))
    })?;
    if refreshed.identity() == repository.identity() {
        return Err(error);
    }
    // Reparse all authority and URL constraints. A moved origin cannot
    // authorize a previously supplied foreign PR URL or stale source checkout.
    run_attempt(arguments, false).map_err(|retry| {
        retry.with_context(&format!(
            "one read retry after origin changed from {} to {}; initial failure: {error}",
            repository.qualified(),
            refreshed.qualified()
        ))
    })
}

fn is_discovery_read(mode: &str, arguments: &[String]) -> bool {
    if arguments.first().is_none_or(|arg| arg != "--") {
        return false;
    }
    let args = &arguments[1..];
    if mode == "git" {
        return args.first().is_some_and(|arg| arg == "ls-remote");
    }
    if mode != "exec" || args.len() < 2 {
        return false;
    }
    matches!(
        (args[0].as_str(), args[1].as_str()),
        ("pr", "view" | "list") | ("repo", "view") | ("release", "view" | "list")
    ) && args
        .iter()
        .any(|arg| arg == "--json" || arg.starts_with("--json="))
        && !args.iter().any(|arg| arg == "--web" || arg == "-w")
}
