//! github-sync's configuration document, its mapping lookups, and remote
//! detection.
//!
//! It used to persist all of that too — `.storyhook/github-sync.toml`, a
//! directory of base snapshots, and a directory of pre-sync backups, all
//! inside the user's repository. Every one of those moved behind
//! [`super::storage::SyncStorage`] and lives in the store or the state home
//! now, so what is left here is the *shape* of the configuration and the two
//! things that are genuinely about the repository rather than about storage:
//! which GitHub project `origin` names, and how to find a story's mapping
//! inside a config someone else loaded.
//!
//! There are consequently no `.storyhook` paths in this module, and none
//! anywhere under `src/github/`.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::error::AppError;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubSyncConfig {
    pub github: GithubRepo,
    pub sync: SyncSettings,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub etags: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mappings: Vec<StoryIssueMapping>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GithubRepo {
    pub owner: String,
    pub repo: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SyncMode {
    Off,
    #[default]
    Manual,
    Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncSettings {
    #[serde(default)]
    pub mode: SyncMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_full_sync_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoryIssueMapping {
    pub story_id: String,
    pub issue_number: u64,
    pub last_synced_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_local_event_index: Option<usize>,
}

// ---------------------------------------------------------------------------
// Mapping lookups
// ---------------------------------------------------------------------------

/// Find the mapping for a given story ID.
pub fn find_mapping<'a>(
    config: &'a GithubSyncConfig,
    story_id: &str,
) -> Option<&'a StoryIssueMapping> {
    config.mappings.iter().find(|m| m.story_id == story_id)
}

/// Find the mapping for a given GitHub issue number.
pub fn find_mapping_by_issue(
    config: &GithubSyncConfig,
    issue_number: u64,
) -> Option<&StoryIssueMapping> {
    config
        .mappings
        .iter()
        .find(|m| m.issue_number == issue_number)
}

// ---------------------------------------------------------------------------
// Git remote detection
// ---------------------------------------------------------------------------

/// Parse a GitHub remote URL into owner/repo.
///
/// Handles:
/// - `https://github.com/{owner}/{repo}.git`
/// - `https://github.com/{owner}/{repo}`
/// - `git@github.com:{owner}/{repo}.git`
pub fn parse_github_url(url: &str) -> Option<GithubRepo> {
    let url = url.trim();

    // SSH format: git@github.com:owner/repo.git
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        let rest = rest.strip_suffix(".git").unwrap_or(rest);
        let (owner, repo) = rest.split_once('/')?;
        if owner.is_empty() || repo.is_empty() {
            return None;
        }
        return Some(GithubRepo {
            owner: owner.to_string(),
            repo: repo.to_string(),
        });
    }

    // HTTPS format: https://github.com/owner/repo[.git]
    if let Some(rest) = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
    {
        let rest = rest.strip_suffix(".git").unwrap_or(rest);
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        let (owner, repo) = rest.split_once('/')?;
        if owner.is_empty() || repo.is_empty() {
            return None;
        }
        return Some(GithubRepo {
            owner: owner.to_string(),
            repo: repo.to_string(),
        });
    }

    None
}

/// Detect owner/repo from git remote origin URL.
/// Handles both HTTPS and SSH formats.
pub fn detect_github_remote(root: &Path) -> Result<Option<GithubRepo>, AppError> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(root)
        .output();

    let output = match output {
        Ok(o) => o,
        Err(_) => return Ok(None),
    };

    if !output.status.success() {
        return Ok(None);
    }

    let url = String::from_utf8_lossy(&output.stdout);
    Ok(parse_github_url(&url))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_roundtrip() {
        let config = GithubSyncConfig {
            github: GithubRepo {
                owner: "acme".into(),
                repo: "widgets".into(),
            },
            sync: SyncSettings {
                mode: SyncMode::Auto,
                last_sync_at: Some("2026-03-30T12:00:00Z".into()),
                last_full_sync_at: None,
            },
            etags: BTreeMap::from([("issues".into(), "abc123".into())]),
            mappings: vec![StoryIssueMapping {
                story_id: "SH-1".into(),
                issue_number: 42,
                last_synced_at: "2026-03-30T12:00:00Z".into(),
                last_local_event_index: Some(5),
            }],
        };

        let text = toml::to_string_pretty(&config).unwrap();
        let parsed: GithubSyncConfig = toml::from_str(&text).unwrap();

        assert_eq!(parsed.github.owner, "acme");
        assert_eq!(parsed.github.repo, "widgets");
        assert_eq!(parsed.sync.mode, SyncMode::Auto);
        assert_eq!(
            parsed.sync.last_sync_at.as_deref(),
            Some("2026-03-30T12:00:00Z")
        );
        assert!(parsed.sync.last_full_sync_at.is_none());
        assert_eq!(parsed.etags.get("issues").unwrap(), "abc123");
        assert_eq!(parsed.mappings.len(), 1);
        assert_eq!(parsed.mappings[0].story_id, "SH-1");
        assert_eq!(parsed.mappings[0].issue_number, 42);
        assert_eq!(parsed.mappings[0].last_local_event_index, Some(5));
    }

    #[test]
    fn sync_mode_serialization() {
        // Serialize via JSON (TOML cannot serialize bare enums)
        assert_eq!(serde_json::to_string(&SyncMode::Off).unwrap(), r#""off""#);
        assert_eq!(
            serde_json::to_string(&SyncMode::Manual).unwrap(),
            r#""manual""#
        );
        assert_eq!(serde_json::to_string(&SyncMode::Auto).unwrap(), r#""auto""#);

        // Deserialize
        let off: SyncMode = serde_json::from_str(r#""off""#).unwrap();
        assert_eq!(off, SyncMode::Off);
        let manual: SyncMode = serde_json::from_str(r#""manual""#).unwrap();
        assert_eq!(manual, SyncMode::Manual);
        let auto: SyncMode = serde_json::from_str(r#""auto""#).unwrap();
        assert_eq!(auto, SyncMode::Auto);
    }

    #[test]
    fn sync_mode_default_is_manual() {
        assert_eq!(SyncMode::default(), SyncMode::Manual);
    }

    fn sample_config() -> GithubSyncConfig {
        GithubSyncConfig {
            github: GithubRepo {
                owner: "org".into(),
                repo: "project".into(),
            },
            sync: SyncSettings {
                mode: SyncMode::Manual,
                last_sync_at: None,
                last_full_sync_at: None,
            },
            etags: BTreeMap::new(),
            mappings: vec![
                StoryIssueMapping {
                    story_id: "SH-1".into(),
                    issue_number: 10,
                    last_synced_at: "2026-01-01T00:00:00Z".into(),
                    last_local_event_index: None,
                },
                StoryIssueMapping {
                    story_id: "SH-2".into(),
                    issue_number: 20,
                    last_synced_at: "2026-01-02T00:00:00Z".into(),
                    last_local_event_index: Some(3),
                },
            ],
        }
    }

    #[test]
    fn find_mapping_by_story_id() {
        let config = sample_config();

        let m = find_mapping(&config, "SH-1").unwrap();
        assert_eq!(m.issue_number, 10);

        let m = find_mapping(&config, "SH-2").unwrap();
        assert_eq!(m.issue_number, 20);

        assert!(find_mapping(&config, "SH-99").is_none());
    }

    #[test]
    fn find_mapping_by_issue_number() {
        let config = sample_config();

        let m = find_mapping_by_issue(&config, 10).unwrap();
        assert_eq!(m.story_id, "SH-1");

        let m = find_mapping_by_issue(&config, 20).unwrap();
        assert_eq!(m.story_id, "SH-2");

        assert!(find_mapping_by_issue(&config, 999).is_none());
    }

    #[test]
    fn parse_https_url() {
        let r = parse_github_url("https://github.com/acme/widgets.git").unwrap();
        assert_eq!(r.owner, "acme");
        assert_eq!(r.repo, "widgets");
    }

    #[test]
    fn parse_https_url_no_dot_git() {
        let r = parse_github_url("https://github.com/acme/widgets").unwrap();
        assert_eq!(r.owner, "acme");
        assert_eq!(r.repo, "widgets");
    }

    #[test]
    fn parse_https_url_trailing_slash() {
        let r = parse_github_url("https://github.com/acme/widgets/").unwrap();
        assert_eq!(r.owner, "acme");
        assert_eq!(r.repo, "widgets");
    }

    #[test]
    fn parse_ssh_url() {
        let r = parse_github_url("git@github.com:acme/widgets.git").unwrap();
        assert_eq!(r.owner, "acme");
        assert_eq!(r.repo, "widgets");
    }

    #[test]
    fn parse_ssh_url_no_dot_git() {
        let r = parse_github_url("git@github.com:acme/widgets").unwrap();
        assert_eq!(r.owner, "acme");
        assert_eq!(r.repo, "widgets");
    }

    #[test]
    fn parse_non_github_url_returns_none() {
        assert!(parse_github_url("https://gitlab.com/acme/widgets.git").is_none());
        assert!(parse_github_url("git@gitlab.com:acme/widgets.git").is_none());
        assert!(parse_github_url("not-a-url").is_none());
    }

    #[test]
    fn parse_url_with_newline_trimmed() {
        // git remote get-url often returns a trailing newline
        let r = parse_github_url("https://github.com/acme/widgets.git\n").unwrap();
        assert_eq!(r.owner, "acme");
        assert_eq!(r.repo, "widgets");
    }
}
