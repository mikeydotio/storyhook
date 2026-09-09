//! Interprets a registered network remote as a GitHub repository and resolves
//! the REST endpoint for the GitHub product its host names.
//!
//! # Why this lives in `domain` rather than `github`
//!
//! `src/github/` is compiled only under the `github-pr` feature, but
//! [`crate::service::pr_link::PrLinkService::link`]/`unlink` are
//! feature-independent by design — see [`crate::domain::pr_url`], which
//! states the same rule for the same reason (SH-49's council verdict) and
//! which this module now sits beside. `refuse_cross_repo`'s cross-repository
//! guard has to run in every build, so what it depends on has to compile in
//! every build too.
//!
//! This repository-identity half used to live in `github::sync_state`,
//! feature-gated alongside the *sync* config it no longer needs to read
//! (SH-408 retired the sync engine that document belonged to, and
//! `sync_state.rs` with it). Its remaining callers — `pr-check`, `link-pr` —
//! need it on the ungated side of that boundary, so it moved here instead of
//! surviving as a re-export.

use serde::{Deserialize, Serialize};

use crate::domain::remote::RemoteUrl;
use crate::error::AppError;

const GITHUB_HOST: &str = "github.com";
const GHE_CLOUD_SUFFIX: &str = ".ghe.com";

/// A GitHub repository, identified by host, owner, and name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GithubRepo {
    /// The exact normalized web host, including a non-default port.
    pub host: String,
    /// The repository owner.
    pub owner: String,
    /// The repository name.
    pub repo: String,
}

/// A validated GitHub REST API base URL.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GithubApiBase(String);

impl GithubApiBase {
    /// Derives the documented REST endpoint for the GitHub web `host`.
    ///
    /// GitHub.com uses its dedicated API hostname, GHE.com inserts `api.`
    /// before the tenant hostname, and GitHub Enterprise Server serves REST
    /// below `/api/v3` on the appliance host.
    #[must_use]
    pub fn for_host(host: &str) -> Self {
        let value = if host.eq_ignore_ascii_case(GITHUB_HOST) {
            "https://api.github.com".to_string()
        } else if host.ends_with(GHE_CLOUD_SUFFIX) && !host.contains(':') {
            format!("https://api.{host}")
        } else {
            format!("https://{host}/api/v3")
        };
        Self(value)
    }

    /// Validates and normalizes a project-authored `[github].api_url`.
    ///
    /// HTTP is accepted only through this explicit override because GitHub
    /// Enterprise Server documents both schemes and an internal appliance may
    /// terminate TLS elsewhere. Credentials, queries, and fragments are
    /// refused so a committed endpoint cannot smuggle request metadata.
    pub fn override_from(raw: &str) -> Result<Self, AppError> {
        let invalid = |reason: &str| {
            AppError::Validation(format!(
                "invalid [github].api_url `{}`: {reason}",
                raw.trim()
            ))
        };
        let trimmed = raw.trim();
        if trimmed.contains('#') {
            return Err(invalid("fragments are not allowed"));
        }
        let uri: ureq::http::Uri = trimmed
            .parse()
            .map_err(|_| invalid("expected an absolute http or https URL"))?;
        if !matches!(uri.scheme_str(), Some("http" | "https")) {
            return Err(invalid("expected an absolute http or https URL"));
        }
        let authority = uri.authority().ok_or_else(|| invalid("expected a host"))?;
        if authority.as_str().contains('@') {
            return Err(invalid("credentials are not allowed"));
        }
        if uri.query().is_some() {
            return Err(invalid("query strings are not allowed"));
        }
        Ok(Self(trimmed.trim_end_matches('/').to_string()))
    }

    /// The normalized base URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Parse a GitHub-compatible remote URL into host/owner/repo, or refuse it.
///
/// # One grammar
///
/// The URL grammar is [`RemoteUrl`]'s and nothing else's. This used to match
/// three literal prefixes of its own, which is how it came to refuse
/// `https://user@github.com/owner/repo.git` — a form two real repositories on
/// the author's machine use — while the identity grammar next door accepted it
/// (SH-137). Two parsers cannot drift apart if there is only one.
///
/// What is left here is the part that is GitHub's rather than git's: the path
/// must be **exactly** `owner/repo`. The host is data, not an allowlist: an
/// arbitrary hostname may run GitHub Enterprise Server. The later PR guard
/// requires that exact host again before any credential is spent.
///
/// # What it accepts
///
/// Every network spelling [`RemoteUrl`] accepts: `https`, `http`, `ssh` and
/// `git` schemes, the scp-like `[user@]host:owner/repo` form, with or without
/// userinfo, `.git`, a trailing slash, repeated slashes, or surrounding
/// whitespace.
///
/// # What it refuses
///
/// A filesystem remote and a path that is not exactly two segments — a browse URL
/// like `.../widgets/tree/main` used to yield the repo name `widgets/tree/main`,
/// which the API can only 404 on, silently persisted into the old sync config.
///
/// # Case
///
/// Owner and repo come back **case-folded**, because identity folds case for
/// every host. Every consumer is case-insensitive — the API paths are
/// `/repos/{owner}/{repo}`, which GitHub resolves either way — or cosmetic.
pub fn parse_github_url(url: &str) -> Option<GithubRepo> {
    // `normalize_for_lookup` rather than `normalize`: here every reason a URL
    // could be refused means the same thing — this remote is not a GitHub
    // project — and telling them apart would be the mistake.
    let remote = RemoteUrl::normalize_for_lookup(url)?;
    let (host, path) = remote.network_parts()?;
    let (owner, repo) = path.split_once('/')?;
    if repo.contains('/') {
        return None;
    }
    Some(GithubRepo {
        host: host.to_string(),
        owner: owner.to_string(),
        repo: repo.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_https_url() {
        let r = parse_github_url("https://github.com/acme/widgets.git").unwrap();
        assert_eq!(r.host, "github.com");
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
    fn parse_unclassifiable_url_returns_none() {
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
        // cosmetic.
        let r = parse_github_url("https://github.com/MikeyWard/KeyMux").unwrap();
        assert_eq!(r.owner, "mikeyward");
        assert_eq!(r.repo, "keymux");
    }

    #[test]
    fn parse_url_with_a_deeper_path_is_refused() {
        // A browse URL pasted as a remote used to yield repo
        // `widgets/tree/main` — a value the API can only 404 on, persisted
        // silently into the old sync config. A refusal is the honest answer.
        assert!(parse_github_url("https://github.com/acme/widgets/tree/main").is_none());
    }

    #[test]
    fn parse_url_naming_an_owner_but_no_repository_is_refused() {
        assert!(parse_github_url("https://github.com/acme").is_none());
        assert!(parse_github_url("https://github.com/").is_none());
    }

    #[test]
    fn parse_github_enterprise_host() {
        let r = parse_github_url("https://github.example.com/acme/widgets").unwrap();
        assert_eq!(r.host, "github.example.com");
        assert_eq!(r.owner, "acme");
        assert_eq!(r.repo, "widgets");
    }

    #[test]
    fn parse_url_on_a_github_port() {
        let r = parse_github_url("https://github.example.com:8443/acme/widgets").unwrap();
        assert_eq!(r.host, "github.example.com:8443");
    }

    #[test]
    fn parse_filesystem_remote_is_refused() {
        // A bare repository on a NAS is a real git remote and no GitHub
        // project. It must not be read as a host named `local`.
        assert!(parse_github_url("/srv/git/widgets.git").is_none());
        assert!(parse_github_url("file:///srv/git/widgets.git").is_none());
    }

    #[test]
    fn api_base_uses_each_github_products_documented_shape() {
        assert_eq!(
            GithubApiBase::for_host("github.com").as_str(),
            "https://api.github.com"
        );
        assert_eq!(
            GithubApiBase::for_host("octocorp.ghe.com").as_str(),
            "https://api.octocorp.ghe.com"
        );
        assert_eq!(
            GithubApiBase::for_host("github.example.com").as_str(),
            "https://github.example.com/api/v3"
        );
        assert_eq!(
            GithubApiBase::for_host("github.example.com:8443").as_str(),
            "https://github.example.com:8443/api/v3"
        );
    }

    #[test]
    fn api_base_override_is_absolute_http_without_credentials_or_query() {
        assert_eq!(
            GithubApiBase::override_from("http://github.internal.test/custom/api/")
                .unwrap()
                .as_str(),
            "http://github.internal.test/custom/api"
        );
        for invalid in [
            "github.internal.test/api/v3",
            "ftp://github.internal.test/api/v3",
            "https://user@github.internal.test/api/v3",
            "https://github.internal.test/api/v3?token=secret",
            "https://github.internal.test/api/v3#fragment",
        ] {
            assert!(GithubApiBase::override_from(invalid).is_err(), "{invalid}");
        }
    }
}
