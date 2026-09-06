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
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "make -n {target} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        stderr
    );
    assert!(
        stderr.trim().is_empty(),
        "make -n {target} must not run a real preflight or postlude\nstderr: {stderr}"
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

/// `make test`'s deferral must say how long the browser tier has gone unrun,
/// not merely that this run skipped it (SH-418).
///
/// SH-394 added the named deferral so the reduced gate "can never silently
/// read as full coverage". It said the browser suite was skipped and nothing
/// about whether it had ever run — the same silence, one tier up, which is
/// exactly the gap SH-418 was filed for. This is the only place that fact is
/// collected with **no bootstrap**: `make browser-watch` and `make
/// merge-watch` both want a per-machine timer, and every session runs `make
/// test` before every push regardless.
///
/// The report must never gate — a merge gate that failed on the release
/// tier's staleness would undo the split SH-394 measured — and `test-full`
/// must not carry it, because there the suite is about to actually run.
#[test]
fn the_merge_gates_deferral_reports_how_stale_the_browser_tier_is() {
    let gate = dry_run("test");
    let full = dry_run("test-full");

    let deferral = gate
        .iter()
        .find(|l| l.contains("leg.sh") && l.contains("--skipped"))
        .unwrap_or_else(|| panic!("make test must print a named deferral:\n{gate:#?}"));

    assert!(
        deferral.contains("scripts/browser-status.sh"),
        "the deferral must also report the browser tier's staleness, else it \
         says only that THIS run skipped it — the SH-418 silence. Line was:\n{deferral}"
    );
    assert!(
        deferral.contains("|| true"),
        "reporting staleness must never fail the merge gate, or the reduced \
         gate starts depending on the release tier. Line was:\n{deferral}"
    );
    assert!(
        !full.iter().any(|l| l.contains("scripts/browser-status.sh")),
        "make test-full runs the suite itself, so a stale reading there would \
         be noise measured moments before it stops being true:\n{full:#?}"
    );
}

/// The detection layer's own two targets must reach their scripts (SH-418).
///
/// `make test-full` is the release gate, and until SH-418 nothing ran it
/// between releases — measured: 109 receipts on this machine, zero at `tier
/// full`. `browser-watch` is what runs it on a cadence and `browser-status`
/// is what reports the distance when nothing has. A target that stopped
/// invoking its script would restore the exact silence this story ended, and
/// would do it without failing anything — the SH-306 shape, one tier up.
///
/// This is a WIRING fence and claims nothing more: it proves the targets
/// reach the scripts, never that the scripts are right.
/// `tests/browser_gate.rs` is what provokes the behaviour, against real git.
#[test]
fn the_browser_tier_detection_targets_reach_their_scripts() {
    let watch = dry_run("browser-watch");
    let status = dry_run("browser-status");

    assert!(
        watch.iter().any(|l| l.contains("scripts/browser-watch.sh")),
        "make browser-watch must invoke scripts/browser-watch.sh, dry run was:\n{watch:#?}"
    );
    assert!(
        status
            .iter()
            .any(|l| l.contains("scripts/browser-status.sh")),
        "make browser-status must invoke scripts/browser-status.sh, dry run \
         was:\n{status:#?}"
    );
}

/// The coverage tier's own detection layer (SH-429) — the same shape as the
/// browser tier's, fenced the same way just above: a target that stopped
/// invoking its script would restore silence without failing anything.
#[test]
fn the_coverage_tier_detection_targets_reach_their_scripts() {
    let map = dry_run("coverage-map");
    let watch = dry_run("coverage-watch");
    let status = dry_run("coverage-status");

    assert!(
        map.iter().any(|l| l.contains("scripts/coverage-map.sh")),
        "make coverage-map must invoke scripts/coverage-map.sh, dry run was:\n{map:#?}"
    );
    assert!(
        watch
            .iter()
            .any(|l| l.contains("scripts/coverage-watch.sh")),
        "make coverage-watch must invoke scripts/coverage-watch.sh, dry run was:\n{watch:#?}"
    );
    assert!(
        status
            .iter()
            .any(|l| l.contains("scripts/coverage-status.sh")),
        "make coverage-status must invoke scripts/coverage-status.sh, dry run \
         was:\n{status:#?}"
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
/// same order, for both targets. The wrapper invocation is filtered because
/// its private target name is how the recursive make receives the tier.
#[test]
fn the_two_tiers_agree_on_every_leg_but_the_browser_suite_and_the_receipt() {
    let differs = |line: &str| {
        line.contains("scripts/run-e2e.sh")
            || (line.contains("leg.sh") && line.contains("--skipped"))
            || line.contains("gate-receipt.sh postlude")
            || line.contains("with-orphan-postlude.sh")
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

/// The selective tier's rust-suite leg (SH-429): `test-changed` must reach
/// `scripts/run-changed.sh`, never the full core-battery invocation `test`
/// itself uses — the whole point of the split, provoked rather than assumed.
#[test]
fn test_changed_runs_run_changed_sh_instead_of_the_full_workspace_runner() {
    let gate = dry_run("test");
    let changed = dry_run("test-changed");

    assert!(
        changed.iter().any(|l| l.contains("scripts/run-changed.sh")),
        "make test-changed must invoke scripts/run-changed.sh, dry run was:\n{changed:#?}"
    );
    assert!(
        !gate.iter().any(|l| l.contains("scripts/run-changed.sh")),
        "make test must NOT invoke scripts/run-changed.sh, dry run was:\n{gate:#?}"
    );
    assert!(
        gate.iter().any(|l| l.contains("run-rust-battery.sh core")),
        "make test must invoke the full core Rust battery, dry run was:\n{gate:#?}"
    );
    assert!(
        !changed
            .iter()
            .any(|l| l.contains("run-rust-battery.sh core")),
        "make test-changed must not ALSO run the full core Rust battery \
         directly, dry run was:\n{changed:#?}"
    );
}

/// Only the rust-suite leg and receipt postlude may differ between `test` and
/// `test-changed` — fmt, clippy, the build, and the plugin harness stay
/// unconditional (SH-429's own design: only test EXECUTION is selective,
/// compilation never is). The e2e deferral line is deliberately excluded
/// from `test-changed`'s recipe entirely (it is not a release gate), so it
/// is filtered on both sides rather than compared. Both bodies must pass
/// through the same unconditional orphan-postlude wrapper (SH-491), even
/// though the private target named on that line differs.
#[test]
fn test_changed_shares_fmt_clippy_build_and_plugin_legs_with_test() {
    let shared_leg_markers = [
        "cargo fmt --all -- --check",
        "cargo clippy --workspace --all-targets",
        "run-rust-battery.sh contracts",
        "leg.sh --reuse build -- cargo build",
        "plugins/story/tests/run-tests.sh",
        "check-no-orphan-servers.sh preflight",
        "with-orphan-postlude.sh",
        "gate-receipt.sh preflight",
    ];

    let gate = dry_run("test");
    let changed = dry_run("test-changed");

    for marker in shared_leg_markers {
        let in_gate = gate.iter().any(|l| l.contains(marker));
        let in_changed = changed.iter().any(|l| l.contains(marker));
        assert!(
            in_gate && in_changed,
            "expected leg containing '{marker}' in BOTH make test and make \
             test-changed's dry runs -- test:{in_gate} test-changed:{in_changed}\n\
             test: {gate:#?}\ntest-changed: {changed:#?}"
        );
    }
}

/// Every public test tier must put its whole fallible body behind SH-491's
/// wrapper. The private target name is load-bearing: it selects the ordinary,
/// browser-inclusive, or changed body while keeping the receipt outside the
/// wrapper and therefore success-only.
#[test]
fn every_test_tier_wraps_the_right_body_in_the_orphan_postlude() {
    for (target, body) in [
        ("test", "_test-body"),
        ("test-full", "_test-full-body"),
        ("test-changed", "_test-changed-body"),
    ] {
        let lines = dry_run(target);
        let wrappers: Vec<&String> = lines
            .iter()
            .filter(|line| line.contains("scripts/with-orphan-postlude.sh"))
            .collect();
        assert_eq!(
            wrappers.len(),
            1,
            "make {target} must invoke exactly one orphan-postlude wrapper, dry run was:\n{lines:#?}"
        );
        assert!(
            wrappers[0].ends_with(body),
            "make {target} must wrap {body}, got: {}",
            wrappers[0]
        );
        assert!(
            wrappers[0].contains("--make-no-exec --"),
            "make -n {target} must bypass the real postlude while expanding its recursive body, got: {}",
            wrappers[0]
        );
    }
}

/// `test-changed`'s postlude must read the tier `scripts/run-changed.sh`
/// actually earned from its own state file, never a hardcoded tier literal
/// the way `test`'s (`... postlude gate`) and `test-full`'s
/// (`... postlude full`) do — the whole reason `run-changed.sh` writes one
/// (`docs/spec/selective-testing.md`: the tier is honest, not aspirational).
#[test]
fn test_changed_reads_its_postlude_tier_from_the_state_file_run_changed_sh_writes() {
    let changed = dry_run("test-changed");

    // A multi-line shell compound (joined by trailing `\` in the Makefile)
    // prints as several SEPARATE lines under `make -n`, one per physical
    // recipe line -- so the state-file reference and the postlude
    // invocation itself are checked independently, not assumed to share one
    // line.
    assert!(
        changed
            .iter()
            .any(|l| l.contains("storyhook-changed-tier-args")),
        "make test-changed's postlude must read the state file \
         scripts/run-changed.sh writes, dry run was:\n{changed:#?}"
    );

    let postlude_invocation = changed
        .iter()
        .find(|l| l.contains("gate-receipt.sh postlude"))
        .unwrap_or_else(|| {
            panic!("make test-changed must certify with a postlude step:\n{changed:#?}")
        });
    assert!(
        !postlude_invocation.ends_with("postlude gate")
            && !postlude_invocation.ends_with("postlude full")
            && !postlude_invocation.ends_with("postlude changed"),
        "the postlude invocation must not hardcode a tier literal -- it must \
         be resolved from the state file at run time, got: {postlude_invocation}"
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

#[test]
fn release_sh_installs_the_plugin_owned_by_the_installed_binary() {
    let src = std::fs::read_to_string(checkout().join("scripts/release.sh"))
        .expect("reading scripts/release.sh");

    assert!(
        src.contains("run story plugin install claude"),
        "release installation must delegate to the binary that embeds the release payload"
    );
    assert!(
        !src.contains("claude plugin marketplace add \"$repo_root\""),
        "registering the checkout recreates SH-538"
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
    assert!(
        err.contains("STORYHOOK_RELEASE_UNGATED=1"),
        "the refusal must name the second confirmation, or the only way past \
         it is to edit this script during the release it is blocking: {err}"
    );
}

/// The flag alone is refused; the flag PLUS the environment variable is not.
/// Provoked rather than read, and stopped at the next preflight step so the
/// test never reaches a bump: running from a temporary directory means the
/// `git rev-parse --show-toplevel` below the gate decision refuses on its own.
///
/// What this pins is only that the ungated branch is reachable and says so.
/// It deliberately does not assert that a release happens — that would mean
/// cutting one.
#[test]
fn an_ungated_public_release_needs_the_flag_and_the_environment_variable() {
    let out = Command::new("bash")
        .arg(checkout().join("scripts/release.sh").display().to_string())
        .args(["--bump", "patch", "--skip-gate"])
        .env("STORYHOOK_RELEASE_UNGATED", "1")
        .current_dir(std::env::temp_dir())
        .output()
        .expect("running scripts/release.sh");

    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("UNGATED release because STORYHOOK_RELEASE_UNGATED=1"),
        "the ungated path must announce itself, every time, in the same run \
         that ships the bytes: {err}"
    );
    assert!(
        !err.contains("--skip-gate is only allowed with --local-only"),
        "the second confirmation must actually clear the refusal: {err}"
    );
    assert!(
        err.contains("No receipt is minted"),
        "an ungated run must say that nothing will claim the battery ran — a \
         gate's silence reading as a pass is the SH-306 shape: {err}"
    );
}
