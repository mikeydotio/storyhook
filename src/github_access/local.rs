//! Local helper protocol; no daemon or store is opened.

use super::Repository;
use crate::error::AppError;
use std::path::Path;

/// Executes the local GitHub helper protocol and returns its machine output.
pub fn run_local(arguments: &[String]) -> Result<Vec<u8>, AppError> {
    let usage = || {
        AppError::Usage(
            "usage: story github resolve|exec|git --checkout PATH [--authority PATH] [-- ARGUMENTS]".into(),
        )
    };
    if arguments.len() < 3 || arguments[1] != "--checkout" {
        return Err(usage());
    }
    let repository = Repository::resolve(Path::new(&arguments[2]))?;
    let remaining = if arguments.get(3).is_some_and(|arg| arg == "--authority") {
        let path = arguments.get(4).ok_or_else(usage)?;
        let authority = Repository::resolve(Path::new(path))?;
        if authority.identity() != repository.identity() {
            return Err(AppError::Validation(format!(
                "checkout {} differs from source authority {}; refusing GitHub operation",
                repository.qualified(),
                authority.qualified()
            )));
        }
        &arguments[5..]
    } else {
        &arguments[3..]
    };
    match arguments[0].as_str() {
        "resolve" if remaining.is_empty() => serde_json::to_vec(&repository).map_err(|error| {
            AppError::GithubApi(format!("encoding resolved GitHub origin: {error}"))
        }),
        "exec" if remaining.len() > 1 && remaining[0] == "--" => repository.gh(&remaining[1..]),
        "git" if remaining.len() > 1 && remaining[0] == "--" => repository.git(&remaining[1..]),
        _ => Err(usage()),
    }
}
