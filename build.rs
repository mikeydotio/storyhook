//! Stamps every build and embeds the provider plugin marketplace.
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
//! # Embedded plugin marketplace
//!
//! SH-538 makes the provider plugin part of the same artifact as the CLI.
//! Cargo builds generate an `include_bytes!` table for both root marketplace
//! manifests and every regular file beneath `plugins/story`, preserving the
//! executable bit. Unlike the optional identity stamp below, this is a release
//! capability: missing, unsafe, symlinked or special entries fail the build.
//! The standalone identity tests explicitly remove `OUT_DIR`, so they
//! exercise only the stamp contract they were built to isolate.
//!
//! # Embedded verifier scripts
//!
//! SH-654 makes the verifier script family part of the binary the same way
//! ([`VERIFIER_SCRIPTS`], written as a second table with paths relative to
//! `scripts/`). The daemon used to spawn `scripts/verify-pr.sh` relative to
//! the registered project's checkout, so only a storyhook checkout could ever
//! be verified; `src/daemon/verifier_bundle.rs` now projects this table under
//! the daemon's own state directory and runs it from there.
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
//! # Where the artifact was written
//!
//! SH-630: the write-side migration guard (`src/migration_guard.rs`) and the
//! login-agent install guard (`src/daemon/install_guard.rs`) both asked one
//! question — *is this the `story` `$PATH` runs?* — and `$PATH` is the
//! caller's own claim about itself. `PATH="$PWD/target/debug:$PATH" story …`
//! made a worktree's debug binary "installed" in its own eyes and migrated
//! the production store past what the real installed release understood.
//! [`STORYHOOK_BUILD_DIR`] is the fact neither caller can rewrite: the
//! directory cargo wrote this binary into, `OUT_DIR`'s third ancestor
//! (`<profile dir>/build/<pkg>-<hash>/out`). A binary still inside it has
//! not been installed by any mechanism this tree knows — `make install`,
//! `story update` and `cargo install` all *copy out* — and both guards refuse
//! it whatever `$PATH` says. Stamped verbatim, never canonicalized: the
//! directory need not exist when this script runs, and the reader
//! canonicalizes against the filesystem it actually has. An `OUT_DIR` of a
//! shape this script does not understand stamps nothing and says so with a
//! [`cargo::warning`]: guessing an ancestor would name a directory the
//! binary does not land in, which turns the guard's refusal into a permit.
//! No `OUT_DIR` at all (the standalone `tests/build_identity.rs` runner)
//! stamps nothing, silently.
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

use std::fs;
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

fn main() {
    if let Err(error) = write_embedded_marketplace() {
        panic!("could not embed the StoryHook plugin marketplace: {error}");
    }
    if let Err(error) = write_embedded_verifier() {
        panic!("could not embed the StoryHook verifier scripts: {error}");
    }

    let build_id = resolve_build_id();

    if let Some(id) = build_id {
        println!("cargo::rustc-env=STORYHOOK_BUILD_ID={id}");
    }

    if let Some(dir) = resolve_build_dir() {
        println!("cargo::rustc-env=STORYHOOK_BUILD_DIR={}", dir.display());
    }

    // Deliberately no cargo::rerun-if-changed or cargo::rerun-if-env-changed
    // directive anywhere in this file — see the module doc's "Why no
    // rerun-if-*" section. Emitting one here would narrow cargo's default
    // rerun trigger rather than add to it.
}

/// Generates the marketplace table consumed by `src/plugin.rs`.
///
/// The standalone build-script tests intentionally remove `OUT_DIR`; only
/// Cargo asks for an embedded artifact. A real Cargo build fails if any
/// marketplace entry is missing or is not a regular file/directory, because a
/// binary without its promised plugin payload is not a usable artifact.
fn write_embedded_marketplace() -> io::Result<()> {
    let Some(output_dir) = std::env::var_os("OUT_DIR") else {
        return Ok(());
    };
    let manifest_dir = manifest_dir()?;
    let mut files = vec![
        manifest_dir.join(".agents/plugins/marketplace.json"),
        manifest_dir.join(".claude-plugin/marketplace.json"),
    ];
    collect_regular_files(&manifest_dir.join("plugins/story"), &mut files)?;
    write_embedded_table(
        &PathBuf::from(output_dir).join("embedded_marketplace.rs"),
        "EMBEDDED_MARKETPLACE",
        &manifest_dir,
        files,
    )
}

/// The verifier script family — everything `scripts/verify-pr.sh` reaches
/// through its own directory, transitively (SH-654). Listed by name rather
/// than derived from a directory because the family shares `scripts/` with
/// forty unrelated scripts; `tests/verifier_bundle.rs` derives the closure
/// from the scripts themselves and fails when a referenced sibling is
/// missing here, so the list cannot drift silently.
const VERIFIER_SCRIPTS: &[&str] = &[
    "activity-log.sh",
    "activity-run.py",
    "gate-progress.sh",
    "land-pr.sh",
    "machine-lock.sh",
    "merge-preflight.sh",
    "merge-watch.sh",
    "test_output.py",
    "tracked-tree.sh",
    "tree-receipt.sh",
    "verify-pr.sh",
    "verify-window.sh",
];

/// Generates the verifier table consumed by `src/daemon/verifier_bundle.rs`,
/// with paths relative to `scripts/` so the projection is the directory the
/// scripts already expect to find each other in.
fn write_embedded_verifier() -> io::Result<()> {
    let Some(output_dir) = std::env::var_os("OUT_DIR") else {
        return Ok(());
    };
    let scripts = manifest_dir()?.join("scripts");
    let files = VERIFIER_SCRIPTS
        .iter()
        .map(|name| scripts.join(name))
        .collect();
    write_embedded_table(
        &PathBuf::from(output_dir).join("embedded_verifier.rs"),
        "EMBEDDED_VERIFIER",
        &scripts,
        files,
    )
}

fn manifest_dir() -> io::Result<PathBuf> {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "CARGO_MANIFEST_DIR is not set"))
}

/// Writes `const <const_name>: &[EmbeddedFile] = &[...]` to `generated`, one
/// entry per file with its path relative to `base`, its bytes via
/// `include_bytes!`, and its executable bit. Every entry must be a regular
/// file beneath `base` with only normal path components; anything else
/// fails the build rather than producing a binary missing part of a payload.
fn write_embedded_table(
    generated: &Path,
    const_name: &str,
    base: &Path,
    mut files: Vec<PathBuf>,
) -> io::Result<()> {
    files.sort();
    let mut output = fs::File::create(generated)?;
    writeln!(output, "const {const_name}: &[EmbeddedFile] = &[")?;
    for file in files {
        let metadata = fs::symlink_metadata(&file)?;
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} is not a regular file", file.display()),
            ));
        }
        let relative = file.strip_prefix(base).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} escapes {}", file.display(), base.display()),
            )
        })?;
        validate_relative_path(relative)?;
        #[cfg(unix)]
        let executable = metadata.permissions().mode() & 0o111 != 0;
        #[cfg(not(unix))]
        let executable = false;
        writeln!(
            output,
            "    EmbeddedFile {{ relative_path: {:?}, bytes: include_bytes!({:?}), executable: {executable} }},",
            relative.to_string_lossy(),
            file.to_string_lossy(),
        )?;
    }
    writeln!(output, "];")?;
    Ok(())
}

fn collect_regular_files(directory: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a directory", directory.display()),
        ));
    }
    let mut entries: Vec<_> = fs::read_dir(directory)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            collect_regular_files(&path, files)?;
        } else if metadata.is_file() {
            files.push(path);
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} is neither a regular file nor a directory",
                    path.display()
                ),
            ));
        }
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> io::Result<()> {
    if path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("embedded path is unsafe: {}", path.display()),
        ))
    }
}

/// The directory cargo writes this package's binaries into, from `OUT_DIR`'s
/// documented shape — see the module doc's "Where the artifact was written".
///
/// `None` with no warning when `OUT_DIR` is unset (a standalone run); `None`
/// with a `cargo::warning` when it is set but is not `…/build/<pkg>-<hash>/out`,
/// because a guessed ancestor is worse than no stamp.
fn resolve_build_dir() -> Option<PathBuf> {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR")?);
    let mut ancestors = out_dir.ancestors();
    let leaf_is_out = ancestors
        .next()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "out");
    // `<pkg>-<hash>`: any name; its parent is what identifies the shape.
    let _package = ancestors.next();
    let under_build = ancestors
        .next()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "build");
    let profile_dir = ancestors.next();
    match (leaf_is_out && under_build, profile_dir) {
        (true, Some(dir)) if !dir.as_os_str().is_empty() => Some(dir.to_path_buf()),
        _ => {
            println!(
                "cargo::warning=STORYHOOK_BUILD_DIR not stamped: OUT_DIR ({}) is not of the \
                 form <profile dir>/build/<pkg>-<hash>/out, so the directory this binary \
                 lands in cannot be named",
                out_dir.display()
            );
            None
        }
    }
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
