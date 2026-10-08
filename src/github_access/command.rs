//! Explicit gh routing and bounded, noninteractive execution.

use super::Repository;
use crate::error::AppError;
use crate::process::{CaptureError, run_captured_private};
use std::process::Command;
use std::time::Duration;

impl Repository {
    /// Revalidate origin and execute under one original operation deadline.
    pub(crate) fn gh_controlled(
        &self,
        arguments: &[String],
        deadline: std::time::Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<u8>, AppError> {
        if Self::resolve_controlled_with_bounds(&self.checkout, self.bounds, deadline, cancelled)?
            .identity
            != self.identity
        {
            return Err(AppError::Validation(
                "GitHub origin changed during bounded recovery inspection".into(),
            ));
        }
        execute_controlled(
            self.bounds.operation,
            &self.identity,
            &self.checkout,
            arguments,
            Some((deadline, cancelled)),
        )
    }

    /// Executes gh only against this repository, after revalidating its origin.
    pub fn gh(&self, arguments: &[String]) -> Result<Vec<u8>, AppError> {
        let current = Self::resolve_with_bounds(&self.checkout, self.bounds)?;
        if current.identity != self.identity {
            return Err(AppError::Validation(
                "GitHub origin changed during the operation; resolve it again before retrying"
                    .into(),
            ));
        }
        execute_with_bound(
            self.bounds.operation,
            &self.identity,
            &self.checkout,
            arguments,
        )
    }
}

/// Runs a routed command for a previously validated checkout or release source.
pub(super) fn execute(
    identity: &crate::domain::github_remote::GithubRepo,
    checkout: &std::path::Path,
    arguments: &[String],
) -> Result<Vec<u8>, AppError> {
    execute_with_bound(Duration::from_secs(120), identity, checkout, arguments)
}

fn execute_with_bound(
    bound: Duration,
    identity: &crate::domain::github_remote::GithubRepo,
    checkout: &std::path::Path,
    arguments: &[String],
) -> Result<Vec<u8>, AppError> {
    execute_controlled(bound, identity, checkout, arguments, None)
}

fn execute_controlled(
    bound: Duration,
    identity: &crate::domain::github_remote::GithubRepo,
    checkout: &std::path::Path,
    arguments: &[String],
    control: Option<(std::time::Instant, &dyn Fn() -> bool)>,
) -> Result<Vec<u8>, AppError> {
    let output = capture_controlled(bound, identity, checkout, arguments, control)?;
    if !output.status.success() {
        let mut detail = String::from_utf8_lossy(&output.stderr).into_owned();
        for name in crate::env::spawn_env::GITHUB_CREDENTIAL_MAY_SEE {
            if name.ends_with("TOKEN")
                && let Ok(value) = std::env::var(name)
                && !value.is_empty()
            {
                detail = detail.replace(&value, "[REDACTED]");
            }
        }
        let detail = crate::daemon::crash::redact(&detail);
        let auth = output.status.code() == Some(4) || detail.contains("401");
        let context = format!(
            "gh for {} failed ({}): {detail}",
            qualified(identity),
            output.status
        );
        return Err(if auth {
            AppError::GithubAuth(format!(
                "{context}\nCheck authentication with gh auth status --hostname {}; if needed, run gh auth login --hostname {} interactively.",
                identity.host, identity.host
            ))
        } else {
            AppError::GithubApi(context)
        });
    }
    // The shared capture bounds diagnostics at 64 KiB. Never return a
    // successful but truncated API answer to a lifecycle decision.
    if output.stdout.len() >= 64 * 1024 {
        return Err(AppError::GithubApi(format!(
            "gh for {} exceeded the capture limit; select bounded JSON fields or download to a file",
            qualified(identity)
        )));
    }
    Ok(output.stdout)
}

fn routed_arguments(
    identity: &crate::domain::github_remote::GithubRepo,
    arguments: &[String],
) -> Result<Vec<String>, AppError> {
    let refuse = |reason: &str| {
        AppError::Validation(format!(
            "GitHub routing for {}: {reason}",
            qualified(identity)
        ))
    };
    if arguments.iter().any(|arg| {
        arg == "--repo"
            || arg.starts_with("--repo=")
            || arg.starts_with("-R")
            || arg == "--hostname"
            || arg.starts_with("--hostname=")
            || arg.starts_with("-H")
            || arg == "--header"
            || arg.starts_with("--header=")
    }) {
        return Err(refuse("destination flags are owned by the origin resolver"));
    }
    let mut result = arguments.to_vec();
    match arguments.first().map(String::as_str) {
        Some("api") => {
            let endpoint = arguments.get(1).ok_or_else(|| {
                refuse("expected a repository API endpoint immediately after api")
            })?;
            let prefix = format!("repos/{}/{}/", identity.owner, identity.repo);
            if !endpoint.starts_with(&prefix)
                || endpoint.contains(['%', '#', '{', '}'])
                || endpoint
                    .split('?')
                    .next()
                    .unwrap_or_default()
                    .split('/')
                    .any(|part| part == ".." || part == "." || part.is_empty())
            {
                return Err(refuse("API endpoint must address the resolved repository"));
            }
            result.extend(["--hostname".into(), identity.host.clone()]);
        }
        Some("pr" | "release" | "repo") => {
            if arguments.len() < 2 {
                return Err(refuse("expected a repository command"));
            }
            if arguments[0] == "repo" && arguments[1] != "view" {
                return Err(refuse(
                    "only repo view is supported by this repository-bound helper",
                ));
            }
            if arguments[0] == "repo" && arguments.get(2).is_some_and(|arg| !arg.starts_with('-')) {
                return Err(refuse("repo view takes its repository only from origin"));
            }
            for (index, value) in arguments.iter().enumerate().skip(2) {
                if arguments[0] != "pr" || !value.contains("://") {
                    continue;
                }
                if matches!(
                    arguments[index - 1].as_str(),
                    "--body" | "--title" | "--notes" | "--template" | "--jq"
                ) {
                    continue;
                }
                let reference = crate::domain::pr_url::parse_pr_url(value)?;
                if !reference.host.eq_ignore_ascii_case(&identity.host)
                    || !reference.owner.eq_ignore_ascii_case(&identity.owner)
                    || !reference.repo.eq_ignore_ascii_case(&identity.repo)
                {
                    return Err(refuse("pull request does not belong to the current origin"));
                }
                result[index] = reference.number.to_string();
            }
            if arguments[0] == "repo" {
                result.insert(2, qualified(identity));
            } else {
                result.extend(["--repo".into(), qualified(identity)]);
            }
        }
        _ => {
            return Err(refuse(
                "only repository API, PR, release and repo view commands are supported",
            ));
        }
    }
    Ok(result)
}

fn qualified(identity: &crate::domain::github_remote::GithubRepo) -> String {
    format!("{}/{}/{}", identity.host, identity.owner, identity.repo)
}

/// Captures one routed command without discarding its refusal diagnostics.
pub(super) fn capture(
    bound: Duration,
    identity: &crate::domain::github_remote::GithubRepo,
    checkout: &std::path::Path,
    arguments: &[String],
) -> Result<crate::process::Captured, AppError> {
    capture_controlled(bound, identity, checkout, arguments, None)
}

fn capture_controlled(
    bound: Duration,
    identity: &crate::domain::github_remote::GithubRepo,
    checkout: &std::path::Path,
    arguments: &[String],
    control: Option<(std::time::Instant, &dyn Fn() -> bool)>,
) -> Result<crate::process::Captured, AppError> {
    let arguments = routed_arguments(identity, arguments)?;
    let mut command = Command::new("gh");
    crate::env::spawn_env::apply_verification_allowlist(&mut command);
    command
        .current_dir(checkout)
        .args(arguments)
        .env("GH_HOST", &identity.host)
        .env("GH_REPO", qualified(identity))
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_PAGER", "cat")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    match control {
        Some((deadline, cancelled)) => crate::process::run_captured_private_until(
            command,
            deadline.min(std::time::Instant::now() + bound),
            cancelled,
        ),
        None => run_captured_private(command, bound),
    }
    .map_err(|error| {
        let detail = match error {
            CaptureError::Spawn(ref source) if source.kind() == std::io::ErrorKind::NotFound => {
                "install gh and make it available on the StoryHook process PATH".to_owned()
            }
            _ => error.detail(),
        };
        AppError::GithubApi(format!(
            "gh for {}: {detail}; the operation was not retried",
            qualified(identity)
        ))
    })
}
