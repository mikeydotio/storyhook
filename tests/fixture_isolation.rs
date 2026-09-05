//! No test file reaches the `story` binary or spawns a child outside the
//! shared harness.
//!
//! For most of this repository's life, forty-three test files reached it
//! directly — `assert_cmd::Command::cargo_bin("story")`, with no `.env()` at
//! all — and which store they wrote to was decided entirely by whatever wrapper
//! script happened to invoke them. Two things kept that survivable and neither
//! is a guarantee: `scripts/run-tests.sh` exported an isolated
//! `$STORYHOOK_DATA_DIR`, and `storyhook::env::is_test_build` refuses to guess
//! a store when *nothing* names one. Neither covers the case that matters most
//! — a developer with `$STORYHOOK_STORE_PATH` exported, which is exactly what
//! somebody debugging a second store has. There, the wrapper is bypassed, the
//! refusal does not fire because something *did* name a store, and every one of
//! those files writes into it.
//!
//! Those files are migrated. This is what stops the forty-fourth.
//!
//! # Why this rule, and why it needs no allowlist
//!
//! The marker is `cargo_bin("story")`, and it is the whole rule. Verified when
//! this was written: every *legitimate* raw invocation in the tree already goes
//! through `storyhook_test_support::story_binary()` — `temp_project_refusal`,
//! `project_burst_refusal`, `project_path_hygiene`, `test_build_guard` and
//! `store_isolation` all build a `std::process::Command` on it by hand, on
//! purpose, because they are testing what happens when an environment is
//! *deliberately* wrong. `plugin_install` runs a packaged copy of it. So a rule
//! naming one marker separates the two populations exactly, with no list of
//! exceptions to maintain — which matters, because a hand-kept list is the
//! shape this project has paid for six times (SH-136, SH-198, SH-258,
//! SH-260/276, SH-360, SH-364).
//!
//! `story_binary()` is not merely tolerated here, it is the *correct* door: it
//! resolves this build's own binary and asserts it is not an installed one. The
//! thing being fenced out is reaching the binary while inheriting an
//! environment nobody chose.
//!
//! SH-535 adds the process-lifetime half of the same rule. A type-based scan
//! for `std::process::Child` misses inferred locals, so the fence rejects the
//! operation that creates the hazard: a direct zero-argument `Command` spawn.
//! `ChildGuard::spawn` and `ChildGuard::spawn_with_output` are the exclusive
//! doors, making panic cleanup and a bounded wait available from the instant a
//! test creates a child.

use std::path::Path;

/// The marker, assembled at run time.
///
/// Never written as a literal in *code*, because this file is itself a tracked
/// `tests/*.rs` and the scan below reads every one of them — the same trick
/// `tests/council_citations.rs` uses for the same structural reason.
///
/// The module doc above names it anyway, in prose, because a rule that cannot
/// state what it forbids is a rule nobody can follow. That is safe only because
/// the scan strips comments first, and it was not safe until it did: this test
/// flagged **itself** the first time it ran against a tree that had it
/// committed. Which is the second lesson — a scan derived over `git ls-files`
/// cannot see its own file until that file is tracked, so it passes while it is
/// being written and fails on the commit that adds it.
fn marker() -> String {
    format!("cargo_bin(\"{}\")", "story")
}

/// The zero-argument process-spawn marker, assembled at run time so this scan
/// does not flag its own source.
fn process_spawn_marker() -> String {
    format!(".{}()", "spawn")
}

/// Every tracked `tests/*.rs`, with its text.
fn tracked_test_files() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let listed = std::process::Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--", "tests/*.rs", "crates/*/tests/*.rs"])
        .output()
        .expect("listing this repository's tracked test files");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|path| {
            let relative = std::str::from_utf8(path).expect("a UTF-8 path").to_string();
            let text = std::fs::read_to_string(root.join(&relative))
                .unwrap_or_else(|e| panic!("reading {relative}: {e}"));
            (relative, text)
        })
        .collect()
}

#[test]
fn no_test_file_reaches_the_binary_outside_the_harness() {
    let marker = marker();
    let offenders: Vec<String> = tracked_test_files()
        .into_iter()
        .filter(|(_, text)| storyhook_test_support::without_rust_comments(text).contains(&marker))
        .map(|(relative, _)| relative)
        .collect();
    assert!(
        offenders.is_empty(),
        "{offenders:?} reach the `story` binary directly. A command built that \
         way inherits whatever environment the process has, so which store it \
         writes to is decided by the wrapper script that happened to invoke the \
         test — and a developer with $STORYHOOK_STORE_PATH exported has no \
         wrapper and no refusal. Use `storyhook_test_support::TestEnv`, or \
         `story_binary()` when the point of the test IS a deliberately wrong \
         environment."
    );
}

#[test]
fn no_test_process_is_spawned_outside_child_guard() {
    let marker = process_spawn_marker();
    let offenders: Vec<String> = tracked_test_files()
        .into_iter()
        .filter(|(_, text)| storyhook_test_support::without_rust_comments(text).contains(&marker))
        .map(|(relative, _)| relative)
        .collect();
    assert!(
        offenders.is_empty(),
        "{offenders:?} spawn a process without immediately giving ownership to \
         `ChildGuard`. Use `ChildGuard::spawn` or \
         `ChildGuard::spawn_with_output`; then use its bounded wait, unless the \
         test deliberately kills and reaps the child first."
    );
}

/// The scan finds files, and the marker it looks for is one that exists.
///
/// Two controls, because the test above passes vacuously in two different ways:
/// a `git ls-files` that returned nothing, and a marker string that no longer
/// matches how anyone spells the call.
#[test]
fn the_scan_can_see_a_violation() {
    let files = tracked_test_files();
    assert!(
        files.len() > 100,
        "this scan is supposed to read every tracked test file, and it found \
         {}. The pattern is broken, not the tree.",
        files.len()
    );

    let planted = format!("let cmd = assert_cmd::Command::{}.unwrap();", marker());
    assert!(
        planted.contains(&marker()),
        "the marker no longer matches the call it is meant to find, so the scan \
         above reports a clean tree whatever the tree contains"
    );

    // …and it is not matching everything either.
    let innocent = "let cmd = TestEnv::shared().story(dir.path());";
    assert!(!innocent.contains(&marker()));

    let process_marker = process_spawn_marker();
    let planted = format!("let child = command{};", process_marker);
    assert!(
        planted.contains(&process_marker),
        "the process marker no longer matches the operation it is meant to find"
    );
}

/// The door this rule points offenders at is real and is used.
///
/// A rule that names a replacement nobody uses is a rule somebody will read as
/// aspirational. These are the files that reach the binary the sanctioned way,
/// and there must be some.
#[test]
fn the_sanctioned_door_is_the_one_in_use() {
    let deliberate: Vec<String> = tracked_test_files()
        .into_iter()
        .filter(|(_, text)| text.contains("story_binary()"))
        .map(|(relative, _)| relative)
        .collect();
    assert!(
        deliberate.len() >= 3,
        "`story_binary()` is what a test that needs the raw binary should use, \
         and only {deliberate:?} do. Either the door moved or this rule is \
         pointing at nothing."
    );

    let guarded: Vec<String> = tracked_test_files()
        .into_iter()
        .filter(|(_, text)| text.contains("ChildGuard::spawn"))
        .map(|(relative, _)| relative)
        .collect();
    assert!(
        guarded.len() >= 10,
        "`ChildGuard::spawn` is the only sanctioned process-spawn door, and \
         only {guarded:?} use it. Either the door moved or this rule is \
         pointing at nothing."
    );
}
