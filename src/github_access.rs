//! Origin-bound GitHub subprocess access shared by services and local helpers.

use crate::domain::github_remote::GithubRepo;
use crate::env::git_env;
use crate::error::AppError;
use crate::process::run_captured_private;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod command;
mod local;
mod transport;
pub use local::run_local;

/// A checkout and its current, validated GitHub origin.
#[derive(Debug, Clone, Serialize)]
pub struct Repository {
    checkout: PathBuf,
    identity: GithubRepo,
}

impl Repository {
    /// Reads the actual origin of an existing checkout, without cached routing.
    pub fn resolve(checkout: &Path) -> Result<Self, AppError> {
        let checkout = checkout.canonicalize().map_err(|error| {
            AppError::Validation(format!(
                "GitHub checkout {} is unavailable: {error}",
                checkout.display()
            ))
        })?;
        let root = git_read(&checkout, &["rev-parse", "--show-toplevel"])?;
        let root = Path::new(root.trim()).canonicalize().map_err(|error| {
            AppError::Validation(format!("cannot resolve GitHub checkout root: {error}"))
        })?;
        if root != checkout {
            return Err(AppError::Validation(format!(
                "{} is inside {}, not a registered checkout root",
                checkout.display(),
                root.display()
            )));
        }
        let origins = git_read(&checkout, &["config", "--get-all", "remote.origin.url"])?;
        let origins: Vec<_> = origins.lines().collect();
        if origins.len() != 1 {
            return Err(AppError::Validation(format!(
                "GitHub checkout {} must have exactly one remote.origin.url",
                checkout.display()
            )));
        }
        let identity = parse_origin(origins[0]).map_err(|error| {
            error.with_context(&format!("GitHub origin in {}", checkout.display()))
        })?;
        Ok(Self { checkout, identity })
    }

    /// The explicit host, owner and repository for every operation.
    pub fn identity(&self) -> &GithubRepo {
        &self.identity
    }

    /// The fully qualified repository argument accepted by gh.
    pub fn qualified(&self) -> String {
        format!(
            "{}/{}/{}",
            self.identity.host, self.identity.owner, self.identity.repo
        )
    }

    /// HTTPS object transport destination derived from this origin.
    pub fn transport_url(&self) -> String {
        format!("https://{}.git", self.qualified())
    }
}

fn git_read(checkout: &Path, arguments: &[&str]) -> Result<String, AppError> {
    let mut command = git_env::command(checkout);
    command.args(arguments);
    let output = run_captured_private(command, Duration::from_secs(30)).map_err(|error| {
        AppError::Validation(format!(
            "reading GitHub origin in {}: {}",
            checkout.display(),
            error.detail()
        ))
    })?;
    if !output.status.success() {
        // Git can include the raw configured URL in a diagnostic. Never copy it:
        // a refused origin may contain credentials that have not been validated.
        return Err(AppError::Validation(format!(
            "cannot read {} in {}; check the checkout and configure exactly one origin",
            arguments.join(" "),
            checkout.display()
        )));
    }
    String::from_utf8(output.stdout).map_err(|_| {
        AppError::Validation(format!(
            "GitHub origin in {} is not UTF-8",
            checkout.display()
        ))
    })
}

fn parse_origin(raw: &str) -> Result<GithubRepo, AppError> {
    let refuse = |reason: &str| AppError::Validation(format!("invalid GitHub origin: {reason}"));
    let raw = raw.trim();
    if raw.chars().any(|c| c.is_control() || c.is_whitespace()) || raw.contains(['?', '#', '%']) {
        return Err(refuse(
            "whitespace, escapes, queries and fragments are unsupported",
        ));
    }
    let ssh = raw.starts_with("ssh://");
    if let Some((scheme, rest)) = raw.split_once("://") {
        if !matches!(scheme, "https" | "ssh") {
            return Err(refuse("use an HTTPS or SSH origin"));
        }
        let authority = rest.split('/').next().unwrap_or_default();
        if let Some((userinfo, _)) = authority.rsplit_once('@')
            && (userinfo.contains(':') || userinfo.contains('@'))
        {
            return Err(refuse(
                "embedded passwords are forbidden; use gh authentication",
            ));
        }
    } else if !raw.contains(':') || raw.starts_with('/') || raw.starts_with('.') {
        return Err(refuse("use an HTTPS or SSH origin"));
    }
    let mut identity = crate::domain::github_remote::parse_github_url(raw)
        .ok_or_else(|| refuse("expected HOST/OWNER/REPO"))?;
    if ssh && let Some((host, port)) = identity.host.rsplit_once(':') {
        if port != "22" {
            return Err(refuse(
                "a nonstandard SSH port cannot determine the API port; configure an HTTPS origin",
            ));
        }
        identity.host = host.to_owned();
    }
    let authority: ureq::http::uri::Authority = identity
        .host
        .parse()
        .map_err(|_| refuse("invalid hostname or port"))?;
    if authority.host().is_empty()
        || authority.host().starts_with('-')
        || authority.host().contains(['@', '[', ']'])
        || (identity.host.contains(':') && authority.port_u16().is_none())
    {
        return Err(refuse("invalid hostname or unsupported port"));
    }
    for segment in [&identity.owner, &identity.repo] {
        if segment.is_empty()
            || matches!(segment.as_str(), "." | "..")
            || !segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            return Err(refuse("invalid owner or repository name"));
        }
    }
    Ok(identity)
}
