//! Local helper protocol; no daemon or store is opened.

use super::Repository;
use crate::error::AppError;
use std::path::Path;

/// Executes the local GitHub helper protocol and returns its machine output.
pub fn run_local(arguments: &[String]) -> Result<Vec<u8>, AppError> {
    let usage = || {
        AppError::Usage(
            "usage: story github resolve|exec|git --checkout PATH [-- ARGUMENTS]".into(),
        )
    };
    if arguments.len() < 3 || arguments[1] != "--checkout" {
        return Err(usage());
    }
    let repository = Repository::resolve(Path::new(&arguments[2]))?;
    match arguments[0].as_str() {
        "resolve" if arguments.len() == 3 => serde_json::to_vec(&repository).map_err(|error| {
            AppError::GithubApi(format!("encoding resolved GitHub origin: {error}"))
        }),
        "exec" if arguments.len() > 4 && arguments[3] == "--" => repository.gh(&arguments[4..]),
        "git" if arguments.len() > 4 && arguments[3] == "--" => repository.git(&arguments[4..]),
        _ => Err(usage()),
    }
}
