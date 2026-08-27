//! `scripts/release-tag-commit.sh` — which commit a release tag belongs on.
//!
//! `scripts/release.sh` used to tag whatever `HEAD` happened to be after
//! `gh pr merge --merge` and `git pull --ff-only`, which is the **merge
//! commit**. The commit that actually modified `VERSION` is its parent, on the
//! release branch. `semver-cli`'s `tag_correct_commit` check compares the tag
//! against `git log -1 -- VERSION`, so the two never agreed and every release
//! `release.sh` cut refused the *next* one before it started (SH-494).
//!
//! The consent check runs against the CURRENT version, which is why the damage
//! is always one release downstream of the mistake: v2.2.0 shipped fine and
//! v2.3.0 could not begin.
//!
//! The fix is not to capture a SHA and hope it is still right by tagging time.
//! It is to ask **the same question semver asks** — the last commit to touch
//! `VERSION` — so the two cannot disagree by construction. This script is that
//! question, extracted so it can be driven against real repositories rather
//! than asserted about by reading `release.sh`. Its contract is
//! `scripts/tracked-tree.sh`'s, for the same reason SH-406 chose it: stdout
//! carries the oid, a nonzero exit means "no answer", and both are provable
//! end to end.
//!
//! Real git throughout, never a fixture that models git's answer. History
//! simplification is the entire mechanism under test — `git log -1 -- VERSION`
//! skips a merge whose tree matches a parent's — and a model of that is a
//! model of the thing that broke.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// The repository root, which is this package's manifest directory.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The script under test.
fn script() -> PathBuf {
    repo_root().join("scripts/release-tag-commit.sh")
}

/// Runs `git` in `dir`, panicking if it cannot be spawned or fails — a broken
/// fixture is a finding, not a red assertion.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("could not spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Runs the script in `dir` with `version`.
fn run(dir: &Path, version: &str) -> Output {
    Command::new("bash")
        .current_dir(dir)
        .arg(script())
        .arg(version)
        .output()
        .expect("could not spawn the script")
}

/// A throwaway repository with one commit and no `VERSION` yet.
fn new_repo() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path();
    git(p, &["init", "-q", "-b", "main"]);
    git(p, &["config", "user.email", "t@t"]);
    git(p, &["config", "user.name", "t"]);
    std::fs::write(p.join("README.md"), "seed\n").unwrap();
    git(p, &["add", "-A"]);
    git(p, &["commit", "-qm", "seed"]);
    dir
}

/// Writes `VERSION` and commits it, returning the commit oid.
fn commit_version(dir: &Path, version: &str, subject: &str) -> String {
    std::fs::write(dir.join("VERSION"), format!("{version}\n")).unwrap();
    git(dir, &["add", "VERSION"]);
    git(dir, &["commit", "-qm", subject]);
    git(dir, &["rev-parse", "HEAD"])
}

#[test]
fn it_names_the_release_commit_and_not_the_merge_that_landed_it() {
    // The exact shape of the defect: a release branch bumps VERSION, a merge
    // commit lands it, and HEAD is then the merge. `--no-ff` is deliberate --
    // a fast-forward would make the two commits the same and the test would
    // pass against the very bug it exists to catch.
    let dir = new_repo();
    let p = dir.path();
    git(p, &["switch", "-qc", "release/v2.3.0"]);
    let release_commit = commit_version(p, "v2.3.0", "chore(release): v2.3.0");
    git(p, &["switch", "-q", "main"]);
    git(p, &["merge", "-q", "--no-ff", "release/v2.3.0", "-m", "Merge PR"]);

    let head = git(p, &["rev-parse", "HEAD"]);
    assert_ne!(
        head, release_commit,
        "fixture premise: HEAD must be the merge, or this proves nothing"
    );

    let out = run(p, "v2.3.0");
    assert!(
        out.status.success(),
        "the script refused a well-formed history: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        release_commit,
        "the tag belongs on the release commit, never the merge that landed it"
    );
}

#[test]
fn it_agrees_with_the_question_semver_actually_asks() {
    // The whole design claim: this script and `semver-cli`'s
    // `tag_correct_commit` must reach the same commit, so they cannot drift.
    // Asserted against git's own answer to that identical query rather than
    // against a second copy of the logic.
    let dir = new_repo();
    let p = dir.path();
    git(p, &["switch", "-qc", "release/v9.9.9"]);
    commit_version(p, "v9.9.9", "chore(release): v9.9.9");
    git(p, &["switch", "-q", "main"]);
    git(p, &["merge", "-q", "--no-ff", "release/v9.9.9", "-m", "Merge PR"]);

    let semver_would_pick = git(p, &["log", "-1", "--format=%H", "--", "VERSION"]);
    let out = run(p, "v9.9.9");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        semver_would_pick,
        "the script must answer semver's own query, not a lookalike"
    );
}

#[test]
fn it_names_head_when_the_release_commit_is_head() {
    // Positive control on the simplification: with no merge in the way the
    // answer IS HEAD, so a script that always walked back one commit -- an
    // obvious wrong fix for this defect -- fails here.
    let dir = new_repo();
    let p = dir.path();
    let release_commit = commit_version(p, "v1.0.0", "chore(release): v1.0.0");

    let out = run(p, "v1.0.0");
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        release_commit,
        "with no merge, the release commit is HEAD"
    );
}

#[test]
fn it_refuses_when_version_at_that_commit_is_not_the_one_asked_for() {
    // The guard that makes the answer trustworthy rather than merely plausible.
    // A caller asking for v2.3.0 against a tree that says v2.2.0 is mid-release
    // or on the wrong branch, and tagging there would mint exactly the kind of
    // wrong pointer this story is about.
    let dir = new_repo();
    let p = dir.path();
    commit_version(p, "v2.2.0", "chore(release): v2.2.0");

    let out = run(p, "v2.3.0");
    assert!(
        !out.status.success(),
        "a version mismatch must refuse, not answer"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("v2.3.0") && stderr.contains("v2.2.0"),
        "the refusal must name BOTH versions so the operator can see the skew: {stderr}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "a refusal writes no oid to stdout -- a caller substituting $(...) must get nothing"
    );
}

#[test]
fn it_refuses_a_repository_with_no_version_file() {
    let dir = new_repo();
    let out = run(dir.path(), "v1.0.0");
    assert!(!out.status.success(), "no VERSION means no answer");
    assert!(
        String::from_utf8_lossy(&out.stdout).trim().is_empty(),
        "a refusal writes no oid to stdout"
    );
}

#[test]
fn it_refuses_without_a_version_argument() {
    let dir = new_repo();
    let p = dir.path();
    commit_version(p, "v1.0.0", "chore(release): v1.0.0");
    let out = Command::new("bash")
        .current_dir(p)
        .arg(script())
        .output()
        .expect("could not spawn the script");
    assert!(
        !out.status.success(),
        "a bare invocation must refuse rather than guess which version is meant"
    );
}

#[test]
fn release_sh_tags_a_named_commit_rather_than_whatever_head_is() {
    // A WIRING fence in SH-360's sense, and its limit is stated: it proves the
    // call site passes a commit, never that the commit is the right one -- the
    // behavioural tests above own that. It exists because the defect was
    // exactly a missing argument, one word wide, and a future edit could drop
    // it again with every test above still green.
    let source = std::fs::read_to_string(repo_root().join("scripts/release.sh"))
        .expect("release.sh is readable");
    let tag_lines: Vec<&str> = source
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter(|line| line.contains("git tag"))
        .collect();
    assert!(
        !tag_lines.is_empty(),
        "positive control: release.sh must still contain a git tag invocation"
    );
    for line in tag_lines {
        assert!(
            line.contains("release_commit"),
            "release.sh tags HEAD implicitly, which is SH-494 exactly: {line}"
        );
    }
}
