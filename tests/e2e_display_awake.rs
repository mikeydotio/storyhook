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

/// The two `--list` enumerations `scripts/run-e2e.sh` makes through
/// `scripts/e2e-selection.sh`, which open no browser: `(opener, argument,
/// ending)` -- the continued line that calls the helper, and the start and end
/// of the Playwright command line it hands over. Each slice lists itself; the
/// plan listing lists the whole selection first (SH-792).
const LISTING_ENVELOPES: [(&str, &str, &str); 2] = [
    (
        "list_output=\"$(e2e_list_selection ",
        "npx playwright test ",
        "\")\" || list_status=$?",
    ),
    (
        "plan_output=\"$(cd \"$repo_root/e2e\" && e2e_list_selection ",
        "env E2E_PLAN_LISTING=1 DASHBOARD_URL=http://plan-listing.invalid npx playwright test ",
        "\")\" || plan_status=$?",
    ),
];

/// Lines that launch a real Playwright run: every `npx playwright test`
/// that is not a `--list` enumeration (which opens no browser).
fn real_playwright_runs(script: &str) -> Vec<(usize, &str)> {
    let lines: Vec<_> = script.lines().collect();
    lines
        .iter()
        .copied()
        .enumerate()
        .filter(|(index, line)| {
            let listing_argument = index.checked_sub(1).is_some_and(|previous| {
                let opener = lines[previous].trim();
                LISTING_ENVELOPES.iter().any(|(start, argument, ending)| {
                    opener.starts_with(start)
                        && opener.ends_with('\\')
                        && line.trim_start().starts_with(argument)
                        && line.trim_end().ends_with(ending)
                        && line.matches("npx playwright test").count() == 1
                        && line.matches("||").count() == 1
                        && !line.contains(';')
                        && !line.contains("&&")
                })
            });
            line.contains("npx playwright test") && !line.contains("--list") && !listing_argument
        })
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

#[test]
fn the_listing_helper_exemption_requires_its_actual_envelope() {
    let opener = r#"list_output="$(e2e_list_selection "$data_root/list.stderr" \"#;
    let argument = r#"  npx playwright test --project="$project" "${playwright_args[@]+"${playwright_args[@]}"}")" || list_status=$?"#;
    let listing = format!("{opener}\n{argument}\n");
    assert!(real_playwright_runs(&listing).is_empty());
    let plan_opener = r#"plan_output="$(cd "$repo_root/e2e" && e2e_list_selection "$results_root/plan-listing.stderr" \"#;
    let plan_argument = r#"  env E2E_PLAN_LISTING=1 DASHBOARD_URL=http://plan-listing.invalid npx playwright test "${project_flags[@]}")" || plan_status=$?"#;
    assert!(real_playwright_runs(&format!("{plan_opener}\n{plan_argument}\n")).is_empty());
    // Each envelope's opener only exempts its own argument shape.
    assert_eq!(
        real_playwright_runs(&format!("{opener}\n{plan_argument}\n")).len(),
        1,
        "the slice listing's opener must not exempt the plan listing's argument"
    );
    for altered in [
        format!("# {opener}\n{argument}\n"),
        format!("{}\n{argument}\n", opener.trim_end_matches('\\')),
        format!("{opener}\n{argument}; npx playwright test\n"),
        format!("{listing}npx playwright test --project=chromium\n"),
    ] {
        let runs = real_playwright_runs(&altered);
        assert_eq!(
            runs.len(),
            1,
            "the listing exemption hid an actual run: {altered}"
        );
        assert!(!is_wrapped(runs[0].1));
    }
    let helper = read("scripts/e2e-selection.sh");
    assert!(
        helper.lines().any(|line| {
            !line.trim_start().starts_with('#')
                && line.contains(r#""$@" --list --reporter=list --pass-with-no-tests"#)
        }),
        "the exempt helper must still force listing-only execution"
    );
}
