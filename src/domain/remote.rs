//! Git remote URLs, reduced to the one key project identity is decided by.
//!
//! # Why this exists
//!
//! storyhook used to answer "which project is this?" by matching the working
//! directory against recorded checkout paths. A path rots: on the machine this
//! module was written, **seven of thirteen real projects** were reported as
//! having "no checkout on this machine" while every one of them had a checkout
//! on that machine right then. Two of the seven had merely been renamed on disk
//! — `openGrid-SCAD` for `opengrid-scad`, `Ourdio` for `ourdio`.
//!
//! A repository's origin URL does not rot that way, so it becomes the identity
//! instead. [`RemoteUrl`] is that identity, and this module is the whole of the
//! grammar behind it.
//!
//! # One function, and why the type exists to enforce it
//!
//! Registration and lookup must normalize *identically*, forever. Two functions
//! that drift is the entire defect this design exists to prevent — and the drift
//! was not hypothetical. [`crate::github::sync_state::parse_github_url`] parsed
//! remote URLs for a different purpose, and because it matched three literal
//! prefixes it refused `https://user@github.com/owner/repo.git` — a form two
//! real repositories on that same machine use — while this module accepted it.
//! One of the two was wrong, and it was not the one with the tests for it.
//! SH-137 fixed that parser and then deleted it: it calls
//! [`RemoteUrl::path_on`] now, so there is one grammar in the binary.
//!
//! So [`RemoteUrl`] has no public fields and no constructor other than
//! [`RemoteUrl::normalize`] and the lookup-shaped wrapper around it. A caller
//! cannot hold a key that some other rule produced, because there is no way to
//! make one.
//!
//! Nor is there a way to take one apart. [`RemoteUrl::path_on`] answers a
//! *question* — "what do you name on this host?" — where a `host()` and a
//! `path()` would hand out the pieces of [`RemoteUrl::key`]'s format and invite
//! the next caller to reassemble them into a third grammar. It takes the host as
//! an argument, which is also what keeps the promise below: this module does not
//! know what `github.com` is.
//!
//! # What the key collapses, and what it must never collapse
//!
//! All four spellings of one repository's origin produce one key:
//!
//! ```
//! use storyhook::domain::remote::RemoteUrl;
//! let forms = [
//!     "git@github.com:acme/widgets.git",
//!     "https://github.com/acme/widgets",
//!     "https://github.com/acme/widgets.git",
//!     "ssh://git@github.com/acme/widgets",
//! ];
//! let keys: Vec<String> = forms
//!     .iter()
//!     .map(|f| RemoteUrl::normalize(f).unwrap().key().to_string())
//!     .collect();
//! assert!(keys.iter().all(|k| k == "github.com/acme/widgets"));
//! ```
//!
//! It must **not** collapse `github.io` into `github.com` — Pages is a different
//! host serving different content. Nothing here ever inspects a host's *value*,
//! only its case, which is what makes that guarantee structural rather than a
//! rule someone remembered.
//!
//! # Case is folded everywhere, deliberately
//!
//! Host and path are both lowercased, for **every** host, with no allowlist of
//! known case-insensitive forges. An allowlist is correct where it applies and
//! silently wrong everywhere else: a GitHub Enterprise install at
//! `github.example.com` is case-insensitive and would not be on it, so one
//! repository would register twice as two projects and a command run in the
//! checkout would quietly operate on whichever the store answered with.
//!
//! Folding uniformly can only fail the other way — on a genuinely
//! case-sensitive host, two repositories differing only in case collide, and the
//! second registration is **refused**, naming a holder that is not the one the
//! user expected. That is loud, and one `unlink` undoes it. The epic this serves
//! requires the failure mode to be a refusal rather than a guess, and this is
//! the only answer consistent with it.
//!
//! # Local paths are a separate key space
//!
//! A git remote can be a filesystem path — a bare repository on a NAS is a real
//! use of git. Those are accepted, but they are **class-tagged** with a `local:`
//! prefix so they can never collide with a host-shaped key, and they are
//! **neither case-folded nor `.git`-stripped**: case-sensitivity on a
//! filesystem is a property of that filesystem rather than of DNS, and
//! `/srv/git/repo.git` and `/srv/git/repo` are two different directories, not
//! two spellings of one repository.
//!
//! A **relative** path is refused. Two unrelated repositories can each set
//! `origin` to `../sibling`, so a relative path used as an identity is a silent
//! collision — the exact failure this module exists to prevent.

use std::fmt;

use thiserror::Error;

use crate::error::AppError;

/// A git remote URL, reduced to the key that decides project identity.
///
/// Carries the raw string beside the normalized key, so what the user typed
/// survives a normalizer that will get better later. Construct with
/// [`RemoteUrl::normalize`], or with [`RemoteUrl::normalize_for_lookup`] on a
/// resolution path.
///
/// # Equality is the key alone
///
/// [`PartialEq`], [`Eq`] and [`Hash`] are implemented by hand over
/// [`key`](Self::key) and ignore [`raw`](Self::raw). A derived implementation
/// would compare both fields, so two values naming the same repository through
/// different spellings would compare **unequal** — which is precisely the bug
/// the key exists to prevent. Deriving here would have made the type's own
/// equality disagree with the uniqueness the store enforces.
#[derive(Clone, Debug)]
pub struct RemoteUrl {
    raw: String,
    key: String,
}

impl RemoteUrl {
    /// Reduces a git remote URL to its identity key, or refuses it.
    ///
    /// This is the **only** grammar. Registration calls it directly, because a
    /// user typing a bad URL needs to know which way it was bad. A resolution
    /// path must call [`normalize_for_lookup`](Self::normalize_for_lookup)
    /// instead: there, every reason for refusal means the same thing — this
    /// origin does not name a project — and telling them apart is the mistake.
    ///
    /// Never touches the filesystem or the network. In particular it never
    /// calls `canonicalize`, which can block or fail on a disconnected network
    /// mount, and which would put an I/O failure on a path that runs before
    /// every command.
    ///
    /// # Errors
    ///
    /// [`NormalizeError::Empty`] for nothing but whitespace,
    /// [`NormalizeError::RelativeLocalPath`] for a path whose meaning depends on
    /// the current directory, and [`NormalizeError::Unclassifiable`] for
    /// anything this grammar cannot read as a remote — including a URL that
    /// names a host but no repository.
    pub fn normalize(raw: &str) -> Result<Self, NormalizeError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(NormalizeError::Empty);
        }
        let key = classify(trimmed)?;
        Ok(Self {
            raw: raw.to_string(),
            key,
        })
    }

    /// [`normalize`](Self::normalize) for a resolution path, which has exactly
    /// one reaction to every possible refusal.
    ///
    /// **This is the entry point a project-selection walk must use.** An origin
    /// that does not normalize has to be indistinguishable from a directory with
    /// no origin at all: both mean "this does not name a project, try the next
    /// rule". Matching on [`NormalizeError`] in such a walk is the error this
    /// function exists to make unnecessary.
    ///
    /// The body is `normalize(raw).ok()` — deliberately a conversion and not a
    /// `match`. A `match` over today's variants would have to be revisited when
    /// a fourth is added, and the revisit is exactly what gets forgotten;
    /// `.ok()` absorbs a new variant without anyone remembering it exists.
    #[must_use]
    pub fn normalize_for_lookup(raw: &str) -> Option<Self> {
        Self::normalize(raw).ok()
    }

    /// The identity key: what the store indexes and matches on.
    ///
    /// `host[:port]/owner/repo` for a network remote, `local:<path>` for a
    /// filesystem one. The scheme is **absent by construction** — dropping it is
    /// what collapses the https, ssh and scp-like spellings of one repository
    /// into one value.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The URL exactly as the caller supplied it.
    ///
    /// Kept because the normalizer will improve, and because a user who typed
    /// `https://Me@GitHub.com/Acme/Widgets.git` should be shown that rather than
    /// the key it folded to.
    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// The repository path this remote names **on `host`**, or `None` if it
    /// names one somewhere else.
    ///
    /// # Why this is a question and not a decomposition
    ///
    /// This is the only way to ask a [`RemoteUrl`] what it points at, and it is
    /// deliberately shaped as a question. There is no `host()` and no `path()`:
    /// publishing those would turn [`key`](Self::key)'s `host/path` format into
    /// a contract a caller could reassemble, and a caller that reassembles a key
    /// is a caller with a second grammar — the one thing this module exists to
    /// prevent. It takes the host as an **argument**, so nothing here ever
    /// inspects a host's *value*, and a forge's own rules stay with the code
    /// that knows about that forge.
    ///
    /// # What it matches
    ///
    /// `host` is compared against the whole host segment, case-insensitively
    /// and **including any port**. `github.com` therefore does not match a
    /// remote on `github.com:8443`: [`key`](Self::key) already treats those as
    /// different endpoints, and this must not disagree with it.
    ///
    /// A filesystem remote always answers `None`, by an explicit prefix guard
    /// rather than by the accident that `local:` happens not to equal any real
    /// host a caller would pass.
    ///
    /// The path comes back as it is stored — case-folded, `.git`-stripped, and
    /// with repeated slashes collapsed. How many segments a repository may have
    /// is the caller's rule, not this module's: GitHub allows exactly two,
    /// GitLab's nested subgroups legitimately allow more.
    #[must_use]
    pub fn path_on(&self, host: &str) -> Option<&str> {
        if self.key.starts_with(LOCAL_PREFIX) {
            return None;
        }
        let (key_host, path) = self.key.split_once('/')?;
        key_host.eq_ignore_ascii_case(host).then_some(path)
    }
}

/// An origin a directory is entitled to **register**, as opposed to one it
/// merely reports.
///
/// The distinction cannot be made by a [`RemoteUrl`], because every directory
/// in a repository — and every worktree of it — reports the same one. That is
/// the whole of SH-151: `story project new` in `monorepo/service-a` used to
/// register the *monorepo's* identity against `service-a`, permanently, because
/// the value it held could not say where it had come from.
///
/// There is no public constructor from a bare [`RemoteUrl`] except
/// [`explicit`](Self::explicit), which is audited by name. Every other one comes
/// from [`RepoOrigin::Owned`], which only the ownership check can build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedOrigin(RemoteUrl);

impl OwnedOrigin {
    /// A URL the user typed, at a directory that is entitled to it.
    ///
    /// **The single escape hatch, and the only place a caller may assert
    /// ownership rather than compute it.** `story project link origin <url>`
    /// needs it: the URL is an argument rather than a fact read from the
    /// working directory, and a project whose checkout lives on another machine
    /// must still be able to register the origin it answers to from a directory
    /// that is no repository at all.
    ///
    /// The caller is responsible for having ruled out the one shape that is
    /// never entitled — a URL that *is* the enclosing repository's origin, given
    /// from a directory that does not own it. That is
    /// [`RepoOrigin::forbids`](RepoOrigin::forbids), and the reason it is a
    /// method there rather than a check here is that this constructor cannot see
    /// the directory.
    #[must_use]
    pub fn explicit(url: RemoteUrl) -> Self {
        Self(url)
    }

    /// The origin an ownership check computed an entitlement to.
    ///
    /// Crate-visible rather than public, so the only *published* way to assert
    /// ownership is [`explicit`](Self::explicit) — the one a reader can grep for
    /// and audit. Inside the crate this is what
    /// [`RepoOrigin::Owned`] is built from.
    #[must_use]
    pub(crate) fn computed(url: RemoteUrl) -> Self {
        Self(url)
    }

    /// The URL, for the store call that records it.
    #[must_use]
    pub fn url(&self) -> &RemoteUrl {
        &self.0
    }
}

/// What a directory's `origin` says about that directory's right to claim it.
///
/// # The rule, and why it is two conditions
///
/// A directory owns its origin when it is its repository's top level **and**
/// its `--git-dir` is its `--git-common-dir`. The second condition is "this is
/// the main working tree": a linked worktree holds a `.git` *file* and shares
/// the main checkout's config, so the remote it reports belongs to a directory
/// that is not it. Together they are the structural form of the rule this type
/// exists to enforce — *only the directory holding the `.git` that records the
/// remote may claim it* — and they are what keeps a **submodule** root an owner,
/// since a submodule has its own `.git` and its own origin.
///
/// # Why `Unknown` is not `Absent`
///
/// Every ordinary failure to find an origin — no git, no repository, no
/// `origin`, a URL that will not normalize — is [`Absent`](Self::Absent), which
/// preserves the contract a resolution path depends on: every failure is the
/// same failure. But a failure of the *ownership probe itself* is different.
/// Reading it as "not owned" would be conservative; reading it as owned would
/// silently restore the defect on a git too old to answer. It is neither, and
/// registration refuses on it by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepoOrigin {
    /// This directory owns the origin and may register it.
    Owned(OwnedOrigin),
    /// The origin belongs to `owner`; this directory only reports it because
    /// `git config` walks up.
    Inherited {
        /// The origin, for a refusal that names it.
        origin: RemoteUrl,
        /// The directory entitled to it — the repository's main working tree.
        owner: std::path::PathBuf,
    },
    /// The ownership probe could not be run or could not be read. Carries the
    /// git invocation that failed, because the user has to be able to act on it.
    Unknown(String),
    /// No origin here at all, for any of the reasons that mean the same thing.
    Absent,
}

impl RepoOrigin {
    /// The origin this directory may register, if any.
    #[must_use]
    pub fn owned(&self) -> Option<&OwnedOrigin> {
        match self {
            Self::Owned(origin) => Some(origin),
            _ => None,
        }
    }

    /// Whether registering `url` **here** would be taking an origin this
    /// directory is not entitled to.
    ///
    /// True only for the one shape a collision check cannot catch: the URL is
    /// the enclosing repository's own origin and this directory does not own it.
    /// Nobody may be holding that origin yet — which is exactly why the store's
    /// uniqueness index stays silent, and why the first sub-directory to ask
    /// would take it permanently and lock out every sibling.
    ///
    /// An unrelated URL is *not* forbidden: naming some other repository's
    /// origin from a directory that has nothing to do with it is what
    /// `story project link origin <url>` is for.
    #[must_use]
    pub fn forbids(&self, url: &RemoteUrl) -> Option<&std::path::Path> {
        match self {
            Self::Inherited { origin, owner } if origin == url => Some(owner),
            _ => None,
        }
    }
}

impl PartialEq for RemoteUrl {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for RemoteUrl {}

impl std::hash::Hash for RemoteUrl {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.key.hash(state);
    }
}

impl fmt::Display for RemoteUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.key)
    }
}

/// Why a string could not become a [`RemoteUrl`].
///
/// Registration-shaped: each variant exists because a user typing a URL at a
/// prompt needs to be told which mistake they made. A resolution path must not
/// distinguish them — see [`RemoteUrl::normalize_for_lookup`].
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum NormalizeError {
    /// Nothing but whitespace.
    #[error("a remote url cannot be empty")]
    Empty,

    /// A path whose meaning depends on the current directory.
    ///
    /// Refused rather than resolved: two unrelated repositories can each set
    /// `origin` to `../sibling`, and resolving it against the process's own
    /// working directory would make one project's identity depend on where a
    /// command happened to be run.
    #[error(
        "`{0}` is a relative path, so it names a different repository depending on where it is \
         read from; register an absolute path or a url"
    )]
    RelativeLocalPath(String),

    /// Not a form this grammar recognises as a git remote.
    #[error("`{0}` is not a git remote url this storyhook understands")]
    Unclassifiable(String),
}

impl From<NormalizeError> for AppError {
    fn from(error: NormalizeError) -> Self {
        Self::Validation(error.to_string())
    }
}

/// The schemes a network remote may carry. `file` is handled separately, as a
/// local path.
const NETWORK_SCHEMES: [&str; 4] = ["https", "http", "ssh", "git"];

/// The class tag a filesystem remote's key carries, and the guard
/// [`RemoteUrl::path_on`] reads it with.
///
/// One constant with two uses rather than two literals: the tag and the guard
/// that recognises it cannot drift apart if there is only one of them.
const LOCAL_PREFIX: &str = "local:/";

/// Reduces a trimmed, non-empty remote to its key, deciding which of the three
/// shapes it is.
fn classify(raw: &str) -> Result<String, NormalizeError> {
    if let Some((scheme, rest)) = raw.split_once("://") {
        let scheme = scheme.to_ascii_lowercase();
        if scheme == "file" {
            // `file:///srv/git/repo` has an empty authority, so the path starts
            // at the third slash. A `file://host/path` form names another
            // machine's filesystem and is not something identity can resolve.
            let path = rest.strip_prefix('/').map(|p| format!("/{p}"));
            return match path {
                Some(path) => local_key(&path, raw),
                None => Err(NormalizeError::Unclassifiable(raw.to_string())),
            };
        }
        if !NETWORK_SCHEMES.contains(&scheme.as_str()) {
            return Err(NormalizeError::Unclassifiable(raw.to_string()));
        }
        let (authority, path) = rest
            .split_once('/')
            .ok_or_else(|| NormalizeError::Unclassifiable(raw.to_string()))?;
        return network_key(authority, path, raw);
    }

    // scp-like: `[user@]host:path`, distinguished from a path by the colon
    // arriving before any slash. This is git's own rule.
    if let Some((before, after)) = raw.split_once(':')
        && !before.contains('/')
    {
        return network_key(before, after, raw);
    }

    if raw.starts_with('/') {
        return local_key(raw, raw);
    }

    // Anything else that looks like a path is relative; anything that does not
    // is not a remote at all.
    if raw.starts_with("./") || raw.starts_with("../") || raw.starts_with('~') || raw.contains('/')
    {
        return Err(NormalizeError::RelativeLocalPath(raw.to_string()));
    }

    Err(NormalizeError::Unclassifiable(raw.to_string()))
}

/// Builds the key for a remote that names a host.
///
/// `authority` may carry userinfo and a port; `path` is everything after the
/// host. Both are case-folded — see this module's header for why the fold has
/// no host allowlist.
fn network_key(authority: &str, path: &str, raw: &str) -> Result<String, NormalizeError> {
    // Everything through the last `@` is userinfo. Dropping it is what makes
    // `https://user@github.com/o/r` and `https://github.com/o/r` one identity —
    // and it is the case the sibling parser in `github::sync_state` gets wrong.
    let host = match authority.rsplit_once('@') {
        Some((_, host)) => host,
        None => authority,
    };
    if host.is_empty() {
        return Err(NormalizeError::Unclassifiable(raw.to_string()));
    }
    // The port stays part of the host segment rather than being parsed out.
    // Keeping it means `host` and `host:8443` are different endpoints, as they
    // are in fact — and *not* splitting on the last colon is what leaves an
    // IPv6 literal like `[::1]` intact instead of mangled.
    let host = host.to_ascii_lowercase();

    let path = normalize_path_segments(path);
    // Lowercased before the suffix is stripped, so `REPO.GIT` and `repo.git`
    // are the same repository.
    let path = path.to_ascii_lowercase();
    // Exactly one suffix. `repo.git.git` becomes `repo.git`, and a repository
    // genuinely named `repo.git` therefore stays distinct from `repo`.
    let path = path.strip_suffix(".git").unwrap_or(&path);

    if path.is_empty() {
        // A url naming a host but no repository is not an identity. Returning a
        // key with an empty path would make every such url for one host collide
        // into a single meaningless project.
        return Err(NormalizeError::Unclassifiable(raw.to_string()));
    }
    Ok(format!("{host}/{path}"))
}

/// Builds the key for a remote that names a filesystem path.
///
/// Neither case-folded nor `.git`-stripped, and tagged so it cannot collide
/// with a host-shaped key. See this module's header.
fn local_key(path: &str, raw: &str) -> Result<String, NormalizeError> {
    let mut resolved: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                // Lexical only: this never asks the filesystem what a symlink
                // points at, because the lookup path must not do I/O.
                if resolved.pop().is_none() {
                    return Err(NormalizeError::Unclassifiable(raw.to_string()));
                }
            }
            other => resolved.push(other),
        }
    }
    if resolved.is_empty() {
        return Err(NormalizeError::Unclassifiable(raw.to_string()));
    }
    Ok(format!("{LOCAL_PREFIX}{}", resolved.join("/")))
}

/// Collapses repeated slashes and strips leading and trailing ones.
fn normalize_path_segments(path: &str) -> String {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(raw: &str) -> String {
        RemoteUrl::normalize(raw)
            .unwrap_or_else(|error| panic!("`{raw}` should normalize, got {error}"))
            .key()
            .to_string()
    }

    // -----------------------------------------------------------------------
    // Acceptance criterion 1 — all four forms are one project
    // -----------------------------------------------------------------------

    #[test]
    fn all_four_url_forms_normalize_to_the_same_key() {
        let forms = [
            "git@github.com:acme/widgets.git",
            "https://github.com/acme/widgets",
            "https://github.com/acme/widgets.git",
            "ssh://git@github.com/acme/widgets",
        ];
        for form in forms {
            assert_eq!(key(form), "github.com/acme/widgets", "{form}");
        }
    }

    #[test]
    fn a_trailing_slash_does_not_change_the_key() {
        assert_eq!(
            key("https://github.com/acme/widgets/"),
            key("https://github.com/acme/widgets")
        );
        assert_eq!(
            key("https://github.com/acme/widgets.git/"),
            "github.com/acme/widgets"
        );
    }

    #[test]
    fn the_scheme_is_not_part_of_identity() {
        assert_eq!(
            key("http://github.com/acme/widgets"),
            key("https://github.com/acme/widgets")
        );
        assert_eq!(
            key("git://github.com/acme/widgets"),
            key("https://github.com/acme/widgets")
        );
    }

    #[test]
    fn userinfo_is_dropped_from_the_key() {
        // The exact form two real repositories on the author's machine use, and
        // the one `github::sync_state::parse_github_url` refused until SH-137.
        assert_eq!(
            key("https://wookiee@github.com/mikeyward/keymux.git"),
            key("https://github.com/mikeyward/keymux")
        );
    }

    // -----------------------------------------------------------------------
    // Acceptance criterion 2 — github.io is not github.com
    // -----------------------------------------------------------------------

    #[test]
    fn github_io_and_github_com_are_different_hosts() {
        assert_ne!(
            key("https://acme.github.io/widgets"),
            key("https://github.com/acme/widgets")
        );
    }

    #[test]
    fn no_host_is_ever_rewritten() {
        assert_eq!(
            key("https://acme.github.io/widgets"),
            "acme.github.io/widgets"
        );
    }

    // -----------------------------------------------------------------------
    // Case folding — the unanimous Q1 answer, with no host allowlist
    // -----------------------------------------------------------------------

    #[test]
    fn host_and_path_case_are_both_folded() {
        assert_eq!(
            key("https://GitHub.com/Acme/Widgets.GIT"),
            "github.com/acme/widgets"
        );
    }

    #[test]
    fn case_folds_on_a_host_no_allowlist_would_have_contained() {
        // The case an allowlist design gets silently wrong: a self-hosted forge
        // that is case-insensitive and is not github.com. If this fails, the
        // fold has been narrowed to a list of known hosts.
        assert_eq!(
            key("https://GitLab.Internal.Example/Team/Repo.git"),
            key("https://gitlab.internal.example/team/repo")
        );
    }

    // -----------------------------------------------------------------------
    // The adversarial edges
    // -----------------------------------------------------------------------

    #[test]
    fn distinct_ports_on_one_host_do_not_collide() {
        assert_ne!(
            key("https://git.acme.com/o/r"),
            key("https://git.acme.com:8443/o/r")
        );
    }

    #[test]
    fn the_git_suffix_is_stripped_exactly_once() {
        // A repository genuinely named `repo.git` must not merge with `repo`.
        assert_eq!(
            key("https://host.example/o/repo.git.git"),
            "host.example/o/repo.git"
        );
        assert_ne!(
            key("https://host.example/o/repo.git.git"),
            key("https://host.example/o/repo")
        );
    }

    #[test]
    fn repeated_slashes_collapse() {
        assert_eq!(key("https://host.example//o//r"), "host.example/o/r");
    }

    #[test]
    fn a_url_naming_a_host_but_no_repository_is_refused() {
        // Not a panic, and not a key with an empty path — which would make every
        // host-only url for one host collide into one meaningless identity.
        for raw in [
            "https://github.com",
            "https://github.com/",
            "https://github.com///",
        ] {
            assert!(
                matches!(
                    RemoteUrl::normalize(raw),
                    Err(NormalizeError::Unclassifiable(_))
                ),
                "{raw}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Local paths
    // -----------------------------------------------------------------------

    #[test]
    fn an_absolute_local_path_is_its_own_identity() {
        assert_eq!(key("/srv/git/repo.git"), "local:/srv/git/repo.git");
    }

    #[test]
    fn a_local_path_is_never_case_folded() {
        // Case-sensitivity on a filesystem is a property of that filesystem, not
        // of DNS. Folding here would be the guess Q1 refuses to make.
        assert_ne!(key("/srv/git/Repo"), key("/srv/git/repo"));
    }

    #[test]
    fn a_local_path_keeps_its_git_suffix() {
        // `/srv/git/repo.git` and `/srv/git/repo` are two different directories,
        // not two spellings of one repository.
        assert_ne!(key("/srv/git/repo.git"), key("/srv/git/repo"));
    }

    #[test]
    fn a_local_path_cannot_collide_with_a_host_shaped_key() {
        assert_ne!(key("/local/srv"), key("https://local/srv"));
    }

    #[test]
    fn a_file_url_is_a_local_path() {
        assert_eq!(key("file:///srv/git/repo.git"), key("/srv/git/repo.git"));
    }

    #[test]
    fn a_local_path_is_lexically_normalized_without_touching_the_filesystem() {
        assert_eq!(key("/srv/git/../git/repo"), "local:/srv/git/repo");
        assert_eq!(key("/srv//git/./repo/"), "local:/srv/git/repo");
    }

    #[test]
    fn a_relative_path_is_refused_by_normalize_itself() {
        // Not by a caller, and not at registration: two unrelated repositories
        // can each set origin to `../sibling`, so a key must not exist for it at
        // all. Refusing here is what makes that structural.
        for raw in ["../sibling", "./repo", "~/repos/thing", "owner/repo"] {
            assert!(
                matches!(
                    RemoteUrl::normalize(raw),
                    Err(NormalizeError::RelativeLocalPath(_))
                ),
                "{raw}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Refusals
    // -----------------------------------------------------------------------

    #[test]
    fn an_empty_or_whitespace_remote_is_refused() {
        assert_eq!(RemoteUrl::normalize(""), Err(NormalizeError::Empty));
        assert_eq!(RemoteUrl::normalize("   \t\n "), Err(NormalizeError::Empty));
    }

    #[test]
    fn an_unclassifiable_string_is_refused_rather_than_guessed() {
        for raw in ["not a remote", "ftp://host.example/o/r", "https:///o/r"] {
            assert!(
                matches!(
                    RemoteUrl::normalize(raw),
                    Err(NormalizeError::Unclassifiable(_))
                ),
                "{raw}"
            );
        }
    }

    #[test]
    fn normalize_never_panics_on_hostile_input() {
        // This function runs before every command once selection lands, against
        // whatever `git remote get-url origin` returned. It may refuse anything;
        // it may not panic on anything.
        for raw in [
            "",
            ":",
            "://",
            "@",
            "a@:",
            "https://",
            "https://@",
            "/",
            "//",
            "/..",
            "/../..",
            "...",
            "::::",
            "git@",
            "git@:",
            "\u{0}",
            "https://[::1]",
            "C:\\repos\\thing",
        ] {
            let _ = RemoteUrl::normalize(raw);
        }
    }

    // -----------------------------------------------------------------------
    // Explicit non-decisions
    //
    // These assert *only* that nothing panics. They must never be tightened to
    // assert a particular key: doing so would freeze whatever the first
    // implementation happened to produce as though somebody had decided it. Both
    // shapes have their own filed story.
    // -----------------------------------------------------------------------

    #[test]
    fn an_ipv6_literal_host_does_not_panic_and_is_otherwise_undecided() {
        let _ = RemoteUrl::normalize("https://[::1]/acme/widgets");
        let _ = RemoteUrl::normalize("ssh://git@[2001:db8::1]:22/acme/widgets");
    }

    #[test]
    fn a_percent_encoded_path_does_not_panic_and_is_otherwise_undecided() {
        let _ = RemoteUrl::normalize("https://host.example/acme%2Fwidgets");
        let _ = RemoteUrl::normalize("https://host.example/acme/wid%67ets");
    }

    // -----------------------------------------------------------------------
    // The type itself
    // -----------------------------------------------------------------------

    #[test]
    fn the_raw_form_survives_normalization_byte_for_byte() {
        let raw = "https://WOOKIEE@GitHub.com/mikeyward/KeyMux.GIT/";
        let remote = RemoteUrl::normalize(raw).unwrap();
        assert_eq!(remote.raw(), raw);
        assert_ne!(remote.raw(), remote.key());
        assert_eq!(remote.key(), "github.com/mikeyward/keymux");
    }

    #[test]
    fn equality_and_hashing_ignore_the_raw_form() {
        // A derived PartialEq would compare `raw` too, so two values naming one
        // repository would compare unequal — the bug the key exists to prevent,
        // reintroduced inside the type meant to prevent it.
        use std::collections::HashSet;

        let a = RemoteUrl::normalize("https://github.com/acme/widgets.git").unwrap();
        let b = RemoteUrl::normalize("git@github.com:Acme/Widgets").unwrap();
        assert_eq!(a, b);
        assert_ne!(a.raw(), b.raw());

        let mut set = HashSet::new();
        set.insert(a);
        assert!(set.contains(&b), "hashing must agree with equality");
        assert_eq!(set.len(), 1);
    }

    // -----------------------------------------------------------------------
    // `path_on` — the one question a caller may ask of a key
    //
    // Shaped as a question rather than a `host()`/`path()` pair on purpose, so
    // no caller can reassemble the key format into a grammar of its own. These
    // pin the properties that shape depends on.
    // -----------------------------------------------------------------------

    #[test]
    fn path_on_answers_for_its_own_host_and_no_other() {
        let remote = RemoteUrl::normalize("git@github.com:acme/widgets.git").unwrap();
        assert_eq!(remote.path_on("github.com"), Some("acme/widgets"));
        assert_eq!(remote.path_on("gitlab.com"), None);
    }

    #[test]
    fn path_on_matches_the_whole_host_never_a_suffix() {
        // The failure this prevents is not cosmetic: a caller with a hardcoded
        // api.github.com would otherwise accept `github.example.com` and query
        // a same-named *public* repository — and `evilgithub.com` besides.
        for raw in [
            "https://github.example.com/acme/widgets",
            "https://evilgithub.com/acme/widgets",
            "https://github.com.example/acme/widgets",
        ] {
            assert_eq!(
                RemoteUrl::normalize(raw).unwrap().path_on("github.com"),
                None,
                "{raw}"
            );
        }
    }

    #[test]
    fn path_on_is_case_insensitive_in_its_argument() {
        // The key's host is already folded; the argument is whatever a caller
        // wrote in its own source, and the two must agree regardless.
        let remote = RemoteUrl::normalize("https://GitHub.com/Acme/Widgets").unwrap();
        assert_eq!(remote.path_on("GitHub.COM"), Some("acme/widgets"));
    }

    #[test]
    fn path_on_treats_a_port_as_part_of_the_host() {
        // `key` already holds these to be different endpoints. If this answered
        // for the bare host it would disagree with the key it reads from.
        let remote = RemoteUrl::normalize("https://git.acme.com:8443/o/r").unwrap();
        assert_eq!(remote.path_on("git.acme.com"), None);
        assert_eq!(remote.path_on("git.acme.com:8443"), Some("o/r"));
    }

    #[test]
    fn path_on_never_answers_for_a_filesystem_remote() {
        // Including when a caller asks for the class tag by name: `local:` is
        // not a host, and the guard is explicit so this cannot come to depend
        // on no real caller happening to pass that string.
        for raw in ["/srv/git/repo.git", "file:///srv/git/repo"] {
            let remote = RemoteUrl::normalize(raw).unwrap();
            assert_eq!(remote.path_on("local:"), None, "{raw}");
            assert_eq!(remote.path_on("local"), None, "{raw}");
        }
    }

    #[test]
    fn path_on_returns_the_path_as_the_key_stores_it() {
        // Folded, `.git`-stripped, slashes collapsed — the caller gets exactly
        // what identity decided, not a re-derivation of it.
        let remote = RemoteUrl::normalize("https://GitHub.com//Acme//Widgets.GIT/").unwrap();
        assert_eq!(remote.path_on("github.com"), Some("acme/widgets"));
    }

    #[test]
    fn path_on_does_not_decide_how_many_segments_a_repository_has() {
        // That rule belongs to the forge-specific caller. GitHub allows exactly
        // two; GitLab's nested subgroups allow more. This hands back the path
        // and lets the caller refuse.
        let remote = RemoteUrl::normalize("https://github.com/acme/widgets/tree/main").unwrap();
        assert_eq!(remote.path_on("github.com"), Some("acme/widgets/tree/main"));
    }

    #[test]
    fn normalize_for_lookup_answers_none_for_every_refusal() {
        // Pins the `.ok()` contract itself rather than a call site, so a fourth
        // NormalizeError variant added later inherits it for free.
        for raw in [
            "",
            "   ",
            "../sibling",
            "not a remote",
            "https://github.com",
        ] {
            assert!(RemoteUrl::normalize_for_lookup(raw).is_none(), "{raw}");
        }
    }

    #[test]
    fn normalize_for_lookup_agrees_with_normalize_on_every_success() {
        for raw in [
            "https://github.com/acme/widgets.git",
            "git@github.com:acme/widgets",
            "/srv/git/repo.git",
        ] {
            assert_eq!(
                RemoteUrl::normalize_for_lookup(raw).map(|r| r.key().to_string()),
                RemoteUrl::normalize(raw).ok().map(|r| r.key().to_string()),
                "{raw}"
            );
        }
    }

    /// The origin the ownership fixtures below are about.
    const URL: &str = "git@github.com:acme/widgets.git";

    fn url(raw: &str) -> RemoteUrl {
        RemoteUrl::normalize(raw).expect("a fixture URL must normalize")
    }

    fn inherited(origin: &str, owner: &str) -> RepoOrigin {
        RepoOrigin::Inherited {
            origin: url(origin),
            owner: std::path::PathBuf::from(owner),
        }
    }

    #[test]
    fn only_an_owned_claim_yields_something_registrable() {
        assert!(
            RepoOrigin::Owned(OwnedOrigin::computed(url(URL)))
                .owned()
                .is_some()
        );
        for claim in [
            inherited(URL, "/code/mono"),
            RepoOrigin::Unknown("git rev-parse".to_string()),
            RepoOrigin::Absent,
        ] {
            assert!(
                claim.owned().is_none(),
                "only an owned claim may be registered: {claim:?}"
            );
        }
    }

    #[test]
    fn an_inherited_claim_forbids_its_own_url_and_nothing_else() {
        let claim = inherited(URL, "/code/mono");
        assert_eq!(
            claim.forbids(&url(URL)),
            Some(std::path::Path::new("/code/mono")),
            "taking the enclosing repository's own origin is the forbidden shape"
        );
        assert!(
            claim
                .forbids(&url("git@github.com:acme/unrelated.git"))
                .is_none(),
            "naming some other repository's origin is what `link origin <url>` is for"
        );
    }

    #[test]
    fn a_forbidden_url_is_matched_by_key_not_by_spelling() {
        // The whole point of the key: `https://github.com/acme/widgets` and
        // `git@github.com:acme/widgets.git` are one identity, so a subdirectory
        // cannot get around the rule by spelling the URL the other way.
        assert!(
            inherited(URL, "/code/mono")
                .forbids(&url("https://github.com/acme/widgets"))
                .is_some()
        );
    }

    #[test]
    fn no_other_claim_forbids_anything() {
        for claim in [
            RepoOrigin::Owned(OwnedOrigin::computed(url(URL))),
            RepoOrigin::Unknown("git rev-parse".to_string()),
            RepoOrigin::Absent,
        ] {
            assert!(claim.forbids(&url(URL)).is_none(), "{claim:?}");
        }
    }
}
