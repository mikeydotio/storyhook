//! Parses a registered git remote as a GitHub repository — the one piece of
//! GitHub knowledge `story link-pr`/`story pr-check` need that must work with
//! the `github-pr` feature off (SH-408).
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

/// The one host a GitHub remote can name.
///
/// A private duplicate rather than a shared constant with
/// [`crate::domain::pr_url`]'s own copy — that module documents the same
/// choice: threading one `&str` across two files that already agree on its
/// value is not worth doing, and never touching anything gated is the whole
/// point of both copies existing.
const GITHUB_HOST: &str = "github.com";

/// A GitHub repository, identified by owner and name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GithubRepo {
    pub owner: String,
    pub repo: String,
}

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
    let (owner, repo) = remote.path_on(GITHUB_HOST)?.split_once('/')?;
    if repo.contains('/') {
        return None;
    }
    Some(GithubRepo {
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
    fn parse_github_enterprise_host_is_refused() {
        // A hardcoded API client base of `https://api.github.com` means
        // accepting a GHE host would build a client that queries a
        // same-named *public* repository and push an internal project's
        // pull requests into a stranger's tracker. Whole-host equality,
        // never a suffix match — which is also what keeps `evilgithub.com`
        // out. Supporting GHE means a host-derived API base, and that is
        // its own story.
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

    #[test]
    fn a_github_io_host_is_refused() {
        // Pages is a different host serving different content — see
        // `RemoteUrl`'s own doc on why the two must never collapse.
        assert!(parse_github_url("https://github.io/acme/widgets").is_none());
    }
}
