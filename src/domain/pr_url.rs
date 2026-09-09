//! Parses a GitHub pull request web URL into its host-aware identity.
//!
//! # Why this lives in `domain` rather than `github`
//!
//! `src/github/` is compiled only under the `github-pr` feature, but
//! `story link-pr`/`unlink-pr` are feature-independent by design: a PR URL is
//! parsed, not fetched, so linking and unlinking never spend a network call
//! or a caller's token and must work in every build. The binding council
//! decision, recorded on SH-49, is explicit that
//! linking is "a pure event-sourced fact requiring no network access." This
//! function has no dependency on anything gated — no [`crate::error`]
//! aside — so it moved here rather
//! than staying behind the feature boundary its callers cannot cross.
//! [`crate::domain::remote::RemoteUrl`] already lives in `domain` for the
//! same class of reason: a URL-parsing utility that several unrelated
//! surfaces depend on belongs with the domain, not with one gated consumer
//! of it.
//!
//! [`crate::domain::github_remote`] sits beside this module for the same
//! reason — see its own doc comment.

use crate::error::AppError;

/// A pull request's host-aware identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestRef {
    /// The exact case-folded web host, including a non-default port.
    pub host: String,
    /// The repository owner.
    pub owner: String,
    /// The repository name.
    pub repo: String,
    /// The positive pull request number.
    pub number: u64,
}

/// Parses a GitHub pull request web URL, or refuses it.
///
/// # A different grammar from `parse_github_url`
///
/// [`crate::domain::github_remote::parse_github_url`] reads a *git remote* —
/// every scheme `git clone` accepts, including the scp-like
/// `[user@]github.com:owner/repo` form, and a path that is exactly
/// `owner/repo`. A pull request URL is never a clone target: it is always a
/// browser URL of the shape `https://{host}/{owner}/{repo}/pull/{number}`,
/// so this function has its own small grammar rather than reusing
/// `RemoteUrl` — reusing it would mean stripping `pull/{number}` back off
/// again to make the two-segment shape `path_on` expects, which loses the
/// part this function exists to keep.
///
/// # What it accepts
///
/// An HTTP(S) browser URL on any host with the exact
/// `/{owner}/{repo}/pull/{number}` path, an optional trailing slash, and
/// surrounding whitespace. Host, owner, and repo come back case-folded.
///
/// # What it refuses
///
/// Any scheme but `http`/`https`; credentials, query strings, or fragments;
/// a path that is not exactly `{owner}/{repo}/pull/{number}`; and a number
/// that is not a positive integer.
pub fn parse_pr_url(url: &str) -> Result<PullRequestRef, AppError> {
    let invalid = || {
        AppError::Validation(format!(
            "invalid GitHub pull request URL `{}`: expected \
             https://<github-host>/<owner>/<repo>/pull/<number>",
            url.trim()
        ))
    };

    let trimmed = url.trim();
    if trimmed.contains('#') {
        return Err(invalid());
    }
    let uri: ureq::http::Uri = trimmed.parse().map_err(|_| invalid())?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.query().is_some() {
        return Err(invalid());
    }
    let authority = uri.authority().ok_or_else(invalid)?;
    if authority.as_str().contains('@') {
        return Err(invalid());
    }

    let path = uri.path().trim_matches('/');
    let segments: Vec<&str> = path.split('/').collect();
    let [owner, repo, "pull", number] = segments[..] else {
        return Err(invalid());
    };
    if owner.is_empty() || repo.is_empty() {
        return Err(invalid());
    }
    let number: u64 = number.parse().map_err(|_| invalid())?;
    if number < 1 {
        return Err(invalid());
    }

    Ok(PullRequestRef {
        host: authority.as_str().to_lowercase(),
        owner: owner.to_lowercase(),
        repo: repo.to_lowercase(),
        number,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pr_url_reads_owner_repo_and_number() {
        let reference = parse_pr_url("https://github.com/acme/widgets/pull/7").unwrap();
        assert_eq!(reference.host, "github.com");
        assert_eq!(reference.owner, "acme");
        assert_eq!(reference.repo, "widgets");
        assert_eq!(reference.number, 7);
    }

    #[test]
    fn parse_pr_url_tolerates_a_trailing_slash() {
        let reference = parse_pr_url("https://github.com/acme/widgets/pull/7/").unwrap();
        assert_eq!(reference.owner, "acme");
        assert_eq!(reference.repo, "widgets");
        assert_eq!(reference.number, 7);
    }

    #[test]
    fn parse_pr_url_case_folds_owner_and_repo() {
        let reference = parse_pr_url("https://GitHub.com/Acme/Widgets/pull/7").unwrap();
        assert_eq!(reference.host, "github.com");
        assert_eq!(reference.owner, "acme");
        assert_eq!(reference.repo, "widgets");
    }

    #[test]
    fn parse_pr_url_accepts_http_as_well_as_https() {
        assert!(parse_pr_url("http://github.com/acme/widgets/pull/7").is_ok());
    }

    #[test]
    fn parse_pr_url_trims_surrounding_whitespace() {
        assert!(parse_pr_url("  https://github.com/acme/widgets/pull/7  ").is_ok());
    }

    #[test]
    fn parse_pr_url_accepts_an_enterprise_host_and_port() {
        let reference =
            parse_pr_url("https://github.example.com:8443/acme/widgets/pull/7").unwrap();
        assert_eq!(reference.host, "github.example.com:8443");
    }

    #[test]
    fn parse_pr_url_refuses_an_issue_url() {
        assert!(parse_pr_url("https://github.com/acme/widgets/issues/7").is_err());
    }

    #[test]
    fn parse_pr_url_refuses_a_pr_sub_page() {
        assert!(parse_pr_url("https://github.com/acme/widgets/pull/7/files").is_err());
    }

    #[test]
    fn parse_pr_url_refuses_a_non_numeric_number() {
        assert!(parse_pr_url("https://github.com/acme/widgets/pull/seven").is_err());
    }

    #[test]
    fn parse_pr_url_refuses_number_zero() {
        assert!(parse_pr_url("https://github.com/acme/widgets/pull/0").is_err());
    }

    #[test]
    fn parse_pr_url_refuses_an_scp_like_remote() {
        // The git-clone grammar `parse_github_url` accepts is not this
        // function's grammar — a pull request is never a clone target.
        assert!(parse_pr_url("git@github.com:acme/widgets.git").is_err());
    }

    #[test]
    fn parse_pr_url_refuses_credentials_query_strings_and_fragments() {
        for invalid in [
            "https://user@github.com/acme/widgets/pull/7",
            "https://github.com/acme/widgets/pull/7?notification_referrer_id=1",
            "https://github.com/acme/widgets/pull/7#issuecomment-1",
        ] {
            assert!(parse_pr_url(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn parse_pr_url_refuses_a_repository_root() {
        assert!(parse_pr_url("https://github.com/acme/widgets").is_err());
    }
}
