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

use crate::domain::remote::RemoteUrl;
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

/// The one host github-sync can talk to.
///
/// Matched by **whole-host equality**, never a suffix. [`super::client`]
/// hardcodes `https://api.github.com`, so accepting `github.example.com` would
/// point the client at a same-named *public* repository and push an internal
/// project's stories into a stranger's issue tracker; `ends_with("github.com")`
/// would admit that and `evilgithub.com` besides. GitHub Enterprise needs a
/// host-derived API base, which is its own story rather than a looser match
/// here.
///
/// A private duplicate of [`crate::domain::pr_url`]'s own copy of this same
/// literal — that module is ungated and this one is not, and threading a
/// shared constant across the feature boundary is not worth it for a value
/// that will never change.
const GITHUB_HOST: &str = "github.com";

/// Parse a GitHub remote URL into owner/repo, or refuse it.
///
/// # One grammar
///
/// The URL grammar is [`RemoteUrl`]'s and nothing else's. This used to match
/// three literal prefixes of its own, which is how it came to refuse
/// `https://user@github.com/owner/repo.git` — a form two real repositories on
/// the author's machine use — while the identity grammar next door accepted it
/// (SH-137). Two parsers cannot drift apart if there is only one.
///
/// What is left here is the part that is GitHub's rather than git's: the host
/// must be [`GITHUB_HOST`], and the path must be **exactly** `owner/repo`. That
/// rule does not belong in [`RemoteUrl`] — GitLab's nested subgroups make three
/// segments legitimate there — so [`RemoteUrl::path_on`] hands back the path and
/// this function decides.
///
/// # What it accepts
///
/// Every spelling identity accepts on `github.com`: `https`, `http`, `ssh` and
/// `git` schemes, the scp-like `[user@]github.com:owner/repo` form, with or
/// without userinfo, `.git`, a trailing slash, repeated slashes, or surrounding
/// whitespace.
///
/// # What it refuses
///
/// Any other host, including a GitHub Enterprise one and any host on a port; a
/// filesystem remote; and a path that is not exactly two segments — a browse URL
/// like `.../widgets/tree/main` used to yield the repo name `widgets/tree/main`,
/// which the API can only 404 on, silently persisted into the sync config.
///
/// # Case
///
/// Owner and repo come back **case-folded**, because identity folds case for
/// every host. Every consumer is case-insensitive — the API paths are
/// `/repos/{owner}/{repo}`, which GitHub resolves either way — or cosmetic. A
/// config written before this change keeps its own spelling and nothing
/// re-derives it, so there is nothing to migrate.
pub fn parse_github_url(url: &str) -> Option<GithubRepo> {
    // `normalize_for_lookup` rather than `normalize`: here every reason a URL
    // could be refused means the same thing — this remote is not a GitHub
    // project — and telling them apart would be the mistake.
    let remote = RemoteUrl::normalize_for_lookup(url)?;
    let (owner, repo) = remote.path_on(GITHUB_HOST)?.split_once('/')?;
    if repo.contains('/') {
        return None;
    }
    Some(GithubRepo {
        owner: owner.to_string(),
        repo: repo.to_string(),
    })
}

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
    fn parse_https_url_with_userinfo() {
        // The exact form two real repositories on the author's machine use.
        // Userinfo is a credential hint git carries in the url, not part of the
        // repository's identity, so it must not decide whether a remote is
        // GitHub — it used to, and github-sync was unreachable for both (SH-137).
        let r = parse_github_url("https://wookiee@github.com/mikeyward/keymux.git").unwrap();
        assert_eq!(r.owner, "mikeyward");
        assert_eq!(r.repo, "keymux");
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

    // -----------------------------------------------------------------------
    // What the one grammar decided (SH-137)
    //
    // Everything above this line predates the delegation and passes unchanged;
    // that is the gate on the URLs that already worked. Everything below is an
    // arm the delegation newly decides, one test each, because delegating to a
    // strictly more permissive grammar is a behaviour change and not a
    // refactor.
    // -----------------------------------------------------------------------

    #[test]
    fn parse_ssh_scheme_url() {
        // A valid GitHub origin the three literal prefixes could not see.
        let r = parse_github_url("ssh://git@github.com/acme/widgets").unwrap();
        assert_eq!(r.owner, "acme");
        assert_eq!(r.repo, "widgets");
    }

    #[test]
    fn parse_git_scheme_url() {
        let r = parse_github_url("git://github.com/acme/widgets.git").unwrap();
        assert_eq!(r.owner, "acme");
        assert_eq!(r.repo, "widgets");
    }

    #[test]
    fn parse_scp_url_with_userinfo_other_than_git() {
        // `git@` was matched as a literal, so any other user missed. The
        // grammar cares that there is userinfo, not what it says.
        let r = parse_github_url("wookiee@github.com:acme/widgets.git").unwrap();
        assert_eq!(r.owner, "acme");
        assert_eq!(r.repo, "widgets");
    }

    #[test]
    fn parse_url_with_repeated_slashes() {
        let r = parse_github_url("https://github.com//acme//widgets").unwrap();
        assert_eq!(r.owner, "acme");
        assert_eq!(r.repo, "widgets");
    }

    #[test]
    fn parse_url_folds_case() {
        // Identity folds case for every host, so the pair this yields is
        // folded too. Every consumer is case-insensitive — the API paths are
        // `/repos/{owner}/{repo}`, which GitHub resolves either way — or
        // cosmetic. A config written before this change keeps its own spelling
        // and nothing re-derives it, so there is nothing to migrate.
        let r = parse_github_url("https://github.com/MikeyWard/KeyMux").unwrap();
        assert_eq!(r.owner, "mikeyward");
        assert_eq!(r.repo, "keymux");
    }

    #[test]
    fn parse_url_with_a_deeper_path_is_refused() {
        // A browse URL pasted as a remote used to yield repo
        // `widgets/tree/main` — a value the API can only 404 on, persisted
        // silently into the sync config. A refusal is the honest answer.
        assert!(parse_github_url("https://github.com/acme/widgets/tree/main").is_none());
    }

    #[test]
    fn parse_url_naming_an_owner_but_no_repository_is_refused() {
        assert!(parse_github_url("https://github.com/acme").is_none());
        assert!(parse_github_url("https://github.com/").is_none());
    }

    #[test]
    fn parse_github_enterprise_host_is_refused() {
        // `GithubClient` hardcodes `https://api.github.com`, so accepting a GHE
        // host would build a client that queries a same-named *public*
        // repository and push an internal project's stories into a stranger's
        // issue tracker. Whole-host equality, never a suffix match — which is
        // also what keeps `evilgithub.com` out. Supporting GHE means a
        // host-derived API base, and that is its own story.
        assert!(parse_github_url("https://github.example.com/acme/widgets").is_none());
        assert!(parse_github_url("https://evilgithub.com/acme/widgets").is_none());
    }

    #[test]
    fn parse_url_on_a_github_port_is_refused() {
        // A port names a different endpoint, and the API base is not it.
        assert!(parse_github_url("https://github.com:8443/acme/widgets").is_none());
    }

    #[test]
    fn parse_filesystem_remote_is_refused() {
        // A bare repository on a NAS is a real git remote and no GitHub
        // project. It must not be read as a host named `local`.
        assert!(parse_github_url("/srv/git/widgets.git").is_none());
        assert!(parse_github_url("file:///srv/git/widgets.git").is_none());
    }

    // `parse_pr_url`'s own tests moved with it to
    // `crate::domain::pr_url` — see that module.
}
