//! SH-635: `scripts/binary-lease.sh` is the shell rendering of
//! `storyhook_test_support::story_binary()`'s lease (SH-532), and this suite
//! proves it the way SH-532's own unit tests prove the Rust one — by driving
//! the tracked script, with real bash, against a fixture artifact, never a
//! copy of its logic pasted here (SH-136).
//!
//! What is pinned: a lease is the artifact's own inode under a `<pid>-<nonce>`
//! entry beside the artifact; an atomic replacement of the artifact leaves the
//! lease on the build it started with; the sweep reclaims only a lease whose
//! owner is provably gone; and the directory name the shell spells equals the
//! Rust constant, because the two sweepers share one root on purpose.

use std::os::unix::fs::MetadataExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use storyhook_test_support::{BINARY_SNAPSHOT_DIR, scratch_dir};

/// The tracked script under test.
fn lease_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/binary-lease.sh")
}

/// Runs `body` in a bash that has sourced the lease script, from `cwd`.
fn bash(cwd: &Path, body: &str) -> Output {
    Command::new("bash")
        .arg("-c")
        .arg(format!(
            "set -euo pipefail\n. '{}'\n{body}",
            lease_script().display()
        ))
        .current_dir(cwd)
        .output()
        .expect("spawning bash")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

/// A fixture `target/debug` holding an executable `story` shim that prints
/// `message`, so a lease and its source can be told apart by running them.
fn write_executable(path: &Path, message: &str) {
    std::fs::write(path, format!("#!/bin/sh\nprintf '%s\\n' '{message}'\n"))
        .unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|e| panic!("chmod {}: {e}", path.display()));
}

struct Fixture {
    _root: tempfile::TempDir,
    debug: PathBuf,
    artifact: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = scratch_dir();
        let debug = root.path().join("target/debug");
        std::fs::create_dir_all(&debug).expect("creating the fixture target dir");
        let artifact = debug.join("story");
        write_executable(&artifact, "leased-build");
        Self {
            _root: root,
            debug,
            artifact,
        }
    }

    fn lease_root(&self) -> PathBuf {
        self.debug.join(BINARY_SNAPSHOT_DIR)
    }

    fn lease(&self, owner: Option<u32>) -> PathBuf {
        let owner = owner.map(|pid| format!(" {pid}")).unwrap_or_default();
        let out = bash(
            &self.debug,
            &format!(
                "storyhook_lease_binary '{}'{owner}",
                self.artifact.display()
            ),
        );
        assert!(
            out.status.success(),
            "leasing should succeed\nstderr: {}",
            stderr(&out)
        );
        PathBuf::from(stdout(&out))
    }
}

// ---------------------------------------------------------------------------
// 1. The lease is the artifact's inode, beside it, named for its owner
// ---------------------------------------------------------------------------

#[test]
fn a_lease_is_a_hard_link_beside_the_artifact_named_for_its_owner() {
    let fx = Fixture::new();
    let lease = fx.lease(None);

    let source = std::fs::metadata(&fx.artifact).expect("stat artifact");
    let leased = std::fs::metadata(&lease).expect("stat lease");
    assert_eq!(
        leased.ino(),
        source.ino(),
        "the lease must be a hard link of the artifact, not a copy"
    );
    assert!(
        source.nlink() >= 2,
        "the artifact's inode must now carry the lease as a second link"
    );
    assert_eq!(
        lease.file_name(),
        fx.artifact.file_name(),
        "the basename is preserved so PATH-based hooks still resolve `story`"
    );
    assert_ne!(
        lease, fx.artifact,
        "the lease must not be Cargo's own mutable path"
    );

    let entry = lease.parent().expect("a lease dir");
    assert_eq!(
        entry.parent(),
        Some(fx.lease_root().as_path()),
        "the lease sits directly under the shared lease root"
    );
    let name = entry.file_name().unwrap().to_str().unwrap();
    let (pid, nonce) = name
        .split_once('-')
        .unwrap_or_else(|| panic!("lease entry {name} is not <pid>-<nonce>"));
    let pid: u32 = pid
        .parse()
        .unwrap_or_else(|_| panic!("owner {pid} is not a pid"));
    assert!(pid > 0, "the owner is the leasing shell's own pid");
    assert!(!nonce.is_empty(), "the entry carries a nonce after the pid");
}

#[test]
fn a_caller_may_name_the_owner_pid() {
    let fx = Fixture::new();
    let lease = fx.lease(Some(4242));
    let name = lease
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        name.starts_with("4242-"),
        "an explicit owner names the entry: {name}"
    );
}

// ---------------------------------------------------------------------------
// 2. An atomic replacement of the artifact leaves the lease on the old build
// ---------------------------------------------------------------------------

#[test]
fn a_lease_survives_cargo_replacing_the_artifact() {
    let fx = Fixture::new();
    let lease = fx.lease(None);

    let replacement = fx.debug.join("replacement");
    write_executable(&replacement, "rebuilt-build");
    std::fs::rename(&replacement, &fx.artifact).expect("renaming over the artifact");

    let leased_says = Command::new(&lease).output().expect("running the lease");
    let artifact_says = Command::new(&fx.artifact)
        .output()
        .expect("running the artifact");
    assert_eq!(stdout(&leased_says), "leased-build");
    assert_eq!(stdout(&artifact_says), "rebuilt-build");

    // The runner's own divergence check is `[ lease -ef artifact ]`; the two
    // must now be different inodes, which is what `-ef` reports on.
    let out = bash(
        &fx.debug,
        &format!(
            "if [ '{}' -ef '{}' ]; then echo same; else echo diverged; fi",
            lease.display(),
            fx.artifact.display()
        ),
    );
    assert_eq!(stdout(&out), "diverged");
}

// ---------------------------------------------------------------------------
// 3. The sweep reclaims only a provably dead owner
// ---------------------------------------------------------------------------

#[test]
fn the_sweep_removes_only_leases_whose_owner_is_provably_gone() {
    let fx = Fixture::new();
    let root = fx.lease_root();
    let live = root.join(format!("{}-live", std::process::id()));
    // `i32::MAX` is outside the platform's live pid range; `kill -0` answers
    // ESRCH for it, which is the one answer that proves absence.
    let dead = root.join(format!("{}-dead", i32::MAX));
    // pid 1 exists and belongs to root: `kill -0` fails with EPERM, which
    // proves presence, not absence.
    let eperm = root.join("1-eperm");
    let malformed = root.join("owner-unknown");
    let zero = root.join("0-zero");
    for dir in [&live, &dead, &eperm, &malformed, &zero] {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| panic!("creating {}: {e}", dir.display()));
    }

    let out = bash(
        &fx.debug,
        &format!("storyhook_sweep_binary_leases '{}'", root.display()),
    );
    assert!(out.status.success(), "sweep failed: {}", stderr(&out));

    assert!(live.is_dir(), "a live owner keeps its lease");
    assert!(
        eperm.is_dir(),
        "an owner that exists but cannot be signalled keeps its lease"
    );
    assert!(
        malformed.is_dir(),
        "an entry with no parsable owner is retained"
    );
    assert!(
        zero.is_dir(),
        "pid 0 is never an owner and is retained rather than reasoned about"
    );
    assert!(!dead.exists(), "a provably dead owner's lease is reclaimed");
}

#[test]
fn leasing_sweeps_the_root_first() {
    let fx = Fixture::new();
    let dead = fx.lease_root().join(format!("{}-stale", i32::MAX));
    std::fs::create_dir_all(&dead).expect("planting a stale lease");
    let lease = fx.lease(None);
    assert!(
        !dead.exists(),
        "minting a lease reclaims stale ones beside it"
    );
    assert!(lease.is_file());
}

#[test]
fn the_sweep_tolerates_a_missing_root() {
    let fx = Fixture::new();
    let out = bash(
        &fx.debug,
        &format!(
            "storyhook_sweep_binary_leases '{}/absent'",
            fx.debug.display()
        ),
    );
    assert!(
        out.status.success(),
        "a root that does not exist is nothing to sweep"
    );
}

// ---------------------------------------------------------------------------
// 4. Refusals name their cause
// ---------------------------------------------------------------------------

#[test]
fn a_missing_artifact_is_refused_by_name() {
    let fx = Fixture::new();
    let missing = fx.debug.join("nope");
    let out = bash(
        &fx.debug,
        &format!("storyhook_lease_binary '{}'", missing.display()),
    );
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains(&missing.display().to_string()),
        "{}",
        stderr(&out)
    );
    assert!(
        stdout(&out).is_empty(),
        "a refusal prints no path a caller could adopt"
    );
}

#[test]
fn a_bad_owner_pid_is_refused_by_name() {
    let fx = Fixture::new();
    for bad in ["abc", "0", "-1"] {
        let out = bash(
            &fx.debug,
            &format!("storyhook_lease_binary '{}' '{bad}'", fx.artifact.display()),
        );
        assert!(!out.status.success(), "owner {bad:?} must be refused");
        assert!(
            stderr(&out).contains(bad),
            "the refusal names the owner: {}",
            stderr(&out)
        );
    }
}

// ---------------------------------------------------------------------------
// 5. One root, spelled once per language, pinned equal
// ---------------------------------------------------------------------------

#[test]
fn the_shell_lease_root_is_the_rust_lease_root() {
    let script = std::fs::read_to_string(lease_script()).expect("reading the lease script");
    let line = script
        .lines()
        .find(|l| l.starts_with("STORYHOOK_BINARY_LEASE_DIR="))
        .expect("binary-lease.sh must assign STORYHOOK_BINARY_LEASE_DIR at top level");
    let spelled = line
        .trim_start_matches("STORYHOOK_BINARY_LEASE_DIR=")
        .trim_matches('"');
    assert_eq!(
        spelled, BINARY_SNAPSHOT_DIR,
        "the shell and Rust leases must share one root so either sweeper reclaims \
         the other's dead leases (SH-532, SH-635)"
    );
    // And the script must actually use the variable, not a second literal.
    assert!(
        script.contains("/$STORYHOOK_BINARY_LEASE_DIR\""),
        "the lease root must be built from the variable, never a repeated literal"
    );
    let code: String = script
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        code.matches(BINARY_SNAPSHOT_DIR).count(),
        1,
        "outside comments, the directory name is spelled exactly once in the script"
    );
}

#[test]
fn sourcing_the_script_defines_the_functions_and_nothing_runs() {
    let fx = Fixture::new();
    let out = bash(
        &fx.debug,
        "declare -F storyhook_lease_binary storyhook_sweep_binary_leases \
         storyhook_binary_lease_owner_is_gone >/dev/null && echo defined",
    );
    assert_eq!(stdout(&out), "defined", "{}", stderr(&out));
    assert!(
        !fx.lease_root().exists(),
        "sourcing must not mint a lease or create the root"
    );
}
