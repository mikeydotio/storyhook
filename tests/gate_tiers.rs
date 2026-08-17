//! The gate tier split, **provoked through `make -n`** — not read as text.
//!
//! SH-394: the merge gate used to run the browser suite too, and that suite
//! is the overwhelming majority of its wall clock (`e2e/playwright.config.ts`'s
//! SH-222 measurement: 2.9-6.4 minutes per desktop project, and the 52 desktop
//! specs run twice). `make test` now covers the CLI alone; `make test-full`
//! adds the browser suite and is what `scripts/release.sh` requires before it
//! will cut a public release.
//!
//! Every fact here comes from `make -n --no-print-directory <target>` against
//! the tracked `Makefile`, run from `CARGO_MANIFEST_DIR` — the artifact that
//! ships, never a copy pasted into this file (the SH-56 lesson `tests/
//! push_gate.rs` already states for the hooks). A text scan of the Makefile
//! would drift the moment someone reordered a line; a dry run cannot, because
//! it is `make` itself answering "what would this target actually run".
//!
//! # What is asserted, and why it is derived rather than hand-listed
//!
//! `test-full`'s dry run and `test`'s must be **identical apart from exactly
//! two lines** — the one that runs (or skips) the browser suite, and the one
//! that writes the receipt's tier. Filtering those two known-different lines
//! out of both outputs and asserting the remainder is byte-for-byte equal is
//! what makes this a fence on drift rather than a snapshot of today's
//! Makefile: any OTHER difference between the tiers — a leg added to one and
//! not the other, a reordered step — fails loudly here, without this file
//! needing to know the leg's name.
//!
//! # Positive control (SH-364's lesson: an oracle nobody has tested is blind)
//!
//! [`make_dash_n_fails_on_an_unknown_target`] proves the harness this file
//! depends on — "a successful `make -n` means something" — actually holds.
//! Without it, a `make` too old to support `--no-print-directory`, or a
//! `Makefile` that stopped parsing, would make every other test in this file
//! either silently vacuous (comparing two empty outputs) or fail for the
//! wrong reason.

use std::path::Path;
use std::process::{Command, Output};

/// The checkout under test — the tracked `Makefile` and `scripts/release.sh`
/// live here, never a copy.
fn checkout() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn run(args: &[&str]) -> Output {
    Command::new("make")
        .args(args)
        .current_dir(checkout())
        .output()
        .unwrap_or_else(|e| panic!("running make {args:?}: {e}"))
}

/// The non-empty, trimmed lines `make -n` says this target would run.
fn dry_run(target: &str) -> Vec<String> {
    let out = run(&["-n", "--no-print-directory", target]);
    assert!(
        out.status.success(),
        "make -n {target} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The positive control. Without this, a `make -n` that silently returns
/// nothing (an ancient `make`, a `Makefile` that stopped parsing) would make
/// every dry-run-based assertion below pass vacuously — an oracle that always
/// says "no difference" between two empty lists is not an oracle.
#[test]
fn make_dash_n_fails_on_an_unknown_target() {
    let out = run(&["-n", "--no-print-directory", "this-target-does-not-exist"]);
    assert!(
        !out.status.success(),
        "make -n against a nonexistent target must fail, or this file's other \
         assertions cannot trust a successful dry run to mean anything"
    );
}

/// `test-full` must reach the browser suite; `test` must not, and must say
/// so loudly rather than silently covering less than a reader would assume
/// (the SH-306 shape one layer up: a green run that answers a question
/// nobody asked).
#[test]
fn test_full_runs_the_browser_suite_and_test_defers_it_loudly() {
    let gate = dry_run("test");
    let full = dry_run("test-full");

    assert!(
        full.iter().any(|l| l.contains("scripts/run-e2e.sh")),
        "make test-full must invoke scripts/run-e2e.sh, dry run was:\n{full:#?}"
    );
    assert!(
        !gate.iter().any(|l| l.contains("scripts/run-e2e.sh")),
        "make test must NOT invoke scripts/run-e2e.sh — that is the entire \
         point of the split — dry run was:\n{gate:#?}"
    );
    assert!(
        gate.iter()
            .any(|l| l.contains("leg.sh") && l.contains("--skipped")),
        "make test must print a named deferral for the leg it is not running, \
         dry run was:\n{gate:#?}"
    );
}

/// The receipt each tier writes must name that tier — `full` from
/// `test-full`, `gate` from `test` — and neither tier may write the other's.
#[test]
fn each_tier_writes_a_receipt_naming_itself() {
    let gate = dry_run("test");
    let full = dry_run("test-full");

    assert!(
        gate.iter()
            .any(|l| l.ends_with("gate-receipt.sh postlude gate")),
        "make test must certify with tier 'gate', dry run was:\n{gate:#?}"
    );
    assert!(
        !gate
            .iter()
            .any(|l| l.contains("gate-receipt.sh postlude full")),
        "make test must not certify with tier 'full', dry run was:\n{gate:#?}"
    );
    assert!(
        full.iter()
            .any(|l| l.ends_with("gate-receipt.sh postlude full")),
        "make test-full must certify with tier 'full', dry run was:\n{full:#?}"
    );
    assert!(
        !full
            .iter()
            .any(|l| l.contains("gate-receipt.sh postlude gate")),
        "make test-full must not certify with tier 'gate', dry run was:\n{full:#?}"
    );
}

/// Everything else the two tiers do must be identical. This is the fence
/// against silent divergence: filter out the two lines the design
/// deliberately varies (already pinned above) and the remainder — fmt,
/// clippy, the Rust suite, the build, the plugin harness, the orphan-server
/// brackets, the preflight — must be byte-for-byte the same list, in the
/// same order, for both targets.
#[test]
fn the_two_tiers_agree_on_every_leg_but_the_browser_suite_and_the_receipt() {
    let differs = |line: &str| {
        line.contains("scripts/run-e2e.sh")
            || (line.contains("leg.sh") && line.contains("--skipped"))
            || line.contains("gate-receipt.sh postlude")
    };

    let gate: Vec<String> = dry_run("test")
        .into_iter()
        .filter(|l| !differs(l))
        .collect();
    let full: Vec<String> = dry_run("test-full")
        .into_iter()
        .filter(|l| !differs(l))
        .collect();

    assert!(
        !gate.is_empty(),
        "the filter removed everything from make test's dry run — this fence \
         is broken, not the Makefile"
    );
    assert_eq!(
        gate, full,
        "make test and make test-full must agree on every leg except the \
         browser suite and the receipt's tier"
    );
}

/// A published release runs the full battery, not the merge gate's reduced
/// one — `scripts/release.sh`'s own header already promises this
/// (`--skip-gate` is refused outside `--local-only`); this pins which
/// `make` target actually backs that promise. A literal-string check rather
/// than a bash parse, but a precise one: `run make test-full` contains `run
/// make test` as a substring (because `test-full` starts with `test`), so a
/// regression to the narrower gate would still match a naive "contains 'make
/// test-full'" search if a second, bare `run make test` were introduced
/// alongside it. Counting exactly one occurrence of the shorter prefix rules
/// that out.
#[test]
fn release_sh_gates_public_releases_with_the_full_battery() {
    let src = std::fs::read_to_string(checkout().join("scripts/release.sh"))
        .expect("reading scripts/release.sh");

    let occurrences = src.matches("run make test").count();
    assert_eq!(
        occurrences, 1,
        "expected exactly one `run make test...` invocation in scripts/release.sh, found {occurrences}"
    );
    let idx = src.find("run make test").expect("checked above");
    let found = &src[idx..idx + "run make test-full".len()];
    assert_eq!(
        found, "run make test-full",
        "scripts/release.sh's gate step must run `make test-full`, found `{found}`"
    );
}

/// `--skip-gate` stays refused for a public release — provoked, not read.
/// This die happens before any git/filesystem preflight, so it is safe to
/// invoke from an arbitrary directory.
#[test]
fn skip_gate_is_refused_outside_local_only() {
    let out = Command::new("bash")
        .arg(checkout().join("scripts/release.sh").display().to_string())
        .args(["--bump", "patch", "--skip-gate"])
        .current_dir(std::env::temp_dir())
        .output()
        .expect("running scripts/release.sh");

    assert!(
        !out.status.success(),
        "a public release with --skip-gate must be refused"
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--local-only") && err.contains("test-full"),
        "the refusal must name both the escape hatch's real scope and the \
         battery a public release runs, got: {err}"
    );
}
