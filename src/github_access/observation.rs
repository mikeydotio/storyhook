//! Generic Git observations: GitHub HTTPS or explicitly file-only transport.

use super::{Repository, git_read};
use crate::{env::git_env, error::AppError, process::run_captured_private};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Destination {
    Github(crate::domain::github_remote::GithubRepo),
    File(PathBuf),
}

/// A pinned origin for default-branch and ancestry observations, not PR authority.
pub struct OriginObservation {
    checkout: PathBuf,
    destination: Destination,
}

impl OriginObservation {
    /// Resolves a checkout's single origin. Filesystem origins remain file-only.
    pub fn resolve(checkout: &Path) -> Result<Self, AppError> {
        let checkout = checkout.canonicalize()?;
        let root = git_read(&checkout, &["rev-parse", "--show-toplevel"])?;
        if Path::new(root.trim()).canonicalize()? != checkout {
            return Err(AppError::Validation(
                "origin observation requires a checkout root".into(),
            ));
        }
        let raw = git_read(&checkout, &["config", "--get-all", "remote.origin.url"])?;
        let origins: Vec<_> = raw.lines().collect();
        if origins.len() != 1 || origins[0].is_empty() || origins[0].chars().any(char::is_control) {
            return Err(AppError::Validation(
                "origin observation requires exactly one valid origin".into(),
            ));
        }
        let origin = origins[0];
        let destination = if !origin.contains(':') && !origin.starts_with('-') {
            Destination::File(checkout.join(origin).canonicalize().map_err(|error| {
                AppError::Validation(format!("local origin is unavailable: {error}"))
            })?)
        } else {
            Destination::Github(Repository::resolve(&checkout)?.identity().clone())
        };
        // A mirror must not discard a source checkout's redirect policy when
        // it copies raw origin. Validate the effective read destination here.
        let effective = git_read(&checkout, &["remote", "get-url", "--all", "origin"])?;
        let urls: Vec<_> = effective.lines().collect();
        let matches = urls.len() == 1
            && match &destination {
                Destination::Github(identity) => {
                    super::parse_origin(urls[0]).is_ok_and(|effective| &effective == identity)
                }
                Destination::File(path) => {
                    !urls[0].contains(':')
                        && checkout.join(urls[0]).canonicalize().ok().as_ref() == Some(path)
                }
            };
        if !matches {
            return Err(AppError::Validation(
                "origin observation: URL rewrite differs from origin".into(),
            ));
        }
        Ok(Self {
            checkout,
            destination,
        })
    }

    /// Requires a private observer checkout to match its freshly resolved source.
    pub(super) fn require_authority(&self, source: &Self) -> Result<(), AppError> {
        if self.destination != source.destination {
            return Err(AppError::Validation(
                "observer checkout differs from source authority".into(),
            ));
        }
        Ok(())
    }

    /// Reads origin refs or fetches an explicit refspec without granting remote write access.
    pub fn git(&self, arguments: &[String]) -> Result<Vec<u8>, AppError> {
        let refuse = |message: &str| AppError::Validation(format!("origin observation: {message}"));
        if !matches!(
            arguments.first().map(String::as_str),
            Some("ls-remote" | "fetch")
        ) {
            return Err(refuse("only ls-remote and fetch are allowed"));
        }
        if Self::resolve(&self.checkout)?.destination != self.destination {
            return Err(refuse("origin changed; resolve it again before retrying"));
        }
        let path = match &self.destination {
            Destination::Github(identity) => {
                return Repository {
                    checkout: self.checkout.clone(),
                    identity: identity.clone(),
                }
                .git(arguments);
            }
            Destination::File(path) => path,
        };
        let remote = arguments
            .iter()
            .position(|s| s == "origin")
            .ok_or_else(|| refuse("an explicit origin is required"))?;
        let flags = [
            "--quiet",
            "-q",
            "--prune",
            "--tags",
            "--heads",
            "--symref",
            "--exit-code",
            "--no-tags",
            "--no-recurse-submodules",
        ];
        if arguments[1..remote]
            .iter()
            .any(|s| !flags.contains(&s.as_str()))
            || arguments[remote + 1..].iter().any(|s| {
                s.starts_with('-') || s.contains("://") || s.chars().any(char::is_whitespace)
            })
        {
            return Err(refuse("unsupported observation arguments"));
        }
        let configured = git_read(&self.checkout, &["remote", "get-url", "--all", "origin"])?;
        let urls: Vec<_> = configured.lines().collect();
        if urls.len() != 1
            || urls[0].contains(':')
            || self.checkout.join(urls[0]).canonicalize().ok().as_ref() != Some(path)
        {
            return Err(refuse("a URL rewrite changes the local origin"));
        }
        let path = path
            .to_str()
            .ok_or_else(|| refuse("local origin is not UTF-8"))?;
        // Refuse even local rewrites: the configured path, not a Git alias, is
        // the observation authority. File-only protocol enforcement is also
        // passed to the actual operation to cover concurrent config changes.
        let effective = git_read(&self.checkout, &["ls-remote", "--get-url", path])?;
        if effective.trim() != path {
            return Err(refuse("a URL rewrite changes the local origin"));
        }
        let mut args = arguments.to_vec();
        args[remote] = path.into();
        let mut command = git_env::command(&self.checkout);
        command
            .env("GIT_ALLOW_PROTOCOL", "file")
            .args([
                "-c",
                "credential.helper=",
                "-c",
                "fetch.recurseSubmodules=false",
                "-c",
                "core.askPass=",
            ])
            .args(args);
        let output = run_captured_private(command, Duration::from_secs(120))
            .map_err(|e| refuse(&e.detail()))?;
        if !output.status.success() {
            return Err(refuse(&format!(
                "local Git failed ({}): {}",
                output.status,
                crate::daemon::crash::redact(&String::from_utf8_lossy(&output.stderr))
            )));
        }
        if output.stdout.len() >= 64 * 1024 {
            return Err(refuse("response exceeded the capture limit"));
        }
        Ok(output.stdout)
    }
}
