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

use serde::{Deserialize, Serialize};

use crate::error::AppError;

/// Re-exported so every existing caller inside this gated module keeps its
/// old import path — see `crate::domain::github_remote`'s own doc for why
/// the definitions moved out from under the feature gate.
pub use crate::domain::github_remote::{GithubRepo, parse_github_url};

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

/// Parses a GitHub pull request web URL into `(owner, repo, number)`, or
/// refuses it (SH-49).
///
/// Re-exported from [`crate::domain::pr_url`] rather than defined here: that
/// module is ungated, so that `story link-pr`/`unlink-pr` work whether or not
/// the `github-sync` feature is compiled in. See its own documentation for
/// the grammar. Kept as `github::sync_state::parse_pr_url` too, so nothing
/// that already imports it from here needs to change.
pub use crate::domain::pr_url::parse_pr_url;

/// Detect owner/repo from this repository's `origin`.
///
/// Every way this can fail — no git, no `origin`, a URL that is not a GitHub
/// project — collapses to `Ok(None)`, which the caller reports as "No GitHub
/// remote found". See [`parse_github_url`] for which URLs are which.
pub fn detect_github_remote(root: &Path) -> Result<Option<GithubRepo>, AppError> {
    let output = crate::env::git_env::command(root)
        .args(["remote", "get-url", "origin"])
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

    // `parse_pr_url`'s own tests moved with it to `crate::domain::pr_url`,
    // and `parse_github_url`'s to `crate::domain::github_remote` — see those
    // modules.
}
