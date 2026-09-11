//! Fences the display-awake wrapper around the browser suite's real
//! Playwright run (SH-628, 2026-09-09).
//!
//! WindowServer retains every IOSurface headless WebKit commits while all
//! displays are asleep, and aborts the whole console session -- every agent
//! session, every GUI app, the gate itself -- when the system-wide count
//! reaches 65,535. Under two concurrent suites that is about twelve minutes
//! of dark display; four such crashes in eight days motivated the fence.
//! The suite therefore runs Playwright through `caffeinate`, whose `-u`
//! turns the display on if it is off and whose `-d` holds it on for the run.
//!
//! In the `e2e_load_grace.rs` mould: a Rust file reading a shell script can
//! only confirm the *wiring* -- that the wrapper is defined with the two
//! flags that matter and that the real run, not merely the `--list` query,
//! goes through it. It cannot confirm the display actually stays on; only
//! `ioclasscount IOSurface` during a real dark-display run answers that,
//! and SH-628 records that measurement.

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()))
}

/// The array the script expands in front of Playwright. Named here once so
/// the definition check and the invocation check cannot drift apart.
const WRAPPER: &str = "keep_display_awake";

/// Lines that launch a real Playwright run: every `npx playwright test`
/// that is not the `--list` enumeration (which opens no browser).
fn real_playwright_runs(script: &str) -> Vec<(usize, &str)> {
    script
        .lines()
        .enumerate()
        .filter(|(_, line)| line.contains("npx playwright test") && !line.contains("--list"))
        .map(|(index, line)| (index + 1, line.trim()))
        .collect()
}

/// Whether one invocation line runs Playwright through the wrapper array.
/// The `[@]+` guard is the script's own `set -u`-safe idiom for a possibly
/// empty array, so the wrapper is required in that exact form.
fn is_wrapped(line: &str) -> bool {
    let expansion = format!("\"${{{WRAPPER}[@]+\"${{{WRAPPER}[@]}}\"}}\" npx playwright test");
    line.contains(&expansion)
}

#[test]
fn the_wrapper_is_caffeinate_with_display_on_and_display_hold() {
    let script = read("scripts/run-e2e.sh");
    let definition = script
        .lines()
        .find(|line| {
            line.trim_start()
                .starts_with(&format!("{WRAPPER}=(caffeinate"))
        })
        .unwrap_or_else(|| {
            panic!(
                "scripts/run-e2e.sh must define `{WRAPPER}=(caffeinate ...)` -- without it a \
                 WebKit run started while the display sleeps takes WindowServer down (SH-628)"
            )
        });
    for flag in ["-u", "-d"] {
        assert!(
            definition.split_whitespace().any(|word| word == flag),
            "`{WRAPPER}` must carry `{flag}`: `-u` turns a sleeping display on, `-d` keeps it \
             on for the run; found `{definition}`"
        );
    }
}

#[test]
fn every_real_playwright_run_goes_through_the_wrapper() {
    let script = read("scripts/run-e2e.sh");
    let runs = real_playwright_runs(&script);
    assert!(
        !runs.is_empty(),
        "scripts/run-e2e.sh no longer launches Playwright with `npx playwright test`; this fence \
         needs updating alongside whatever replaced it"
    );
    let unwrapped: Vec<String> = runs
        .iter()
        .filter(|(_, line)| !is_wrapped(line))
        .map(|(number, line)| format!("line {number}: {line}"))
        .collect();
    assert!(
        unwrapped.is_empty(),
        "every real Playwright run must expand `{WRAPPER}` in front of it (SH-628); these do not:\n{}",
        unwrapped.join("\n")
    );
}

/// A vacuous-pass guard: the scan must be able to see an unwrapped run and
/// must not be fooled by the `--list` query, or the fence above proves
/// nothing.
#[test]
fn the_scan_can_still_see_an_unwrapped_run() {
    let fake = "\
  list_output=\"$(npx playwright test --project=\"$project\" --list --reporter=list)\"
  npx playwright test --project=\"$project\" || status=$?
  \"${keep_display_awake[@]+\"${keep_display_awake[@]}\"}\" npx playwright test --project=\"$project\" || status=$?
";
    let runs = real_playwright_runs(fake);
    assert_eq!(
        runs.len(),
        2,
        "the --list query must be excluded, the two runs kept"
    );
    assert!(!is_wrapped(runs[0].1), "a bare run must read as unwrapped");
    assert!(
        is_wrapped(runs[1].1),
        "the wrapped form must read as wrapped"
    );
}
