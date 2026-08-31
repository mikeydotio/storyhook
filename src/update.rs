//! Self-update: download and install the latest `story` release in place.
//!
//! Mirrors the download/extract flow of `install.sh` (same GitHub release assets
//! and URLs), but replaces the *currently running* binary rather than a fixed
//! install directory. Unconditional (SH-408): it used `ureq`, which rode the
//! sync engine's cargo feature purely by accident of history, and `ureq` has
//! been an unconditional dependency of this crate since the daemon transport
//! landed — this module uses nothing else that feature gates.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use ureq::Agent;

use crate::error::AppError;

/// GitHub `owner/repo` releases are published under (matches `install.sh`).
const REPO: &str = "mikeydotio/storyhook";
/// GitHub REST endpoint for the most recent published release.
const API_LATEST: &str = "https://api.github.com/repos/mikeydotio/storyhook/releases/latest";

/// Minimal shape of the GitHub "latest release" response.
#[derive(Deserialize)]
struct LatestRelease {
    tag_name: String,
}

/// Outcome of comparing the installed version against the latest release.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    /// A newer release is available.
    Update,
    /// Installed version equals the latest release.
    UpToDate,
    /// Installed version is newer than the latest release (local dev build).
    DevAhead,
    /// One side could not be parsed, so ordering is undefined.
    Incomparable,
}

/// What a completed `story update` invocation did to the installed binary.
///
/// The distinction is part of the result rather than inferred from its prose:
/// callers may report post-replacement diagnostics only for [`Self::Replaced`].
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The latest release replaced the running executable, including a forced
    /// reinstall or downgrade.
    Replaced(String),
    /// The command only reported status; no executable was replaced.
    Unchanged(String),
}

/// Entry point for `story update`.
///
/// * `check` — report availability only; never download or replace anything.
/// * `force` — download and (re)install the latest release even when the
///   installed version is already current or newer (reinstall / downgrade).
pub fn run(check: bool, force: bool) -> Result<Outcome, AppError> {
    let current = env!("CARGO_PKG_VERSION");
    let agent = build_agent();

    let tag = fetch_latest_tag(&agent)?;
    let latest = tag.trim_start_matches('v').to_string();
    let decision = decide(parse_semver(current), parse_semver(&latest));

    // `--check` never mutates and never errors on an odd tag — it just reports.
    if check {
        return Ok(Outcome::Unchanged(check_message(
            &decision, current, &latest,
        )));
    }

    let proceed = match decision {
        Decision::Update => true,
        Decision::UpToDate | Decision::DevAhead => force,
        Decision::Incomparable => {
            if !force {
                return Err(AppError::Storage(format!(
                    "cannot compare installed version {current} with release tag {tag:?}; \
                     re-run `story update --force` to reinstall the latest release"
                )));
            }
            true
        }
    };

    if !proceed {
        return Ok(Outcome::Unchanged(up_to_date_message(
            &decision, current, &latest,
        )));
    }

    install_release(&agent, &tag, &latest, current).map(Outcome::Replaced)
}

/// Build an HTTP agent for update downloads.
///
/// Unlike the API client (`github/client.rs`), this omits a global timeout: a
/// release tarball is several MB and a 30s cap would truncate the download on a
/// slow link. Status codes are inspected manually rather than raised as errors.
fn build_agent() -> Agent {
    let config = Agent::config_builder()
        .https_only(true)
        .http_status_as_error(false)
        .build();
    config.into()
}

/// Query GitHub for the latest release tag (e.g. `"v0.14.0"`).
///
/// Unauthenticated, on purpose: this endpoint is public release metadata, not
/// anything a credential should be spent on. It used to attach
/// `STORYHOOK_GITHUB_TOKEN` to raise the anonymous rate limit, but that ran
/// inside the daemon and spent whichever client's token the daemon happened to
/// have inherited — a credential leak this update check has no business
/// causing (SH-153). Deleted outright rather than re-plumbed through the
/// envelope: `story update` has no need of a GitHub identity at all.
fn fetch_latest_tag(agent: &Agent) -> Result<String, AppError> {
    let req = agent
        .get(API_LATEST)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "storyhook")
        .header("X-GitHub-Api-Version", "2022-11-28");

    let mut resp = req
        .call()
        .map_err(|e| AppError::GithubApi(format!("failed to query latest release: {e}")))?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(AppError::GithubApi(format!(
            "failed to query latest release: HTTP {status}"
        )));
    }

    let release: LatestRelease = resp
        .body_mut()
        .read_json()
        .map_err(|e| AppError::GithubApi(format!("failed to parse latest release: {e}")))?;
    let tag = release.tag_name.trim().to_string();
    if tag.is_empty() {
        return Err(AppError::GithubApi(
            "latest release has an empty tag_name".to_string(),
        ));
    }
    Ok(tag)
}

/// Download and swap in the release identified by `tag`.
fn install_release(
    agent: &Agent,
    tag: &str,
    latest: &str,
    current: &str,
) -> Result<String, AppError> {
    let target = target_for(std::env::consts::ARCH, std::env::consts::OS).ok_or_else(|| {
        AppError::Storage(format!(
            "unsupported platform {}-{}: no prebuilt story release. \
             Install manually from https://github.com/{REPO}/releases",
            std::env::consts::ARCH,
            std::env::consts::OS
        ))
    })?;
    let artifact = artifact_name(target);
    let url = download_url(tag, &artifact);

    // Resolve the real binary path (follow symlinks) so we replace the actual
    // file, not a `~/.local/bin/story` symlink pointing at it.
    let exe = std::env::current_exe()
        .map_err(|e| AppError::Storage(format!("cannot locate current executable: {e}")))?;
    let exe = fs::canonicalize(&exe).unwrap_or(exe);
    let dir = exe
        .parent()
        .ok_or_else(|| AppError::Storage("executable has no parent directory".to_string()))?;

    // Stage everything inside the exe's own directory so the final rename is a
    // same-filesystem atomic swap (never EXDEV). Creating this directory also
    // probes for write permission *before* we spend time downloading.
    let work = TempDir::create_in(dir)?;

    let tarball = work.path().join(&artifact);
    download_to(agent, &url, &tarball)?;

    let staged = extract_story(&tarball, work.path())?;
    make_executable(&staged)?;
    smoke_test(&staged)?;

    replace_exe(&staged, &exe)?;

    Ok(format!("story updated to v{latest} (was v{current})"))
}

/// Map `(arch, os)` — as produced by `std::env::consts` — to a release target
/// triple. Returns `None` for platforms with no prebuilt release.
fn target_for(arch: &str, os: &str) -> Option<&'static str> {
    match (arch, os) {
        ("x86_64", "linux") => Some("x86_64-unknown-linux-gnu"),
        ("aarch64", "linux") => Some("aarch64-unknown-linux-gnu"),
        ("x86_64", "macos") => Some("x86_64-apple-darwin"),
        ("aarch64", "macos") => Some("aarch64-apple-darwin"),
        _ => None,
    }
}

/// Release asset filename for a given target triple.
fn artifact_name(target: &str) -> String {
    format!("story-{target}.tar.gz")
}

/// Full download URL for a release asset (same pattern as `install.sh`).
fn download_url(tag: &str, artifact: &str) -> String {
    format!("https://github.com/{REPO}/releases/download/{tag}/{artifact}")
}

/// Parse a version string like `"v1.2.3"`, `"1.2.3"`, or `"1.2.3-rc1+build"`
/// into `(major, minor, patch)`. A leading `v` is stripped and any
/// pre-release/build suffix is ignored. Returns `None` unless exactly three
/// numeric core components are present.
fn parse_semver(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.trim().trim_start_matches('v');
    // Drop `-prerelease` and `+build` metadata before splitting on `.`.
    let core = core.split(['-', '+']).next().unwrap_or(core);
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None; // more than three components → not a plain semver
    }
    Some((major, minor, patch))
}

/// Compare installed vs. latest to decide what `run` should do.
fn decide(current: Option<(u64, u64, u64)>, latest: Option<(u64, u64, u64)>) -> Decision {
    match (current, latest) {
        (Some(c), Some(l)) if l > c => Decision::Update,
        (Some(c), Some(l)) if l == c => Decision::UpToDate,
        (Some(_), Some(_)) => Decision::DevAhead,
        _ => Decision::Incomparable,
    }
}

/// Message for `story update --check` (report only).
fn check_message(decision: &Decision, current: &str, latest: &str) -> String {
    match decision {
        Decision::Update => {
            format!(
                "update available: story v{current} -> v{latest}. Run `story update` to install."
            )
        }
        Decision::UpToDate => format!("story is up to date (v{current} is the latest release)."),
        Decision::DevAhead => format!(
            "story v{current} is newer than the latest release (v{latest}); nothing to update."
        ),
        Decision::Incomparable => format!(
            "installed story v{current}; latest release is v{latest} (could not compare versions). \
             Run `story update --force` to reinstall the latest release."
        ),
    }
}

/// Message when an update was requested but nothing needs to change.
fn up_to_date_message(decision: &Decision, current: &str, latest: &str) -> String {
    match decision {
        Decision::DevAhead => format!(
            "story v{current} is newer than the latest release (v{latest}); nothing to update. \
             Use --force to reinstall the release version."
        ),
        _ => format!("story is already up to date (v{current}). Use --force to reinstall."),
    }
}

/// Stream a URL to a file using the uncapped body reader.
///
/// `read_to_vec`/`read_to_string` cap at 10 MB and would truncate a multi-MB
/// binary; `as_reader` is unlimited. ureq follows GitHub's 302 to
/// `objects.githubusercontent.com` automatically and strips auth headers across
/// the host boundary by default.
fn download_to(agent: &Agent, url: &str, dest: &Path) -> Result<(), AppError> {
    let mut resp = agent
        .get(url)
        .header("User-Agent", "storyhook")
        .call()
        .map_err(|e| AppError::GithubApi(format!("download failed: {e}")))?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(AppError::GithubApi(format!(
            "download failed: HTTP {status} for {url}"
        )));
    }

    let mut file = fs::File::create(dest)
        .map_err(|e| AppError::Storage(format!("failed to create {}: {e}", dest.display())))?;
    let mut reader = resp.body_mut().as_reader();
    std::io::copy(&mut reader, &mut file)
        .map_err(|e| AppError::GithubApi(format!("failed to download release asset: {e}")))?;
    Ok(())
}

/// Extract the `story` binary from `tarball` into `into` using the system `tar`
/// (as `install.sh` does; present on all supported platforms).
fn extract_story(tarball: &Path, into: &Path) -> Result<PathBuf, AppError> {
    let status = Command::new("tar")
        .arg("xzf")
        .arg(tarball)
        .arg("-C")
        .arg(into)
        .status()
        .map_err(|e| AppError::Storage(format!("failed to run tar: {e}")))?;
    if !status.success() {
        return Err(AppError::Storage(
            "failed to extract release archive (tar exited non-zero)".to_string(),
        ));
    }
    let story = into.join("story");
    if !story.exists() {
        return Err(AppError::Storage(
            "release archive did not contain a `story` binary".to_string(),
        ));
    }
    Ok(story)
}

/// Ensure the staged binary is executable (mode 0o755) before running/swapping.
#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), AppError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms)
        .map_err(|e| AppError::Storage(format!("failed to set permissions: {e}")))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), AppError> {
    Ok(())
}

/// Run the freshly downloaded binary once to confirm it executes before we swap
/// it in — guards against a corrupt or truncated download being installed.
///
/// Uses `--help` rather than `--version`: `--help` is understood by every story
/// release, whereas `--version` is new, so probing with it would falsely reject
/// a download of any release that predates the flag.
fn smoke_test(staged: &Path) -> Result<(), AppError> {
    let ok = Command::new(staged)
        .arg("--help")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return Err(AppError::GithubApi(
            "downloaded story binary failed to execute; update aborted (nothing was changed)"
                .to_string(),
        ));
    }
    Ok(())
}

/// Atomically replace the running executable with the staged binary.
///
/// Both paths live in the same directory, so the rename is same-filesystem and
/// atomic; replacing a running executable is safe on Unix (the kernel keeps the
/// old inode mapped for the live process).
fn replace_exe(staged: &Path, exe: &Path) -> Result<(), AppError> {
    fs::rename(staged, exe).map_err(|e| {
        if e.kind() == ErrorKind::PermissionDenied {
            not_writable(exe.parent().unwrap_or(exe))
        } else {
            AppError::Storage(format!("failed to replace {}: {e}", exe.display()))
        }
    })
}

/// Actionable error for a non-writable install directory.
fn not_writable(dir: &Path) -> AppError {
    AppError::Storage(format!(
        "cannot write to {}: permission denied. \
         Re-run with elevated privileges (e.g. `sudo story update`) or reinstall via the installer: \
         curl -fsSL https://raw.githubusercontent.com/{REPO}/main/install.sh | sh",
        dir.display()
    ))
}

/// A temporary directory removed on drop. Zero-dependency stand-in for the
/// dev-only `tempfile` crate.
struct TempDir(PathBuf);

impl TempDir {
    /// Create a uniquely named working directory inside `parent`.
    ///
    /// A `PermissionDenied` error here means the executable's directory is not
    /// writable (e.g. a root-owned `/usr/local/bin`) — surfaced with an
    /// actionable message before any download happens.
    fn create_in(parent: &Path) -> Result<Self, AppError> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = parent.join(format!(".story-update-{}-{}", std::process::id(), nanos));
        match fs::create_dir(&path) {
            Ok(()) => Ok(Self(path)),
            Err(e) if e.kind() == ErrorKind::PermissionDenied => Err(not_writable(parent)),
            Err(e) => Err(AppError::Storage(format!(
                "failed to create work directory in {}: {e}",
                parent.display()
            ))),
        }
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_distinguishes_a_replaced_binary_from_an_unchanged_one() {
        let replaced = Outcome::Replaced("updated".to_string());
        let unchanged = Outcome::Unchanged("already current".to_string());

        assert!(matches!(replaced, Outcome::Replaced(_)));
        assert!(matches!(unchanged, Outcome::Unchanged(_)));
    }

    #[test]
    fn parse_semver_variants() {
        assert_eq!(parse_semver("v0.14.0"), Some((0, 14, 0)));
        assert_eq!(parse_semver("0.14.0"), Some((0, 14, 0)));
        assert_eq!(parse_semver("v1.2.3-rc1"), Some((1, 2, 3)));
        assert_eq!(parse_semver("1.2.3+build.5"), Some((1, 2, 3)));
        assert_eq!(parse_semver("  v2.0.1  "), Some((2, 0, 1)));
        assert_eq!(parse_semver("1.2"), None);
        assert_eq!(parse_semver("1.2.3.4"), None);
        assert_eq!(parse_semver("vabc"), None);
        assert_eq!(parse_semver(""), None);
    }

    #[test]
    fn decide_covers_all_orderings() {
        assert_eq!(decide(Some((0, 13, 0)), Some((0, 14, 0))), Decision::Update);
        assert_eq!(
            decide(Some((0, 14, 0)), Some((0, 14, 0))),
            Decision::UpToDate
        );
        assert_eq!(
            decide(Some((0, 15, 0)), Some((0, 14, 0))),
            Decision::DevAhead
        );
        assert_eq!(decide(None, Some((0, 14, 0))), Decision::Incomparable);
        assert_eq!(decide(Some((0, 14, 0)), None), Decision::Incomparable);
    }

    #[test]
    fn target_for_supported_platforms() {
        assert_eq!(
            target_for("x86_64", "linux"),
            Some("x86_64-unknown-linux-gnu")
        );
        assert_eq!(
            target_for("aarch64", "linux"),
            Some("aarch64-unknown-linux-gnu")
        );
        assert_eq!(target_for("x86_64", "macos"), Some("x86_64-apple-darwin"));
        assert_eq!(target_for("aarch64", "macos"), Some("aarch64-apple-darwin"));
        assert_eq!(target_for("x86_64", "windows"), None);
        assert_eq!(target_for("riscv64", "linux"), None);
    }

    #[test]
    fn artifact_and_url_formatting() {
        assert_eq!(
            artifact_name("aarch64-apple-darwin"),
            "story-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            download_url("v0.14.0", "story-aarch64-apple-darwin.tar.gz"),
            "https://github.com/mikeydotio/storyhook/releases/download/v0.14.0/story-aarch64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn our_own_version_is_parseable() {
        assert!(parse_semver(env!("CARGO_PKG_VERSION")).is_some());
    }

    #[test]
    fn check_message_reflects_decision() {
        assert!(check_message(&Decision::Update, "0.13.0", "0.14.0").contains("update available"));
        assert!(check_message(&Decision::UpToDate, "0.14.0", "0.14.0").contains("up to date"));
        assert!(check_message(&Decision::DevAhead, "0.15.0", "0.14.0").contains("newer"));
    }
}
