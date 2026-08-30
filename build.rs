//! Stamps every build with an identity distinct from `CARGO_PKG_VERSION`.
//!
//! SH-406: `make install` puts `target/release/story` on `PATH` under
//! whatever `VERSION` already says, so two builds with different
//! capabilities can report the identical `story --version` string. The
//! incident this closes (SH-404): the binary that broke the store and the
//! binary that fixed it both reported `story 2.1.1` — one understood schema
//! 16, the other 17 — and nothing distinguished them until the daemon
//! refused to start.
//!
//! A semver bump was considered and rejected (see the story's own comment
//! trail — `story show SH-406`, SH-363's rule against citing a council's own
//! directory, which does not survive worktree teardown). It cannot be
//! implemented without breaking `scripts/release.sh --bump`: an install-time
//! bump must skip tagging (an unpublished build must never be tagged), which
//! leaves `VERSION` permanently ahead of any tag, which is exactly the
//! condition `semver-cli validate`'s `tag_exists` check fails on — and that
//! failure then blocks every later release bump too. A patch bump is also
//! blind to the actual incident shape: two builds from one commit with
//! different uncommitted edits would still report the same number.
//!
//! Instead, this build script stamps [`STORYHOOK_BUILD_ID`] with the git tree
//! object id of the tracked content the binary was built from — the same
//! identity primitive `scripts/gate-receipt.sh` uses to key gate receipts
//! (SH-306) and `scripts/merge-preflight.sh` uses for merge certification
//! (SH-396). Two builds share a version string if and only if their tracked
//! content is byte-identical, which is what the story's title actually
//! promises and a version bump structurally cannot.
//!
//! # Resolution order
//!
//! 1. `$STORYHOOK_BUILD_ID`, verbatim (first line, trimmed) — the deliberate
//!    escape hatch, in the `STORYHOOK_INSTALL_DIR` style this project already
//!    uses for build/install overrides.
//! 2. `scripts/tracked-tree.sh` (SH-406; extracted from `gate-receipt.sh`
//!    when this became a second caller — see that script's header for the
//!    council verdict on why it is invoked as a subprocess rather than
//!    sourced), first 12 hex characters of its stdout.
//! 3. No stamp. Expected whenever there is no `.git` to ask (a release
//!    tarball, `cargo install` from a packaged source, a vendored copy) —
//!    silent, because this is the normal case for a published crate, not an
//!    error. If a `.git` directory *is* present and the script still fails,
//!    that is unexpected, so a [`cargo::warning`] is emitted (but the build
//!    still succeeds) — the SH-306 doctrine that a gate's silence must never
//!    be mistaken for "nothing needed reporting."
//!
//! # Why no `rerun-if-*` directive
//!
//! Emitting any `cargo::rerun-if-*` directive replaces cargo's default rerun
//! policy ("rerun if any file in the package changed") with exactly the
//! directives listed. This script deliberately emits none, so the default
//! policy stays in force — which is the correct trigger for this stamp: any
//! change to tracked content is a change this stamp must reflect, and cargo's
//! own file-mtime-based default already fires on it. A commit that changes no
//! content leaves the tree oid unchanged, so no rerun is *needed*; cargo may
//! still choose to rerun on the coarser file-changed signal, but recomputing
//! an unchanged oid is cheap correctness, not a bug. `STORYHOOK_BUILD_ID`
//! taking effect requires a rebuild that reruns this script, which any edit
//! to a tracked file already provides.
//!
//! `tests/build_identity.rs` fences this file's no-`rerun-if-` invariant
//! mechanically, and proves the resolution order end to end against real git
//! repositories built at test time — never a copy of this logic pasted into
//! the test, per this project's own SH-136 doctrine against hand-duplicated
//! constants drifting from their source.

use std::path::Path;
use std::process::Command;

fn main() {
    let build_id = resolve_build_id();

    if let Some(id) = build_id {
        println!("cargo::rustc-env=STORYHOOK_BUILD_ID={id}");
    }

    // Deliberately no cargo::rerun-if-changed or cargo::rerun-if-env-changed
    // directive anywhere in this file — see the module doc's "Why no
    // rerun-if-*" section. Emitting one here would narrow cargo's default
    // rerun trigger rather than add to it.
}

/// Resolves the build id via the order documented on the module.
fn resolve_build_id() -> Option<String> {
    if let Some(id) = build_id_from_env() {
        return Some(id);
    }
    build_id_from_tracked_tree()
}

fn build_id_from_env() -> Option<String> {
    let raw = std::env::var("STORYHOOK_BUILD_ID").ok()?;
    let trimmed = raw.lines().next().unwrap_or("").trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Shells out to `scripts/tracked-tree.sh` and truncates its stdout to 12 hex
/// characters. Returns `None` on any failure — no `.git` is the expected
/// case and stays silent; a `.git` directory present alongside an
/// unexpected failure gets a `cargo::warning` so it is never silently
/// dropped (SH-306: a gate's silence must not be mistaken for "no news").
fn build_id_from_tracked_tree() -> Option<String> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let script = Path::new(&manifest_dir)
        .join("scripts")
        .join("tracked-tree.sh");

    let git_present = Path::new(&manifest_dir).join(".git").exists();

    if !script.exists() {
        if git_present {
            println!(
                "cargo::warning=STORYHOOK_BUILD_ID not stamped: {} is missing",
                script.display()
            );
        }
        return None;
    }

    // `tracked-tree.sh` resolves `git rev-parse --show-toplevel` from its own
    // process's working directory, not from an argument -- cargo already runs
    // build scripts with the package root as their cwd, but setting it here
    // explicitly makes this call correct regardless of how it is invoked
    // (this file is also compiled and run standalone by
    // `tests/build_identity.rs`, which has no reason to share cargo's cwd
    // convention).
    let output = match Command::new("bash")
        .arg(&script)
        .current_dir(&manifest_dir)
        // The tracked-tree producer owns the writable primary for this call.
        // A read-only alternate is deliberately retained: merge-watch checks
        // out a speculative commit whose objects live in its caller-owned
        // lease, and the build stamp still has to describe that exact tree.
        // tracked-tree.sh appends the inherited alternate behind its own
        // canonical source store while keeping all generated objects private.
        .env_remove("GIT_OBJECT_DIRECTORY")
        .output()
    {
        Ok(output) => output,
        Err(err) => {
            if git_present {
                println!(
                    "cargo::warning=STORYHOOK_BUILD_ID not stamped: could not run {}: {err}",
                    script.display()
                );
            }
            return None;
        }
    };

    if !output.status.success() {
        // A missing .git (release tarball, packaged source) is the expected
        // way this script fails, and tracked-tree.sh's own contract is to
        // exit nonzero with empty stdout for it — silent by design.
        if git_present {
            println!(
                "cargo::warning=STORYHOOK_BUILD_ID not stamped: {} exited with {}",
                script.display(),
                output.status
            );
        }
        return None;
    }

    let oid = String::from_utf8_lossy(&output.stdout);
    let oid = oid.trim();
    if oid.len() < 12 || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        if git_present {
            println!(
                "cargo::warning=STORYHOOK_BUILD_ID not stamped: {} printed an unexpected value",
                script.display()
            );
        }
        return None;
    }

    Some(oid[..12].to_string())
}
