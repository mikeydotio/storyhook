//! The one question two guards ask about this machine: *which `story` does
//! `$PATH` run, and is it the one running now?* — and the second fact a third
//! guard asks alone: *has this binary left the directory cargo wrote it into?*
//! ([`crate::daemon::seat_guard`], SH-634, which needs no `$PATH` at all).
//!
//! Extracted from [`crate::migration_guard`] (SH-404) when SH-411 gave it a
//! second caller. The two guards ask the same question and reach opposite
//! conclusions from a *missing* answer, so what they share is the fact, never
//! the judgement — see [`crate::daemon::install_guard`]'s module doc for why
//! absence refuses there and permits here. Sharing the lookup rather than
//! copying it is the SH-136 rule; sharing `decide` would be coupling to a
//! judgement.
//!
//! # Two spellings of one binary, and why both are kept
//!
//! [`InstalledStory`] carries the `$PATH` entry's own `dir.join("story")`
//! alongside its canonical form, because the two are wanted by different
//! callers for opposite reasons:
//!
//! * every **comparison** canonicalizes, or a symlinked `~/.local/bin/story`
//!   would never equal the build it points at;
//! * every path **written into a launchd plist** must be the spelling, because
//!   a plist outlives the build it names. `~/.local/bin/story` survives the
//!   next upgrade; `~/.local/share/storyhook/versions/2.1.1/story` does not.
//!   Asking what a binary *is* rather than what it is *spelled* is SH-239's
//!   rule; this is the one place where the spelling is the durable answer and
//!   the identity is not.
//!
//! Measured rather than assumed: on macOS [`std::env::current_exe`] returns the
//! path the process was invoked through, **not** its realpath — a binary run
//! through a symlink reports the symlink. So the common case already records a
//! stable spelling, and only an operator invoking a version-pinned real path
//! directly needs [`InstalledStory::spelling`] to correct it.
//!
//! # `$PATH` is the caller's claim, not the machine's (SH-630)
//!
//! Both guards used to ask *only* the question above, and on 2026-09-09 a
//! `PATH="$PWD/target/debug:$PATH" story …` from the main checkout answered
//! it for a worktree's debug binary: `$PATH` resolved `story` to the very
//! binary asking, the comparison agreed with itself, and the production store
//! was migrated past what the installed release understood. `$PATH` is a
//! per-process variable the caller sets, and prefixing a build directory onto
//! it is the most natural way anyone tries a build — so a refusal that
//! protects durable state may never have [`installed_story`] as its only
//! clause. [`build_dir`] is the second fact, and it is one the caller cannot
//! rewrite: `build.rs` stamps the directory cargo wrote the binary into, and
//! a binary that has not left it has not been installed by any mechanism this
//! tree knows (`make install`, `story update` and `cargo install` all copy
//! out). The judgement each guard draws from it stays its own, as above.
//!
//! # A known limit, inherited rather than created
//!
//! A shim manager (`mise`, `asdf`, `direnv`) puts a shim directory first on
//! `$PATH`. A shim is usually a real executable file that `exec`s the real
//! binary, so [`std::fs::canonicalize`] does not reach through it and the
//! comparison reports a disagreement that is not one. SH-404 already ships this
//! resolver and this comparison against the real store, so SH-411 widens an
//! inherited class rather than opening a new one; each caller carries its own
//! way through (`--this-binary` at install,
//! [`crate::migration_guard::OVERRIDE_VAR`] at migration). Unmeasured, and
//! recorded here rather than in one caller so both read it.

use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The `story` `$PATH` resolves, in both spellings.
///
/// See the module doc: comparisons want [`Self::canonical`], anything written
/// into a file that outlives this process wants [`Self::spelling`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledStory {
    /// The `$PATH` entry's own `dir.join("story")`, exactly as a shell would
    /// have spelled it.
    pub spelling: PathBuf,
    /// [`Self::spelling`] canonicalized, falling back to itself when it cannot
    /// be.
    pub canonical: PathBuf,
}

/// The `story` this process's own `$PATH` would run, or `None` when it names
/// none.
#[must_use]
pub fn installed_story() -> Option<InstalledStory> {
    resolve_on_path(env::var_os("PATH").as_deref(), "story")
}

/// This process's own executable, in both spellings, or `None` when the
/// platform cannot report it at all.
///
/// [`std::env::current_exe`] failing is not a case this binary is known to hit
/// on a supported target; callers treat it the same way they treat `$PATH`
/// naming nothing.
#[must_use]
pub fn running_exe() -> Option<InstalledStory> {
    let spelling = env::current_exe().ok()?;
    Some(InstalledStory {
        canonical: canonicalize_or(spelling.clone(), spelling.clone()),
        spelling,
    })
}

/// The directory cargo wrote this binary into, as `build.rs` stamped it —
/// `None` for a build that carried no `OUT_DIR` (see `build.rs`'s "Where the
/// artifact was written").
pub const BUILD_DIR: Option<&str> = option_env!("STORYHOOK_BUILD_DIR");

/// [`BUILD_DIR`], canonicalized against the filesystem this process has.
///
/// A stamped directory that no longer exists (the checkout was moved, or
/// `cargo clean` ran under a copied binary) falls back to its literal
/// spelling, which then matches nothing a canonicalized executable path
/// reports — so the binary reads as *outside* it. That is the honest answer:
/// a binary whose build directory is gone is not sitting in it.
#[must_use]
pub fn build_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(BUILD_DIR?);
    Some(canonicalize_or(dir.clone(), dir))
}

/// Whether `exe` sits inside `build_dir` — the fact both guards refuse on.
///
/// Both paths are expected canonical; [`Path::starts_with`] compares whole
/// components, so `…/target/debug-old/story` is not inside `…/target/debug`.
#[must_use]
pub fn is_inside_build_dir(exe: &Path, build_dir: &Path) -> bool {
    exe.starts_with(build_dir)
}

/// Resolves `name` against `path` the way a shell would — the first entry
/// holding an executable, regular file by that name.
///
/// Nothing in this crate resolves a bare command name against `$PATH` today,
/// and no dependency in `Cargo.toml` offers it, so this is the ~15 lines that
/// would otherwise be a new dependency for one lookup. It checks the
/// executable bit it claims to check — unlike the `is_executable` SH-198 found
/// and deleted, whose doc promised exactly this and whose body never performed
/// it.
///
/// Takes `path` as a value rather than reading `$PATH` itself so a test can
/// drive it without touching the process environment. An empty entry is
/// skipped rather than read as `.`, the way a shell would: resolving `story`
/// out of whichever directory a command happened to run in is not a question
/// either guard should ask.
#[must_use]
pub fn resolve_on_path(path: Option<&OsStr>, name: &str) -> Option<InstalledStory> {
    let path = path?;
    for dir in env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(InstalledStory {
                canonical: canonicalize_or(candidate.clone(), candidate.clone()),
                spelling: candidate,
            });
        }
    }
    None
}

/// Whether `path` is a regular file with at least one executable bit set.
///
/// Unix-only permission bits, unconditionally: every target in
/// `scripts/release-targets.sh` is a Unix target, so this needs no
/// `cfg(target_os = "windows")` arm naming a platform nothing here builds for
/// (the class SH-260/SH-276 removed).
#[cfg(unix)]
#[must_use]
pub fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// [`std::fs::canonicalize`], falling back to the literal path on failure — the
/// same fallback `update`'s own exe resolution uses (`src/update.rs`), for the
/// same reason: a path that cannot be canonicalized is still the best answer
/// available, and the caller comparing it is about to fail for a better reason
/// than this function inventing a third state.
#[must_use]
pub fn canonicalize_or(path: PathBuf, fallback: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, b"#!/bin/sh\nexit 0\n").expect("writing a fake `story`");
        let mut perms = std::fs::metadata(path)
            .expect("reading metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("setting the executable bit");
    }

    #[test]
    fn resolves_the_first_executable_entry() {
        let dir = storyhook_test_support::scratch_dir();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        make_executable(&second.join("story"));
        make_executable(&first.join("story"));

        let path = env::join_paths([&first, &second]).unwrap();
        let found = resolve_on_path(Some(path.as_os_str()), "story").expect("a match");
        assert_eq!(
            found.canonical,
            std::fs::canonicalize(first.join("story")).unwrap()
        );
    }

    /// A directory entry that has nothing by that name is skipped, not fatal —
    /// the same behaviour a shell has walking `$PATH`.
    #[test]
    fn skips_an_entry_with_nothing_by_that_name() {
        let dir = storyhook_test_support::scratch_dir();
        let empty = dir.path().join("empty");
        let real = dir.path().join("real");
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::create_dir_all(&real).unwrap();
        make_executable(&real.join("story"));

        let path = env::join_paths([&empty, &real]).unwrap();
        let found = resolve_on_path(Some(path.as_os_str()), "story").expect("a match");
        assert_eq!(
            found.canonical,
            std::fs::canonicalize(real.join("story")).unwrap()
        );
    }

    /// A non-executable file by the right name is not a match — the whole
    /// point of checking the bit rather than only the name (SH-198).
    #[test]
    fn a_non_executable_file_is_not_a_match() {
        let dir = storyhook_test_support::scratch_dir();
        std::fs::write(dir.path().join("story"), b"not a program").unwrap();

        let path = env::join_paths([dir.path()]).unwrap();
        assert_eq!(resolve_on_path(Some(path.as_os_str()), "story"), None);
    }

    /// An empty `$PATH` entry is POSIX for "the current directory" — and
    /// neither guard must ever resolve `story` against whatever directory a
    /// command happened to be run from. `env::join_paths` refuses to emit an
    /// empty component at all, so this constructs the raw OS string by hand; a
    /// bare empty string is one such entry (`std::env::split_paths` yields it
    /// as a single empty component), and it is deterministic regardless of
    /// this test process's own working directory precisely because the empty
    /// entry is skipped *before* anything is ever joined against it.
    #[test]
    fn an_empty_path_entry_is_skipped_rather_than_read_as_the_current_directory() {
        assert_eq!(resolve_on_path(Some(OsStr::new("")), "story"), None);
    }

    /// The same skip, with a real match on either side — proving the empty
    /// entry is bypassed rather than merely happening to find nothing, and
    /// that a genuine entry after it is still reached.
    #[test]
    fn an_empty_entry_between_two_real_ones_does_not_stop_the_search() {
        let dir = storyhook_test_support::scratch_dir();
        make_executable(&dir.path().join("story"));
        let mut raw = std::ffi::OsString::from(":");
        raw.push(dir.path());
        assert_eq!(
            resolve_on_path(Some(raw.as_os_str()), "story").map(|found| found.canonical),
            Some(std::fs::canonicalize(dir.path().join("story")).unwrap())
        );
    }

    #[test]
    fn no_path_at_all_resolves_nothing() {
        assert_eq!(resolve_on_path(None, "story"), None);
    }

    /// Whole components, never a string prefix: a sibling directory that
    /// happens to share a prefix is not inside the build directory.
    #[test]
    fn inside_is_decided_on_whole_path_components() {
        let build = Path::new("/repo/target/debug");
        assert!(is_inside_build_dir(
            Path::new("/repo/target/debug/story"),
            build
        ));
        assert!(is_inside_build_dir(
            Path::new("/repo/target/debug/deps/story-abc"),
            build
        ));
        assert!(is_inside_build_dir(
            Path::new("/repo/target/debug/.storyhook-test-binaries/1/story"),
            build
        ));
        assert!(!is_inside_build_dir(
            Path::new("/repo/target/debug-old/story"),
            build
        ));
        assert!(!is_inside_build_dir(
            Path::new("/repo/target/release/story"),
            build
        ));
        assert!(!is_inside_build_dir(
            Path::new("/home/dev/.local/bin/story"),
            build
        ));
    }

    /// This very test binary was written by cargo, so the stamp reaches it and
    /// names a directory it sits inside — the positive control for every
    /// guard that reads [`build_dir`].
    #[test]
    fn this_test_binary_sits_inside_its_stamped_build_directory() {
        let dir = build_dir().expect("a cargo-built test binary carries STORYHOOK_BUILD_DIR");
        let running = running_exe().expect("current_exe").canonical;
        assert!(
            is_inside_build_dir(&running, &dir),
            "{} must be inside {}",
            running.display(),
            dir.display()
        );
    }

    /// The spelling is the `$PATH` entry's own `dir.join(name)`, never its
    /// realpath — the property a launchd plist depends on, since a plist
    /// outlives the build it names (SH-411, and see the module doc).
    #[test]
    fn the_spelling_is_the_path_entry_not_the_realpath() {
        let dir = storyhook_test_support::scratch_dir();
        let real = dir.path().join("real");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        make_executable(&real.join("story"));
        std::os::unix::fs::symlink(real.join("story"), bin.join("story")).unwrap();

        let path = env::join_paths([&bin]).unwrap();
        let found = resolve_on_path(Some(path.as_os_str()), "story").expect("a match");
        assert_eq!(
            found.spelling,
            bin.join("story"),
            "the spelling must be the $PATH entry, so a plist naming it survives an upgrade"
        );
        assert_eq!(
            found.canonical,
            std::fs::canonicalize(real.join("story")).unwrap(),
            "the canonical form must reach through the symlink, so comparisons agree"
        );
    }
}
