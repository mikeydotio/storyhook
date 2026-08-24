//! Fences the load-grace wiring the browser suite carries for SH-347's user
//! determination (2026-08-17): *"relax the timeouts when machine is under
//! load... reset timeout timer (up to a maximum of 15 minutes) rather than
//! ending the test."*
//!
//! Weaker than most fences in this style, and its own module doc says so
//! rather than implying otherwise: a Rust file reading TypeScript can only
//! confirm the *wiring* is in place -- that the config calls into the grace
//! module rather than carrying a bare literal, that the two base budgets
//! SH-222 measured are still what they were, that exactly one file may read
//! the machine's own load average, that an extension is never silent, and
//! that every spec is actually subject to the watchdog. It cannot confirm
//! the multiplier is *right*, that the watchdog ever *fires*, that a grant
//! is ever *used*, that the ceiling is ever *reached*, or that any of this
//! makes a single WebKit test pass. Only a real run under real contention
//! answers those, which is what `e2e/specs/load-grace.spec.ts`'s executed
//! unit tests and the browser suite's own runs are for.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

/// Every tracked path under `e2e/` matching `glob_suffix` (a plain suffix
/// match, not a real glob -- this repo's own `all_specs()` helpers use the
/// same "one file extension, no wildcards inside a directory" shape).
fn tracked_e2e_files(glob_suffix: &str) -> Vec<String> {
    let listed = std::process::Command::new("git")
        .current_dir(repo_root())
        .args(["ls-files", "-z", "--", "e2e/"])
        .output()
        .expect("listing this repository's tracked e2e files");
    assert!(
        listed.status.success(),
        "`git ls-files` failed, so this scan proved nothing: {}",
        String::from_utf8_lossy(&listed.stderr)
    );
    listed
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .map(|path| std::str::from_utf8(path).expect("a UTF-8 path").to_string())
        .filter(|path| path.ends_with(glob_suffix))
        .collect()
}

#[test]
fn the_config_grants_grace_through_calls_not_bare_literals() {
    let config = read("e2e/playwright.config.ts");

    assert!(
        config.contains("from \"./load-grace\""),
        "e2e/playwright.config.ts must import ./load-grace -- without it there is nothing to \
         grant grace at all"
    );

    // Every `timeout:` assignment in the file must be a call, never a bare
    // `<digits>,`/`<digits>}` the way it was before SH-347. A positive
    // control (below) proves this scan can still fail.
    let offending: Vec<String> = timeout_assignments(&config)
        .into_iter()
        .filter(|rhs| is_bare_literal(rhs))
        .collect();
    assert!(
        offending.is_empty(),
        "e2e/playwright.config.ts assigns `timeout:` a bare numeric literal ({offending:?}) -- \
         both the test timeout and expect.timeout must call gracedBudget(...)/BASE_*_TIMEOUT_MS \
         (or the E2E_LOAD_GRACE=0 fallback), never a raw number"
    );

    assert!(
        config.contains("gracedBudget("),
        "e2e/playwright.config.ts must call gracedBudget(...) -- found neither test nor expect \
         budget routed through it"
    );
}

/// Every `timeout: <rhs>` assignment's right-hand side, up to the next `,`
/// or `}` -- mirrors `tests/dashboard_deadline_knobs.rs`'s own
/// `xhr_timeout_assignments()`, applied to this file's `timeout:` idiom
/// instead. Deliberately dumb string scanning, not a TS parser.
fn timeout_assignments(source: &str) -> Vec<String> {
    let marker = "timeout:";
    let mut found = Vec::new();
    let mut rest = source;
    while let Some(at) = rest.find(marker) {
        let after = &rest[at + marker.len()..];
        let end = after
            .find([',', '}'])
            .unwrap_or_else(|| panic!("a `timeout:` assignment with no closing `,`/`}}` in scope"));
        found.push(after[..end].trim().to_string());
        rest = &after[end..];
    }
    found
}

/// A right-hand side is a "bare literal" if it is nothing but ASCII digits
/// and underscores (`15_000`), the same rule `dashboard_deadline_knobs.rs`
/// uses for `xhr.timeout`, extended to tolerate this codebase's `_`-grouped
/// literal style.
fn is_bare_literal(rhs: &str) -> bool {
    !rhs.is_empty() && rhs.chars().all(|c| c.is_ascii_digit() || c == '_')
}

#[test]
fn the_scan_can_still_see_a_bare_config_literal() {
    // Assembled at run time, same style as this project's other positive
    // controls (SH-364, dashboard_deadline_knobs.rs), so the literal never
    // sits in this file as something the real scan could trip on.
    let offender = format!("  {}: {},", "timeout", "15_000");
    let found = timeout_assignments(&offender);
    assert_eq!(found, vec!["15_000".to_string()]);
    assert!(is_bare_literal(&found[0]));

    let clean = "  timeout: gracedBudget(BASE_TEST_TIMEOUT_MS),";
    let found = timeout_assignments(clean);
    assert_eq!(
        found,
        vec!["gracedBudget(BASE_TEST_TIMEOUT_MS)".to_string()]
    );
    assert!(!is_bare_literal(&found[0]));
}

#[test]
fn the_idle_budgets_are_still_sh_222s_measured_numbers() {
    let module = read("e2e/load-grace.ts");

    let base_test = extract_const_ms(&module, "BASE_TEST_TIMEOUT_MS");
    assert_eq!(
        base_test, 15_000,
        "BASE_TEST_TIMEOUT_MS has drifted from SH-222's measured 15_000ms test budget -- if a \
         new measurement justifies this, that is a conversation to have explicitly (this \
         assertion is the one that forces it), not a silent edit riding along with load-grace."
    );

    let base_expect = extract_const_ms(&module, "BASE_EXPECT_TIMEOUT_MS");
    assert_eq!(
        base_expect, 5_000,
        "BASE_EXPECT_TIMEOUT_MS has drifted from SH-222's measured 5_000ms expect budget -- \
         same rule as BASE_TEST_TIMEOUT_MS above."
    );

    assert!(
        module.contains("MAX_TEST_TIMEOUT_MS = 15 * 60_000"),
        "e2e/load-grace.ts's MAX_TEST_TIMEOUT_MS must still read `15 * 60_000` -- the user's own \
         determination on SH-347 (2026-08-17) named 15 minutes, expressed as an arithmetic \
         product of the two numbers actually in that sentence, not a raw millisecond literal \
         with no visible provenance."
    );
}

/// Parses `export const <name> = <digits>;` out of `source`, tolerant of the
/// `_`-separated literal style this codebase writes (`15_000`).
fn extract_const_ms(source: &str, name: &str) -> u64 {
    let marker = format!("export const {name} = ");
    let after = source
        .split_once(marker.as_str())
        .unwrap_or_else(|| panic!("e2e/load-grace.ts must declare `{marker}<n>;`"))
        .1;
    let digits: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '_')
        .collect();
    digits.replace('_', "").parse().unwrap_or_else(|error| {
        panic!("{name}'s value {digits:?} must parse as an integer: {error}")
    })
}

#[test]
fn only_load_grace_reads_the_machines_own_load_average() {
    let offenders: Vec<String> = tracked_e2e_files(".ts")
        .into_iter()
        .filter(|path| path != "e2e/load-grace.ts")
        .filter(|path| read(path).contains("os.loadavg("))
        .collect();

    assert!(
        offenders.is_empty(),
        "{offenders:?} call os.loadavg() directly. One policy, one place (the same shape \
         tests/e2e_fixture_hygiene.rs already enforces for the page clock, `only \
         support.ts pauses the clock`): every other file that needs the current contention \
         reading must import contention()/cores() from ./load-grace, not read the OS itself."
    );

    assert!(
        read("e2e/load-grace.ts").contains("os.loadavg("),
        "e2e/load-grace.ts itself no longer calls os.loadavg() -- either the policy moved \
         elsewhere (update this fence to match) or the sampling was accidentally deleted"
    );
}

#[test]
fn an_extension_is_never_silent() {
    let support = read("e2e/specs/support.ts");
    let at = support
        .find("testInfo.setTimeout(wantMs)")
        .expect("support.ts's load-grace watchdog must call testInfo.setTimeout(wantMs)");
    let window: String = support[at..].chars().take(400).collect();

    assert!(
        window.contains("annotations.push("),
        "the load-grace watchdog's testInfo.setTimeout() call must be followed by an \
         annotations.push(...) in the same block -- a grant with no machine-readable record is \
         the SH-306 shape one layer up: a gate whose verdict depends on state it never reported."
    );
    assert!(
        window.contains("process.stderr.write("),
        "the load-grace watchdog's testInfo.setTimeout() call must be followed by a \
         process.stderr.write(...) in the same block -- visible under the `list` reporter this \
         suite runs with, not only in a machine-readable artifact nobody reads live."
    );
}

#[test]
fn watchdog_resets_from_now_without_moving_the_absolute_wall_clock_ceiling() {
    let module = read("e2e/load-grace.ts");
    let support = read("e2e/specs/support.ts");

    assert!(
        module.contains("MAX_TEST_TIMEOUT_MS - elapsed")
            && module.contains("Math.max(1,")
            && !module.contains("elapsedMs)) + gracedTestBudget"),
        "resetTestBudget must grant a duration from now that shrinks to the remaining wall-clock \
         ceiling, never elapsed-plus-window (which Playwright treats as a fresh fixture duration \
         and can therefore extend forever); the floor must stay above zero because zero disables \
         Playwright timeouts"
    );
    assert!(
        support.contains("resetTestBudget(floorMs, elapsedMs, ratio)"),
        "the running watchdog must call resetTestBudget with measured elapsed time"
    );
    assert!(
        support.contains("absoluteCeilingAtMs")
            && support.contains("grantedUntilMs")
            && support.contains("remainingWallMs"),
        "the running watchdog must preserve earlier grants as absolute deadlines while clamping \
         every fresh fixture-duration reset to the same wall-clock ceiling"
    );
}

#[test]
fn every_tracked_spec_is_subject_to_the_watchdog() {
    let specs = tracked_e2e_files(".spec.ts");
    assert!(
        specs.len() >= 50,
        "expected at least 50 tracked browser specs, found {}: either most were deleted, or \
         tracked_e2e_files() no longer matches this tree's shape -- in which case this fence is \
         clearing a corpus it never read",
        specs.len()
    );

    let offenders: Vec<String> = specs
        .into_iter()
        .filter(|path| {
            let text = read(path);
            // A type-only import of `test`'s type (none of this suite's specs do this, but the
            // exemption exists for the same reason the plan names it) would not match this
            // marker; only a VALUE import of `test`/`expect` from the raw package does.
            text.contains("import { test, expect } from \"@playwright/test\"")
                || text.contains("import { expect, test } from \"@playwright/test\"")
                || text.contains(", test } from \"@playwright/test\"")
                || text.contains(", expect } from \"@playwright/test\"")
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "{offenders:?} import `test`/`expect` as VALUES directly from \"@playwright/test\" \
         rather than from \"./support\" -- that spec's tests never see the load-grace watchdog, \
         silently, which is exactly the SH-306 shape (a mechanism whose absence produces no \
         signal). A type-only `import type {{ Page }} from \"@playwright/test\"` alongside it is \
         fine and does not trip this."
    );
}

#[test]
fn the_scan_can_still_see_a_direct_playwright_test_import() {
    let offending = "import { test, expect } from \"@playwright/test\";\n";
    assert!(offending.contains("import { test, expect } from \"@playwright/test\""));

    let clean = "import { test, expect } from \"./support\";\n";
    assert!(!clean.contains("import { test, expect } from \"@playwright/test\""));
    assert!(!clean.contains(", test } from \"@playwright/test\""));
}

/// Guards `only_load_grace_reads_the_machines_own_load_average`'s own
/// exclusion list against silently growing stale if `e2e/load-grace.ts` is
/// ever renamed.
#[test]
fn load_grace_module_exists_at_the_path_this_file_assumes() {
    assert!(
        Path::new(&repo_root().join("e2e/load-grace.ts")).is_file(),
        "e2e/load-grace.ts must exist at this exact path -- every assertion in this file reads \
         it by that literal path"
    );
}
