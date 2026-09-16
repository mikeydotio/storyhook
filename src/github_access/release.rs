//! Explicit distribution authority, independent of every project checkout.

use crate::domain::github_remote::GithubRepo;
use crate::error::AppError;
use std::path::Path;

/// A validated, explicitly selected release repository.
#[derive(Debug, Clone)]
pub struct ReleaseSource(GithubRepo);

impl ReleaseSource {
    /// Parses HOST/OWNER/REPO without an implicit host or checkout fallback.
    pub fn parse(value: &str) -> Result<Self, AppError> {
        if value.split('/').count() != 3 || value.contains('@') || value.trim() != value {
            return Err(AppError::Validation(
                "release source must be HOST/OWNER/REPO".into(),
            ));
        }
        let identity = super::parse_origin(&format!("https://{value}"))?;
        Ok(Self(identity))
    }

    /// Canonical source identity, suitable for persisted installation metadata.
    pub fn qualified(&self) -> String {
        format!("{}/{}/{}", self.0.host, self.0.owner, self.0.repo)
    }

    /// Reads the latest published tag through the shared private gh boundary.
    pub fn latest_tag(&self) -> Result<String, AppError> {
        let bytes = super::command::execute(
            &self.0,
            Path::new("/"),
            &[
                "release".into(),
                "view".into(),
                "--json".into(),
                "tagName".into(),
            ],
        )?;
        #[derive(serde::Deserialize)]
        struct Release {
            #[serde(rename = "tagName")]
            tag: String,
        }
        let release: Release = serde_json::from_slice(&bytes).map_err(|e| {
            AppError::GithubApi(format!(
                "invalid release metadata for {}: {e}",
                self.qualified()
            ))
        })?;
        validate_tag(&release.tag)?;
        Ok(release.tag)
    }

    /// Downloads exactly one named asset to a new file, with no automatic retry.
    pub fn download(&self, tag: &str, artifact: &str, destination: &Path) -> Result<(), AppError> {
        validate_tag(tag)?;
        if artifact.is_empty()
            || !artifact
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(AppError::Validation(
                "release asset must be a literal filename".into(),
            ));
        }
        if !destination.is_absolute() || destination.exists() {
            return Err(AppError::Validation(
                "release download needs a new absolute destination".into(),
            ));
        }
        super::command::execute(
            &self.0,
            Path::new("/"),
            &[
                "release".into(),
                "download".into(),
                tag.into(),
                "--pattern".into(),
                artifact.into(),
                "--output".into(),
                destination.to_string_lossy().into_owned(),
            ],
        )?;
        if !destination.is_file() {
            return Err(AppError::GithubApi(
                "gh did not create the release asset".into(),
            ));
        }
        Ok(())
    }
}

fn validate_tag(tag: &str) -> Result<(), AppError> {
    if tag.is_empty()
        || tag.starts_with('-')
        || tag.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(AppError::GithubApi(
            "release metadata contains an invalid tag".into(),
        ));
    }
    Ok(())
}
