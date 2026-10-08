//! Local helper protocol; no daemon or store is opened.

use super::Repository;
use crate::cli::model::GithubVerb;
use crate::error::AppError;
use std::path::Path;

/// Executes the local GitHub helper protocol and returns its machine output.
pub fn run_local(arguments: &[String]) -> Result<Vec<u8>, AppError> {
    run_local_with_env(arguments, None)
}

/// Runs the same local protocol with one fixture's explicit subprocess policy.
/// Authority checks and the existing discovery-only retry are unchanged.
#[cfg(feature = "test-seam")]
pub fn run_local_for_fixture(
    arguments: &[String],
    env: &crate::env::Environment,
) -> Result<Vec<u8>, AppError> {
    run_local_with_env(arguments, Some(env))
}

fn resolve_repository(
    path: &Path,
    env: Option<&crate::env::Environment>,
) -> Result<Repository, AppError> {
    match env {
        Some(env) => Repository::resolve_with_env(path, env),
        None => Repository::resolve(path),
    }
}

fn resolve_observation(
    path: &Path,
    env: Option<&crate::env::Environment>,
) -> Result<super::OriginObservation, AppError> {
    match env {
        Some(env) => super::OriginObservation::resolve_with_env(path, env),
        None => super::OriginObservation::resolve(path),
    }
}

fn run_local_with_env(
    arguments: &[String],
    env: Option<&crate::env::Environment>,
) -> Result<Vec<u8>, AppError> {
    if arguments
        .first()
        .is_some_and(|mode| GithubVerb::find(mode) == Some(GithubVerb::Observe))
    {
        let usage = || AppError::Usage(crate::cli::model::usage::U111.into());
        if arguments.len() < 5 || arguments[1] != "--checkout" {
            return Err(usage());
        }
        let observation = resolve_observation(Path::new(&arguments[2]), env)?;
        let mut remaining = &arguments[3..];
        if remaining.first().is_some_and(|arg| arg == "--authority") {
            let authority = remaining.get(1).ok_or_else(usage)?;
            let source = resolve_observation(Path::new(authority), env)?;
            observation.require_authority(&source)?;
            remaining = &remaining[2..];
        }
        if remaining.len() < 2 || remaining[0] != "--" {
            return Err(usage());
        }
        return observation.git(&remaining[1..]);
    }
    run_attempt(arguments, true, env)
}

fn run_attempt(
    arguments: &[String],
    may_refresh: bool,
    env: Option<&crate::env::Environment>,
) -> Result<Vec<u8>, AppError> {
    let usage = || AppError::Usage(crate::cli::model::usage::U112.into());
    if arguments.len() < 3 || arguments[1] != "--checkout" {
        return Err(usage());
    }
    let repository = resolve_repository(Path::new(&arguments[2]), env)?;
    let mut remaining = &arguments[3..];
    let mut seen_authority = false;
    let mut seen_expected = false;
    while let Some(option) = remaining.first() {
        match option.as_str() {
            "--authority" if !seen_authority => {
                let path = remaining.get(1).ok_or_else(usage)?;
                let authority = resolve_repository(Path::new(path), env)?;
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
    let result = match GithubVerb::find(&arguments[0]) {
        Some(GithubVerb::Resolve) if remaining.is_empty() => serde_json::to_vec(&repository)
            .map_err(|error| {
                AppError::GithubApi(format!("encoding resolved GitHub origin: {error}"))
            }),
        Some(GithubVerb::Merge) if remaining.len() == 3 && remaining[0] == "--" => {
            let number = remaining[1].parse::<u64>().map_err(|_| usage())?;
            repository
                .merge_once(number, &remaining[2])
                .and_then(|reply| serde_json::to_vec(&reply).map_err(Into::into))
        }
        Some(GithubVerb::Exec) if remaining.len() > 1 && remaining[0] == "--" => {
            repository.gh(&remaining[1..])
        }
        Some(GithubVerb::Git) if remaining.len() > 1 && remaining[0] == "--" => {
            repository.git(&remaining[1..])
        }
        None
        | Some(
            GithubVerb::Observe
            | GithubVerb::Resolve
            | GithubVerb::Merge
            | GithubVerb::Exec
            | GithubVerb::Git,
        ) => Err(usage()),
    };
    let Err(error) = result else {
        return result;
    };
    if !may_refresh || seen_expected || !is_discovery_read(&arguments[0], remaining) {
        return Err(error);
    }
    let refreshed = resolve_repository(Path::new(&arguments[2]), env).map_err(|refresh| {
        refresh.with_context(&format!("origin refresh after failed read: {error}"))
    })?;
    if refreshed.identity() == repository.identity() {
        return Err(error);
    }
    // Reparse all authority and URL constraints. A moved origin cannot
    // authorize a previously supplied foreign PR URL or stale source checkout.
    run_attempt(arguments, false, env).map_err(|retry| {
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
