//! Parses a GitHub pull request web URL into `(owner, repo, number)`
//! (SH-49).
//!
//! # Why this lives in `domain` rather than `github`
//!
//! `src/github/` is compiled only under the `github-sync` feature, but
//! `story link-pr`/`unlink-pr` are feature-independent by design: a PR URL is
//! parsed, not fetched, so linking and unlinking never spend a network call
//! or a caller's token and must work in every build. The binding council
//! decision, recorded on SH-49, is explicit that
//! linking is "a pure event-sourced fact requiring no network access." This
//! function has no dependency on anything gated — no [`crate::error`]
//! aside, it only touches a local host constant — so it moved here rather
//! than staying behind the feature boundary its callers cannot cross.
//! [`crate::domain::remote::RemoteUrl`] already lives in `domain` for the
//! same class of reason: a URL-parsing utility that several unrelated
//! surfaces depend on belongs with the domain, not with one gated consumer
//! of it.
//!
//! `crate::github::sync_state` re-exports this function so every existing
//! caller inside the gated module keeps its old import path.

use crate::error::AppError;

/// The one host a pull request URL can name.
///
/// A private duplicate of `crate::github::sync_state::GITHUB_HOST` — that
/// copy is feature-gated and this one is not, and threading a shared
/// constant across the feature boundary is not worth it for a value that
/// will never change. Matched by **whole-host equality**, never a suffix,
/// for the identical reason `sync_state`'s copy documents: a hardcoded
/// `api.github.com` client must never be pointed at a same-named public
/// repository on a lookalike host.
const GITHUB_HOST: &str = "github.com";

/// Parses a GitHub pull request web URL into `(owner, repo, number)`, or
/// refuses it (SH-49).
///
/// # A different grammar from `parse_github_url`
///
/// `crate::github::sync_state::parse_github_url` reads a *git remote* —
/// every scheme `git clone` accepts, including the scp-like
/// `[user@]github.com:owner/repo` form, and a path that is exactly
/// `owner/repo`. A pull request URL is never a clone target: it is always a
/// browser URL of the shape `https://github.com/{owner}/{repo}/pull/{number}`,
/// so this function has its own small grammar rather than reusing
/// `RemoteUrl` — reusing it would mean stripping `pull/{number}` back off
/// again to make the two-segment shape `path_on` expects, which loses the
/// part this function exists to keep.
///
/// It still shares the same [`GITHUB_HOST`], and matches it the same
/// whole-host-equality way `path_on` does — never a suffix — for the
/// identical reason `path_on`'s own documentation gives: a hardcoded
/// `api.github.com` client must never be pointed at a same-named public
/// repository on a lookalike host.
///
/// # What it accepts
///
/// `https://github.com/{owner}/{repo}/pull/{number}`, `http://` as well, a
/// trailing slash, and surrounding whitespace. `owner`/`repo` come back
/// case-folded, matching `parse_github_url`'s convention that every consumer
/// of a GitHub identity is case-insensitive.
///
/// # What it refuses
///
/// Any other host; any scheme but `http`/`https`; a path that is not exactly
/// `{owner}/{repo}/pull/{number}` (an issue URL, a PR's `/files` or
/// `/commits` sub-page, a repository root); and a number that is not a
/// positive integer.
pub fn parse_pr_url(url: &str) -> Result<(String, String, u64), AppError> {
    let invalid = || {
        AppError::Validation(format!(
            "invalid GitHub pull request URL `{}`: expected \
             https://github.com/<owner>/<repo>/pull/<number>",
            url.trim()
        ))
    };

    let trimmed = url.trim();
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .ok_or_else(invalid)?;
    let (host, path) = rest.split_once('/').ok_or_else(invalid)?;
    if !host.eq_ignore_ascii_case(GITHUB_HOST) {
        return Err(invalid());
    }

    let path = path.trim_end_matches('/');
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

    Ok((owner.to_lowercase(), repo.to_lowercase(), number))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pr_url_reads_owner_repo_and_number() {
        let (owner, repo, number) = parse_pr_url("https://github.com/acme/widgets/pull/7").unwrap();
        assert_eq!(owner, "acme");
        assert_eq!(repo, "widgets");
        assert_eq!(number, 7);
    }

    #[test]
    fn parse_pr_url_tolerates_a_trailing_slash() {
        let (owner, repo, number) =
            parse_pr_url("https://github.com/acme/widgets/pull/7/").unwrap();
        assert_eq!(owner, "acme");
        assert_eq!(repo, "widgets");
        assert_eq!(number, 7);
    }

    #[test]
    fn parse_pr_url_case_folds_owner_and_repo() {
        let (owner, repo, _) = parse_pr_url("https://github.com/Acme/Widgets/pull/7").unwrap();
        assert_eq!(owner, "acme");
        assert_eq!(repo, "widgets");
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
    fn parse_pr_url_refuses_a_lookalike_host() {
        // The identical whole-host-equality property `path_on` enforces:
        // never a suffix match, so `evilgithub.com` and a GitHub Enterprise
        // install are both refused rather than silently accepted.
        assert!(parse_pr_url("https://evilgithub.com/acme/widgets/pull/7").is_err());
        assert!(parse_pr_url("https://github.example.com/acme/widgets/pull/7").is_err());
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
    fn parse_pr_url_refuses_a_repository_root() {
        assert!(parse_pr_url("https://github.com/acme/widgets").is_err());
    }
}
